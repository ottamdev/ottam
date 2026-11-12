//! The upstream connections sessions' MCP calls go to: one local process per
//! server and workspace, and one connection per remote server shared by
//! every workspace.
//!
//! Opening a session that gets a server takes a [`McpLease`] on its slot and
//! starts the connection in the background. A connection that fails or
//! crashes shows failed and starts again on the next call, once its backoff
//! has passed. A slot with no lease left stops after the idle period; a
//! changed or removed server, an archived workspace, and quitting stop
//! slots at once. Stopping a local process SIGTERMs its process group and
//! reaps it.
//!
//! A remote server that answers 401 is signed out: its calls fail at once
//! with [`UpstreamError::SignedOut`] until a sign-in or a change to the
//! server clears that, rather than backing off and trying again. Each
//! clearing starts a new sign-in epoch, and a 401 from a connection begun in
//! an earlier one is too late to count.

use super::calls::UpstreamEvents;
use super::config::McpServers;
use super::upstream::{self, Connection, ConnectionHooks, McpWorkspace, Upstream, UpstreamError};
use crate::core_error::CoreError;
use ottam_protocol::{
    McpServerID, McpServerState, McpServerStatus, McpSignInFinishedParams, McpSignInNeededParams,
    WorkspaceID,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// How long a slot no session uses keeps its connection.
pub(crate) const IDLE_STOP: Duration = Duration::from_secs(10 * 60);
const BACKOFF_FIRST: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// How long trying a newly added remote server may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// A connection up this long has recovered: its next failure backs off
/// from the start again.
const RECOVERED_AFTER: Duration = Duration::from_secs(60);

/// What core tells clients about its servers.
#[derive(Clone, Debug)]
pub(crate) enum McpNotice {
    /// A server's state changed.
    State(McpServerID, McpServerState),
    SignInFinished(McpSignInFinishedParams),
    /// A chat's call reached a server that needs a sign-in.
    SignInNeeded(McpSignInNeededParams),
    /// A server was added, changed, or removed.
    ServersChanged,
}

/// Receives every [`McpNotice`].
pub(crate) type McpNoticeSink = Arc<dyn Fn(McpNotice) + Send + Sync>;

/// Remote servers that answered 401 since their last sign-in, and each
/// server's epoch, which every sign-in or change to it starts.
#[derive(Default)]
struct SignedOut {
    servers: HashSet<String>,
    epochs: HashMap<String, u64>,
}

impl SignedOut {
    fn epoch(&self, server_id: &str) -> u64 {
        self.epochs.get(server_id).copied().unwrap_or_default()
    }

    fn clear(&mut self, server_id: &str) {
        self.servers.remove(server_id);
        *self.epochs.entry(server_id.to_owned()).or_default() += 1;
    }

    /// Mark `server_id` signed out, unless `since` is an earlier epoch of
    /// that server.
    fn mark(&mut self, server_id: &str, since: Option<u64>) -> bool {
        if since.is_some_and(|since| since != self.epoch(server_id)) {
            return false;
        }
        self.servers.insert(server_id.to_owned());
        true
    }
}

/// The bin directory of the Node runtime agents launch with, if one was
/// already found. Never searches, as agent launches don't.
pub(crate) type McpToolchain = Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct McpRuntime {
    inner: Arc<Inner>,
}

struct Inner {
    servers: McpServers,
    /// Both are let go at shutdown: the sink is an outbound clone, so the
    /// router can exit only once every one is gone, even while a service
    /// clone outlives the shutdown.
    sink: Mutex<Option<McpNoticeSink>>,
    events: Mutex<Option<Arc<dyn UpstreamEvents>>>,
    toolchain: Mutex<Option<McpToolchain>>,
    idle_after: Duration,
    slots: Mutex<HashMap<SlotKey, Arc<Slot>>>,
    reported: Mutex<HashMap<String, McpServerState>>,
    signed_out: Mutex<SignedOut>,
    closed: AtomicBool,
    next_connection: AtomicU64,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct SlotKey {
    server_id: String,
    /// `None` for a remote server, which every workspace shares.
    workspace_id: Option<String>,
}

struct Slot {
    key: SlotKey,
    workspace: Option<McpWorkspace>,
    state: Mutex<SlotState>,
    /// One start at a time; later callers wait for its outcome.
    starting: tokio::sync::Mutex<()>,
    /// Fires when the slot is stopped, abandoning a start in progress.
    retired: CancellationToken,
}

#[derive(Default)]
struct SlotState {
    live: Option<Live>,
    starting: bool,
    error: Option<String>,
    failures: u32,
    retry_at: Option<Instant>,
    leases: usize,
    idle: Option<JoinHandle<()>>,
    /// Whether the slot had a connection before, whose tools its sessions
    /// may have listed.
    connected: bool,
}

struct Live {
    id: u64,
    upstream: Upstream,
    stop: CancellationToken,
    task: JoinHandle<()>,
    started_at: Instant,
}

/// A session's hold on a server's slot. Dropping the last one starts the
/// slot's idle period. It names the slot by key, so it holds whichever slot
/// replaced the one it was taken on when the server changed.
pub(crate) struct McpLease {
    runtime: Weak<Inner>,
    key: SlotKey,
}

impl Drop for McpLease {
    fn drop(&mut self) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let slots = lock(&runtime.slots);
        // Gone with its server or workspace: nothing is left to release.
        let Some(slot) = slots.get(&self.key) else {
            return;
        };
        let mut state = slot.lock();
        state.leases = state.leases.saturating_sub(1);
        if state.leases > 0 || runtime.closed.load(Ordering::Acquire) {
            return;
        }
        let (weak, slot, idle_after) = (
            Arc::downgrade(&runtime),
            Arc::clone(slot),
            runtime.idle_after,
        );
        // Lease drops can happen outside a runtime during teardown; nothing
        // is left to stop then.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            state.idle = Some(handle.spawn(async move {
                tokio::time::sleep(idle_after).await;
                if let Some(runtime) = weak.upgrade() {
                    Inner::stop_if_idle(&runtime, &slot).await;
                }
            }));
        }
    }
}

