use super::environment::{canonicalize_directory, default_shell, resolve_cwd};
use super::input::{EnqueueError, TERMINAL_INPUT_QUEUE_BYTES};
use super::output_queue::TerminalEventSender;
use super::pty::{terminal_not_found, TerminalRuntimeStatus, TerminalSession, TERMINAL_DRAIN_TIMEOUT};
use super::{TerminalIdentity, TerminalSpawnSpec, TerminalSpawner, TerminalWorkspaceIdentity};
use crate::core_error::CoreError;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ottam_protocol::{
    ProjectID, TerminalCloseParams, TerminalEmptyResult, TerminalID, TerminalListResult,
    TerminalOpenParams, TerminalReplayResult, TerminalResizeParams, TerminalSessionID,
    TerminalSessionSummary, TerminalWriteParams, WorkingRootSummary, WorkspaceID,
};
use portable_pty::PtySize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use tokio::runtime::Handle;
use tokio_util::task::TaskTracker;

pub struct TerminalManager {
    sessions: Arc<Mutex<HashMap<TerminalSessionID, Arc<TerminalSession>>>>,
    next_session_number: Arc<AtomicU64>,
    events: TerminalEventSender,
    handle: Handle,
    tasks: TaskTracker,
}

impl TerminalManager {
    pub fn new(events: TerminalEventSender, handle: Handle) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            next_session_number: Arc::new(AtomicU64::new(1)),
            events,
            handle,
            tasks: TaskTracker::new(),
        }
    }

    pub async fn open(
        &mut self,
        params: TerminalOpenParams,
        working_root: WorkingRootSummary,
        root: &Path,
    ) -> Result<TerminalSessionSummary, CoreError> {
        let root = canonicalize_directory(root).map_err(|message| {
            CoreError::new("terminalCwdInvalid", message)
                .with_path(root.to_string_lossy())
                .recoverable(true)
        })?;
        let cwd = resolve_cwd(
            params.cwd.as_deref(),
            &root,
            working_root.project_id.as_ref(),
        )?;
        let workspace = TerminalWorkspaceIdentity {
            workspace_id: params.workspace_id,
            working_root,
        };
        let identity = self
            .spawn_session(TerminalSpawnSpec {
                workspace: Some(workspace.clone()),
                cwd,
                program: default_shell(),
                args: Vec::new(),
                extra_env: Vec::new(),
                rows: params.rows,
                cols: params.cols,
                hidden: false,
                exit_observer: None,
            })
            .await?;
        Ok(identity.session_summary(&workspace))
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
        self.spawner().spawn_session(spec).await
    }

    /// A detachable handle onto this manager's session registry.
    pub(crate) fn spawner(&self) -> TerminalSpawner {
        TerminalSpawner::from_shared(
            Arc::clone(&self.sessions),
            Arc::clone(&self.next_session_number),
            self.events.clone(),
            self.handle.clone(),
            self.tasks.clone(),
        )
    }

    #[cfg(test)]
    pub(super) fn shared_sessions(
        &self,
    ) -> Arc<Mutex<HashMap<TerminalSessionID, Arc<TerminalSession>>>> {
        Arc::clone(&self.sessions)
    }

    pub fn write(&self, params: TerminalWriteParams) -> Result<TerminalEmptyResult, CoreError> {
        let bytes = STANDARD
            .decode(params.data_base64.as_bytes())
            .map_err(|error| {
                CoreError::new(
                    "terminalWriteInvalid",
                    format!("invalid terminal write dataBase64: {error}"),
                )
                .recoverable(true)
            })?;
        let session = self.session(&params.session_id)?;
        // Queue and return: the session's writer drains into the PTY as it
        // becomes writable, so a program that stops reading stdin stalls only
        // its own input, not the dispatcher. Success means accepted, in order.
        let mut input = session.input.lock().map_err(|_| {
            CoreError::new("terminalWriteFailed", "terminal writer lock is poisoned")
                .recoverable(true)
        })?;
        let input = input.as_mut().ok_or_else(|| {
            CoreError::new(
                "unknownTerminalSession",
                format!("unknown terminal sessionID: {}", params.session_id.0),
            )
            .recoverable(true)
        })?;
        let write_failed = |detail: String| {
            CoreError::new(
                "terminalWriteFailed",
                format!("failed to write terminal input: {detail}"),
            )
            .recoverable(true)
        };
        let writer = input
            .writer(&session.cancel, &self.tasks, &self.handle)
            .map_err(|error| write_failed(error.to_string()))?;
        writer.enqueue(bytes).map_err(|error| match error {
            EnqueueError::Busy => CoreError::new(
                "terminalInputBusy",
                format!(
                    "terminal input queue is full ({TERMINAL_INPUT_QUEUE_BYTES} bytes); \
                     the program is not reading its input"
                ),
            )
            .recoverable(true),
            EnqueueError::Closed => write_failed("the terminal input writer stopped".to_string()),
        })?;
        Ok(TerminalEmptyResult {})
    }

    pub async fn resize(
        &self,
        params: TerminalResizeParams,
    ) -> Result<TerminalEmptyResult, CoreError> {
        let session = self.session(&params.session_id)?;
        let size = PtySize {
            rows: params.rows.max(1),
            cols: params.cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        };
        {
            let master = session.master.lock().map_err(|_| {
                CoreError::new("terminalResizeFailed", "terminal PTY lock is poisoned")
                    .recoverable(true)
            })?;
            let master = master.as_ref().ok_or_else(|| {
                CoreError::new(
                    "unknownTerminalSession",
                    format!("unknown terminal sessionID: {}", params.session_id.0),
                )
                .recoverable(true)
            })?;
            master.resize(size).map_err(|error| {
                CoreError::new(
                    "terminalResizeFailed",
                    format!("failed to resize terminal: {error}"),
                )
                .recoverable(true)
            })?;
        }
        if let Ok(mut identity) = session.identity.lock() {
            identity.rows = params.rows.max(1);
            identity.cols = params.cols.max(1);
        }
        Ok(TerminalEmptyResult {})
    }

    pub fn close(&mut self, params: TerminalCloseParams) -> Result<TerminalEmptyResult, CoreError> {
        let session = self
            .sessions
            .lock()
            .map_err(|_| {
                CoreError::new("terminalCloseFailed", "terminal registry lock is poisoned")
                    .recoverable(true)
            })?
            .remove(&params.session_id)
            .ok_or_else(|| {
                CoreError::new(
                    "unknownTerminalSession",
                    format!("unknown terminal sessionID: {}", params.session_id.0),
                )
                .recoverable(true)
            })?;
        // Kill first: the child dies, its slave closes, the master EOFs, and the
        // read task drains to EOF before reaping. `cancel` is only the bounded
        // fallback for a wedged grandchild that holds the PTY open.
        session.terminate();
        Ok(TerminalEmptyResult {})
    }

    pub fn close_terminal_id(
        &mut self,
        workspace_id: &WorkspaceID,
        terminal_id: &TerminalID,
    ) -> Result<(), CoreError> {
        let session = self
            .sessions
            .lock()
            .map_err(|_| {
                CoreError::new("terminalCloseFailed", "terminal registry lock is poisoned")
                    .recoverable(true)
            })?
            .iter()
            .find(|(_, session)| {
                session.belongs_to(workspace_id) && session.terminal_id == *terminal_id
            })
            .map(|(session_id, session)| (session_id.clone(), Arc::clone(session)));

        let Some((session_id, session)) = session else {
            return Ok(());
        };
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(&session_id);
        }
        session.terminate();
        Ok(())
    }

    pub fn list(&self, workspace_id: &WorkspaceID) -> Result<TerminalListResult, CoreError> {
        let sessions = self.sessions.lock().map_err(|_| {
            CoreError::new("terminalListFailed", "terminal registry lock is poisoned")
                .recoverable(true)
        })?;
        let mut terminals = sessions
            .values()
            // Hidden sessions belong to a flow core is driving, not to the
            // user's terminal list; showing one would offer a tab whose input
            // nobody owns.
            .filter_map(|session| {
                session
                    .workspace
                    .as_ref()
                    .filter(|workspace| workspace.workspace_id == *workspace_id)
                    .map(|workspace| (session, workspace))
            })
            .filter(|(session, _)| !session.hidden)
            .map(|(session, workspace)| session.record_summary(workspace))
            .collect::<Result<Vec<_>, _>>()?;
        terminals.sort_by(|left, right| left.terminal_id.0.cmp(&right.terminal_id.0));
        Ok(TerminalListResult { terminals })
    }

    pub fn update_metadata(
        &self,
        workspace_id: &WorkspaceID,
        terminal_id: &TerminalID,
        title: Option<&str>,
    ) -> Result<TerminalEmptyResult, CoreError> {
        let session = self
            .session_for_terminal_id(Some(workspace_id), terminal_id)?
            .ok_or_else(|| terminal_not_found(terminal_id))?;
        let Some(title) = title.map(str::trim).filter(|title| !title.is_empty()) else {
            return Ok(TerminalEmptyResult {});
        };
        *session.title.lock().map_err(|_| {
            CoreError::new("terminalUpdateFailed", "terminal title lock is poisoned")
                .recoverable(true)
        })? = title.to_string();
        Ok(TerminalEmptyResult {})
    }

    /// `workspace_id` is a scoping assertion, not an address: terminal ids are
    /// unique on their own. Omitted, any terminal matches — which is how a
    /// sign-in login, belonging to no workspace, gets replayed.
    pub fn replay(
        &self,
        workspace_id: Option<&WorkspaceID>,
        terminal_id: &TerminalID,
        max_bytes: u64,
    ) -> Result<TerminalReplayResult, CoreError> {
        let session = self
            .session_for_terminal_id(workspace_id, terminal_id)?
            .ok_or_else(|| terminal_not_found(terminal_id))?;
        let result = session
            .scrollback
            .lock()
            .map_err(|_| {
                CoreError::new(
                    "terminalReplayFailed",
                    "terminal scrollback lock is poisoned",
                )
                .recoverable(true)
            })?
            .replay(terminal_id.clone(), max_bytes);
        result
    }

    pub fn close_project_terminals(&mut self, workspace_id: &WorkspaceID, project_id: &ProjectID) {
        let session_ids = self
            .sessions
            .lock()
            .map(|sessions| {
                sessions
                    .iter()
                    .filter(|(_, session)| {
                        session.belongs_to(workspace_id)
                            && session.project_id.as_ref() == Some(project_id)
                    })
                    .map(|(session_id, _)| session_id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for session_id in session_ids {
            let _ = self.close(TerminalCloseParams { session_id });
        }
    }

    /// Tear down every terminal on server shutdown: kill + cancel each session,
    /// close the event sender (so trailing `terminal.exited` notifications are
    /// suppressed, matching the previous synchronous shutdown), then wait —
    /// bounded — for the read tasks to finish their EOF-driven reap.
    pub async fn close_all(&mut self) {
        let session_ids = self
            .sessions
            .lock()
            .map(|sessions| sessions.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for session_id in session_ids {
            let _ = self.close(TerminalCloseParams { session_id });
        }
        self.events.close();
        self.tasks.close();
        if tokio::time::timeout(TERMINAL_DRAIN_TIMEOUT, self.tasks.wait())
            .await
            .is_err()
        {
            tracing::warn!("terminal read tasks did not drain before shutdown timeout");
        }
    }

    fn session(&self, session_id: &TerminalSessionID) -> Result<Arc<TerminalSession>, CoreError> {
        let session = self
            .sessions
            .lock()
            .map_err(|_| {
                CoreError::new(
                    "terminalRegistryFailed",
                    "terminal registry lock is poisoned",
                )
                .recoverable(true)
            })?
            .get(session_id)
            .cloned()
            .ok_or_else(|| {
                CoreError::new(
                    "unknownTerminalSession",
                    format!("unknown terminal sessionID: {}", session_id.0),
                )
                .recoverable(true)
            })?;
        let running = session
            .status
            .lock()
            .map(|status| *status == TerminalRuntimeStatus::Running)
            .unwrap_or(false);
        if !running {
            return Err(CoreError::new(
                "unknownTerminalSession",
                format!("unknown terminal sessionID: {}", session_id.0),
            )
            .recoverable(true));
        }
        Ok(session)
    }

    /// `workspace_id` scopes the lookup when given; terminal ids are unique
    /// without it.
    fn session_for_terminal_id(
        &self,
        workspace_id: Option<&WorkspaceID>,
        terminal_id: &TerminalID,
    ) -> Result<Option<Arc<TerminalSession>>, CoreError> {
        Ok(self
            .sessions
            .lock()
            .map_err(|_| {
                CoreError::new(
                    "terminalRegistryFailed",
                    "terminal registry lock is poisoned",
                )
                .recoverable(true)
            })?
            .values()
            .find(|session| {
                workspace_id.is_none_or(|workspace_id| session.belongs_to(workspace_id))
                    && session.terminal_id == *terminal_id
            })
            .cloned())
    }
}
