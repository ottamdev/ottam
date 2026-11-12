use super::environment::{attach_project, terminal_term, terminal_terminfo};
use super::input::SessionInput;
#[cfg(test)]
use super::output_queue::TerminalOutputQueue;
use super::output_queue::{TerminalEventSender, READ_BUFFER_BYTES};
use super::scrollback::{
    TerminalScrollback, COMPLETED_SCROLLBACK_MAX_BYTES, SCROLLBACK_MAX_BYTES,
};
use crate::core_error::CoreError;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ottam_protocol::{
    ProjectID, TerminalExitedParams, TerminalID, TerminalOutputDeltaParams, TerminalRecordSummary,
    TerminalSessionID, TerminalSessionSummary, WorkingRootSummary, WorkspaceID,
    METHOD_TERMINAL_EXITED,
};
use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::unix::AsyncFd;
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// How long a cancelled (close/shutdown) terminal read task keeps draining the
/// PTY to EOF before giving up — mirrors the old waiter's 5 s `reader_done` budget.
pub(super) const TERMINAL_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// The slice of the terminal registry owner that starts and stops sessions,
/// detached from the server that owns the registry behind `&mut self`.
///
/// Core's own flows spawn PTYs from background tasks — a sign-in that outlives
/// the request that began it cannot borrow the server. Cloning this shares the
/// same registry, so a session started through a spawner is an ordinary session:
/// `terminal.write`, `replay`, `close` and shutdown reaping all find it.
#[derive(Clone)]
pub(crate) struct TerminalSpawner {
    sessions: Arc<Mutex<HashMap<TerminalSessionID, Arc<TerminalSession>>>>,
    next_session_number: Arc<AtomicU64>,
    events: TerminalEventSender,
    handle: Handle,
    tasks: TaskTracker,
    #[cfg(test)]
    startup_failure: Option<TerminalStartupFailure>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalStartupFailure {
    PtyOpen,
    ChildSpawn,
    MasterFd,
    ReaderDup,
    WriterDup,
    ReactorRegistration,
    RegistryInsertion,
    WaiterThread,
}

/// Everything needed to start a PTY session that the `terminal.open` request
/// does not itself carry. `terminal.open` fills it with the user's shell and no
/// arguments; core's own flows name a program.
pub(crate) struct TerminalSpawnSpec {
    /// Absent for a session that belongs to no workspace — a sign-in login,
    /// which core drives and the user never lists.
    pub workspace: Option<TerminalWorkspaceIdentity>,
    /// Already resolved and canonicalized — `spawn_session` does not interpret it.
    pub cwd: PathBuf,
    pub program: String,
    pub args: Vec<String>,
    /// Applied after the terminal's own `TERM`/`TERMINFO`/`COLORTERM`, so a
    /// caller can override them deliberately.
    pub extra_env: Vec<(String, String)>,
    pub rows: u16,
    pub cols: u16,
    pub hidden: bool,
    /// Receives the child's exit status when the session is reaped. This is the
    /// only exit signal available to core itself: `terminal.exited` goes to the
    /// client, which by definition never asked for a hidden session.
    pub exit_observer: Option<tokio::sync::oneshot::Sender<Option<i32>>>,
}

/// A session's workspace, with the root it resolved to. The two always travel
/// together: a terminal that belongs to no workspace resolved no working root
/// either, so neither can be present without the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalWorkspaceIdentity {
    pub workspace_id: WorkspaceID,
    pub working_root: WorkingRootSummary,
}

/// What a session knows about itself without reference to a workspace.
///
/// The wire's `TerminalSessionSummary` keeps its required `workspaceID`, and is
/// built from this at the request boundary — where the caller named a workspace
/// and so one is always in hand. That is what lets a workspace-less login
/// terminal exist without weakening the type every ordinary terminal uses.
#[derive(Debug, Clone)]
pub(crate) struct TerminalIdentity {
    pub session_id: TerminalSessionID,
    pub terminal_id: TerminalID,
    pub cwd: String,
    pub shell: String,
    pub rows: u16,
    pub cols: u16,
}

impl TerminalIdentity {
    pub(super) fn session_summary(
        &self,
        workspace: &TerminalWorkspaceIdentity,
    ) -> TerminalSessionSummary {
        TerminalSessionSummary {
            session_id: self.session_id.clone(),
            terminal_id: self.terminal_id.clone(),
            workspace_id: workspace.workspace_id.clone(),
            working_root: workspace.working_root.clone(),
            cwd: self.cwd.clone(),
            shell: self.shell.clone(),
            rows: self.rows,
            cols: self.cols,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TerminalRuntimeStatus {
    Running,
    Exited,
}

impl TerminalRuntimeStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
        }
    }
}

struct PreparedTerminalRuntime {
    identity: TerminalIdentity,
    session: Arc<TerminalSession>,
    workspace_id: Option<WorkspaceID>,
    next_sequence: Arc<AtomicU64>,
    last_byte_offset: Arc<AtomicU64>,
    scrollback: Arc<Mutex<TerminalScrollback>>,
    exit_observer: Option<tokio::sync::oneshot::Sender<Option<i32>>>,
    cancel: CancellationToken,
}