impl McpRuntime {
    pub(crate) fn new(servers: McpServers, sink: McpNoticeSink) -> Self {
        Self::with_idle_after(servers, sink, IDLE_STOP)
    }

    pub(crate) fn with_idle_after(
        servers: McpServers,
        sink: McpNoticeSink,
        idle_after: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                servers,
                sink: Mutex::new(Some(sink)),
                events: Mutex::default(),
                toolchain: Mutex::default(),
                idle_after,
                slots: Mutex::default(),
                reported: Mutex::default(),
                signed_out: Mutex::default(),
                closed: AtomicBool::new(false),
                next_connection: AtomicU64::new(1),
            }),
        }
    }

    /// Where connections started from now on send their servers' own
    /// requests and notifications. Set once, when the proxy is listening.
    pub(crate) fn set_events(&self, events: Arc<dyn UpstreamEvents>) {
        let mut current = lock(&self.inner.events);
        if current.is_none() && !self.inner.closed.load(Ordering::Acquire) {
            *current = Some(events);
        }
    }

    /// Hold `server_id`'s slot for a session in `workspace`, starting its
    /// connection in the background if it has none.
    #[cfg(test)]
    pub(crate) async fn acquire(
        &self,
        server_id: &McpServerID,
        workspace: &McpWorkspace,
    ) -> Result<McpLease, CoreError> {
        let local = self.inner.servers.is_local(server_id).await?;
        self.lease(server_id, local, workspace)
    }

    /// [`Self::acquire`] for a server already known to be `local` or not.
    pub(crate) fn lease(
        &self,
        server_id: &McpServerID,
        local: bool,
        workspace: &McpWorkspace,
    ) -> Result<McpLease, CoreError> {
        let key = SlotKey {
            server_id: server_id.0.clone(),
            workspace_id: local.then(|| workspace.id.0.clone()),
        };
        let slot = {
            if self.inner.closed.load(Ordering::Acquire) {
                return Err(CoreError::new("coreShuttingDown", "core is shutting down"));
            }
            let mut slots = lock(&self.inner.slots);
            let slot = slots.entry(key.clone()).or_insert_with(|| {
                Arc::new(Slot::new(
                    key.clone(),
                    local.then(|| workspace.clone()),
                    SlotState::default(),
                ))
            });
            // Counted under the slots lock, so an idle stop never retires a
            // slot a lease was just taken on.
            let mut state = slot.lock();
            state.leases += 1;
            if let Some(idle) = state.idle.take() {
                idle.abort();
            }
            drop(state);
            Arc::clone(slot)
        };
        let lease = McpLease {
            runtime: Arc::downgrade(&self.inner),
            key,
        };
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let _ = Inner::connect(&inner, &slot).await;
        });
        Ok(lease)
    }

    /// The upstream connection for `server_id` in `workspace_id`, starting
    /// it again if it failed and its backoff has passed.
    pub(crate) async fn upstream(
        &self,
        server_id: &McpServerID,
        workspace_id: &WorkspaceID,
    ) -> Result<Upstream, UpstreamError> {
        if self.is_signed_out(server_id) {
            return Err(UpstreamError::SignedOut);
        }
        let slot = {
            let slots = lock(&self.inner.slots);
            let local = SlotKey {
                server_id: server_id.0.clone(),
                workspace_id: Some(workspace_id.0.clone()),
            };
            let remote = SlotKey {
                workspace_id: None,
                ..local.clone()
            };
            slots.get(&local).or_else(|| slots.get(&remote)).cloned()
        };
        let slot = slot
            .ok_or_else(|| UpstreamError::failed("The server is not running for this workspace"))?;
        Inner::connect(&self.inner, &slot).await
    }

    fn is_signed_out(&self, server_id: &McpServerID) -> bool {
        lock(&self.inner.signed_out).servers.contains(&server_id.0)
    }

    /// Tell clients `notice`, unless core is shutting down.
    pub(crate) fn notify(&self, notice: McpNotice) {
        let sink = lock(&self.inner.sink).clone();
        if let Some(sink) = sink {
            sink(notice);
        }
    }

    /// A sign-in just stored new tokens: connections start again with them.
    pub(crate) async fn signed_in(&self, server_id: &McpServerID) {
        lock(&self.inner.signed_out).clear(&server_id.0);
        self.restart_server(server_id, false).await;
    }

    /// The server needs a sign-in: its connections stop, and its calls fail
    /// until it has one. With `since`, the epoch of the connection that was
    /// refused, only if no sign-in or change to the server came after it;
    /// otherwise its connections from before that stop, so the next call
    /// starts one that knows of it. Returns whether it was marked.
    pub(crate) async fn mark_signed_out(
        &self,
        server_id: &McpServerID,
        since: Option<u64>,
    ) -> bool {
        if !lock(&self.inner.signed_out).mark(&server_id.0, since) {
            self.inner.stop_stale(&server_id.0).await;
            return false;
        }
        tracing::info!(server = %server_id.0, "an MCP server needs a sign-in");
        self.restart_server(server_id, false).await;
        true
    }

    /// Try a remote server once, without a slot, so one that needs a
    /// sign-in shows signed out before any session uses it. Any other
    /// failure waits for a session to show it.
    pub(crate) async fn probe(&self, server_id: &McpServerID) {
        let Ok(server) = self.inner.servers.resolve(server_id).await else {
            return;
        };
        let hooks = ConnectionHooks {
            server_id: server_id.clone(),
            workspace_id: None,
            events: None,
        };
        let oauth = self.inner.servers.oauth(server_id);
        let since = lock(&self.inner.signed_out).epoch(&server_id.0);
        let connected = tokio::time::timeout(
            PROBE_TIMEOUT,
            upstream::connect(&server, None, None, hooks, oauth),
        )
        .await;
        match connected {
            Ok(Ok(connection)) => {
                let stop = CancellationToken::new();
                stop.cancel();
                let _ = connection.run_until(stop).await;
            }
            Ok(Err(UpstreamError::SignedOut)) => {
                if lock(&self.inner.signed_out).mark(&server_id.0, Some(since)) {
                    self.inner.report(&server_id.0);
                }
            }
            Ok(Err(UpstreamError::Failed(_))) | Err(_) => {}
        }
    }

    /// Forget that a changed or removed server needed a sign-in, so its
    /// next connection finds out again.
    pub(crate) fn forget_sign_in(&self, server_id: &McpServerID) {
        lock(&self.inner.signed_out).clear(&server_id.0);
    }

    /// `server_id`'s current sign-in epoch, for tests.
    #[cfg(test)]
    pub(crate) fn sign_in_epoch(&self, server_id: &McpServerID) -> u64 {
        lock(&self.inner.signed_out).epoch(&server_id.0)
    }

    /// The state `mcp.list` reports for a server: signed out if it needs a
    /// sign-in, else failed if any of its slots failed, else starting, else
    /// running, else not started.
    pub(crate) fn state(&self, server_id: &McpServerID) -> McpServerState {
        self.inner.aggregate(&server_id.0)
    }

    /// Stop every connection of a changed server, now `local` or not. The
    /// slots sessions hold stay, emptied, so their next call starts the
    /// server again with its new config. A server that moved between local
    /// and remote has slots of the other kind, so they go instead, and its
    /// sessions get it back when they next open.
    pub(crate) async fn restart_server(&self, server_id: &McpServerID, local: bool) {
        let stopped = {
            let mut slots = lock(&self.inner.slots);
            let keys: Vec<SlotKey> = slots
                .keys()
                .filter(|key| key.server_id == server_id.0)
                .cloned()
                .collect();
            let mut stopped = Vec::new();
            for key in keys {
                let Some(old) = slots.remove(&key) else {
                    continue;
                };
                let (leases, connected) = {
                    let state = old.lock();
                    (state.leases, state.connected)
                };
                if leases > 0 && key.workspace_id.is_some() == local {
                    let state = SlotState {
                        leases,
                        // Its sessions saw the old server's tools.
                        connected,
                        ..SlotState::default()
                    };
                    let fresh = Slot::new(key.clone(), old.workspace.clone(), state);
                    slots.insert(key, Arc::new(fresh));
                }
                stopped.push(old);
            }
            stopped
        };
        Inner::stop_all(stopped).await;
        self.inner.report(&server_id.0);
    }

    /// Stop every slot of a removed server.
    pub(crate) async fn stop_server(&self, server_id: &McpServerID) {
        let removed = self.inner.take_slots(|key| key.server_id == server_id.0);
        Inner::stop_all(removed).await;
        self.inner.report(&server_id.0);
    }

    /// Stop every local process of an archived workspace.
    pub(crate) async fn stop_workspace(&self, workspace_id: &WorkspaceID) {
        let removed = self
            .inner
            .take_slots(|key| key.workspace_id.as_deref() == Some(workspace_id.0.as_str()));
        let servers: Vec<String> = removed
            .iter()
            .map(|slot| slot.key.server_id.clone())
            .collect();
        Inner::stop_all(removed).await;
        for server_id in servers {
            self.inner.report(&server_id);
        }
    }

    /// Put `toolchain`'s directory first on local servers' PATH, so `npx`
    /// and `node` resolve as they do for agents.
    pub(crate) fn set_toolchain(&self, toolchain: McpToolchain) {
        *lock(&self.inner.toolchain) = Some(toolchain);
    }

    /// Stop everything and refuse new leases; returns once every local
    /// process has been reaped.
    pub(crate) async fn shutdown(&self) {
        self.inner.closed.store(true, Ordering::Release);
        lock(&self.inner.events).take();
        lock(&self.inner.toolchain).take();
        let removed = self.inner.take_slots(|_| true);
        Inner::stop_all(removed).await;
        lock(&self.inner.sink).take();
    }
}