struct TerminalRuntimeSpec {
    workspace: Option<TerminalWorkspaceIdentity>,
    cwd: PathBuf,
    shell: String,
    rows: u16,
    cols: u16,
    hidden: bool,
    exit_observer: Option<tokio::sync::oneshot::Sender<Option<i32>>>,
    session_id: TerminalSessionID,
    terminal_id: TerminalID,
}

fn build_terminal_command(
    shell: &str,
    args: &[String],
    cwd: &Path,
    extra_env: &[(String, String)],
) -> CommandBuilder {
    let mut command = CommandBuilder::new(shell);
    for argument in args {
        command.arg(argument);
    }
    command.cwd(cwd);
    // Set on every session, hidden included: a hidden terminal can still be
    // revealed, and a program started without these renders as garbage once
    // it is on screen.
    command.env("TERM", terminal_term());
    if let Some(terminfo) = terminal_terminfo() {
        command.env("TERMINFO", terminfo);
    }
    command.env("COLORTERM", "truecolor");
    command.env("TERM_PROGRAM", "Ottam");
    for (name, value) in extra_env {
        command.env(name, value);
    }
    command
}

fn build_terminal_runtime(
    pending: &mut PendingPtySession,
    spec: TerminalRuntimeSpec,
) -> PreparedTerminalRuntime {
    let TerminalRuntimeSpec {
        workspace,
        cwd,
        shell,
        rows,
        cols,
        hidden,
        exit_observer,
        session_id,
        terminal_id,
    } = spec;
    let cancel = CancellationToken::new();
    let next_sequence = Arc::new(AtomicU64::new(1));
    let last_byte_offset = Arc::new(AtomicU64::new(0));
    let scrollback = Arc::new(Mutex::new(TerminalScrollback::new(SCROLLBACK_MAX_BYTES)));
    let identity = TerminalIdentity {
        session_id,
        terminal_id,
        cwd: cwd.to_string_lossy().into_owned(),
        shell,
        rows,
        cols,
    };
    let workspace_id = workspace
        .as_ref()
        .map(|workspace| workspace.workspace_id.clone());
    let session = Arc::new(TerminalSession {
        terminal_id: identity.terminal_id.clone(),
        project_id: workspace
            .as_ref()
            .and_then(|workspace| workspace.working_root.project_id.clone()),
        workspace,
        hidden,
        title: Mutex::new("Terminal".to_string()),
        status: Mutex::new(TerminalRuntimeStatus::Running),
        exit_status: Mutex::new(None),
        identity: Mutex::new(identity.clone()),
        next_sequence: Arc::clone(&next_sequence),
        last_byte_offset: Arc::clone(&last_byte_offset),
        scrollback: Arc::clone(&scrollback),
        input: Mutex::new(Some(SessionInput::new(pending.take_write_fd()))),
        master: Mutex::new(Some(pending.take_master())),
        killer: Mutex::new(Some(pending.take_killer())),
        cancel: cancel.clone(),
    });
    pending.set_session(Arc::clone(&session));
    PreparedTerminalRuntime {
        identity,
        session,
        workspace_id,
        next_sequence,
        last_byte_offset,
        scrollback,
        exit_observer,
        cancel,
    }
}

/// Owns every fallible piece of PTY startup until the session has a waiter,
/// registry entry, and read loop. Dropping an uncommitted startup synchronously
/// retires the child before releasing its partially constructed PTY handles.
struct PendingPtySession {
    sessions: Arc<Mutex<HashMap<TerminalSessionID, Arc<TerminalSession>>>>,
    session_id: TerminalSessionID,
    child: Arc<Mutex<Option<Box<dyn Child + Send + Sync>>>>,
    waiter: Option<std::thread::JoinHandle<()>>,
    killer: Option<Box<dyn ChildKiller + Send + Sync>>,
    master: Option<Box<dyn MasterPty + Send>>,
    read_fd: Option<OwnedFd>,
    write_fd: Option<OwnedFd>,
    async_read_fd: Option<AsyncFd<OwnedFd>>,
    session: Option<Arc<TerminalSession>>,
    registered: bool,
    committed: bool,
}

impl PendingPtySession {
    fn new(
        sessions: Arc<Mutex<HashMap<TerminalSessionID, Arc<TerminalSession>>>>,
        session_id: TerminalSessionID,
        child: Box<dyn Child + Send + Sync>,
        killer: Box<dyn ChildKiller + Send + Sync>,
        master: Box<dyn MasterPty + Send>,
    ) -> Self {
        Self {
            sessions,
            session_id,
            child: Arc::new(Mutex::new(Some(child))),
            waiter: None,
            killer: Some(killer),
            master: Some(master),
            read_fd: None,
            write_fd: None,
            async_read_fd: None,
            session: None,
            registered: false,
            committed: false,
        }
    }

    fn master_fd(&self) -> Option<RawFd> {
        self.master.as_ref()?.as_raw_fd()
    }

    fn set_read_fd(&mut self, read_fd: OwnedFd) {
        self.read_fd = Some(read_fd);
    }

    fn set_write_fd(&mut self, write_fd: OwnedFd) {
        self.write_fd = Some(write_fd);
    }

    fn register_reader(&mut self) -> io::Result<()> {
        let read_fd = self
            .read_fd
            .take()
            .expect("terminal reader exists before reactor registration");
        self.async_read_fd = Some(AsyncFd::new(read_fd)?);
        Ok(())
    }

    fn take_write_fd(&mut self) -> OwnedFd {
        self.write_fd
            .take()
            .expect("terminal writer exists before session construction")
    }