impl Inner {
    async fn connect(self: &Arc<Self>, slot: &Arc<Slot>) -> Result<Upstream, UpstreamError> {
        let _single = slot.starting.lock().await;
        {
            let mut state = slot.lock();
            if let Some(live) = &state.live {
                return Ok(live.upstream.clone());
            }
            if slot.retired.is_cancelled() {
                return Err(stopped());
            }
            if lock(&self.signed_out).servers.contains(&slot.key.server_id) {
                return Err(UpstreamError::SignedOut);
            }
            if let Some(retry_at) = state.retry_at {
                if Instant::now() < retry_at {
                    return Err(UpstreamError::Failed(
                        state
                            .error
                            .clone()
                            .unwrap_or_else(|| "The server failed".to_owned()),
                    ));
                }
            }
            state.starting = true;
        }
        let since = lock(&self.signed_out).epoch(&slot.key.server_id);
        self.report(&slot.key.server_id);
        let started = tokio::select! {
            started = self.open(slot) => Some(started),
            // Dropping a start in progress kills its process group.
            () = slot.retired.cancelled() => None,
        };
        let outcome = match started {
            None => {
                slot.lock().starting = false;
                Err(stopped())
            }
            Some(Err(UpstreamError::SignedOut)) => {
                slot.lock().starting = false;
                if lock(&self.signed_out).mark(&slot.key.server_id, Some(since)) {
                    Err(UpstreamError::SignedOut)
                } else {
                    Err(sign_in_changed())
                }
            }
            Some(Err(UpstreamError::Failed(reason))) => {
                let mut state = slot.lock();
                state.starting = false;
                state.fail(reason.clone(), None);
                Err(UpstreamError::Failed(reason))
            }
            Some(Ok(connection)) => match self.install(slot, connection, since) {
                Ok((upstream, restarted)) => {
                    // A restarted server may list different tools than the
                    // sessions last saw; a first start lists them anyway.
                    let events = lock(&self.events).clone().filter(|_| restarted);
                    if let Some(events) = events {
                        let workspace = slot.workspace.as_ref().map(|workspace| &workspace.id);
                        events.tools_changed(&McpServerID(slot.key.server_id.clone()), workspace);
                    }
                    Ok(upstream)
                }
                Err(connection) => {
                    // Stopped while finishing its start; stop the process
                    // before anyone waiting on the slot moves on.
                    let stop = CancellationToken::new();
                    stop.cancel();
                    let _ = connection.run_until(stop).await;
                    Err(stopped())
                }
            },
        };
        self.report(&slot.key.server_id);
        outcome
    }