    fn take_master(&mut self) -> Box<dyn MasterPty + Send> {
        self.master
            .take()
            .expect("terminal master exists before session construction")
    }

    fn take_killer(&mut self) -> Box<dyn ChildKiller + Send + Sync> {
        self.killer
            .take()
            .expect("terminal killer exists before session construction")
    }

    fn set_session(&mut self, session: Arc<TerminalSession>) {
        self.session = Some(session);
    }

    fn insert_session(&mut self) -> Result<(), CoreError> {
        let session = Arc::clone(
            self.session
                .as_ref()
                .expect("terminal session exists before registry insertion"),
        );
        self.sessions
            .lock()
            .map_err(|_| {
                CoreError::new("terminalOpenFailed", "terminal registry lock is poisoned")
                    .recoverable(true)
            })?
            .insert(self.session_id.clone(), session);
        self.registered = true;
        Ok(())
    }

    fn take_async_read_fd(&mut self) -> AsyncFd<OwnedFd> {
        self.async_read_fd
            .take()
            .expect("terminal reader is registered before read-loop launch")
    }

    fn spawn_waiter(
        &mut self,
        exit_tx: tokio::sync::oneshot::Sender<Option<i32>>,
    ) -> io::Result<()> {
        let child = Arc::clone(&self.child);
        let waiter = std::thread::Builder::new()
            .name("ottam-terminal-waiter".to_string())
            .spawn(move || {
                let child = child
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
                let status = child.and_then(|mut child| {
                    child.wait().ok().map(|status| status.exit_code() as i32)
                });
                let _ = exit_tx.send(status);
            })?;
        self.waiter = Some(waiter);
        Ok(())
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for PendingPtySession {
    fn drop(&mut self) {
        if self.committed {
            return;
        }

        if self.registered {
            if let Ok(mut sessions) = self.sessions.lock() {
                sessions.remove(&self.session_id);
            }
            self.registered = false;
        }

        if let Some(session) = self.session.take() {
            session.terminate();
        } else if let Some(mut killer) = self.killer.take() {
            let _ = killer.kill();
        }

        let child = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(mut child) = child {
            let _ = child.wait();
        }
        if let Some(waiter) = self.waiter.take() {
            let _ = waiter.join();
        }
    }
}

impl TerminalSpawner {
    pub(super) fn from_shared(
        sessions: Arc<Mutex<HashMap<TerminalSessionID, Arc<TerminalSession>>>>,
        next_session_number: Arc<AtomicU64>,
        events: TerminalEventSender,
        handle: Handle,
        tasks: TaskTracker,
    ) -> Self {
        Self {
            sessions,
            next_session_number,
            events,
            handle,
            tasks,
            #[cfg(test)]
            startup_failure: None,
        }
    }

    #[cfg(test)]
    fn with_startup_failure(mut self, failure: TerminalStartupFailure) -> Self {
        self.startup_failure = Some(failure);
        self
    }

    #[cfg(test)]
    fn should_fail_startup(&self, failure: TerminalStartupFailure) -> bool {
        self.startup_failure == Some(failure)
    }

    fn prepare_terminal_io(&self, pending: &mut PendingPtySession) -> Result<(), CoreError> {
        #[cfg(test)]
        if self.should_fail_startup(TerminalStartupFailure::MasterFd) {
            return Err(injected_terminal_startup_error(
                TerminalStartupFailure::MasterFd,
            ));
        }
        // `O_NONBLOCK` is a flag on the shared open file description, so it is
        // visible through every fd that dups the master (read fd, write fd, and
        // the master itself). Reads and the session's input writer go through
        // `AsyncFd`; `resize()` is an ioctl and is unaffected.
        let master_fd = match pending.master_fd() {
            Some(master_fd) => master_fd,
            None => {
                return Err(CoreError::new(
                    "terminalOpenFailed",
                    "terminal PTY master file descriptor is unavailable",
                )
                .recoverable(true));
            }
        };
        #[cfg(test)]
        if self.should_fail_startup(TerminalStartupFailure::ReaderDup) {
            return Err(injected_terminal_startup_error(
                TerminalStartupFailure::ReaderDup,
            ));
        }
        pending.set_read_fd(dup_nonblocking(master_fd).map_err(|error| {
            CoreError::new(
                "terminalOpenFailed",
                format!("failed to attach terminal reader: {error}"),
            )
            .recoverable(true)
        })?);
        #[cfg(test)]
        if self.should_fail_startup(TerminalStartupFailure::WriterDup) {
            return Err(injected_terminal_startup_error(
                TerminalStartupFailure::WriterDup,
            ));
        }
        pending.set_write_fd(dup_nonblocking(master_fd).map_err(|error| {
            CoreError::new(
                "terminalOpenFailed",
                format!("failed to attach terminal writer: {error}"),
            )
            .recoverable(true)
        })?);
        #[cfg(test)]
        if self.should_fail_startup(TerminalStartupFailure::ReactorRegistration) {
            return Err(injected_terminal_startup_error(
                TerminalStartupFailure::ReactorRegistration,
            ));
        }
        pending.register_reader().map_err(|error| {
            CoreError::new(
                "terminalOpenFailed",
                format!("failed to register terminal reader with the reactor: {error}"),
            )
            .recoverable(true)
        })?;
        Ok(())
    }

    fn launch_terminal_tasks(
        &self,
        pending: &mut PendingPtySession,
        runtime: PreparedTerminalRuntime,
    ) -> Result<(TerminalIdentity, Arc<TerminalSession>), CoreError> {
        #[cfg(test)]
        if self.should_fail_startup(TerminalStartupFailure::WaiterThread) {
            return Err(injected_terminal_startup_error(
                TerminalStartupFailure::WaiterThread,
            ));
        }

        // Wait for the child on a dedicated OS thread — not Tokio's blocking
        // pool, which also backs `tokio::io::stdin()`/`stdout()`, so a
        // process-lifetime wait can never starve the JSONL control plane. This
        // mirrors the old per-terminal waiter thread; the exit status is handed
        // back to the async read loop over a oneshot.
        let (exit_tx, exit_rx) = tokio::sync::oneshot::channel::<Option<i32>>();
        if let Err(error) = pending.spawn_waiter(exit_tx) {
            return Err(CoreError::new(
                "terminalOpenFailed",
                format!("failed to spawn terminal waiter thread: {error}"),
            )
            .recoverable(true));
        }

        let PreparedTerminalRuntime {
            identity,
            session,
            workspace_id,
            next_sequence,
            last_byte_offset,
            scrollback,
            exit_observer,
            cancel,
        } = runtime;
        let output_context = TerminalOutputContext {
            events: self.events.clone(),
            terminal_id: identity.terminal_id.clone(),
            workspace_id,
            session_id: identity.session_id.clone(),
            next_sequence,
            last_byte_offset,
            scrollback,
            exit_observer,
        };
        let task_sessions = Arc::clone(&self.sessions);
        let async_read_fd = pending.take_async_read_fd();
        self.tasks.spawn_on(
            terminal_read_loop(
                async_read_fd,
                exit_rx,
                output_context,
                cancel,
                task_sessions,
            ),
            &self.handle,
        );
        Ok((identity, session))
    }

    /// Start a PTY session from an already-resolved spec.
    ///
    /// `terminal.open` is one caller; core's own flows are the other, and they
    /// need a specific program rather than the user's shell. Everything the
    /// wire protocol does not carry lives in [`TerminalSpawnSpec`].
    pub(crate) async fn spawn_session(
        &self,
        spec: TerminalSpawnSpec,
    ) -> Result<TerminalIdentity, CoreError> {
        let TerminalSpawnSpec {
            workspace,
            cwd,
            program,
            args,
            extra_env,
            rows,
            cols,
            hidden,
            exit_observer,
        } = spec;
        let shell = program;
        let rows = rows.max(1);
        let cols = cols.max(1);
        let session_id = self.next_session_id();
        let terminal_id = TerminalID(uuid::Uuid::now_v7().to_string());
        validate_terminal_id(
            &terminal_id.0,
            workspace
                .as_ref()
                .and_then(|workspace| workspace.working_root.project_id.as_ref()),
        )?;
        #[cfg(test)]
        if self.should_fail_startup(TerminalStartupFailure::PtyOpen) {
            return Err(injected_terminal_startup_error(
                TerminalStartupFailure::PtyOpen,
            ));
        }
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| {
                CoreError::new(
                    "terminalOpenFailed",
                    format!("failed to open terminal PTY: {error}"),
                )
                .recoverable(true)
            })?;

        let command = build_terminal_command(&shell, &args, &cwd, &extra_env);

        #[cfg(test)]
        if self.should_fail_startup(TerminalStartupFailure::ChildSpawn) {
            return Err(injected_terminal_startup_error(
                TerminalStartupFailure::ChildSpawn,
            ));
        }
        let child = pair.slave.spawn_command(command).map_err(|error| {
            CoreError::new(
                "terminalOpenFailed",
                format!("failed to spawn terminal shell: {error}"),
            )
            .recoverable(true)
        })?;
        let killer = child.clone_killer();
        let mut pending = PendingPtySession::new(
            Arc::clone(&self.sessions),
            session_id.clone(),
            child,
            killer,
            pair.master,
        );
        self.prepare_terminal_io(&mut pending)?;

        let runtime = build_terminal_runtime(
            &mut pending,
            TerminalRuntimeSpec {
                workspace,
                cwd,
                shell,
                rows,
                cols,
                hidden,
                exit_observer,
                session_id,
                terminal_id,
            },
        );
        #[cfg(test)]
        if self.should_fail_startup(TerminalStartupFailure::RegistryInsertion) {
            return Err(injected_terminal_startup_error(
                TerminalStartupFailure::RegistryInsertion,
            ));
        }
        pending.insert_session()?;

        let (identity, _session) = self.launch_terminal_tasks(&mut pending, runtime)?;
        pending.commit();
        Ok(identity)
    }

    fn next_session_id(&self) -> TerminalSessionID {
        let sequence = self.next_session_number.fetch_add(1, Ordering::Relaxed);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        TerminalSessionID(format!("terminal_local_{now:x}_{sequence:x}"))
    }

    /// Total bytes this session has written to its PTY so far, or `None` once
    /// the session is gone.
    ///
    /// A program that has stopped producing output has either finished its work
    /// or is waiting for someone to type — core cannot tell which from the bytes
    /// themselves, but it can tell that nothing is happening, which is what the
    /// sign-in reveal timer is built on.
    pub(crate) fn output_bytes(&self, session_id: &TerminalSessionID) -> Option<u64> {
        let sessions = self.sessions.lock().ok()?;
        let session = sessions.get(session_id)?;
        Some(session.last_byte_offset.load(Ordering::Relaxed))
    }

    /// Kill a session and drop it from the registry, by id.
    ///
    /// Idempotent — a session that already exited or was closed is simply
    /// absent, which is the normal case for a login the user completed.
    pub(crate) fn close_session(&self, session_id: &TerminalSessionID) {
        let session = match self.sessions.lock() {
            Ok(mut sessions) => sessions.remove(session_id),
            Err(_) => None,
        };
        if let Some(session) = session {
            session.terminate();
        }
    }
}

#[cfg(test)]
fn injected_terminal_startup_error(failure: TerminalStartupFailure) -> CoreError {
    CoreError::new(
        "terminalOpenFailed",
        format!("injected terminal startup failure at {failure:?}"),
    )
    .recoverable(true)
}

pub(super) struct TerminalSession {
    pub(super) terminal_id: TerminalID,
    /// Absent for a session core drives on nobody's behalf — a sign-in login.
    pub(super) workspace: Option<TerminalWorkspaceIdentity>,
    pub(super) project_id: Option<ProjectID>,
    /// Withheld from `terminal.list`. Everything else — write, resize, replay,
    /// close, shutdown reaping — stays reachable by id, which is what lets a
    /// hidden session be revealed in place rather than restarted.
    pub(super) hidden: bool,
    pub(super) title: Mutex<String>,
    pub(super) status: Mutex<TerminalRuntimeStatus>,
    exit_status: Mutex<Option<i32>>,
    pub(super) identity: Mutex<TerminalIdentity>,
    pub(super) next_sequence: Arc<AtomicU64>,
    pub(super) last_byte_offset: Arc<AtomicU64>,
    pub(super) scrollback: Arc<Mutex<TerminalScrollback>>,
    // `terminal.write` input: a non-blocking dup of the master fd (the OFD is
    // shared with the read fd), drained by a per-session writer task.
    pub(super) input: Mutex<Option<SessionInput>>,
    pub(super) master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    killer: Mutex<Option<Box<dyn ChildKiller + Send + Sync>>>,
    pub(super) cancel: CancellationToken,
}

impl TerminalSession {
    pub(super) fn terminate(&self) {
        if let Ok(mut killer) = self.killer.lock() {
            if let Some(mut killer) = killer.take() {
                let _ = killer.kill();
            }
        }
        self.cancel.cancel();
        self.release_runtime_handles();
    }

    fn release_runtime_handles(&self) {
        if let Ok(mut input) = self.input.lock() {
            input.take();
        }
        if let Ok(mut master) = self.master.lock() {
            master.take();
        }
        if let Ok(mut killer) = self.killer.lock() {
            killer.take();
        }
    }

    /// True when this session belongs to `workspace_id`. A workspace-less
    /// session belongs to none, so it never matches — which is what keeps a
    /// login terminal out of `terminal.list` and out of workspace teardown.
    pub(super) fn belongs_to(&self, workspace_id: &WorkspaceID) -> bool {
        self.workspace
            .as_ref()
            .is_some_and(|workspace| workspace.workspace_id == *workspace_id)
    }

    /// Takes the workspace the caller already matched, rather than reading it
    /// back off the session: only a listed terminal has one.
    pub(super) fn record_summary(
        &self,
        workspace: &TerminalWorkspaceIdentity,
    ) -> Result<TerminalRecordSummary, CoreError> {
        let identity = self
            .identity
            .lock()
            .map_err(|_| {
                CoreError::new("terminalListFailed", "terminal identity lock is poisoned")
                    .recoverable(true)
            })?
            .clone();
        let title = self
            .title
            .lock()
            .map_err(|_| {
                CoreError::new("terminalListFailed", "terminal title lock is poisoned")
                    .recoverable(true)
            })?
            .clone();
        let status = *self.status.lock().map_err(|_| {
            CoreError::new("terminalListFailed", "terminal status lock is poisoned")
                .recoverable(true)
        })?;
        let exit_status = *self.exit_status.lock().map_err(|_| {
            CoreError::new(
                "terminalListFailed",
                "terminal exit status lock is poisoned",
            )
            .recoverable(true)
        })?;
        Ok(TerminalRecordSummary {
            terminal_id: self.terminal_id.clone(),
            workspace_id: workspace.workspace_id.clone(),
            working_root: workspace.working_root.clone(),
            runtime_session_id: (status == TerminalRuntimeStatus::Running)
                .then_some(identity.session_id),
            title,
            cwd: identity.cwd,
            shell: identity.shell,
            rows: identity.rows,
            cols: identity.cols,
            status: status.as_str().to_string(),
            last_sequence: self.next_sequence.load(Ordering::Relaxed).saturating_sub(1),
            last_byte_offset: self.last_byte_offset.load(Ordering::Relaxed),
            exit_status,
        })
    }