    /// Record a started connection and watch it, unless the slot retired
    /// meanwhile; then the connection comes back for the caller to stop.
    /// Also says whether the slot had a connection before this one.
    fn install(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        mut connection: Connection,
        since: u64,
    ) -> Result<(Upstream, bool), Box<Connection>> {
        connection.upstream.sign_in_epoch = since;
        let mut state = slot.lock();
        state.starting = false;
        if slot.retired.is_cancelled() {
            return Err(Box::new(connection));
        }
        let upstream = connection.upstream.clone();
        let stop = CancellationToken::new();
        let id = self.next_connection.fetch_add(1, Ordering::Relaxed);
        // The watcher takes the state lock only after this section, so it
        // always finds this connection recorded.
        let task = tokio::spawn({
            let (runtime, slot, stop) = (Arc::downgrade(self), Arc::clone(slot), stop.clone());
            async move {
                if let Some(reason) = connection.run_until(stop).await {
                    if let Some(runtime) = runtime.upgrade() {
                        runtime.crashed(&slot, id, reason);
                    }
                }
            }
        });
        state.live = Some(Live {
            id,
            upstream: upstream.clone(),
            stop,
            task,
            started_at: Instant::now(),
        });
        state.error = None;
        state.retry_at = None;
        let restarted = std::mem::replace(&mut state.connected, true);
        Ok((upstream, restarted))
    }

    async fn open(&self, slot: &Slot) -> Result<Connection, UpstreamError> {
        let server_id = McpServerID(slot.key.server_id.clone());
        let server = self
            .servers
            .resolve(&server_id)
            .await
            .map_err(|error| UpstreamError::Failed(error.message))?;
        let oauth = self.servers.oauth(&server_id);
        let hooks = ConnectionHooks {
            server_id,
            workspace_id: slot
                .workspace
                .as_ref()
                .map(|workspace| workspace.id.clone()),
            events: lock(&self.events).clone(),
        };
        let toolchain = lock(&self.toolchain).clone();
        // Only local slots hold a workspace.
        let path_prefix = toolchain
            .filter(|_| slot.workspace.is_some())
            .and_then(|toolchain| toolchain());
        upstream::connect(
            &server,
            slot.workspace.as_ref(),
            path_prefix.as_deref(),
            hooks,
            oauth,
        )
        .await
    }

    fn crashed(&self, slot: &Slot, id: u64, reason: UpstreamError) {
        {
            let mut state = slot.lock();
            let Some(live) = state.live.take_if(|live| live.id == id) else {
                return;
            };
            match reason {
                UpstreamError::SignedOut => {
                    let since = live.upstream.sign_in_epoch;
                    lock(&self.signed_out).mark(&slot.key.server_id, Some(since));
                }
                UpstreamError::Failed(reason) => state.fail(reason, Some(live.started_at)),
            }
        }
        self.report(&slot.key.server_id);
    }

    async fn stop_if_idle(self: &Arc<Self>, slot: &Arc<Slot>) {
        let idle = {
            let mut slots = lock(&self.slots);
            let idle = slot.lock().leases == 0
                && slots
                    .get(&slot.key)
                    .is_some_and(|current| Arc::ptr_eq(current, slot));
            if idle {
                slots.remove(&slot.key);
            }
            idle
        };
        if idle {
            slot.stop().await;
            self.report(&slot.key.server_id);
        }
    }

    /// Stop the connections of `server_id` that started before its latest
    /// sign-in or change. Their slots stay, so the next call starts again.
    async fn stop_stale(&self, server_id: &str) {
        let epoch = lock(&self.signed_out).epoch(server_id);
        let stale: Vec<Live> = {
            let slots = lock(&self.slots);
            slots
                .values()
                .filter(|slot| slot.key.server_id == server_id)
                .filter_map(|slot| {
                    slot.lock()
                        .live
                        .take_if(|live| live.upstream.sign_in_epoch != epoch)
                })
                .collect()
        };
        if stale.is_empty() {
            return;
        }
        for live in stale {
            live.stop.cancel();
            let _ = live.task.await;
        }
        self.report(server_id);
    }