    fn is_exited(&self) -> bool {
        self.status
            .lock()
            .map(|status| *status == TerminalRuntimeStatus::Exited)
            .unwrap_or(false)
    }
}

struct TerminalOutputContext {
    events: TerminalEventSender,
    terminal_id: TerminalID,
    /// Absent for a session that belongs to no workspace (a sign-in login).
    workspace_id: Option<WorkspaceID>,
    session_id: TerminalSessionID,
    next_sequence: Arc<AtomicU64>,
    last_byte_offset: Arc<AtomicU64>,
    scrollback: Arc<Mutex<TerminalScrollback>>,
    exit_observer: Option<tokio::sync::oneshot::Sender<Option<i32>>>,
}

/// Per-session async read loop on the tokio reactor. Reads the PTY master via
/// `AsyncFd` until EOF (or a bounded drain after cancellation), then reaps the
/// child for its exit status. Replaces the old dedicated reader + waiter threads
/// with a single task: draining and reaping happen in order in the same place.
async fn terminal_read_loop(
    async_fd: AsyncFd<OwnedFd>,
    mut exit_rx: tokio::sync::oneshot::Receiver<Option<i32>>,
    context: TerminalOutputContext,
    cancel: CancellationToken,
    sessions: Arc<Mutex<HashMap<TerminalSessionID, Arc<TerminalSession>>>>,
) {
    // The dedicated waiter thread (spawned in `open`) reports the child's exit
    // over `exit_rx`. Watching it concurrently — rather than relying on PTY EOF
    // alone — lets us still reap when a background process inherits the slave and
    // holds the master open past the shell's own exit (the Linux hang case).
    // `Some` once the child has exited, carrying its captured exit status.
    let mut child_exit: Option<Option<i32>> = None;

    let mut buffer = [0u8; READ_BUFFER_BYTES];
    // Set once cancellation or child-exit begins the drain: a single *total*
    // deadline, not a per-read timeout — otherwise a grandchild that keeps the
    // PTY open and chatters could reset the budget on every read and never let
    // us reap.
    let mut drain_deadline: Option<tokio::time::Instant> = None;
    loop {
        // One PTY read, driven by the reactor. The readiness guard is dropped
        // inside this future so a slow consumer never holds readiness.
        //
        // Do not try to coalesce reads here. The PTY buffer is 1024 bytes on
        // macOS, so a read never returns more than that and, once drained,
        // nothing more is buffered until the shell is rescheduled — an
        // opportunistic top-up measured as a no-op (mean chunk 686 B vs 688 B),
        // and a top-up that waited would add latency to interactive echo.
        let read_fut = async {
            loop {
                let mut guard = async_fd.readable().await?;
                match guard.try_io(|inner| {
                    let fd = inner.get_ref().as_raw_fd();
                    let count = unsafe {
                        libc::read(fd, buffer.as_mut_ptr() as *mut libc::c_void, buffer.len())
                    };
                    if count < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(count as usize)
                    }
                }) {
                    Ok(result) => return result,
                    Err(_would_block) => continue,
                }
            }
        };

        let read_result: io::Result<usize> = if let Some(deadline) = drain_deadline {
            match tokio::time::timeout_at(deadline, read_fut).await {
                Ok(result) => result,
                Err(_elapsed) => break, // total drain budget exhausted
            }
        } else {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    // Begin a bounded drain to EOF; do not preempt buffered
                    // output. The deadline is fixed here so the whole drain is
                    // bounded, regardless of how much output keeps arriving.
                    drain_deadline = Some(tokio::time::Instant::now() + TERMINAL_DRAIN_TIMEOUT);
                    continue;
                }
                status = &mut exit_rx, if child_exit.is_none() => {
                    // The waiter thread reported the child exit; drain whatever
                    // it left buffered in the PTY (bounded), then reap.
                    child_exit = Some(status.ok().flatten());
                    drain_deadline = Some(tokio::time::Instant::now() + TERMINAL_DRAIN_TIMEOUT);
                    continue;
                }
                result = read_fut => result,
            }
        };

        let count = match read_result {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => {
                tracing::debug!(error = %error, "terminal reader ended");
                break;
            }
        };
        process_terminal_chunk(&context, &buffer[..count]);
    }

    reap_terminal(exit_rx, child_exit, context, sessions).await;
}

fn process_terminal_chunk(context: &TerminalOutputContext, data: &[u8]) {
    let sequence = context.next_sequence.fetch_add(1, Ordering::Relaxed);
    let byte_start = context
        .last_byte_offset
        .fetch_add(data.len() as u64, Ordering::Relaxed);
    let byte_end = byte_start.saturating_add(data.len() as u64);
    if let Ok(mut scrollback) = context.scrollback.lock() {
        scrollback.append(sequence, byte_start, data);
    } else {
        tracing::warn!(
            terminal_session_id = %context.session_id.0,
            terminal_id = %context.terminal_id.0,
            "terminal scrollback lock is poisoned"
        );
    }
    context
        .events
        .send_terminal_output_delta(TerminalOutputDeltaParams {
            session_id: context.session_id.clone(),
            terminal_id: Some(context.terminal_id.clone()),
            workspace_id: context.workspace_id.clone(),
            sequence,
            byte_start: Some(byte_start),
            byte_end: Some(byte_end),
            data_base64: STANDARD.encode(data),
            cap_reached: false,
            replay: false,
            delivery_state: None,
            dropped_bytes: None,
            dropped_chunks: None,
            replay_suggested: false,
        });
}

/// Runs once after the read loop ends (EOF, child exit, or bounded-drain
/// timeout): resolve the child's exit status, retain the completed session for
/// replay until the client closes it, and emit `terminal.exited`. The exit notification is suppressed on shutdown
/// because `close_all` has already closed the event sender by then.
async fn reap_terminal(
    exit_rx: tokio::sync::oneshot::Receiver<Option<i32>>,
    child_exit: Option<Option<i32>>,
    mut context: TerminalOutputContext,
    sessions: Arc<Mutex<HashMap<TerminalSessionID, Arc<TerminalSession>>>>,
) {
    // Resolve the exit status BEFORE removing the session, so a terminal whose
    // PTY closed while the child is still alive stays visible until it actually
    // exits — matching the old waiter, which removed the session only after
    // `child.wait()` returned.
    let exit_status = match child_exit {
        // The read loop already observed the child exit via the exit branch.
        Some(status) => status,
        // Loop ended on EOF, read error, or a cancel-initiated drain before the
        // child exited; wait for the waiter thread to report the real status.
        None => exit_rx.await.ok().flatten(),
    };
    let session = sessions
        .lock()
        .ok()
        .and_then(|sessions| sessions.get(&context.session_id).cloned());
    if let Some(session) = session {
        if let Ok(mut status) = session.status.lock() {
            *status = TerminalRuntimeStatus::Exited;
        }
        if let Ok(mut stored_exit_status) = session.exit_status.lock() {
            *stored_exit_status = exit_status;
        }
        session.release_runtime_handles();
    }
    enforce_completed_scrollback_budget(
        &sessions,
        &context.session_id,
        COMPLETED_SCROLLBACK_MAX_BYTES,
    );
    if let Some(observer) = context.exit_observer.take() {
        let _ = observer.send(exit_status);
    }
    context.events.send_notification(
        METHOD_TERMINAL_EXITED,
        TerminalExitedParams {
            session_id: context.session_id.clone(),
            workspace_id: context.workspace_id,
            terminal_id: context.terminal_id,
            exit_status,
        },
    );
}