    fn take_slots(&self, matches: impl Fn(&SlotKey) -> bool) -> Vec<Arc<Slot>> {
        let mut slots = lock(&self.slots);
        let keys: Vec<SlotKey> = slots.keys().filter(|key| matches(key)).cloned().collect();
        keys.iter().filter_map(|key| slots.remove(key)).collect()
    }

    async fn stop_all(slots: Vec<Arc<Slot>>) {
        futures::future::join_all(slots.iter().map(|slot| slot.stop())).await;
    }

    fn aggregate(&self, server_id: &str) -> McpServerState {
        if lock(&self.signed_out).servers.contains(server_id) {
            return McpServerState {
                status: McpServerStatus::SignedOut,
                error: None,
            };
        }
        let slots = lock(&self.slots);
        let (mut failed, mut starting, mut running) = (None, false, false);
        for slot in slots
            .values()
            .filter(|slot| slot.key.server_id == server_id)
        {
            let state = slot.lock();
            if state.starting {
                starting = true;
            } else if state.live.is_some() {
                running = true;
            } else if let Some(error) = &state.error {
                failed.get_or_insert_with(|| error.clone());
            }
        }
        let (status, error) = match failed {
            Some(error) => (McpServerStatus::Failed, Some(error)),
            None if starting => (McpServerStatus::Starting, None),
            None if running => (McpServerStatus::Running, None),
            None => (McpServerStatus::NotStarted, None),
        };
        McpServerState { status, error }
    }

    /// Tell the sink when a server's aggregate state changed.
    fn report(&self, server_id: &str) {
        let state = self.aggregate(server_id);
        let changed = {
            let mut reported = lock(&self.reported);
            let previous = reported.insert(server_id.to_owned(), state.clone());
            previous.as_ref() != Some(&state)
                && !(previous.is_none() && state.status == McpServerStatus::NotStarted)
        };
        let sink = lock(&self.sink).clone();
        if let Some(sink) = sink.filter(|_| changed) {
            sink(McpNotice::State(McpServerID(server_id.to_owned()), state));
        }
    }
}

fn stopped() -> UpstreamError {
    UpstreamError::failed("The server was stopped")
}

/// Why a refusal from before a sign-in or change to the server fails.
pub(crate) const SIGN_IN_CHANGED: &str =
    "The server's sign-in changed while this was under way. Try again.";

fn sign_in_changed() -> UpstreamError {
    UpstreamError::failed(SIGN_IN_CHANGED)
}

impl Slot {
    fn new(key: SlotKey, workspace: Option<McpWorkspace>, state: SlotState) -> Self {
        Self {
            key,
            workspace,
            state: Mutex::new(state),
            starting: tokio::sync::Mutex::new(()),
            retired: CancellationToken::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, SlotState> {
        lock(&self.state)
    }

    /// Stop the connection and any start in progress; returns once a local
    /// process has been reaped.
    async fn stop(&self) {
        self.retired.cancel();
        let live = {
            let mut state = self.lock();
            if let Some(idle) = state.idle.take() {
                idle.abort();
            }
            state.error = None;
            state.retry_at = None;
            state.live.take()
        };
        if let Some(live) = live {
            live.stop.cancel();
            let _ = live.task.await;
        }
        // Wait out a start that was finishing as the slot retired.
        let _ = self.starting.lock().await;
    }
}

impl SlotState {
    fn fail(&mut self, reason: String, started_at: Option<Instant>) {
        let recovered = started_at.is_some_and(|started| started.elapsed() >= RECOVERED_AFTER);
        self.failures = if recovered { 1 } else { self.failures + 1 };
        self.retry_at = Some(Instant::now() + backoff(self.failures));
        self.error = Some(reason);
    }
}

/// 1 s after the first failure, doubling to at most a minute.
fn backoff(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    (BACKOFF_FIRST * 2u32.pow(doublings)).min(BACKOFF_MAX)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::backoff;
    use std::time::Duration;

    #[test]
    fn backoff_doubles_from_a_second_to_a_minute() {
        let waits: Vec<_> = (1..=8).map(backoff).collect();
        let seconds: Vec<_> = waits.iter().map(Duration::as_secs).collect();
        assert_eq!(seconds, [1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(backoff(u32::MAX), Duration::from_secs(60));
    }
}