fn enforce_completed_scrollback_budget(
    sessions: &Arc<Mutex<HashMap<TerminalSessionID, Arc<TerminalSession>>>>,
    newest_session_id: &TerminalSessionID,
    max_bytes: usize,
) {
    let completed = sessions
        .lock()
        .map(|sessions| {
            sessions
                .iter()
                .filter(|(_, session)| session.is_exited())
                .map(|(id, session)| (id.clone(), Arc::clone(session)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut total = completed.iter().fold(0usize, |total, (_, session)| {
        total.saturating_add(
            session
                .scrollback
                .lock()
                .ok()
                .map_or(0, |buffer| buffer.byte_count),
        )
    });
    if total <= max_bytes {
        return;
    }
    // Preserve the session that just exited until older completed buffers have
    // been discarded. Hash-map order is sufficient for the older set: the
    // invariant is a hard aggregate cap, while every retained session keeps its
    // exact sequence bounds.
    for (session_id, session) in completed
        .iter()
        .filter(|(session_id, _)| session_id != newest_session_id)
        .chain(
            completed
                .iter()
                .filter(|(session_id, _)| session_id == newest_session_id),
        )
    {
        let _ = session_id;
        if total <= max_bytes {
            break;
        }
        if let Ok(mut scrollback) = session.scrollback.lock() {
            total = total.saturating_sub(scrollback.clear_for_completed_budget());
        }
    }
}

/// Duplicate a fd and set `O_NONBLOCK` on the (shared) open file description.
fn dup_nonblocking(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: `dup` returns a fresh owned fd or -1; we take ownership immediately
    // so it is closed on any early return.
    let duplicated = unsafe { libc::dup(fd) };
    if duplicated < 0 {
        return Err(io::Error::last_os_error());
    }
    let owned = unsafe { OwnedFd::from_raw_fd(duplicated) };
    // SAFETY: `owned` holds a valid fd for the duration of these fcntl calls.
    let flags = unsafe { libc::fcntl(duplicated, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(duplicated, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(owned)
}

pub(super) fn terminal_not_found(terminal_id: &TerminalID) -> CoreError {
    CoreError::new(
        "terminalNotFound",
        format!("terminal not found: {}", terminal_id.0),
    )
    .recoverable(true)
}

fn validate_terminal_id(
    terminal_id: &str,
    project_id: Option<&ProjectID>,
) -> Result<(), CoreError> {
    let is_valid = !terminal_id.is_empty()
        && terminal_id.len() <= 128
        && terminal_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if is_valid {
        return Ok(());
    }
    Err(attach_project(
        CoreError::new(
            "terminalIDInvalid",
            format!(
                "invalid terminalID {terminal_id:?}; expected ASCII letters, digits, '_' or '-'"
            ),
        )
        .recoverable(true),
        project_id,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ottam_protocol::{WorkingRootAvailability, WorkingRootKind};

    #[derive(Debug, Clone)]
    struct RecordingChildKiller {
        lifecycle: Arc<(Mutex<Vec<&'static str>>, std::sync::Condvar)>,
    }

    impl ChildKiller for RecordingChildKiller {
        fn kill(&mut self) -> io::Result<()> {
            let (events, wake) = &*self.lifecycle;
            events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push("kill");
            wake.notify_all();
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(self.clone())
        }
    }

    #[derive(Debug)]
    struct RecordingChild {
        killer: RecordingChildKiller,
    }

    impl ChildKiller for RecordingChild {
        fn kill(&mut self) -> io::Result<()> {
            self.killer.kill()
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(self.killer.clone())
        }
    }

    impl Child for RecordingChild {
        fn try_wait(&mut self) -> io::Result<Option<portable_pty::ExitStatus>> {
            Ok(None)
        }

        fn wait(&mut self) -> io::Result<portable_pty::ExitStatus> {
            let (events, wake) = &*self.killer.lifecycle;
            let mut events = events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            while !events.contains(&"kill") {
                events = wake
                    .wait(events)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            events.push("wait");
            Ok(portable_pty::ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    fn pending_session(
        lifecycle: Arc<(Mutex<Vec<&'static str>>, std::sync::Condvar)>,
    ) -> PendingPtySession {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("opens test PTY");
        let killer = RecordingChildKiller { lifecycle };
        PendingPtySession::new(
            Arc::new(Mutex::new(HashMap::new())),
            TerminalSessionID("terminal_pending_test".to_string()),
            Box::new(RecordingChild {
                killer: killer.clone(),
            }),
            Box::new(killer),
            pair.master,
        )
    }

    fn lifecycle_events(
        lifecycle: &Arc<(Mutex<Vec<&'static str>>, std::sync::Condvar)>,
    ) -> Vec<&'static str> {
        lifecycle
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    #[test]
    fn pending_session_drop_kills_then_waits_exactly_once() {
        let lifecycle = Arc::new((Mutex::new(Vec::new()), std::sync::Condvar::new()));

        drop(pending_session(Arc::clone(&lifecycle)));

        assert_eq!(lifecycle_events(&lifecycle), vec!["kill", "wait"]);
    }

    #[tokio::test]
    async fn committed_pending_session_leaves_cleanup_to_runtime_owners() {
        let lifecycle = Arc::new((Mutex::new(Vec::new()), std::sync::Condvar::new()));
        let mut pending = pending_session(Arc::clone(&lifecycle));
        let mut runtime_killer = pending.take_killer();
        let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
        pending.spawn_waiter(exit_tx).expect("spawns test waiter");

        pending.commit();
        assert!(
            lifecycle_events(&lifecycle).is_empty(),
            "commit must disarm guard cleanup"
        );

        runtime_killer.kill().expect("runtime kills child");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), exit_rx)
                .await
                .expect("waiter reports exit before timeout")
                .expect("waiter reports exit"),
            Some(0)
        );
        assert_eq!(lifecycle_events(&lifecycle), vec!["kill", "wait"]);
    }

    fn startup_spec(
        root: &Path,
        exit_observer: tokio::sync::oneshot::Sender<Option<i32>>,
    ) -> TerminalSpawnSpec {
        TerminalSpawnSpec {
            workspace: Some(TerminalWorkspaceIdentity {
                workspace_id: WorkspaceID("workspace_hidden_test".to_string()),
                working_root: WorkingRootSummary {
                    kind: WorkingRootKind::WorkspaceFiles,
                    display_name: "Hidden test".to_string(),
                    canonical_path: root.to_string_lossy().into_owned(),
                    project_id: None,
                    source_id: None,
                    availability: WorkingRootAvailability::Available,
                },
            }),
            cwd: root.to_path_buf(),
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "sleep 30".to_string()],
            extra_env: Vec::new(),
            rows: 24,
            cols: 80,
            hidden: true,
            exit_observer: Some(exit_observer),
        }
    }

    #[tokio::test]
    async fn terminal_startup_failures_leave_no_registered_session_or_task() {
        let stages = [
            TerminalStartupFailure::PtyOpen,
            TerminalStartupFailure::ChildSpawn,
            TerminalStartupFailure::MasterFd,
            TerminalStartupFailure::ReaderDup,
            TerminalStartupFailure::WriterDup,
            TerminalStartupFailure::ReactorRegistration,
            TerminalStartupFailure::RegistryInsertion,
            TerminalStartupFailure::WaiterThread,
        ];

        for stage in stages {
            let root = tempfile::TempDir::new().expect("temp root");
            let (outbound, mut outbound_rx) = crate::server::test_sink();
            let events =
                TerminalEventSender::new(outbound, TerminalOutputQueue::new(READ_BUFFER_BYTES));
            let sessions = Arc::new(Mutex::new(HashMap::new()));
            let tasks = TaskTracker::new();
            let spawner = TerminalSpawner::from_shared(
                Arc::clone(&sessions),
                Arc::new(AtomicU64::new(1)),
                events,
                Handle::current(),
                tasks.clone(),
            )
            .with_startup_failure(stage);
            let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
            let error = spawner
                .spawn_session(startup_spec(root.path(), exit_tx))
                .await
                .expect_err("the configured startup stage must fail");

            assert_eq!(error.kind, "terminalOpenFailed", "stage: {stage:?}");
            assert!(error.recoverable, "stage: {stage:?}");
            assert!(
                sessions.lock().expect("sessions lock").is_empty(),
                "stage {stage:?} left a registered terminal"
            );
            assert_eq!(
                tasks.len(),
                0,
                "stage {stage:?} started a terminal read task"
            );
            assert!(
                exit_rx.await.is_err(),
                "stage {stage:?} emitted an exit observation"
            );
            assert!(
                outbound_rx.try_recv().is_err(),
                "stage {stage:?} emitted a protocol notification"
            );
        }
    }
}
