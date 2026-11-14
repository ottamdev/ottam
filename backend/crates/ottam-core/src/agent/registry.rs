use super::acp::AcpHarnessAdapter;
use super::catalog_probe::ProbeCleanup;
#[cfg(test_seams)]
use super::native_test::NativeTestAdapter;
use super::node_resolver::{NodeResolution, NodeResolver, NodeSource, ResolvedNodeRuntime};
use super::types::{
    AccountKind, AdapterLifecycleState, AdapterPackage, AdapterSource, AdapterStatus,
    AgentDefinition, AgentId, AgentLaunchSpec, AgentModelOption, AgentModelSelects,
    AgentPresentation, AgentRuntimeStatus, AgentSessionSelects, BinaryAdapter, BinaryArchive,
    BinaryArtifact, CancelPermissionBroker, ChildSessionTags, ClientPlugin, FormSkip,
    HarnessAdapter, HarnessAuthState, HarnessError, HarnessErrorKind, HarnessPermissionBroker,
    HomeSetting, HomeTarget, HomeVariable, McpConnections, RuntimeAvailability, RuntimeSource,
    SignInProof, SubagentFollowUpTool, SubagentHandbackTool, SubagentMessageTools,
    TerminalLoginReveal,
};
use std::collections::BTreeMap;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub const CODEX_AGENT_ID: &str = "codex";
pub const CLAUDE_AGENT_ID: &str = "claude";
pub const TEST_NATIVE_AGENT_ID: &str = "test.native";
pub const OPENCODE_AGENT_ID: &str = "opencode";
pub const OMP_AGENT_ID: &str = "omp";
/// A binary-lane agent that installs from `OTTAM_TEST_BINARY_AGENT_URL`,
/// for exercising the download path without a real vendor binary.
pub const TEST_BINARY_AGENT_ID: &str = "test.binary";
/// Adapter versions are pinned rather than tracked with `@latest` so an
/// upstream release cannot change what every user runs without an app release.
/// Bump deliberately; `ModelCatalogService` re-probes when a pin moves. Each
/// definition's `catalog_probe` rests on what that version stores for a bare
/// `session/new`: recheck it with `scripts/check-probe-leftovers.py`.
const CODEX_ACP_PACKAGE: (&str, &str) = ("@agentclientprotocol/codex-acp", "2.5.0");
const CLAUDE_ACP_PACKAGE: (&str, &str) = ("@agentclientprotocol/claude-agent-acp", "0.100.0");
/// Each adapter's own `engines.node`, per engine rather than one figure for
/// all: a floor is a rejection threshold, so borrowing the strictest engine's
/// would turn setups that genuinely work into "Node too old". Re-check these
/// when moving a pin above — an adapter can raise its floor in a patch release.
/// Codex declares no `engines` field, so it keeps the conservative default.
const CODEX_MINIMUM_NODE_MAJOR: u64 = 18;
const CLAUDE_MINIMUM_NODE_MAJOR: u64 = 22;
/// Below this, an installed managed runtime beats the system one.
///
/// It is the *maximum* of the floors above, not a floor itself — nothing is
/// ever rejected for falling under it. A system Node below this line cannot
/// satisfy every engine, so a managed runtime that clears every floor is strictly
/// better; at or above it, the user's own toolchain wins and Ottam's
/// download stays unused. `preference_floor_matches_the_strictest_engine`
/// keeps it honest when a pin bump raises an adapter's `engines.node`.
pub(super) const PREFERRED_NODE_MAJOR: u64 = CLAUDE_MINIMUM_NODE_MAJOR;
const RUNTIME_PROBE_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);

pub trait RuntimeProbe: Send + Sync {
    fn probe(&self, definition: &AgentDefinition) -> AgentRuntimeStatus;

    /// Drop probe-specific status caches before an explicit runtime refresh.
    /// Test and custom probes are uncached by default.
    fn invalidate(&self) {}
}

/// How long a ready runtime status is reused before re-shelling out.
///
/// `engine.list` probes every registered agent, and each probe runs both
/// `node --version` and `npx --version` — roughly 100ms per agent, repeated on
/// every poll. The installed toolchain only changes when the user installs one,
/// so a short TTL removes the repeat cost while still noticing an upgrade
/// quickly.
const RUNTIME_STATUS_CACHE_TTL: Duration = Duration::from_secs(300);

/// A not-ready answer that took this long is worth keeping briefly: without
/// it, a list that outran its budget would announce the answer, and the
/// re-read that announcement causes would outrun the budget again, forever.
const SLOW_NOT_READY_THRESHOLD: Duration = Duration::from_secs(1);
const SLOW_NOT_READY_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
pub struct SystemRuntimeProbe {
    /// Ready and slow not-ready statuses, keyed by harness id. A fast not-ready
    /// result is never cached: an immediate ENOENT costs nothing to repeat, and
    /// caching one would keep reporting "missing" after the user installs the
    /// runtime.
    cache: StdMutex<BTreeMap<String, (Instant, AgentRuntimeStatus)>>,
    probe_gates: StdMutex<BTreeMap<String, Arc<StdMutex<()>>>>,
    /// Bumped under `cache` by `invalidate`, so a probe that started before an
    /// explicit refresh or a finished install cannot answer the one after it.
    invalidations: AtomicU64,
    resolver: Arc<NodeResolver>,
}

impl SystemRuntimeProbe {
    pub(super) fn with_resolver(resolver: Arc<NodeResolver>) -> Self {
        Self {
            cache: StdMutex::new(BTreeMap::new()),
            probe_gates: StdMutex::new(BTreeMap::new()),
            invalidations: AtomicU64::new(0),
            resolver,
        }
    }
}

impl RuntimeProbe for SystemRuntimeProbe {
    fn probe(&self, definition: &AgentDefinition) -> AgentRuntimeStatus {
        // Serialize cold checks within a harness; other harnesses stay available.
        let gate = self
            .probe_gates
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(definition.harness_id().to_owned())
            .or_default()
            .clone();
        let _gate = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(status) = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(definition.harness_id())
            .filter(|(observed_at, status)| observed_at.elapsed() < status_cache_ttl(status))
            .map(|(_, status)| status.clone())
        {
            return status;
        }
        let invalidations = self.invalidations.load(Ordering::Acquire);
        // Resolving first is what makes the probe agree with the spawn: both
        // run the same absolute binaries, so a machine that probes Ready can
        // actually start an adapter.
        let runtime = self.resolver.resolve();
        // The bin directory rides along for the same reason the launch spec
        // carries it: npx re-resolves `node` through PATH from its own shebang.
        let bin_dir = match &runtime {
            NodeResolution::Resolved(resolved) => resolved.bin_dir().map(Path::to_path_buf),
            NodeResolution::Unresolved => None,
        };
        let version_checks_started = Instant::now();
        let mut status = probe_runtime_with(definition, |program, argument| {
            let program = resolved_program(&runtime, program);
            command_output_with_path(
                &program,
                &[argument],
                RUNTIME_PROBE_COMMAND_TIMEOUT,
                bin_dir.as_deref(),
            )
        });
        // The probe just ran these exact binaries, so it is the one place that
        // can say truthfully which toolchain the answer describes.
        status.source = match &runtime {
            NodeResolution::Resolved(resolved) => Some(match resolved.source {
                NodeSource::EnvOverride => RuntimeSource::Override,
                NodeSource::Managed => RuntimeSource::Managed,
                NodeSource::LoginShell | NodeSource::WellKnown => RuntimeSource::System,
            }),
            NodeResolution::Unresolved => None,
        };
        if worth_caching(&status, version_checks_started.elapsed()) {
            self.cache_unless_invalidated(definition.harness_id(), invalidations, status.clone());
        }
        status
    }

    fn invalidate(&self) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.clear();
        self.invalidations.fetch_add(1, Ordering::AcqRel);
    }
}

impl SystemRuntimeProbe {
    fn cache_unless_invalidated(
        &self,
        harness_id: &str,
        invalidations: u64,
        status: AgentRuntimeStatus,
    ) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.invalidations.load(Ordering::Acquire) == invalidations {
            cache.insert(harness_id.to_owned(), (Instant::now(), status));
        }
    }
}

fn worth_caching(status: &AgentRuntimeStatus, took: Duration) -> bool {
    status.is_ready() || took >= SLOW_NOT_READY_THRESHOLD
}

fn status_cache_ttl(status: &AgentRuntimeStatus) -> Duration {
    if status.is_ready() {
        RUNTIME_STATUS_CACHE_TTL
    } else {
        SLOW_NOT_READY_CACHE_TTL
    }
}

/// Map a probe's logical program name onto the resolved toolchain. Unknown
/// names and an unresolved toolchain pass through as bare names, which is the
/// pre-resolver behaviour.
fn resolved_program(runtime: &NodeResolution, program: &str) -> String {
    let NodeResolution::Resolved(resolved) = runtime else {
        return program.to_string();
    };
    match program {
        "node" => resolved.node.to_string_lossy().into_owned(),
        "npx" => resolved.npx.to_string_lossy().into_owned(),
        other => other.to_string(),
    }
}

pub(super) fn command_output_args_with_timeout(
    program: &str,
    arguments: &[&str],
    timeout: Duration,
) -> io::Result<Output> {
    command_output_with_path(program, arguments, timeout, None)
}

/// As above, with an optional directory prepended to the child's PATH.
///
/// Needed because `npx` is a `#!/usr/bin/env node` script: running it by
/// absolute path is not enough, since its own shebang re-resolves `node`
/// through PATH. Under launchd's PATH that lookup fails, and the probe reports
/// a perfectly good npx as invalid.
pub(super) fn command_output_with_path(
    program: &str,
    arguments: &[&str],
    timeout: Duration,
    prepend_path: Option<&Path>,
) -> io::Result<Output> {
    let mut command = Command::new(program);
    if let Some(dir) = prepend_path {
        let inherited = std::env::var("PATH").unwrap_or_default();
        let path = if inherited.is_empty() {
            dir.to_string_lossy().into_owned()
        } else {
            format!("{}:{inherited}", dir.to_string_lossy())
        };
        command.env("PATH", path);
    }
    command.args(arguments);
    command_output_with_timeout(command, timeout)
}

pub(super) fn command_output_with_timeout(command: Command, timeout: Duration) -> io::Result<Output> {
    command_output_until(command, timeout, || false)
}

/// Runs `command` in its own process group with null stdin, collecting its
/// output until it exits. Past `timeout` the group is killed with
/// `ErrorKind::TimedOut`; once `cancelled()` returns true, with
/// `ErrorKind::Interrupted`.
///
/// Both pipes drain on reader threads while the wait polls, so a child that
/// writes more than the pipe buffer still exits instead of blocking on a full
/// pipe until the deadline.
pub(super) fn command_output_until(
    mut command: Command,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
) -> io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn()?;
    let stdout = drain_pipe(child.stdout.take());
    let stderr = drain_pipe(child.stderr.take());
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Output {
                status,
                stdout: stdout.join().unwrap_or_default(),
                stderr: stderr.join().unwrap_or_default(),
            });
        }
        let timed_out = Instant::now() >= deadline;
        if timed_out || cancelled() {
            // The runtime probe may be a wrapper that spawned descendants. Kill its
            // process group so those descendants cannot retain the captured pipes and
            // keep a supposedly bounded passive probe alive.
            // SAFETY: `child.id()` is the live process-group leader created above;
            // negating it targets only that group with a constant signal.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            return Err(if timed_out {
                io::Error::new(io::ErrorKind::TimedOut, "command timed out")
            } else {
                io::Error::new(io::ErrorKind::Interrupted, "command cancelled")
            });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn drain_pipe(pipe: Option<impl io::Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

pub(crate) trait AdapterFactory: Send + Sync {
    fn create(&self, definition: AgentDefinition, generation: u64) -> Arc<dyn HarnessAdapter>;
}

struct AcpAdapterFactory {
    permission_broker: Arc<dyn HarnessPermissionBroker>,
}

impl AdapterFactory for AcpAdapterFactory {
    fn create(&self, definition: AgentDefinition, generation: u64) -> Arc<dyn HarnessAdapter> {
        #[cfg(test_seams)]
        if definition.agent_id.0 == TEST_NATIVE_AGENT_ID {
            return Arc::new(NativeTestAdapter::new(definition, generation));
        }
        Arc::new(AcpHarnessAdapter::with_generation_and_permission_broker(
            definition,
            generation,
            Arc::clone(&self.permission_broker),
        ))
    }
}

struct ManagedAdapter {
    adapter: Arc<dyn HarnessAdapter>,
    status_watch: tokio::task::JoinHandle<()>,
    conversation_uses: Arc<AtomicUsize>,
}

/// Keeps auth rechecks from replacing a handle before its session is bound.
pub(crate) struct ConversationAdapterLease {
    uses: Arc<AtomicUsize>,
}

impl Drop for ConversationAdapterLease {
    fn drop(&mut self) {
        self.uses.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Drop for ManagedAdapter {
    fn drop(&mut self) {
        self.status_watch.abort();
    }
}

type AuthChangeSink = Arc<dyn Fn(&AgentId) + Send + Sync>;
/// Hears a managed adapter's status whenever a live `session/new` or
/// `session/load` changed the models or selects it advertises.
/// Carries only changed session fields; retained models/defaults and selects
/// are omitted so their freshness is independent of another field's update.
pub(crate) type SessionCatalogSink = Arc<dyn Fn(&AgentDefinition, &AdapterStatus) + Send + Sync>;

type SessionCatalog = (
    Vec<AgentModelOption>,
    Option<String>,
    Option<AgentSessionSelects>,
    BTreeMap<String, AgentModelSelects>,
);

fn session_catalog(status: &AdapterStatus) -> SessionCatalog {
    (
        status.models.clone(),
        status.default_model_id.clone(),
        status.session_selects.clone(),
        status.model_selects.clone(),
    )
}

type ReplayLiveWorkGuard = Arc<dyn Fn(&AgentId) -> bool + Send + Sync>;

/// A toolchain path held against pruning; see
/// [`AgentRegistry::lease_toolchain`].
pub(crate) struct ToolchainLease {
    leases: Arc<StdMutex<Vec<PathBuf>>>,
    path: PathBuf,
}

impl Drop for ToolchainLease {
    fn drop(&mut self) {
        let mut leases = self
            .leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = leases.iter().position(|path| path == &self.path) {
            leases.swap_remove(index);
        }
    }
}

#[derive(Debug)]
pub(crate) struct ReplayAdmission {
    _instance: tokio::sync::OwnedMutexGuard<()>,
    _auth: Option<super::auth::AgentSignInRemovalGuard>,
}

pub struct AgentRegistry {
    harnesses: BTreeMap<super::HarnessId, super::HarnessDefinition>,
    instances: StdMutex<BTreeMap<AgentId, super::AgentInstance>>,
    persistent_instances: std::sync::atomic::AtomicBool,
    pub(crate) instance_changes: Arc<Mutex<()>>,
    instance_admissions: StdMutex<BTreeMap<AgentId, Arc<Mutex<()>>>>,
    replay_sign_in: OnceLock<Weak<super::auth::AgentSignInService>>,
    replay_live_work: OnceLock<ReplayLiveWorkGuard>,
    /// Whether an adapter install is in flight for an agent. While one is, a
    /// freshly published pin may still fail verification and roll back, so
    /// running adapters are not moved onto it.
    install_in_flight: OnceLock<ReplayLiveWorkGuard>,
    /// Toolchain paths an adapter install is running npm from. A runtime
    /// upgrade landing mid-install must not prune the Node that npm is
    /// still loading modules from.
    toolchain_leases: Arc<StdMutex<Vec<PathBuf>>>,
    shutting_down: std::sync::atomic::AtomicBool,
    shutdown_owner: StdMutex<Option<String>>,
    live_adapters: StdMutex<Vec<Weak<dyn HarnessAdapter>>>,
    shutdown_operations: StdMutex<BTreeMap<String, tokio_util::sync::CancellationToken>>,
    /// Registration order, which is the default-pick order: lists come out in
    /// it, and clients break readiness ties by position.
    order: Vec<AgentId>,
    adapters: Mutex<BTreeMap<AgentId, ManagedAdapter>>,
    probe: Arc<dyn RuntimeProbe>,
    factory: Arc<dyn AdapterFactory>,
    resolver: Arc<NodeResolver>,
    home_policy: Option<AgentHomePolicy>,
    /// Absent in test registries and when the user has opted out; without it,
    /// launch falls back to npx-on-demand exactly as before installs existed.
    adapter_installs: Option<super::adapter_install::ManagedAdapterInstalls>,
    /// The sign-in verdict the model-catalog probe's throwaway adapter reached,
    /// per agent. The probe runs before any managed adapter exists — right
    /// after install, and on every cold launch — which is exactly when the
    /// managed status has nothing to say. Consulted only while no managed
    /// adapter exists; once one starts, its own verdict wins.
    probed_auth: StdMutex<BTreeMap<AgentId, HarnessAuthState>>,
    auth_changes: Arc<OnceLock<AuthChangeSink>>,
    session_catalogs: Arc<OnceLock<SessionCatalogSink>>,
    refresh_homes: StdMutex<BTreeMap<AgentId, super::refresh_lock::HomeUse>>,
    /// Agents whose last catalog probe is still waiting out a CLI home's
    /// sign-in refresh before it stops. One at a time per agent, so a held
    /// lock never collects a process per probe.
    probe_teardowns: StdMutex<std::collections::BTreeSet<AgentId>>,
}

/// Where each agent's isolated home lives. Ottam points every vendor CLI
/// at `~/.ottam/agents/<id>/home` instead of its default (`~/.claude`,
/// `~/.codex`) so app sessions never pollute the user's own
/// CLI state — no Ottam entries in their resume lists, no shared
/// credentials, and no refresh-token race between the terminal `claude` and
/// ours (the cause of daily forced re-logins when both share one rotating
/// token). The isolation lever is each CLI's own config-dir variable; never
/// override HOME itself — agents spawn shells and git, and those children
/// inheriting a fake HOME would corrupt everything they touch.
#[derive(Clone, Debug)]
pub(crate) struct AgentHomePolicy {
    root: std::path::PathBuf,
}

impl AgentHomePolicy {
    fn from_environment() -> Option<Self> {
        let ottam_home = crate::storage::ottam_home().ok()?;
        Some(Self {
            root: ottam_home.join("agents"),
        })
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::built_in()
    }
}

impl AgentRegistry {
    pub fn built_in() -> Self {
        Self::built_in_with_permission_broker(Arc::new(CancelPermissionBroker))
    }

    pub fn built_in_with_permission_broker(
        permission_broker: Arc<dyn HarnessPermissionBroker>,
    ) -> Self {
        // One resolver shared by the probe and the registry: the probe warms
        // the cache, and the launch rewrite below reads it. Production only:
        // the test constructors leave the managed runtime out entirely, so unit
        // tests never see `~/.ottam`.
        let resolver = Arc::new(NodeResolver::with_managed(
            super::managed_node::ManagedNode::from_environment(),
        ));
        let mut registry = Self::with_components_and_resolver(
            built_in_definitions(),
            Arc::new(SystemRuntimeProbe::with_resolver(Arc::clone(&resolver))),
            Arc::new(AcpAdapterFactory { permission_broker }),
            resolver,
        );
        // Production only: the test constructors leave homes shared so unit
        // tests never touch the real filesystem.
        registry.home_policy = AgentHomePolicy::from_environment();
        registry.adapter_installs =
            super::adapter_install::ManagedAdapterInstalls::from_environment();
        registry
    }

    pub fn with_runtime_probe(probe: Arc<dyn RuntimeProbe>) -> Self {
        Self::with_components(
            built_in_definitions(),
            probe,
            Arc::new(AcpAdapterFactory {
                permission_broker: Arc::new(CancelPermissionBroker),
            }),
        )
    }

    pub(crate) fn with_components(
        definitions: Vec<AgentDefinition>,
        probe: Arc<dyn RuntimeProbe>,
        factory: Arc<dyn AdapterFactory>,
    ) -> Self {
        Self::with_components_and_resolver(
            definitions,
            probe,
            factory,
            Arc::new(NodeResolver::default()),
        )
    }

    pub(crate) fn with_components_and_resolver(
        definitions: Vec<AgentDefinition>,
        probe: Arc<dyn RuntimeProbe>,
        factory: Arc<dyn AdapterFactory>,
        resolver: Arc<NodeResolver>,
    ) -> Self {
        let mut order = Vec::with_capacity(definitions.len());
        let mut by_id = BTreeMap::new();
        for definition in definitions {
            let agent_id = definition.agent_id.clone();
            if by_id
                .insert(
                    super::HarnessId(agent_id.0.clone()),
                    super::HarnessDefinition::from_definition(definition),
                )
                .is_none()
            {
                order.push(agent_id);
            }
        }
        Self {
            harnesses: by_id,
            instances: StdMutex::new(BTreeMap::new()),
            persistent_instances: std::sync::atomic::AtomicBool::new(false),
            instance_changes: Arc::new(Mutex::new(())),
            instance_admissions: StdMutex::new(BTreeMap::new()),
            replay_sign_in: OnceLock::new(),
            replay_live_work: OnceLock::new(),
            install_in_flight: OnceLock::new(),
            toolchain_leases: Arc::new(StdMutex::new(Vec::new())),
            shutting_down: std::sync::atomic::AtomicBool::new(false),
            shutdown_owner: StdMutex::new(None),
            live_adapters: StdMutex::new(Vec::new()),
            shutdown_operations: StdMutex::new(BTreeMap::new()),
            order,
            adapters: Mutex::new(BTreeMap::new()),
            probe,
            factory,
            resolver,
            home_policy: None,
            adapter_installs: None,
            probed_auth: StdMutex::new(BTreeMap::new()),
            auth_changes: Arc::new(OnceLock::new()),
            session_catalogs: Arc::new(OnceLock::new()),
            refresh_homes: StdMutex::new(BTreeMap::new()),
            probe_teardowns: StdMutex::new(std::collections::BTreeSet::new()),
        }
    }

    pub(crate) fn set_auth_change_sink(&self, sink: AuthChangeSink) {
        let _ = self.auth_changes.set(sink);
    }

    pub(crate) fn set_session_catalog_sink(&self, sink: SessionCatalogSink) {
        let _ = self.session_catalogs.set(sink);
    }

    fn manage(&self, adapter: &Arc<dyn HarnessAdapter>) -> ManagedAdapter {
        self.track_adapter(adapter);
        let mut status = adapter.status();
        let (mut auth, mut catalog) = {
            let current = status.borrow_and_update();
            (current.auth_state, session_catalog(&current))
        };
        let definition = adapter.definition().clone();
        let auth_changes = Arc::clone(&self.auth_changes);
        let session_catalogs = Arc::clone(&self.session_catalogs);
        let status_watch = tokio::spawn(async move {
            while status.changed().await.is_ok() {
                // Cloned so the sinks never run under the watch lock: a
                // catalog record persists to disk.
                let current = status.borrow_and_update().clone();
                if current.auth_state != auth {
                    auth = current.auth_state;
                    if let Some(sink) = auth_changes.get() {
                        sink(&definition.agent_id);
                    }
                }
                let next = session_catalog(&current);
                if next != catalog {
                    let models_changed = next.0 != catalog.0 || next.1 != catalog.1;
                    let selects_changed = next.2 != catalog.2;
                    let model_selects_changed = next.3 != catalog.3;
                    catalog = next;
                    if let Some(sink) = session_catalogs.get() {
                        // Report only newly observed fields, even when watch
                        // coalesces them with a subsequent stop/death. Retained
                        // fields must not gain freshness from another field's
                        // update or from the final lifecycle snapshot.
                        let mut observation = current;
                        if !models_changed {
                            observation.models.clear();
                            observation.default_model_id = None;
                        }
                        if !selects_changed {
                            observation.session_selects = None;
                        }
                        if !model_selects_changed {
                            observation.model_selects.clear();
                        }
                        sink(&definition, &observation);
                    }
                }
            }
        });
        ManagedAdapter {
            adapter: Arc::clone(adapter),
            status_watch,
            conversation_uses: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Record what the catalog probe's throwaway adapter learned about
    /// sign-in. `Unknown` is not news and is ignored; `Missing` is what puts
    /// a *Sign In* button in front of the user before they have sent anything.
    pub(crate) fn note_probed_auth(&self, agent_id: &AgentId, auth_state: HarnessAuthState) {
        if auth_state == HarnessAuthState::Unknown {
            return;
        }
        self.probed_auth
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(agent_id.clone(), auth_state);
    }

    /// Forget the probed verdict — the adapter is gone, so it says nothing.
    pub(crate) fn forget_probed_auth(&self, agent_id: &AgentId) {
        self.probed_auth
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(agent_id);
    }

    fn probed_auth(&self, agent_id: &AgentId) -> Option<HarnessAuthState> {
        self.probed_auth
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(agent_id)
            .copied()
    }

    #[cfg(test)]
    fn with_home_policy(mut self, policy: AgentHomePolicy) -> Self {
        self.home_policy = Some(policy);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_home_root(self, root: std::path::PathBuf) -> Self {
        self.with_home_policy(AgentHomePolicy { root })
    }

    #[cfg(test)]
    pub(super) fn with_adapter_installs(
        mut self,
        installs: super::adapter_install::ManagedAdapterInstalls,
    ) -> Self {
        self.adapter_installs = Some(installs);
        self
    }

    /// The isolated home Ottam gives this agent, or None when the agent's
    /// CLI has no config-dir override (or this registry runs without a home
    /// policy, as test registries do) — callers hide home details entirely in
    /// that case. The path is reported whether or not the directory exists;
    /// it is created on the agent's first start.
    pub fn agent_home(&self, agent_id: &AgentId) -> Option<std::path::PathBuf> {
        let definition = self.definition(agent_id).ok()?;
        if let Some(home) = definition.existing_home() {
            return Some(home.to_path_buf());
        }
        if definition.home_variables.is_empty() {
            return None;
        }
        Some(
            self.home_policy
                .as_ref()?
                .root
                .join(&agent_id.0)
                .join("home"),
        )
    }

    /// Serialize cleanup decisions with every launch into the same home.
    /// Terminal logins always obtain a managed adapter first, so a managed
    /// launch permanently rules out proving later locks belong only to probes.
    fn with_refresh_home<R: Default>(
        &self,
        agent: &AgentId,
        action: impl FnOnce(&mut super::refresh_lock::HomeUse, &Path) -> R,
    ) -> R {
        let Ok(definition) = self.definition(agent) else {
            return R::default();
        };
        if definition.harness_id() != CLAUDE_AGENT_ID || definition.existing_home().is_some() {
            return R::default();
        }
        let Some(home) = self.agent_home(agent) else {
            return R::default();
        };
        let mut homes = self
            .refresh_homes
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        action(homes.entry(agent.clone()).or_default(), &home)
    }

    /// After a blocked refresh: removes the agent's refresh lock if its owner
    /// record proves the holder exited. True when the request is worth
    /// sending again. Existing CLI homes are never touched.
    pub(crate) fn clear_abandoned_refresh_lock(&self, agent: &AgentId) -> bool {
        self.with_refresh_home(agent, |_, home| {
            super::refresh_lock::clear_if_owner_gone(home)
        })
    }

    pub(crate) fn begin_catalog_probe(&self, agent: &AgentId) {
        self.with_refresh_home(agent, |state, home| state.probe_start(home));
    }

    pub(crate) fn finish_catalog_probe(&self, agent: &AgentId, stopped: bool) {
        self.with_refresh_home(agent, |state, home| state.probe_finished(home, stopped));
    }

    fn prepare_managed_start(&self, agent: &AgentId) {
        self.with_refresh_home(agent, |state, home| state.managed_start(home));
    }

    pub(crate) fn load_instances(&self, instances: Vec<super::AgentInstance>) {
        self.persistent_instances.store(true, Ordering::Release);
        let mut stored = self
            .instances
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *stored = instances
            .into_iter()
            .map(|instance| (instance.id.clone(), instance))
            .collect();
    }

    pub(crate) fn publish_instance(&self, instance: super::AgentInstance) {
        self.instances
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(instance.id.clone(), instance);
    }

    pub(crate) fn remove_instance(&self, id: &AgentId) {
        self.instances
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(id);
        self.forget_probed_auth(id);
    }

    pub(crate) async fn retire_instance(&self, id: &AgentId) {
        self.adapters.lock().await.remove(id);
        self.remove_instance(id);
    }

    pub(crate) fn purge_home(
        &self,
        id: &AgentId,
        retained_cli_homes: &[PathBuf],
    ) -> Result<Option<PathBuf>, HarnessError> {
        if self.definition(id)?.existing_home().is_some() {
            return Err(HarnessError::new(
                HarnessErrorKind::Internal,
                "Your CLI home cannot be removed by Ottam.",
            ));
        }
        let home = self.agent_home(id);
        if let Some(home) = &home {
            // Never traverse a redirected instance parent or home to purge
            // someone else's files. The shared root may itself be relocated.
            for path in [home.as_path(), home.parent().unwrap()] {
                if std::fs::symlink_metadata(path)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    return Err(HarnessError::new(
                        HarnessErrorKind::Internal,
                        "This profile's home is redirected; delete without home removal.",
                    ));
                }
            }
            let canonical = match std::fs::canonicalize(home) {
                Ok(path) => path,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(_) => {
                    return Err(HarnessError::new(
                        HarnessErrorKind::Internal,
                        "Cannot verify this profile's home; delete without home removal.",
                    ))
                }
            };
            let siblings = self
                .instances
                .lock()
                .unwrap()
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for path in retained_cli_homes {
                let other = match std::fs::canonicalize(path) {
                    Ok(path) => path,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(_) => {
                        return Err(HarnessError::new(
                            HarnessErrorKind::Internal,
                            "Cannot verify a retained CLI home; delete without home removal.",
                        ))
                    }
                };
                if canonical.starts_with(&other) || other.starts_with(&canonical) {
                    return Err(HarnessError::new(
                        HarnessErrorKind::Internal,
                        "This home overlaps a retained CLI setup; delete without home removal.",
                    ));
                }
            }
            for sibling in siblings {
                if sibling.id == *id {
                    continue;
                }
                if let Some(path) = self.agent_home(&sibling.id) {
                    let other = match std::fs::canonicalize(&path) {
                        Ok(path) => path,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(_) => return Err(HarnessError::new(
                            HarnessErrorKind::Internal,
                            "Cannot verify another profile's home; delete without home removal.",
                        )),
                    };
                    if canonical.starts_with(&other) || other.starts_with(&canonical) {
                        return Err(HarnessError::new(HarnessErrorKind::Internal,
                            "This home overlaps another profile or CLI setup; delete without home removal."));
                    }
                }
            }
        }
        Ok(home)
    }

    /// Existing-home attachment must not turn an Ottam-owned directory into
    /// a second instance's CLI home. Check canonical paths to catch aliases.
    pub(crate) fn validate_attachment(&self, home: &super::HomeLayout) -> Result<(), HarnessError> {
        let super::HomeLayout::Existing { path, .. } = home else {
            return Ok(());
        };
        let Some(policy) = &self.home_policy else {
            return Ok(());
        };
        let existing = std::fs::canonicalize(path).map_err(|_| {
            HarnessError::new(HarnessErrorKind::Internal, "Cannot verify your CLI home.")
        })?;
        // Resolve the managed root even before its first directory exists.
        let mut ancestor = policy.root.as_path();
        let mut suffix = Vec::new();
        let root = loop {
            match std::fs::canonicalize(ancestor) {
                Ok(mut root) => {
                    for part in suffix.iter().rev() {
                        root.push(part);
                    }
                    break root;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    suffix.push(ancestor.file_name().ok_or_else(|| {
                        HarnessError::new(
                            HarnessErrorKind::Internal,
                            "Cannot verify the managed profile root.",
                        )
                    })?);
                    ancestor = ancestor.parent().ok_or_else(|| {
                        HarnessError::new(
                            HarnessErrorKind::Internal,
                            "Cannot verify the managed profile root.",
                        )
                    })?;
                }
                Err(_) => {
                    return Err(HarnessError::new(
                        HarnessErrorKind::Internal,
                        "Cannot verify the managed profile root.",
                    ))
                }
            }
        };
        if existing.starts_with(&root) || root.starts_with(&existing) {
            return Err(HarnessError::new(HarnessErrorKind::Internal,
                "This CLI home overlaps Ottam's managed profiles. Use the managed profile instead."));
        }
        Ok(())
    }

    pub(crate) fn instance_admission(&self, id: &AgentId) -> Arc<Mutex<()>> {
        self.instance_admissions
            .lock()
            .unwrap()
            .entry(id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub(crate) async fn delete_adapters(&self, id: &AgentId) -> Result<(), HarnessError> {
        let adapters = self
            .live_adapter_snapshot()
            .into_iter()
            .filter(|adapter| adapter.definition().agent_id == *id)
            .collect::<Vec<_>>();
        for adapter in adapters {
            adapter.shutdown().await?;
        }
        self.shutdown(id).await?;
        Ok(())
    }

    pub(crate) async fn instance_in_use(&self, id: &AgentId) -> bool {
        self.adapters
            .lock()
            .await
            .get(id)
            .is_some_and(|managed| managed.conversation_uses.load(Ordering::Acquire) > 0)
    }

    /// Only installation operations may resolve a harness without an instance.
    pub(crate) fn install_definition(&self, id: &AgentId) -> Result<AgentDefinition, HarnessError> {
        self.harnesses
            .get(&super::HarnessId(id.0.clone()))
            .map(|harness| self.resolved_definition(harness.template_definition()))
            .map(Ok)
            .unwrap_or_else(|| self.definition(id))
    }

    pub(crate) fn prepare_home(&self, agent: &AgentId) -> Result<(), HarnessError> {
        let definition = self.definition(agent)?;
        self.write_client_plugin(&definition)?;
        if let Some(home) = definition.existing_home() {
            return std::fs::read_dir(home).map(|_| ()).map_err(|_| HarnessError::new(HarnessErrorKind::Internal, "Your CLI setup is missing or unreadable. Ottam will not create or replace it."));
        }
        if let Some(home) = self.agent_home(agent) {
            create_managed_home(&home, definition.home_variables).map_err(|_| HarnessError::new(HarnessErrorKind::Internal, "Cannot create the agent's managed home; refusing to use your CLI home as a fallback."))?;
            apply_home_settings(&home, definition.home_settings).map_err(|error| {
                HarnessError::new(
                    HarnessErrorKind::Internal,
                    format!("Cannot write the agent's managed settings: {error}"),
                )
            })?;
        }
        Ok(())
    }

    pub(crate) fn harness_instances(&self, harness_id: &str) -> Vec<AgentId> {
        self.definitions()
            .into_iter()
            .filter(|definition| definition.harness_id() == harness_id)
            .map(|definition| definition.agent_id)
            .collect()
    }

    pub(crate) fn shutdown_waiting_on(&self) -> Vec<String> {
        self.live_shared_definitions()
            .into_iter()
            .filter(|definition| {
                super::refresh_lock::is_refreshing(definition.refresh_lock_path().as_deref())
            })
            .map(|definition| {
                definition.instance.as_ref().map_or_else(
                    || definition.presentation.product_name.into(),
                    |instance| instance.label.clone(),
                )
            })
            .collect()
    }

    /// Snapshot without waiting for the adapter map, whose owner may itself be
    /// waiting for a refresh. Stale locks in homes we have never used cannot
    /// endanger an Ottam child and must not prevent quitting.
    fn track_adapter(&self, adapter: &Arc<dyn HarnessAdapter>) {
        let mut live = self
            .live_adapters
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        live.retain(|adapter| adapter.strong_count() != 0);
        live.push(Arc::downgrade(adapter));
    }

    fn live_adapter_snapshot(&self) -> Vec<Arc<dyn HarnessAdapter>> {
        self.live_adapters
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }

    fn live_shared_definitions(&self) -> Vec<AgentDefinition> {
        self.live_adapter_snapshot()
            .into_iter()
            .filter(|adapter| {
                matches!(
                    adapter.status().borrow().lifecycle,
                    AdapterLifecycleState::Starting
                        | AdapterLifecycleState::Ready
                        | AdapterLifecycleState::Stopping
                )
            })
            .map(|adapter| adapter.definition().clone())
            .filter(|definition| definition.refresh_lock_path().is_some())
            .collect()
    }

    pub(crate) fn active_refresh_lock_paths(&self) -> Vec<PathBuf> {
        self.live_shared_definitions()
            .into_iter()
            .filter_map(|definition| definition.refresh_lock_path())
            .collect()
    }

    pub(crate) fn begin_shutdown_operation(
        &self,
        key: &str,
    ) -> Result<ShutdownOperation<'_>, HarnessError> {
        if key.is_empty() || key.len() > 80 {
            return Err(HarnessError::new(
                HarnessErrorKind::Protocol,
                "Invalid shutdown operation key",
            ));
        }
        let mut operations = self
            .shutdown_operations
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(previous) = operations.remove(key) {
            if previous.is_cancelled() {
                return Err(HarnessError::new(
                    HarnessErrorKind::RequestCancelled,
                    "shutdown preparation cancelled",
                ));
            }
            operations.insert(key.into(), previous);
            return Err(HarnessError::new(
                HarnessErrorKind::Protocol,
                "Shutdown operation already exists",
            ));
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        operations.insert(key.into(), cancel.clone());
        Ok(ShutdownOperation {
            registry: self,
            key: key.into(),
            cancel,
        })
    }

    pub(crate) fn cancel_shutdown_operation(&self, key: &str) {
        if key.is_empty() || key.len() > 80 {
            return;
        }
        // The client can cancel after preparation has replied successfully.
        // Retain ownership until teardown so that late cancellation unlatches
        // this operation, without cancelling a different quit attempt.
        let mut owner = self
            .shutdown_owner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if owner.as_deref() == Some(key) {
            *owner = None;
            self.shutting_down
                .store(false, std::sync::atomic::Ordering::Release);
        }
        let mut operations = self
            .shutdown_operations
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(cancel) = operations.get(key) {
            cancel.cancel();
        } else if operations.len() < 128 {
            let cancel = tokio_util::sync::CancellationToken::new();
            cancel.cancel();
            operations.insert(key.into(), cancel);
        }
        // Keep owner locked until the cancellation token is set: preparation
        // checks that token while taking ownership, including this race.
        drop(operations);
        drop(owner);
    }

    pub(crate) async fn prepare_shutdown(&self, key: &str) -> Result<(), HarnessError> {
        let operation = self.begin_shutdown_operation(key)?;
        let _changes = tokio::select! {
            biased;
            _ = operation.cancel.cancelled() => return Err(HarnessError::new(HarnessErrorKind::RequestCancelled, "shutdown preparation cancelled")),
            changes = self.instance_changes.lock() => changes,
        };
        while !self.shutdown_waiting_on().is_empty() {
            tokio::select! {
                biased;
                _ = operation.cancel.cancelled() => return Err(HarnessError::new(HarnessErrorKind::RequestCancelled, "shutdown preparation cancelled")),
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
        {
            let mut owner = self
                .shutdown_owner
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if operation.cancel.is_cancelled() {
                return Err(HarnessError::new(
                    HarnessErrorKind::RequestCancelled,
                    "shutdown preparation cancelled",
                ));
            }
            *owner = Some(key.into());
            self.shutting_down
                .store(true, std::sync::atomic::Ordering::Release);
        }
        let result = async {
            let adapters = self.live_adapter_snapshot();
            for adapter in adapters {
                loop {
                    let result = tokio::select! {
                        biased;
                        _ = operation.cancel.cancelled() => return Err(HarnessError::new(HarnessErrorKind::RequestCancelled, "shutdown preparation cancelled")),
                        result = adapter.shutdown() => result,
                    };
                    if result.as_ref().is_err_and(|error| error.kind == HarnessErrorKind::AuthRefreshBusy) {
                        tokio::select! {
                            _ = operation.cancel.cancelled() => return Err(HarnessError::new(HarnessErrorKind::RequestCancelled, "shutdown preparation cancelled")),
                            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                        }
                        continue;
                    }
                    result?;
                    break;
                }
            }
            Ok(())
        }.await;
        if let Err(error) = result {
            self.cancel_shutdown_operation(key);
            return Err(error);
        }
        Ok(())
    }

    /// The managed runtime this registry's resolver would consider, for the
    /// install path. None when the user opted out or there is no app home.
    pub(crate) fn managed_node(&self) -> Option<super::managed_node::ManagedNode> {
        self.resolver.managed().cloned()
    }

    /// The installed-adapter store, for the install service. None when the
    /// user opted out or there is no app home.
    pub(crate) fn adapter_installs(
        &self,
    ) -> Option<super::adapter_install::ManagedAdapterInstalls> {
        self.adapter_installs.clone()
    }

    /// The bin directory of the Node runtime agents launch with, read as
    /// their launches read it: from the cache, never searching. Local MCP
    /// servers put it first on their PATH and outlive any one call, so a
    /// directory handed out here is held against pruning for the rest of
    /// this process; the next launch's reconcile removes it once superseded.
    pub(crate) fn node_bin_dir(&self) -> Option<PathBuf> {
        let dir = self
            .cached_node_runtime()?
            .bin_dir()
            .map(Path::to_path_buf)?;
        let mut leases = self
            .toolchain_leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !leases.contains(&dir) {
            leases.push(dir.clone());
        }
        Some(dir)
    }

    /// The toolchain the resolver last found, without searching. Warm after
    /// any `runtime_status` call; a cold read returns None.
    pub(crate) fn cached_node_runtime(&self) -> Option<ResolvedNodeRuntime> {
        self.resolver.cached_runtime()
    }

    /// In registration order, which `agent.list` and `engine.list` keep.
    pub fn definitions(&self) -> Vec<AgentDefinition> {
        let extras: Vec<_> = self
            .instances
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .keys()
            .filter(|id| !self.order.contains(id))
            .cloned()
            .collect();
        self.order
            .iter()
            .chain(extras.iter())
            .filter_map(|id| self.definition(id).ok())
            .collect()
    }

    /// Point an npm-launched definition at the resolved toolchain.
    ///
    /// Every consumer of a definition goes through here — the adapter spawn,
    /// the model-catalog probe, and the sign-in terminal — so the launchd PATH
    /// is corrected once rather than at each call site.
    ///
    /// Reads the resolver cache without ever searching: adapter starts run
    /// `require_runtime` first, which resolves, so the cache is warm by then. A
    /// cold read leaves the bare `npx` name in place, matching the behaviour
    /// before the resolver existed.
    fn resolved_definition(&self, mut definition: AgentDefinition) -> AgentDefinition {
        // The isolated home rides the same everyone-passes-here path as the
        // PATH fix, so the adapter spawn, the model-catalog probe, and the
        // sign-in terminal all see one consistent home per agent.
        if let Some(super::AgentInstance {
            home: super::HomeLayout::Existing { environment, .. },
            ..
        }) = &definition.instance
        {
            for (name, value) in environment {
                definition.launch.environment.retain(|(key, _)| key != name);
                if let Some(value) = value {
                    definition
                        .launch
                        .environment
                        .push((name.clone(), value.clone()));
                }
            }
        } else if let Some(policy) = &self.home_policy {
            let home = policy.root.join(&definition.agent_id.0).join("home");
            set_home_environment(&mut definition, &home);
            if let Some(variable) = definition.client_plugin.and_then(|p| p.shell_environment) {
                let own: serde_json::Map<_, _> = definition
                    .home_variables
                    .iter()
                    .map(|home| {
                        let value = std::env::var(home.name).ok();
                        (home.name.to_string(), value.into())
                    })
                    .collect();
                definition
                    .launch
                    .environment
                    .retain(|(key, _)| key != variable);
                definition
                    .launch
                    .environment
                    .push((variable.into(), serde_json::Value::Object(own).to_string()));
            }
        }
        // The plugin loads whatever the home: it is Ottam's, not the
        // profile's.
        if let (Some(plugin), Some(directory)) = (
            definition.client_plugin,
            self.client_plugin_dir(&definition),
        ) {
            let quoted = serde_json::to_string(&directory.to_string_lossy()).unwrap_or_default();
            let mut value = plugin.template.replace("\"{dir}\"", &quoted);
            // An existing setup keeps its own value of the variable, with the
            // plugin merged in; an Ottam profile's value is Ottam's.
            if definition.existing_home().is_some() {
                let own = definition
                    .launch
                    .environment
                    .iter()
                    .find(|(key, _)| key == plugin.variable)
                    .map(|(_, value)| value.clone())
                    .or_else(|| std::env::var(plugin.variable).ok());
                if let Some(own) = own {
                    value = merge_json_config(&own, &value);
                }
            }
            definition
                .launch
                .environment
                .retain(|(key, _)| key != plugin.variable);
            definition
                .launch
                .environment
                .push((plugin.variable.to_string(), value));
        }
        // A binary-lane adapter runs its installed executable directly. Not
        // installed, it is left as declared; `require_runtime` refuses the
        // start before anything spawns.
        if definition.runs_binary() {
            if let Some(executable) = self.installed_entrypoint(&definition) {
                definition.launch.command = executable.to_string_lossy().into_owned();
            }
            return definition;
        }
        if definition.launch.command != "npx" {
            return definition;
        }
        let runtime = self.resolver.cached_runtime();

        // An installed adapter runs `node <entrypoint>` directly: no npx, no
        // registry, no `.bin/` shim re-resolving through `env`.
        if let Some(entrypoint) = self.installed_entrypoint(&definition) {
            definition.launch.command = runtime
                .as_ref()
                .map(|resolved| resolved.node.to_string_lossy().into_owned())
                .unwrap_or_else(|| "node".to_string());
            definition.launch.arguments = vec![entrypoint.to_string_lossy().into_owned()];
            prepend_toolchain_path(&mut definition, runtime.as_ref());
            return definition;
        }

        let Some(runtime) = runtime else {
            return definition;
        };
        definition.launch.command = runtime.npx.to_string_lossy().into_owned();
        prepend_toolchain_path(&mut definition, Some(&runtime));
        definition
    }

    /// The entrypoint this definition launches: its pin when installed, or
    /// the older version standing in while an app update's new pin installs.
    fn installed_entrypoint(&self, definition: &AgentDefinition) -> Option<std::path::PathBuf> {
        self.launchable_adapter(definition)
            .map(|installed| installed.entrypoint)
    }

    pub(crate) fn launchable_adapter(
        &self,
        definition: &AgentDefinition,
    ) -> Option<super::adapter_install::InstalledAdapter> {
        let installs = self.adapter_installs.as_ref()?;
        let package = definition.adapter.as_ref()?;
        installs.launchable(definition.harness_id(), package)
    }

    /// The installed version that launches now: the pin, or an older one
    /// standing in while an app update's new pin installs.
    pub(crate) fn launched_adapter_version(&self, definition: &AgentDefinition) -> Option<String> {
        self.launchable_adapter(definition)
            .map(|installed| installed.version)
    }

    /// Hold `path` against pruning until the lease drops.
    pub(crate) fn lease_toolchain(&self, path: PathBuf) -> ToolchainLease {
        self.toolchain_leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(path.clone());
        ToolchainLease {
            leases: Arc::clone(&self.toolchain_leases),
            path,
        }
    }

    /// Run `prune` with the test for what it must keep: a directory a live
    /// adapter was launched from, or one a toolchain lease holds. Leases are
    /// locked for the whole pass, so an install cannot lease a directory
    /// between the test and its removal; it waits, then finds it gone.
    pub(crate) fn prune_unused<R>(&self, prune: impl FnOnce(&dyn Fn(&Path) -> bool) -> R) -> R {
        let leases = self
            .toolchain_leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune(&|directory: &Path| {
            leases.iter().any(|path| path.starts_with(directory))
                || self.adapter_launched_from(directory)
        })
    }

    /// Whether something [`Self::prune_unused`] would keep runs from
    /// `directory`.
    #[cfg(test)]
    pub(crate) fn launch_uses(&self, directory: &Path) -> bool {
        self.prune_unused(|in_use| in_use(directory))
    }

    /// Whether a live adapter was launched out of `directory` — its command
    /// or entrypoint lives there.
    fn adapter_launched_from(&self, directory: &Path) -> bool {
        self.live_adapter_snapshot().iter().any(|adapter| {
            // A stopped or dead adapter may still be referenced, but runs
            // nothing.
            if matches!(
                adapter.status().borrow().lifecycle,
                AdapterLifecycleState::Stopped | AdapterLifecycleState::Dead
            ) {
                return false;
            }
            let launch = &adapter.definition().launch;
            std::iter::once(&launch.command)
                .chain(launch.arguments.iter())
                .any(|part| std::path::Path::new(part).starts_with(directory))
        })
    }

    /// Create a standalone adapter for the model-catalog probe, outside the
    /// managed map, so a throwaway session never disturbs the shared
    /// adapter's lifecycle or status. It is still tracked as a live child, so
    /// Quit, removal, and install pruning wait for it like any other.
    /// `extra_arguments` follow the launch's own, for a probe that points the
    /// agent somewhere else.
    pub(crate) fn create_probe_adapter(
        &self,
        agent_id: &AgentId,
        extra_arguments: Vec<String>,
    ) -> Result<Arc<dyn HarnessAdapter>, HarnessError> {
        self.prepare_home(agent_id)?;
        let mut definition = self.definition(agent_id)?;
        definition.launch.arguments.extend(extra_arguments);
        let adapter = self.factory.create(definition, 1);
        self.track_adapter(&adapter);
        Ok(adapter)
    }

    /// Claims the agent's one background probe teardown; false while the
    /// previous one is still waiting.
    pub(crate) fn begin_probe_teardown(&self, agent_id: &AgentId) -> bool {
        self.probe_teardowns
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(agent_id.clone())
    }

    pub(crate) fn probe_teardown_pending(&self, agent_id: &AgentId) -> bool {
        self.probe_teardowns
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains(agent_id)
    }

    pub(crate) fn finish_probe_teardown(&self, agent_id: &AgentId) {
        self.probe_teardowns
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(agent_id);
    }

    /// Installation verification must never borrow a user's shared home, even
    /// when first-run onboarding changed the seeded instance's layout.
    pub(crate) fn create_install_verifier(
        &self,
        agent_id: &AgentId,
    ) -> Result<Arc<dyn HarnessAdapter>, HarnessError> {
        let mut definition = self.definition(agent_id)?;
        let policy = self.home_policy.as_ref().ok_or_else(|| {
            HarnessError::new(
                HarnessErrorKind::Internal,
                "Installation verification requires an Ottam profile",
            )
        })?;
        let home = policy.root.join(&agent_id.0).join("install-verification");
        if let Some(instance) = &mut definition.instance {
            instance.home = super::HomeLayout::Managed;
        }
        create_managed_home(&home, definition.home_variables).map_err(|error| {
            HarnessError::new(
                HarnessErrorKind::Internal,
                format!("Cannot prepare installation verification home: {error}"),
            )
        })?;
        apply_home_settings(&home, definition.home_settings).map_err(|error| {
            HarnessError::new(
                HarnessErrorKind::Internal,
                format!("Cannot prepare installation verification home: {error}"),
            )
        })?;
        set_home_environment(&mut definition, &home);
        self.write_client_plugin(&definition)?;
        Ok(self.factory.create(definition, 1))
    }

    /// Where a definition's client plugin lives: beside the harness's
    /// installs, shared by every instance. `None` without a home policy.
    fn client_plugin_dir(&self, definition: &AgentDefinition) -> Option<PathBuf> {
        let plugin = definition.client_plugin?;
        Some(
            self.home_policy
                .as_ref()?
                .root
                .join(definition.harness_id())
                .join("plugins")
                .join(plugin.name),
        )
    }

    /// Write the client plugin's files, rewriting only what differs so a
    /// running agent never sees a half-written file it already loaded.
    fn write_client_plugin(&self, definition: &AgentDefinition) -> Result<(), HarnessError> {
        let (Some(plugin), Some(directory)) =
            (definition.client_plugin, self.client_plugin_dir(definition))
        else {
            return Ok(());
        };
        let failed = |error: io::Error| {
            HarnessError::new(
                HarnessErrorKind::Internal,
                format!("Cannot write Ottam's plugin for this agent: {error}"),
            )
        };
        std::fs::create_dir_all(&directory).map_err(failed)?;
        for (name, contents) in plugin.files {
            let path = directory.join(name);
            if std::fs::read(&path).ok().as_deref() == Some(contents.as_bytes()) {
                continue;
            }
            // Unique per write: two starts may stage the same file at once.
            static STAGED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let staged = directory.join(format!(
                ".{name}.{}.{}.tmp",
                std::process::id(),
                STAGED.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::write(&staged, contents).map_err(failed)?;
            std::fs::rename(&staged, &path).map_err(failed)?;
        }
        Ok(())
    }

    pub(crate) fn set_replay_live_work_guard(&self, guard: ReplayLiveWorkGuard) {
        let _ = self.replay_live_work.set(guard);
    }

    pub(crate) fn set_install_in_flight_guard(&self, guard: ReplayLiveWorkGuard) {
        let _ = self.install_in_flight.set(guard);
    }

    pub(crate) fn set_replay_sign_in_service(
        &self,
        service: &Arc<super::auth::AgentSignInService>,
    ) {
        let _ = self.replay_sign_in.set(Arc::downgrade(service));
    }

    pub(crate) async fn prepare_replay(
        &self,
        agent_id: &AgentId,
    ) -> Result<ReplayAdmission, HarnessError> {
        // Match sign-in/delete lock order: authentication first, then instance.
        // Timeout admission before creating a replay child; cleanup itself keeps
        // this instance fenced until refresh-safe child exit.
        let admission = tokio::time::timeout(Duration::from_secs(60), async {
            let auth = match self.replay_sign_in.get().and_then(Weak::upgrade) {
                Some(service) => Some(
                    service
                        .guard_removal(&ottam_protocol::AgentID(agent_id.0.clone()))
                        .await?,
                ),
                None => None,
            };
            let instance = self.instance_admission(agent_id).lock_owned().await;
            Ok::<_, HarnessError>(ReplayAdmission {
                _instance: instance,
                _auth: auth,
            })
        })
        .await
        .map_err(|_| {
            HarnessError::new(
                HarnessErrorKind::Protocol,
                "Session replay admission timed out",
            )
        })??;
        self.prepare_home(agent_id)?;
        if self.instance_in_use(agent_id).await
            || self
                .replay_live_work
                .get()
                .is_some_and(|guard| guard(agent_id))
        {
            return Err(HarnessError::new(
                HarnessErrorKind::Protocol,
                "Stop this profile's active runs and tasks before restoring tool results.",
            ));
        }
        // Codex locks a loaded session even between turns. Retire only this
        // idle instance's adapter before the read-only replay loads it. The
        // admission stays owned through replay cleanup, preventing a new turn
        // or deletion from racing for the same persisted session.
        if self.definition(agent_id)?.harness_id() == CODEX_AGENT_ID {
            self.shutdown(agent_id).await?;
        }
        Ok(admission)
    }

    pub(crate) async fn create_replay_adapter(
        &self,
        agent_id: &AgentId,
    ) -> Result<
        (
            Arc<dyn HarnessAdapter>,
            Result<AdapterStatus, HarnessError>,
            ReplayAdmission,
        ),
        HarnessError,
    > {
        let admission = self.prepare_replay(agent_id).await?;
        let _changes = tokio::time::timeout(Duration::from_secs(60), self.instance_changes.lock())
            .await
            .map_err(|_| {
                HarnessError::new(
                    HarnessErrorKind::Protocol,
                    "Session replay admission timed out",
                )
            })?;
        if self
            .shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(HarnessError::new(
                HarnessErrorKind::RequestCancelled,
                "Ottam is quitting",
            ));
        }
        let adapter: Arc<dyn HarnessAdapter> = Arc::new(
            super::acp::AcpHarnessAdapter::with_generation_and_permission_broker(
                self.definition(agent_id)?,
                1,
                Arc::new(super::types::CancelPermissionBroker),
            ),
        );
        self.track_adapter(&adapter);
        // Start under the same gate as chat adapters. Keep ownership even on
        // initialization failure so replay cleanup can join the supervisor.
        let initialized = tokio::time::timeout(Duration::from_secs(60), adapter.start())
            .await
            .unwrap_or_else(|_| {
                Err(HarnessError::new(
                    HarnessErrorKind::Protocol,
                    "Session replay initialization timed out",
                ))
            });
        Ok((adapter, initialized, admission))
    }

    pub fn definition(&self, agent_id: &AgentId) -> Result<AgentDefinition, HarnessError> {
        if let Some(instance) = self
            .instances
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(agent_id)
            .cloned()
        {
            let template = self
                .harnesses
                .get(&instance.harness_id)
                .cloned()
                .ok_or_else(|| {
                    HarnessError::new(HarnessErrorKind::AgentNotFound, "Unknown agent harness")
                })?;
            return Ok(self.resolved_definition(template.resolve(instance)));
        }
        // The opt-in in-process test engine has no persisted account/home.
        // Product harnesses must never resolve a deleted seed via a template.
        if self.persistent_instances.load(Ordering::Acquire)
            && agent_id.0 != TEST_NATIVE_AGENT_ID
            && agent_id.0 != TEST_BINARY_AGENT_ID
        {
            return Err(HarnessError::new(
                HarnessErrorKind::AgentNotFound,
                "This agent was removed or is unavailable",
            ));
        }
        self.harnesses
            .get(&super::HarnessId(agent_id.0.clone()))
            .cloned()
            .map(|harness| self.resolved_definition(harness.template_definition()))
            .ok_or_else(|| {
                HarnessError::new(
                    HarnessErrorKind::AgentNotFound,
                    format!("unknown agent {agent_id}"),
                )
            })
    }

    pub async fn cached_status(
        &self,
        agent_id: &AgentId,
    ) -> Result<super::types::AdapterStatus, HarnessError> {
        self.definition(agent_id)?;
        let adapters = self.adapters.lock().await;
        Ok(adapters
            .get(agent_id)
            .map(|managed| {
                let mut status = managed.adapter.status().borrow().clone();
                // A reinstall retains the managed instance so its generation
                // remains monotonic, but shuts it down before replacing the
                // files. While stopped, the fresh verification probe is the
                // authoritative sign-in verdict just as it is before a first
                // launch. Once this adapter starts, its own status wins again
                // — as soon as it has one. A running adapter that has not
                // opened a session yet (the one a sign-in starts to read its
                // methods, say) knows nothing, and the probe's verdict is
                // what keeps Sign In on offer. A login that succeeds forgets
                // the probe, so a stale "signed out" cannot outlive it.
                if status.lifecycle == AdapterLifecycleState::Stopped
                    || status.auth_state == HarnessAuthState::Unknown
                {
                    if let Some(auth_state) = self.probed_auth(agent_id) {
                        status.auth_state = auth_state;
                    }
                }
                status
            })
            .unwrap_or_else(|| {
                let mut status = super::types::AdapterStatus::stopped();
                // Nothing managed has run, but the catalog probe may have: a
                // signed-out verdict from it is what lets Settings and the
                // engine picker offer Sign In on a cold launch instead of
                // waiting for the first prompt to fail.
                if let Some(auth_state) = self.probed_auth(agent_id) {
                    status.auth_state = auth_state;
                }
                status.capabilities.steering = if agent_id.0 == TEST_NATIVE_AGENT_ID {
                    super::types::SteeringCapability::Native
                } else {
                    super::types::SteeringCapability::InterruptThenPrompt
                };
                status
            }))
    }

    pub async fn running_adapter(
        &self,
        agent_id: &AgentId,
    ) -> Result<Arc<dyn HarnessAdapter>, HarnessError> {
        self.definition(agent_id)?;
        let adapters = self.adapters.lock().await;
        let adapter = adapters
            .get(agent_id)
            .map(|managed| Arc::clone(&managed.adapter))
            .ok_or_else(|| {
                HarnessError::new(
                    HarnessErrorKind::NotRunning,
                    format!("agent {agent_id} is not running"),
                )
            })?;
        match adapter.status().borrow().lifecycle {
            AdapterLifecycleState::Ready => Ok(adapter),
            AdapterLifecycleState::Dead => Err(HarnessError::new(
                HarnessErrorKind::AdapterDead,
                format!("agent {agent_id} is dead"),
            )),
            _ => Err(HarnessError::new(
                HarnessErrorKind::NotRunning,
                format!("agent {agent_id} is not ready"),
            )),
        }
    }

    pub async fn runtime_status(
        &self,
        agent_id: &AgentId,
    ) -> Result<AgentRuntimeStatus, HarnessError> {
        let definition = self.definition(agent_id)?;
        self.probe_runtime_definition(definition).await
    }

    pub(crate) async fn install_runtime_status(
        &self,
        id: &AgentId,
    ) -> Result<AgentRuntimeStatus, HarnessError> {
        self.probe_runtime_definition(self.install_definition(id)?)
            .await
    }

    async fn probe_runtime_definition(
        &self,
        definition: AgentDefinition,
    ) -> Result<AgentRuntimeStatus, HarnessError> {
        if definition.agent_id.0 == TEST_NATIVE_AGENT_ID {
            return Ok(AgentRuntimeStatus::ready("in-process", "in-process"));
        }
        if definition.runs_binary() {
            return Ok(AgentRuntimeStatus::not_required());
        }
        let probe = Arc::clone(&self.probe);
        tokio::task::spawn_blocking(move || probe.probe(&definition))
            .await
            .map_err(|error| {
                HarnessError::new(
                    HarnessErrorKind::Internal,
                    format!("runtime probe task failed: {error}"),
                )
            })
    }

    /// Resolve the Node agents launch with, searching on a cold cache. May
    /// block on a login-shell probe; call from a blocking-capable thread.
    pub(crate) fn resolve_node(&self) -> super::node_resolver::NodeResolution {
        self.resolver.resolve()
    }

    /// Force the next runtime probe to rediscover Node and re-read its
    /// versions. Ordinary engine-list polling stays cached; the UI's explicit
    /// "Check Again" action uses this after an install or version-manager
    /// switch.
    pub fn invalidate_runtime_cache(&self) {
        self.resolver.invalidate();
        self.probe.invalidate();
    }

    pub async fn get_or_start(
        &self,
        agent_id: &AgentId,
    ) -> Result<Arc<dyn HarnessAdapter>, HarnessError> {
        let _admission = self.instance_admission(agent_id).lock_owned().await;
        let _instance_change = self.instance_changes.lock().await;
        self.get_or_start_locked(agent_id).await
    }

    /// Claim the adapter before returning it to a run, including the gap
    /// before session/new or session/load publishes a session summary.
    pub(crate) async fn get_or_start_for_conversation(
        &self,
        agent_id: &AgentId,
    ) -> Result<(Arc<dyn HarnessAdapter>, ConversationAdapterLease), HarnessError> {
        let _admission = self.instance_admission(agent_id).lock_owned().await;
        let _changes = self.instance_changes.lock().await;
        let adapter = self.get_or_start_locked(agent_id).await?;
        let adapters = self.adapters.lock().await;
        let uses = Arc::clone(&adapters.get(agent_id).unwrap().conversation_uses);
        uses.fetch_add(1, Ordering::AcqRel);
        Ok((adapter, ConversationAdapterLease { uses }))
    }

    async fn get_or_start_locked(
        &self,
        agent_id: &AgentId,
    ) -> Result<Arc<dyn HarnessAdapter>, HarnessError> {
        if self
            .shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(HarnessError::new(
                HarnessErrorKind::RequestCancelled,
                "Ottam is quitting",
            ));
        }
        self.prepare_home(agent_id)?;
        self.retire_stale_idle_adapter(agent_id).await;
        let mut adapters = self.adapters.lock().await;
        let adapter = if let Some(managed) = adapters.get(agent_id) {
            if managed.adapter.status().borrow().lifecycle == AdapterLifecycleState::Stopped {
                self.require_runtime(agent_id).await?;
                let definition = self.definition(agent_id)?;
                if managed.adapter.definition() != &definition {
                    let generation = managed
                        .adapter
                        .status()
                        .borrow()
                        .generation
                        .saturating_add(1);
                    let adapter = self.factory.create(definition, generation);
                    adapters.insert(agent_id.clone(), self.manage(&adapter));
                    adapter
                } else {
                    Arc::clone(&managed.adapter)
                }
            } else {
                Arc::clone(&managed.adapter)
            }
        } else {
            // The probe resolves Node. Fetch the launch definition only after
            // it completes so a cold first start cannot capture bare `npx`
            // before the resolver cache is warm.
            self.require_runtime(agent_id).await?;
            let definition = self.definition(agent_id)?;
            let adapter = self.factory.create(definition, 1);
            adapters.insert(agent_id.clone(), self.manage(&adapter));
            adapter
        };
        self.prepare_managed_start(agent_id);
        let start = adapter.start();
        drop(adapters);
        start.await?;
        Ok(adapter)
    }

    /// Stop a running adapter whose launch target has moved — an upgraded
    /// adapter or Node landed after it started — when nothing is using it, so
    /// the acquisition that follows starts the new version through the
    /// ordinary stopped-adapter path. A busy adapter keeps running what it
    /// was launched with; its sessions restore on the new version at the
    /// first acquisition that finds it idle.
    ///
    /// "Idle" is the same bar replay uses: no conversation holds a lease, no
    /// task is live, and no browser sign-in is waiting on the adapter. An
    /// install still verifying its pin also holds the move off. Loaded but
    /// idle sessions restore through `session/load` on the next turn.
    async fn retire_stale_idle_adapter(&self, agent_id: &AgentId) {
        let previous = {
            let adapters = self.adapters.lock().await;
            let Some(managed) = adapters.get(agent_id) else {
                return;
            };
            if managed.adapter.status().borrow().lifecycle == AdapterLifecycleState::Stopped
                || managed.conversation_uses.load(Ordering::Acquire) > 0
            {
                return;
            }
            Arc::clone(&managed.adapter)
        };
        let Ok(current) = self.definition(agent_id) else {
            return;
        };
        let running = &previous.definition().launch;
        // A cold resolver yields bare `node`/`npx`, on either side; that is
        // the same command found later, not a new target.
        let bare = |command: &str| matches!(command, "node" | "npx");
        let same_command = running.command == current.launch.command
            || bare(&running.command)
            || bare(&current.launch.command);
        if same_command && running.arguments == current.launch.arguments {
            return;
        }
        let busy = self
            .replay_live_work
            .get()
            .is_some_and(|guard| guard(agent_id))
            || self
                .install_in_flight
                .get()
                .is_some_and(|guard| guard(agent_id))
            || self
                .replay_sign_in
                .get()
                .and_then(Weak::upgrade)
                .is_some_and(|service| {
                    service.is_signing_in(&ottam_protocol::AgentID(agent_id.0.clone()))
                });
        if busy {
            return;
        }
        tracing::info!(
            target: "ottam_core::agent",
            agent_id = %agent_id,
            "restarting an idle adapter onto its upgraded launch"
        );
        if let Err(error) = previous.shutdown().await {
            tracing::warn!(
                target: "ottam_core::agent",
                agent_id = %agent_id,
                error = %error.message,
                "could not retire the stale adapter; it keeps running"
            );
        }
    }

    pub async fn shutdown(&self, agent_id: &AgentId) -> Result<(), HarnessError> {
        let adapter = self
            .adapters
            .lock()
            .await
            .get(agent_id)
            .map(|managed| Arc::clone(&managed.adapter));
        match adapter {
            Some(adapter) => adapter.shutdown().await,
            None if self.definition(agent_id).is_ok() => Ok(()),
            None => Err(HarnessError::new(
                HarnessErrorKind::AgentNotFound,
                format!("unknown agent {agent_id}"),
            )),
        }
    }

    /// Ottam is quitting: shutdown preparation or teardown has begun,
    /// and adapters are stopping or stopped.
    pub(crate) fn is_shutting_down(&self) -> bool {
        self.shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub async fn shutdown_all(&self) -> Result<(), HarnessError> {
        self.shutting_down
            .store(true, std::sync::atomic::Ordering::Release);
        let mut first_error = None;
        loop {
            for adapter in self.live_adapter_snapshot() {
                loop {
                    if matches!(
                        adapter.status().borrow().lifecycle,
                        AdapterLifecycleState::Starting
                            | AdapterLifecycleState::Ready
                            | AdapterLifecycleState::Stopping
                    ) {
                        super::refresh_lock::wait_for_release(
                            adapter.definition().refresh_lock_path().as_deref(),
                        )
                        .await;
                    }
                    match adapter.shutdown().await {
                        Err(error) if error.kind == HarnessErrorKind::AuthRefreshBusy => continue,
                        Err(error) => {
                            first_error.get_or_insert(error);
                            break;
                        }
                        Ok(()) => break,
                    }
                }
            }
            // A permanent failure need not change the adapter's lifecycle or
            // release a pending start's admission gate. Finish this snapshot's
            // other adapters, then report it instead of spinning on liveness.
            if let Some(error) = first_error.take() {
                return Err(error);
            }
            // Stop tracked starts before joining their admission gate. A start
            // may hold that gate until shutdown releases its initialize waiter.
            // Once acquired, no admitted start can publish another child; later
            // starters observe shutting_down and are rejected.
            if let Ok(_changes) = self.instance_changes.try_lock() {
                if !self.live_adapter_snapshot().iter().any(|adapter| {
                    matches!(
                        adapter.status().borrow().lifecycle,
                        AdapterLifecycleState::Starting
                            | AdapterLifecycleState::Ready
                            | AdapterLifecycleState::Stopping
                    )
                }) {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(())
    }

    pub async fn restart(
        &self,
        agent_id: &AgentId,
    ) -> Result<Arc<dyn HarnessAdapter>, HarnessError> {
        let _admission = self.instance_admission(agent_id).lock_owned().await;
        let _changes = self.instance_changes.lock().await;
        self.restart_locked(agent_id).await
    }

    /// Authentication rechecks may replace only initialization-only adapters.
    /// Use the same gate as conversation acquisition, not a session snapshot
    /// taken before a later restart acquires its own gate.
    pub(crate) async fn recheck_existing_auth(
        &self,
        agent_id: &AgentId,
    ) -> Result<(), HarnessError> {
        // A browser sign-in's callback server lives in the adapter; a restart
        // would end it. The sign-in reports its own outcome.
        let signing_in = self
            .replay_sign_in
            .get()
            .and_then(Weak::upgrade)
            .is_some_and(|service| {
                service.is_signing_in(&ottam_protocol::AgentID(agent_id.0.clone()))
            });
        if signing_in {
            return Ok(());
        }
        let _admission = self.instance_admission(agent_id).lock_owned().await;
        let _changes = self.instance_changes.lock().await;
        let restart = {
            let adapters = self.adapters.lock().await;
            match adapters.get(agent_id) {
                Some(managed)
                    if managed.conversation_uses.load(Ordering::Acquire) > 0
                        || !managed.adapter.sessions().is_empty() =>
                {
                    return Ok(())
                }
                Some(_) => true,
                None => false,
            }
        };
        if restart {
            self.restart_locked(agent_id).await?;
        } else {
            self.get_or_start_locked(agent_id).await?;
        }
        Ok(())
    }

    async fn restart_locked(
        &self,
        agent_id: &AgentId,
    ) -> Result<Arc<dyn HarnessAdapter>, HarnessError> {
        if self
            .shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(HarnessError::new(
                HarnessErrorKind::NotRunning,
                "Ottam is preparing to quit.",
            ));
        }
        self.prepare_home(agent_id)?;
        self.require_runtime(agent_id).await?;
        let definition = self.definition(agent_id)?;
        let previous = self
            .adapters
            .lock()
            .await
            .get(agent_id)
            .map(|managed| Arc::clone(&managed.adapter));
        let next_generation = previous.as_ref().map_or(1, |adapter| {
            adapter.status().borrow().generation.saturating_add(1)
        });
        if let Some(previous) = previous {
            previous.shutdown().await?;
        }

        let adapter = self.factory.create(definition, next_generation);
        let mut adapters = self.adapters.lock().await;
        adapters.insert(agent_id.clone(), self.manage(&adapter));
        self.prepare_managed_start(agent_id);
        let start = adapter.start();
        drop(adapters);
        start.await?;
        Ok(adapter)
    }

    /// Whether this agent needs an install before it can start: the store is
    /// active, the definition pins a package, and the pin is not on disk.
    /// `false` for registries without a store (tests, opted-out machines),
    /// which then behave exactly as before installs existed.
    pub(crate) fn adapter_needs_install(&self, definition: &AgentDefinition) -> bool {
        self.adapter_installs.is_some()
            && definition.adapter.is_some()
            && self.installed_entrypoint(definition).is_none()
    }

    /// Whether the exact pin is on disk. Unlike [`Self::adapter_needs_install`],
    /// an older stand-in does not count: this is what decides whether a
    /// reinstall replaces the running tree or installs beside it.
    pub(crate) fn adapter_pin_installed(&self, definition: &AgentDefinition) -> bool {
        let (Some(installs), Some(package)) = (&self.adapter_installs, &definition.adapter) else {
            return false;
        };
        installs
            .installed_entrypoint(definition.harness_id(), package)
            .is_some()
    }

    async fn require_runtime(&self, agent_id: &AgentId) -> Result<(), HarnessError> {
        let status = self.runtime_status(agent_id).await?;
        // Install is mandatory: npx-on-demand would silently pull an
        // unverified tree on first prompt, and "ready" is a handshake-backed
        // claim the install flow makes, not something a cold start may skip.
        if let Ok(definition) = self.definition(agent_id) {
            if self.adapter_needs_install(&definition) {
                return Err(HarnessError::new(
                    HarnessErrorKind::AdapterNotInstalled,
                    format!("{agent_id}'s adapter is not installed"),
                ));
            }
        }
        match status.availability {
            RuntimeAvailability::Ready => Ok(()),
            RuntimeAvailability::Missing { component } => Err(HarnessError::new(
                HarnessErrorKind::RuntimeMissing,
                format!("{component} is required to run {agent_id}"),
            )),
            RuntimeAvailability::Invalid { component } => Err(HarnessError::new(
                HarnessErrorKind::RuntimeInvalid,
                format!("{component} returned an invalid version"),
            )),
            RuntimeAvailability::Unsupported { found, minimum } => Err(HarnessError::new(
                HarnessErrorKind::RuntimeUnsupported,
                format!("Node {found} is unsupported; {agent_id} requires Node {minimum}+"),
            )),
        }
    }
}

pub(crate) struct ShutdownOperation<'a> {
    registry: &'a AgentRegistry,
    key: String,
    pub(crate) cancel: tokio_util::sync::CancellationToken,
}

impl Drop for ShutdownOperation<'_> {
    fn drop(&mut self) {
        self.registry
            .shutdown_operations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.key);
    }
}

/// Lead the child's PATH with the resolved toolchain's bin directory.
///
/// An absolute command is not sufficient on its own: npx is a
/// `#!/usr/bin/env node` script, and the adapters spawn `node` for their own
/// subprocesses — both re-resolve through PATH.
fn prepend_toolchain_path(definition: &mut AgentDefinition, runtime: Option<&ResolvedNodeRuntime>) {
    let Some(bin_dir) = runtime.and_then(ResolvedNodeRuntime::bin_dir) else {
        return;
    };
    let path = toolchain_path(bin_dir).to_string_lossy().into_owned();
    definition
        .launch
        .environment
        .push(("PATH".to_string(), path));
}

/// Core's own PATH with `bin_dir` first.
pub(crate) fn toolchain_path(bin_dir: &Path) -> std::ffi::OsString {
    let mut path = bin_dir.as_os_str().to_owned();
    if let Some(inherited) = std::env::var_os("PATH").filter(|path| !path.is_empty()) {
        path.push(":");
        path.push(inherited);
    }
    path
}

fn pinned(package: (&str, &str)) -> AdapterPackage {
    AdapterPackage {
        name: package.0.to_string(),
        version: package.1.to_string(),
        source: AdapterSource::Npm,
    }
}

/// Point every home variable at `home`, replacing values the launch spec or
/// an earlier pass set.
fn set_home_environment(definition: &mut AgentDefinition, home: &Path) {
    for variable in definition.home_variables {
        definition
            .launch
            .environment
            .retain(|(key, _)| key != variable.name);
        definition.launch.environment.push((
            variable.name.into(),
            variable.value(home).to_string_lossy().into_owned(),
        ));
    }
}

/// Merge Ottam's JSON config into the user's: objects merge key by key,
/// arrays gain the entries they lack, and Ottam's scalars win. A user
/// value that isn't a JSON object gives way to Ottam's.
fn merge_json_config(own: &str, ours: &str) -> String {
    fn merge(own: &mut serde_json::Value, ours: serde_json::Value) {
        match (own, ours) {
            (serde_json::Value::Object(own), serde_json::Value::Object(ours)) => {
                for (key, value) in ours {
                    match own.get_mut(&key) {
                        Some(existing) => merge(existing, value),
                        None => {
                            own.insert(key, value);
                        }
                    }
                }
            }
            (serde_json::Value::Array(own), serde_json::Value::Array(ours)) => {
                for value in ours {
                    if !own.contains(&value) {
                        own.push(value);
                    }
                }
            }
            (own, ours) => *own = ours,
        }
    }
    let (Ok(mut merged @ serde_json::Value::Object(_)), Ok(ours_value)) = (
        serde_json::from_str::<serde_json::Value>(own),
        serde_json::from_str::<serde_json::Value>(ours),
    ) else {
        return ours.to_string();
    };
    merge(&mut merged, ours_value);
    merged.to_string()
}

/// Set each managed setting, keeping the file's other keys. A file that
/// isn't a JSON object is replaced: the home is Ottam's.
fn apply_home_settings(home: &Path, settings: &[HomeSetting]) -> io::Result<()> {
    for setting in settings {
        let path = home.join(setting.file);
        let mut object = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|value| match value {
                serde_json::Value::Object(object) => Some(object),
                _ => None,
            })
            .unwrap_or_default();
        if object.get(setting.key) == Some(&serde_json::Value::Bool(setting.value)) {
            continue;
        }
        object.insert(setting.key.into(), serde_json::Value::Bool(setting.value));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut text = serde_json::to_string_pretty(&object).map_err(io::Error::other)?;
        text.push('\n');
        std::fs::write(&path, text)?;
    }
    Ok(())
}

/// Create a managed home and the directories its variables name inside it.
fn create_managed_home(home: &Path, variables: &[HomeVariable]) -> io::Result<()> {
    std::fs::create_dir_all(home)?;
    for variable in variables {
        if let HomeTarget::Dir(_) = variable.target {
            std::fs::create_dir_all(variable.value(home))?;
        }
    }
    Ok(())
}

/// Order is the default-pick order: with equal readiness, a new chat starts on
/// the first agent here.
pub(super) fn built_in_definitions() -> Vec<AgentDefinition> {
    let claude = pinned(CLAUDE_ACP_PACKAGE);
    let codex = pinned(CODEX_ACP_PACKAGE);
    #[cfg_attr(not(test_seams), allow(unused_mut))]
    let mut definitions = vec![
        AgentDefinition {
            agent_id: AgentId::new(CLAUDE_AGENT_ID),
            instance: None,
            presentation: AgentPresentation {
                product_name: "Claude Code",
                vendor_name: "Anthropic",
                vendor_home: Some("~/.claude"),
                brand: Some("anthropic"),
                account_label: "Claude account",
                account_kind: AccountKind::Subscription,
            },
            // Claude's login prints a browser URL and exits on its own; the
            // terminal only matters when that flow gets stuck.
            terminal_login_reveal: TerminalLoginReveal::WhenStuck,
            launch: AgentLaunchSpec {
                command: "npx".to_string(),
                arguments: vec!["-y".to_string(), claude.spec()],
                environment: Vec::new(),
                minimum_node_major: CLAUDE_MINIMUM_NODE_MAJOR,
            },
            adapter: Some(claude),
            home_variables: &[HomeVariable {
                name: "CLAUDE_CONFIG_DIR",
                target: HomeTarget::Root,
            }],
            // Claude Code may have a subagent deliver its final report
            // through `SubagentHandback` instead of its last message, and the
            // parent messages a running subagent through `SendMessage`.
            subagent_messages: SubagentMessageTools {
                handback: Some(SubagentHandbackTool {
                    tool: "SubagentHandback",
                    message: "message",
                }),
                follow_up: Some(SubagentFollowUpTool {
                    tool: "SendMessage",
                    target: "to",
                    message: "message",
                }),
            },
            child_sessions: None,
            client_plugin: None,
            home_settings: &[],
            form_skip: FormSkip::Decline,
            sign_in_proof: SignInProof::Session,
            terminal_login_args: None,
            mcp_connections: McpConnections::PerSession,
            // At 0.100.0 a transcript appears only with the first prompt; the
            // adapter spreads `claudeCode.options` into the CLI's query
            // options, so this keeps even that from being written.
            catalog_probe: ProbeCleanup::SessionMeta(|| {
                serde_json::Map::from_iter([(
                    "claudeCode".to_string(),
                    serde_json::json!({"options": {"persistSession": false}}),
                )])
            }),
        },
        AgentDefinition {
            agent_id: AgentId::new(CODEX_AGENT_ID),
            instance: None,
            presentation: AgentPresentation {
                product_name: "Codex",
                vendor_name: "OpenAI",
                vendor_home: Some("~/.codex"),
                brand: Some("openai"),
                account_label: "ChatGPT account",
                account_kind: AccountKind::Subscription,
            },
            terminal_login_reveal: TerminalLoginReveal::Immediate,
            launch: AgentLaunchSpec {
                command: "npx".to_string(),
                arguments: vec!["-y".to_string(), codex.spec()],
                environment: Vec::new(),
                minimum_node_major: CODEX_MINIMUM_NODE_MAJOR,
            },
            adapter: Some(codex),
            home_variables: &[HomeVariable {
                name: "CODEX_HOME",
                target: HomeTarget::Root,
            }],
            // codex-acp hides its collaboration calls in native mode.
            subagent_messages: SubagentMessageTools::default(),
            child_sessions: None,
            client_plugin: None,
            home_settings: &[],
            form_skip: FormSkip::Decline,
            sign_in_proof: SignInProof::Session,
            terminal_login_args: None,
            mcp_connections: McpConnections::PerSession,
            // At 2.5.0 the rollout file is written with the first prompt, so
            // a bare `session/new` stores no thread.
            catalog_probe: ProbeCleanup::Nothing,
        },
        opencode_definition(),
        omp_definition(),
    ];
    #[cfg(test_seams)]
    push_test_definitions(&mut definitions);
    definitions
}

/// The opt-in test agents, compiled out of release builds (see `build.rs`).
#[cfg(test_seams)]
fn push_test_definitions(definitions: &mut Vec<AgentDefinition>) {
    if std::env::var("OTTAM_TEST_NATIVE_STEER_ENGINE").as_deref() == Ok("1") {
        definitions.push(AgentDefinition {
            agent_id: AgentId::new(TEST_NATIVE_AGENT_ID),
            instance: None,
            presentation: AgentPresentation {
                product_name: "Native steering test",
                vendor_name: "Ottam",
                vendor_home: None,
                brand: None,
                account_label: "Provider API key",
                account_kind: AccountKind::ApiKey,
            },
            terminal_login_reveal: TerminalLoginReveal::Immediate,
            launch: AgentLaunchSpec {
                command: "in-process".to_string(),
                arguments: Vec::new(),
                environment: Vec::new(),
                minimum_node_major: 0,
            },
            adapter: None,
            home_variables: &[],
            subagent_messages: SubagentMessageTools::default(),
            child_sessions: None,
            client_plugin: None,
            home_settings: &[],
            form_skip: FormSkip::Decline,
            sign_in_proof: SignInProof::Session,
            terminal_login_args: None,
            mcp_connections: McpConnections::PerSession,
            catalog_probe: ProbeCleanup::Nothing,
        });
    }
    if let Some(definition) = test_binary_definition() {
        definitions.push(definition);
    }
}

/// The opt-in binary-lane test agent. Its download is whatever
/// `OTTAM_TEST_BINARY_AGENT_URL` serves, checked against
/// `OTTAM_TEST_BINARY_AGENT_SHA256`; with
/// `OTTAM_TEST_BINARY_AGENT_MEMBER` set the download is a tarball holding
/// the executable at that path.
#[cfg(test_seams)]
fn test_binary_definition() -> Option<AgentDefinition> {
    let url = std::env::var("OTTAM_TEST_BINARY_AGENT_URL").ok()?;
    let sha256 = std::env::var("OTTAM_TEST_BINARY_AGENT_SHA256").ok()?;
    let archive = match std::env::var("OTTAM_TEST_BINARY_AGENT_MEMBER") {
        Ok(member) => BinaryArchive::Tgz { member },
        Err(_) => BinaryArchive::Raw,
    };
    Some(AgentDefinition {
        agent_id: AgentId::new(TEST_BINARY_AGENT_ID),
        instance: None,
        presentation: AgentPresentation {
            product_name: "Binary test",
            vendor_name: "Ottam",
            vendor_home: None,
            brand: None,
            account_label: "Provider API key",
            account_kind: AccountKind::ApiKey,
        },
        terminal_login_reveal: TerminalLoginReveal::Immediate,
        launch: AgentLaunchSpec {
            command: "binary-test-acp".to_string(),
            arguments: vec!["acp".to_string()],
            environment: Vec::new(),
            minimum_node_major: 0,
        },
        adapter: Some(AdapterPackage {
            name: "binary-test-acp".to_string(),
            version: "1.0.0".to_string(),
            source: AdapterSource::Binary(BinaryAdapter {
                command: "binary-test-acp".to_string(),
                artifacts: vec![BinaryArtifact {
                    arch: std::env::consts::ARCH,
                    url,
                    sha256,
                    archive,
                }],
            }),
        }),
        home_variables: &[
            HomeVariable {
                name: "OTTAM_TEST_BINARY_DB",
                target: HomeTarget::File("state.db"),
            },
            HomeVariable {
                name: "OTTAM_TEST_BINARY_CONFIG",
                target: HomeTarget::Dir("config"),
            },
        ],
        subagent_messages: SubagentMessageTools::default(),
        child_sessions: None,
        client_plugin: None,
        home_settings: &[],
        form_skip: FormSkip::Decline,
        sign_in_proof: SignInProof::Session,
        terminal_login_args: None,
        mcp_connections: McpConnections::PerSession,
        catalog_probe: ProbeCleanup::Nothing,
    })
}

/// OpenCode v2 ships as Bun-compiled executables inside npm platform
/// tarballs; there are no GitHub release assets for v2. The digests are
/// sha256 of each tarball, cross-checked against npm's sha512 integrity.
const OPENCODE_VERSION: &str = "2.2.5";
const OPENCODE_ARTIFACTS: [(&str, &str, &str); 2] = [
    (
        "aarch64",
        "https://registry.npmjs.org/@opencode/cli-darwin-arm64/-/cli-darwin-arm64-2.2.5.tgz",
        "dfd827de3281b4c3ca2b0bfc8f4beb103fa660c419d148a1a515c4344f9e9c02",
    ),
    (
        "x86_64",
        "https://registry.npmjs.org/@opencode/cli-darwin-x64/-/cli-darwin-x64-2.2.5.tgz",
        "304f4666e7e37eac338de258f763b7f5b8987ab1bde2341dc1a613a8205c0010",
    ),
];

fn opencode_definition() -> AgentDefinition {
    AgentDefinition {
        agent_id: AgentId::new(OPENCODE_AGENT_ID),
        instance: None,
        presentation: AgentPresentation {
            product_name: "OpenCode",
            vendor_name: "Anomaly",
            vendor_home: Some("~/.config/opencode"),
            brand: Some("anomaly"),
            account_label: "Model providers",
            // With no connection OpenCode lists Zen's free `opencode/*` models
            // (`plugin/provider/opencode.ts:332-350` at 2.0.27). A Zen key
            // shares the prefix, so a Zen-only setup also reads as none.
            account_kind: AccountKind::MultiProvider {
                keyless_providers: &["opencode"],
            },
        },
        // `opencode auth login` opens a provider picker that waits for a
        // choice.
        terminal_login_reveal: TerminalLoginReveal::Immediate,
        launch: AgentLaunchSpec {
            command: "opencode".to_string(),
            arguments: vec!["acp".to_string()],
            // OpenCode checks for updates on its own; the pin is Ottam's.
            environment: vec![("OPENCODE_DISABLE_AUTOUPDATE".to_string(), "1".to_string())],
            minimum_node_major: 0,
        },
        adapter: Some(AdapterPackage {
            name: "@opencode/cli".to_string(),
            version: OPENCODE_VERSION.to_string(),
            source: AdapterSource::Binary(BinaryAdapter {
                command: "opencode".to_string(),
                artifacts: OPENCODE_ARTIFACTS
                    .iter()
                    .map(|(arch, url, sha256)| BinaryArtifact {
                        arch,
                        url: url.to_string(),
                        sha256: sha256.to_string(),
                        archive: BinaryArchive::Tgz {
                            member: "package/bin/opencode".to_string(),
                        },
                    })
                    .collect(),
            }),
        }),
        // Sessions and credentials live in the database, so the database and
        // the config directory are the profile. The data and state homes hold
        // workspace snapshots, shell output, logs, and the service
        // registration; OpenCode has no override of its own for them
        // (`global-roots.ts`), so the XDG variables move them into the
        // profile and the plugin gives the user's values back to shell
        // commands. The cache stays shared: it holds only downloads.
        home_variables: &[
            HomeVariable {
                name: "OPENCODE_DB",
                target: HomeTarget::File("opencode.db"),
            },
            HomeVariable {
                name: "OPENCODE_CONFIG_DIR",
                target: HomeTarget::Dir("config"),
            },
            HomeVariable {
                name: "XDG_DATA_HOME",
                target: HomeTarget::Dir("data"),
            },
            HomeVariable {
                name: "XDG_STATE_HOME",
                target: HomeTarget::Dir("state"),
            },
        ],
        subagent_messages: SubagentMessageTools::default(),
        child_sessions: Some(ChildSessionTags {
            meta_key: "opencode/child-session",
            control_tool: "subagent",
            name: "description",
            prompt: "prompt",
        }),
        // `OPENCODE_CONFIG_CONTENT` is OpenCode's highest-precedence config
        // layer, so the plugin loads for an Ottam profile and an existing
        // setup alike without touching either's files.
        client_plugin: Some(ClientPlugin {
            name: "ottam-foreground",
            files: &[
                ("index.js", include_str!("plugins/opencode-foreground.js")),
                ("package.json", "{\"type\": \"module\"}\n"),
            ],
            variable: "OPENCODE_CONFIG_CONTENT",
            template: r#"{"plugins":["{dir}"]}"#,
            shell_environment: Some("OTTAM_SHELL_ENVIRONMENT"),
        }),
        // CLI commands such as `opencode auth login` otherwise reuse the one
        // background service registered under `$XDG_STATE_HOME/opencode`,
        // whatever database it opened, so a profile's sign-in could land in
        // another profile or the user's own CLI. Disabled, each command
        // starts its own server on this profile's database. OpenCode reads
        // the setting from its config directory (`service-config.ts`).
        home_settings: &[HomeSetting {
            file: "config/service.json",
            key: "disabled",
            value: true,
        }],
        form_skip: FormSkip::EmptyAccept,
        sign_in_proof: SignInProof::Session,
        terminal_login_args: None,
        mcp_connections: McpConnections::PerDirectory,
        // At 2.2.5 every `session/new` stores a session row, and
        // `session/delete` removes it.
        catalog_probe: ProbeCleanup::DeleteSession,
    }
}

/// oh-my-pi ships signed Bun-compiled executables as raw GitHub release
/// assets; the digests are the release's own `SHA256SUMS.txt`. omp changes
/// ACP behavior in patch releases, so a bump re-records
/// `fixtures/omp-<version>.json` and goes through the roster spec.
const OMP_VERSION: &str = "18.16.0";
const OMP_ARTIFACTS: [(&str, &str, &str); 2] = [
    (
        "aarch64",
        "https://github.com/can1357/oh-my-pi/releases/download/v18.16.0/omp-darwin-arm64",
        "d4a10d97488d6054d2b18198bd24f461079952c7e0fb75ba66336ca0df0cb99e",
    ),
    (
        "x86_64",
        "https://github.com/can1357/oh-my-pi/releases/download/v18.16.0/omp-darwin-x64",
        "abd62d315b66314f0f635d537407037e7b0d06782513d8035669db5ef744242d",
    ),
];

fn omp_definition() -> AgentDefinition {
    AgentDefinition {
        agent_id: AgentId::new(OMP_AGENT_ID),
        instance: None,
        presentation: AgentPresentation {
            product_name: "Oh-My-Pi",
            vendor_name: "omp.sh",
            vendor_home: Some("~/.omp"),
            brand: Some("omp"),
            account_label: "Model providers",
            // omp lists Apple's on-device model whenever the Mac has one
            // (`apple-foundation-models`), credentials or not.
            account_kind: AccountKind::MultiProvider {
                keyless_providers: &["apple"],
            },
        },
        // `omp login` opens straight on a provider picker that waits for a
        // choice.
        terminal_login_reveal: TerminalLoginReveal::Immediate,
        launch: AgentLaunchSpec {
            command: "omp".to_string(),
            arguments: vec!["acp".to_string()],
            // A named profile overrides `PI_CODING_AGENT_DIR` (`dirs.ts`), so
            // an empty one keeps the home in force whatever the user exports.
            environment: vec![("OMP_PROFILE".to_string(), String::new())],
            minimum_node_major: 0,
        },
        adapter: Some(AdapterPackage {
            name: "omp".to_string(),
            version: OMP_VERSION.to_string(),
            source: AdapterSource::Binary(BinaryAdapter {
                command: "omp".to_string(),
                artifacts: OMP_ARTIFACTS
                    .iter()
                    .map(|(arch, url, sha256)| BinaryArtifact {
                        arch,
                        url: url.to_string(),
                        sha256: sha256.to_string(),
                        archive: BinaryArchive::Raw,
                    })
                    .collect(),
            }),
        }),
        // The agent directory holds credentials (`agent.db`), settings,
        // sessions, and the agent `.env`, and an absolute override skips
        // omp's XDG redirection. The `~/.omp` root stays shared: its `.env`,
        // plugins, worktrees, and logs (decided 2026-10-04 in
        // agent-instances).
        home_variables: &[HomeVariable {
            name: "PI_CODING_AGENT_DIR",
            target: HomeTarget::Root,
        }],
        subagent_messages: SubagentMessageTools::default(),
        child_sessions: None,
        client_plugin: None,
        home_settings: &[],
        // omp reads anything but `accept` with content as a dismissal
        // (`elicitFormFromAcpClient`).
        form_skip: FormSkip::Decline,
        // omp opens a session with no provider set up, then fails the prompt.
        sign_in_proof: SignInProof::ModelSelector,
        // The advertised `omp acp --acp-terminal-auth` is omp's plain TUI:
        // its provider picker shows only in the first-run setup wizard, so a
        // second setup lands on a prompt. `omp login` lists every provider,
        // stores the one picked, and exits.
        terminal_login_args: Some(&["login"]),
        mcp_connections: McpConnections::PerSession,
        // At 18.16.0 every `session/new` writes a `.jsonl` under the agent
        // directory's `sessions/`. omp has no `session/delete`, ignores
        // `--no-session` in ACP, and records a handle in
        // `acp-session-handles.json` only for a switched or forked session.
        catalog_probe: ProbeCleanup::ScratchSessionDir("--session-dir"),
    }
}

fn probe_runtime_with(
    definition: &AgentDefinition,
    mut run: impl FnMut(&str, &str) -> io::Result<Output>,
) -> AgentRuntimeStatus {
    let node = match command_version("node", &mut run) {
        Ok(version) => version,
        Err(ProbeFailure::Missing) => return missing_status("node"),
        Err(ProbeFailure::Invalid) => return invalid_status("node"),
    };
    let node_major = match parse_node_major(&node) {
        Some(major) => major,
        None => return invalid_status("node"),
    };
    if node_major < definition.launch.minimum_node_major {
        return AgentRuntimeStatus {
            availability: RuntimeAvailability::Unsupported {
                found: node_major,
                minimum: definition.launch.minimum_node_major,
            },
            source: None,
            node_version: Some(node),
            npx_version: None,
        };
    }

    let npx = match command_version("npx", &mut run) {
        Ok(version) if parse_semver_major(&version).is_some() => version,
        Ok(_) | Err(ProbeFailure::Invalid) => return invalid_status_with_node("npx", node),
        Err(ProbeFailure::Missing) => return missing_status_with_node("npx", node),
    };
    AgentRuntimeStatus::ready(node, npx)
}

enum ProbeFailure {
    Missing,
    Invalid,
}

fn command_version(
    program: &str,
    run: &mut impl FnMut(&str, &str) -> io::Result<Output>,
) -> Result<String, ProbeFailure> {
    let output = run(program, "--version").map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            ProbeFailure::Missing
        } else {
            ProbeFailure::Invalid
        }
    })?;
    if !output.status.success() {
        return Err(ProbeFailure::Invalid);
    }
    let version = String::from_utf8(output.stdout)
        .map_err(|_| ProbeFailure::Invalid)?
        .trim()
        .to_string();
    if version.is_empty() {
        Err(ProbeFailure::Invalid)
    } else {
        Ok(version)
    }
}

pub(super) fn parse_node_major(version: &str) -> Option<u64> {
    parse_semver_major(version.strip_prefix('v').unwrap_or(version))
}

fn parse_semver_major(version: &str) -> Option<u64> {
    let mut components = version.split('.');
    let major = components.next()?;
    let minor = components.next()?;
    let patch = components.next()?;
    if components.next().is_some()
        || [major, minor, patch].iter().any(|component| {
            component.is_empty() || !component.chars().all(|value| value.is_ascii_digit())
        })
    {
        return None;
    }
    major.parse().ok()
}

fn missing_status(component: &str) -> AgentRuntimeStatus {
    AgentRuntimeStatus {
        availability: RuntimeAvailability::Missing {
            component: component.to_string(),
        },
        source: None,
        node_version: None,
        npx_version: None,
    }
}

fn invalid_status(component: &str) -> AgentRuntimeStatus {
    AgentRuntimeStatus {
        availability: RuntimeAvailability::Invalid {
            component: component.to_string(),
        },
        source: None,
        node_version: None,
        npx_version: None,
    }
}

fn missing_status_with_node(component: &str, node: String) -> AgentRuntimeStatus {
    AgentRuntimeStatus {
        availability: RuntimeAvailability::Missing {
            component: component.to_string(),
        },
        source: None,
        node_version: Some(node),
        npx_version: None,
    }
}

fn invalid_status_with_node(component: &str, node: String) -> AgentRuntimeStatus {
    AgentRuntimeStatus {
        availability: RuntimeAvailability::Invalid {
            component: component.to_string(),
        },
        source: None,
        node_version: Some(node),
        npx_version: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        AdapterLifecycleState, AdapterStatus, HarnessAuthState, HarnessFuture, HarnessPrompt,
        HarnessSessionId, HarnessSessionSummary, HarnessStopReason, HarnessUpdateReceiver,
        HarnessUpdateSender,
    };
    use std::collections::HashMap;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::ExitStatus;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use tokio::sync::watch;

    /// Output past the pipe buffer must not stall the child: the wait drains
    /// both pipes while it polls for exit.
    #[test]
    fn command_output_with_large_output_does_not_time_out() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "head -c 200000 /dev/zero; head -c 200000 /dev/zero >&2"]);
        let output = command_output_with_timeout(command, Duration::from_secs(5))
            .expect("command finishes before the timeout");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 200_000);
        assert_eq!(output.stderr.len(), 200_000);
    }

    #[test]
    fn ottam_config_merges_into_the_users() {
        let merged = |own: &str| -> serde_json::Value {
            serde_json::from_str(&merge_json_config(own, r#"{"plugins":["/ottam"]}"#)).unwrap()
        };
        assert_eq!(
            merged(r#"{"plugins":["/mine","/ottam"],"mcp":{"servers":{}}}"#),
            serde_json::json!({"plugins": ["/mine", "/ottam"], "mcp": {"servers": {}}})
        );
        assert_eq!(
            merged(r#"{"plugins":"/odd"}"#),
            serde_json::json!({"plugins": ["/ottam"]})
        );
        for unusable in ["", "not json", "[1]"] {
            assert_eq!(
                merged(unusable),
                serde_json::json!({"plugins": ["/ottam"]})
            );
        }
    }

    /// The managed runtime only earns its download if it can run *every*
    /// engine. If a pin bump raises an adapter's `engines.node` past the
    /// preference floor, this fails — and it should, because at that point a
    /// system Node at the old floor would be silently preferred while leaving
    /// the strictest engine broken.
    #[test]
    fn preference_floor_matches_the_strictest_engine() {
        let strictest = built_in_definitions()
            .iter()
            .map(|definition| definition.launch.minimum_node_major)
            .max()
            .expect("at least one built-in agent");
        assert_eq!(PREFERRED_NODE_MAJOR, strictest);
    }

    /// Versions are spelled out rather than compared against the constants:
    /// moving a pin should be a deliberate edit here, not something a rename
    /// carries along silently.
    #[test]
    fn built_ins_define_both_account_backed_agents() {
        let definitions = built_in_definitions();
        let codex = definitions
            .iter()
            .find(|definition| definition.agent_id.0 == CODEX_AGENT_ID)
            .expect("Codex definition");
        assert_eq!(
            codex.presentation,
            AgentPresentation {
                product_name: "Codex",
                vendor_name: "OpenAI",
                vendor_home: Some("~/.codex"),
                brand: Some("openai"),
                account_label: "ChatGPT account",
                account_kind: AccountKind::Subscription,
            }
        );
        // The only built-in whose terminal login shows from the first frame;
        // the sign-in Done path depends on one existing.
        assert_eq!(codex.terminal_login_reveal, TerminalLoginReveal::Immediate);
        assert_eq!(
            codex.launch.arguments,
            ["-y", "@agentclientprotocol/codex-acp@2.5.0"]
        );
        assert_eq!(
            codex.adapter.as_ref().map(AdapterPackage::spec).as_deref(),
            Some("@agentclientprotocol/codex-acp@2.5.0")
        );
        // Literals, not the constants: mirroring each adapter's declared
        // `engines.node` is the whole point, so moving a floor should be a
        // deliberate edit here rather than something a rename carries along.
        assert_eq!(codex.launch.minimum_node_major, 18);
        assert!(codex.launch.environment.is_empty());

        let claude = definitions
            .iter()
            .find(|definition| definition.agent_id.0 == CLAUDE_AGENT_ID)
            .expect("Claude definition");
        assert_eq!(
            claude.presentation,
            AgentPresentation {
                product_name: "Claude Code",
                vendor_name: "Anthropic",
                vendor_home: Some("~/.claude"),
                brand: Some("anthropic"),
                account_label: "Claude account",
                account_kind: AccountKind::Subscription,
            }
        );
        // Claude's login bounces to a browser and exits on its own; the
        // terminal stays hidden unless that flow gets stuck.
        assert_eq!(claude.terminal_login_reveal, TerminalLoginReveal::WhenStuck);
        assert_eq!(
            claude.launch.arguments,
            ["-y", "@agentclientprotocol/claude-agent-acp@0.100.0"]
        );
        assert_eq!(
            claude.adapter.as_ref().map(AdapterPackage::spec).as_deref(),
            Some("@agentclientprotocol/claude-agent-acp@0.100.0")
        );
        assert_eq!(claude.launch.minimum_node_major, 22);
        assert!(claude.launch.environment.is_empty());
    }

    /// Lists keep registration order, not id order: clients take position as
    /// the default-pick tiebreak, so Claude Code leads and a later agent
    /// (whatever its id sorts as) follows the built-ins.
    #[test]
    fn definitions_keep_registration_order() {
        let ids = |registry: &AgentRegistry| {
            registry
                .definitions()
                .into_iter()
                .map(|definition| definition.agent_id.0)
                .collect::<Vec<_>>()
        };
        let registry = |definitions| {
            AgentRegistry::with_components(
                definitions,
                Arc::new(ReadyProbe),
                Arc::new(FakeFactory::default()),
            )
        };
        let built_in = registry(built_in_definitions());
        assert_eq!(ids(&built_in)[..2], [CLAUDE_AGENT_ID, CODEX_AGENT_ID]);

        let custom = registry(vec![
            AgentDefinition::for_test("zeta"),
            AgentDefinition::for_test("alpha"),
            AgentDefinition::for_test("zeta"),
        ]);
        assert_eq!(ids(&custom), ["zeta", "alpha"]);
    }

    /// The launchd-PATH fix: an npm-launched adapter has to spawn through the
    /// resolved toolchain, and its bin directory has to lead PATH — npx is a
    /// `#!/usr/bin/env node` script and the adapter spawns `node` itself.
    #[test]
    fn a_resolved_toolchain_rewrites_npm_launch_specs() {
        let resolver = Arc::new(NodeResolver::default());
        resolver.store_for_test(NodeResolution::Resolved(ResolvedNodeRuntime {
            node: PathBuf::from("/opt/homebrew/bin/node"),
            npx: PathBuf::from("/opt/homebrew/bin/npx"),
            npm: PathBuf::from("/opt/homebrew/bin/npm"),
            source: NodeSource::WellKnown,
        }));
        let registry = AgentRegistry::with_components_and_resolver(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
            resolver,
        );

        let claude = registry
            .definition(&AgentId::new(CLAUDE_AGENT_ID))
            .expect("claude definition");
        assert_eq!(claude.launch.command, "/opt/homebrew/bin/npx");
        let path = claude
            .launch
            .environment
            .iter()
            .find(|(name, _)| name == "PATH")
            .map(|(_, value)| value.clone())
            .expect("PATH injected");
        assert!(path.starts_with("/opt/homebrew/bin"));
    }

    fn seed_install(
        installs: &super::super::adapter_install::ManagedAdapterInstalls,
        root: &Path,
        agent_id: &str,
        package: &AdapterPackage,
    ) -> PathBuf {
        let _ = installs;
        let version_dir = root.join(agent_id).join("adapter").join(&package.version);
        let entrypoint_relative = format!(
            "node_modules/{}/dist/acp.js",
            package.name.trim_start_matches('@').replace('/', "-")
        );
        let entrypoint = version_dir.join(&entrypoint_relative);
        std::fs::create_dir_all(entrypoint.parent().expect("parent")).expect("dirs");
        std::fs::write(&entrypoint, "// adapter").expect("entrypoint");
        std::fs::write(
            version_dir.join("manifest.json"),
            serde_json::json!({
                "name": package.name,
                "version": package.version,
                "resolved": null,
                "integrity": null,
                "entrypoint": entrypoint_relative,
                "nodeVersion": null,
                "installedAtUnix": 1,
            })
            .to_string(),
        )
        .expect("manifest");
        entrypoint
    }

    /// The installed path: `node <entrypoint>` through the resolved toolchain,
    /// with the same PATH lead the npx path gets — adapters spawn `node` for
    /// their own subprocesses either way.
    #[test]
    fn an_installed_adapter_launches_node_on_its_entrypoint() {
        let store = tempfile::tempdir().expect("tempdir");
        let resolver = Arc::new(NodeResolver::default());
        resolver.store_for_test(NodeResolution::Resolved(ResolvedNodeRuntime {
            node: PathBuf::from("/opt/homebrew/bin/node"),
            npx: PathBuf::from("/opt/homebrew/bin/npx"),
            npm: PathBuf::from("/opt/homebrew/bin/npm"),
            source: NodeSource::WellKnown,
        }));
        let installs =
            super::super::adapter_install::ManagedAdapterInstalls::at(store.path().to_path_buf());
        let registry = AgentRegistry::with_components_and_resolver(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
            resolver,
        )
        .with_adapter_installs(installs.clone());

        let claude = registry
            .definition(&AgentId::new(CLAUDE_AGENT_ID))
            .expect("claude definition");
        let package = claude.adapter.clone().expect("claude pin");
        let entrypoint = seed_install(&installs, store.path(), CLAUDE_AGENT_ID, &package);

        let claude = registry
            .definition(&AgentId::new(CLAUDE_AGENT_ID))
            .expect("claude definition");
        assert_eq!(claude.launch.command, "/opt/homebrew/bin/node");
        assert_eq!(
            claude.launch.arguments,
            vec![entrypoint.to_string_lossy().into_owned()]
        );
        let path = claude
            .launch
            .environment
            .iter()
            .find(|(name, _)| name == "PATH")
            .map(|(_, value)| value.clone())
            .expect("PATH injected");
        assert!(path.starts_with("/opt/homebrew/bin"));
        assert!(!registry.adapter_needs_install(&claude));
    }

    /// An older app's pin still on disk stands in for a moved pin: the agent
    /// keeps launching it, reports the stand-in version, and is not sent back
    /// through first-run setup while the new pin installs beside it.
    #[test]
    fn an_older_install_stands_in_for_a_moved_pin() {
        let store = tempfile::tempdir().expect("tempdir");
        let installs =
            super::super::adapter_install::ManagedAdapterInstalls::at(store.path().to_path_buf());
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
        )
        .with_adapter_installs(installs.clone());

        let claude = registry
            .definition(&AgentId::new(CLAUDE_AGENT_ID))
            .expect("claude definition");
        let pin = claude.adapter.clone().expect("claude pin");
        let mut old = pin.clone();
        old.version = "0.0.1".to_string();
        let old_entrypoint = seed_install(&installs, store.path(), CLAUDE_AGENT_ID, &old);

        let claude = registry
            .definition(&AgentId::new(CLAUDE_AGENT_ID))
            .expect("claude definition");
        assert_eq!(
            claude.launch.arguments,
            vec![old_entrypoint.to_string_lossy().into_owned()]
        );
        assert!(!registry.adapter_needs_install(&claude));
        assert!(!registry.adapter_pin_installed(&claude));
        assert_eq!(
            registry.launched_adapter_version(&claude).as_deref(),
            Some("0.0.1")
        );

        let new_entrypoint = seed_install(&installs, store.path(), CLAUDE_AGENT_ID, &pin);
        let claude = registry
            .definition(&AgentId::new(CLAUDE_AGENT_ID))
            .expect("claude definition");
        assert_eq!(
            claude.launch.arguments,
            vec![new_entrypoint.to_string_lossy().into_owned()]
        );
        assert!(registry.adapter_pin_installed(&claude));
        assert_eq!(
            registry.launched_adapter_version(&claude),
            Some(pin.version)
        );
    }

    /// An adapter install running npm from an older Node holds that Node
    /// against pruning until it ends.
    #[test]
    fn a_toolchain_lease_holds_its_runtime_until_dropped() {
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
        );
        let runtime = PathBuf::from("/ottam/runtime/node/v24.19.0");
        let other = PathBuf::from("/ottam/runtime/node/v24.0.0");
        let lease = registry.lease_toolchain(runtime.join("bin/npm"));
        assert!(registry.launch_uses(&runtime));
        assert!(!registry.launch_uses(&other));
        drop(lease);
        assert!(!registry.launch_uses(&runtime));
    }

    /// When the pin lands under a running adapter, the next acquisition that
    /// finds it idle restarts it onto the new version; one a conversation is
    /// using keeps running what it was launched with, and pruning sees it as
    /// still in use.
    #[tokio::test]
    async fn an_idle_adapter_moves_to_a_landed_upgrade_and_a_busy_one_waits() {
        let store = tempfile::tempdir().expect("tempdir");
        let resolver = Arc::new(NodeResolver::default());
        resolver.store_for_test(NodeResolution::Resolved(ResolvedNodeRuntime {
            node: PathBuf::from("/opt/homebrew/bin/node"),
            npx: PathBuf::from("/opt/homebrew/bin/npx"),
            npm: PathBuf::from("/opt/homebrew/bin/npm"),
            source: NodeSource::WellKnown,
        }));
        let installs =
            super::super::adapter_install::ManagedAdapterInstalls::at(store.path().to_path_buf());
        let factory = Arc::new(FakeFactory::default());
        let registry = AgentRegistry::with_components_and_resolver(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            factory.clone(),
            resolver,
        )
        .with_adapter_installs(installs.clone());
        let agent_id = AgentId::new(CLAUDE_AGENT_ID);
        let pin = registry
            .definition(&agent_id)
            .expect("claude definition")
            .adapter
            .expect("claude pin");
        let mut old = pin.clone();
        old.version = "0.0.1".to_string();
        let old_entrypoint = seed_install(&installs, store.path(), CLAUDE_AGENT_ID, &old);
        let old_dir = old_entrypoint
            .ancestors()
            .find(|dir| dir.ends_with("0.0.1"))
            .expect("version dir")
            .to_path_buf();

        let (busy, lease) = registry
            .get_or_start_for_conversation(&agent_id)
            .await
            .expect("start on the stand-in");
        assert!(registry.launch_uses(&old_dir));
        let new_entrypoint = seed_install(&installs, store.path(), CLAUDE_AGENT_ID, &pin);

        let (same, second) = registry
            .get_or_start_for_conversation(&agent_id)
            .await
            .expect("join the busy adapter");
        assert!(Arc::ptr_eq(&busy, &same), "a busy adapter was restarted");
        drop((lease, second));

        let (moved, _lease) = registry
            .get_or_start_for_conversation(&agent_id)
            .await
            .expect("restart onto the upgrade");
        assert!(!Arc::ptr_eq(&busy, &moved));
        assert_eq!(
            moved.definition().launch.arguments,
            vec![new_entrypoint.to_string_lossy().into_owned()]
        );
        assert_eq!(factory.created.load(Ordering::SeqCst), 2);
        drop(busy);
        drop(same);
        assert!(
            !registry.launch_uses(&old_dir),
            "nothing live runs from the superseded version any more"
        );
    }

    /// Mandatory install: with the store active and nothing installed, a start
    /// is refused up front rather than falling back to `npx -y`.
    #[tokio::test]
    async fn starting_an_uninstalled_agent_is_refused() {
        let store = tempfile::tempdir().expect("tempdir");
        let installs =
            super::super::adapter_install::ManagedAdapterInstalls::at(store.path().to_path_buf());
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
        )
        .with_adapter_installs(installs.clone());

        let error = match registry.get_or_start(&AgentId::new(CLAUDE_AGENT_ID)).await {
            Ok(_) => panic!("must refuse an uninstalled adapter"),
            Err(error) => error,
        };
        assert_eq!(error.kind, HarnessErrorKind::AdapterNotInstalled);

        // Installing the pin lifts the refusal with no other change.
        let claude = registry
            .definition(&AgentId::new(CLAUDE_AGENT_ID))
            .expect("claude definition");
        let package = claude.adapter.clone().expect("claude pin");
        seed_install(&installs, store.path(), CLAUDE_AGENT_ID, &package);
        assert!(
            registry
                .get_or_start(&AgentId::new(CLAUDE_AGENT_ID))
                .await
                .is_ok(),
            "an installed adapter starts"
        );
    }

    /// A cold resolver cache must leave the definition alone rather than block
    /// on a search: that is exactly the pre-resolver behaviour, and the runtime
    /// probe warms the cache before any adapter start.
    #[test]
    fn an_unresolved_toolchain_leaves_launch_specs_untouched() {
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
        );
        let claude = registry
            .definition(&AgentId::new(CLAUDE_AGENT_ID))
            .expect("claude definition");
        assert_eq!(claude.launch.command, "npx");
        assert!(claude.launch.environment.is_empty());
    }

    /// Every account-backed agent gets its own home under the policy root so
    /// Ottam sessions never land in `~/.claude` or `~/.codex` — each via
    /// that CLI's own config-dir variable.
    #[test]
    fn a_home_policy_gives_each_agent_an_isolated_home() {
        let root = tempfile::tempdir().expect("tempdir");
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
        )
        .with_home_policy(AgentHomePolicy {
            root: root.path().to_path_buf(),
        });

        for (agent_id, variable) in [
            (CLAUDE_AGENT_ID, "CLAUDE_CONFIG_DIR"),
            (CODEX_AGENT_ID, "CODEX_HOME"),
        ] {
            let definition = registry
                .definition(&AgentId::new(agent_id))
                .expect("definition");
            let expected = root.path().join(agent_id).join("home");
            let value = definition
                .launch
                .environment
                .iter()
                .find(|(name, _)| name == variable)
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| panic!("{variable} injected for {agent_id}"));
            assert_eq!(value, expected.to_string_lossy());
            assert!(!expected.exists(), "definition lookup must be read-only");
            registry
                .prepare_home(&AgentId::new(agent_id))
                .expect("managed home");
            assert!(expected.is_dir(), "{agent_id} home created at launch");
        }
    }

    /// OpenCode's profile is its database, config directory, and data and
    /// state homes; Ottam's plugin loads whatever the home, from a
    /// directory beside the harness's installs.
    #[test]
    fn opencode_launches_with_its_profile_and_ottams_plugin() {
        let root = tempfile::tempdir().expect("tempdir");
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
        )
        .with_home_policy(AgentHomePolicy {
            root: root.path().to_path_buf(),
        });
        let id = AgentId::new(OPENCODE_AGENT_ID);
        let definition = registry.definition(&id).expect("definition");
        let env = |definition: &AgentDefinition, name: &str| {
            definition
                .launch
                .environment
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        let home = root.path().join(OPENCODE_AGENT_ID).join("home");
        let plugin = root
            .path()
            .join(OPENCODE_AGENT_ID)
            .join("plugins")
            .join("ottam-foreground");
        assert_eq!(definition.launch.arguments, ["acp"]);
        assert_eq!(
            env(&definition, "OPENCODE_DB").as_deref(),
            Some(home.join("opencode.db").to_str().unwrap())
        );
        assert_eq!(
            env(&definition, "OPENCODE_CONFIG_DIR").as_deref(),
            Some(home.join("config").to_str().unwrap())
        );
        assert_eq!(
            env(&definition, "OPENCODE_DISABLE_AUTOUPDATE").as_deref(),
            Some("1")
        );
        let content: serde_json::Value =
            serde_json::from_str(&env(&definition, "OPENCODE_CONFIG_CONTENT").unwrap()).unwrap();
        assert_eq!(
            content,
            serde_json::json!({"plugins": [plugin.to_str().unwrap()]})
        );
        assert_eq!(
            env(&definition, "XDG_DATA_HOME").as_deref(),
            Some(home.join("data").to_str().unwrap())
        );
        assert_eq!(
            env(&definition, "XDG_STATE_HOME").as_deref(),
            Some(home.join("state").to_str().unwrap())
        );
        assert_eq!(env(&definition, "XDG_CACHE_HOME"), None);
        // The plugin gives shell commands the core's own values back.
        let own: serde_json::Value =
            serde_json::from_str(&env(&definition, "OTTAM_SHELL_ENVIRONMENT").unwrap())
                .unwrap();
        for name in [
            "OPENCODE_DB",
            "OPENCODE_CONFIG_DIR",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
        ] {
            assert_eq!(
                own[name],
                serde_json::json!(std::env::var(name).ok()),
                "{name}"
            );
        }
        let binary = definition.adapter.as_ref().unwrap().binary().unwrap();
        assert_eq!(binary.command, "opencode");
        assert_eq!(
            binary
                .artifacts
                .iter()
                .map(|artifact| artifact.arch)
                .collect::<Vec<_>>(),
            ["aarch64", "x86_64"]
        );

        registry.prepare_home(&id).expect("managed home");
        // Each CLI command starts its own server on this profile's database
        // instead of reusing the shared background service.
        let service = home.join("config/service.json");
        let read = || -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(&service).expect("service.json")).unwrap()
        };
        assert_eq!(read(), serde_json::json!({"disabled": true}));
        std::fs::write(&service, r#"{"disabled": false, "port": 4096}"#).unwrap();
        registry.prepare_home(&id).expect("managed home again");
        assert_eq!(read(), serde_json::json!({"disabled": true, "port": 4096}));
        let script = std::fs::read_to_string(plugin.join("index.js")).expect("plugin");
        assert!(script.contains("ottam.foreground"));
        assert!(std::fs::read_to_string(plugin.join("package.json"))
            .unwrap()
            .contains("\"module\""));

        // An existing setup keeps the user's homes, and still loads the plugin.
        registry.publish_instance(super::super::AgentInstance {
            id: AgentId::new("opencode.cli"),
            harness_id: super::super::HarnessId(OPENCODE_AGENT_ID.into()),
            label: "OpenCode — Your setup".into(),
            home: super::super::HomeLayout::Existing {
                path: "/fixture/.local/share/opencode".into(),
                environment: BTreeMap::from([
                    ("XDG_DATA_HOME".into(), None),
                    ("XDG_CONFIG_HOME".into(), None),
                    ("OPENCODE_DB".into(), None),
                    ("OPENCODE_CONFIG_DIR".into(), Some("/fixture/config".into())),
                ]),
            },
            claimed: true,
        });
        let existing = registry
            .definition(&AgentId::new("opencode.cli"))
            .expect("existing");
        assert_eq!(env(&existing, "OPENCODE_DB"), None);
        assert_eq!(
            env(&existing, "OPENCODE_CONFIG_DIR").as_deref(),
            Some("/fixture/config")
        );
        assert!(env(&existing, "OPENCODE_CONFIG_CONTENT").is_some());
        assert_eq!(env(&existing, "XDG_STATE_HOME"), None);
        assert_eq!(env(&existing, "OTTAM_SHELL_ENVIRONMENT"), None);

        // The setup's own inline config keeps its settings; the plugin joins.
        registry.publish_instance(super::super::AgentInstance {
            id: AgentId::new("opencode.inline"),
            harness_id: super::super::HarnessId(OPENCODE_AGENT_ID.into()),
            label: "OpenCode — Inline".into(),
            home: super::super::HomeLayout::Existing {
                path: "/fixture/inline".into(),
                environment: BTreeMap::from([(
                    "OPENCODE_CONFIG_CONTENT".into(),
                    Some(r#"{"model":"go/kimi","plugins":["/mine"]}"#.into()),
                )]),
            },
            claimed: true,
        });
        let inline = registry
            .definition(&AgentId::new("opencode.inline"))
            .expect("inline");
        let content: serde_json::Value =
            serde_json::from_str(&env(&inline, "OPENCODE_CONFIG_CONTENT").unwrap()).unwrap();
        assert_eq!(
            content,
            serde_json::json!({
                "model": "go/kimi",
                "plugins": ["/mine", plugin.to_str().unwrap()],
            })
        );

        // The user's own setup keeps sharing its CLI's service: same database.
        let user = tempfile::tempdir().expect("user setup");
        registry.publish_instance(super::super::AgentInstance {
            id: AgentId::new("opencode.mine"),
            harness_id: super::super::HarnessId(OPENCODE_AGENT_ID.into()),
            label: "OpenCode — Mine".into(),
            home: super::super::HomeLayout::Existing {
                path: user.path().to_path_buf(),
                environment: BTreeMap::from([
                    ("XDG_DATA_HOME".into(), None),
                    ("XDG_CONFIG_HOME".into(), None),
                    ("OPENCODE_DB".into(), None),
                    (
                        "OPENCODE_CONFIG_DIR".into(),
                        Some(user.path().join("config").to_string_lossy().into_owned()),
                    ),
                ]),
            },
            claimed: true,
        });
        registry
            .prepare_home(&AgentId::new("opencode.mine"))
            .expect("existing setup");
        assert!(!user.path().join("config/service.json").exists());
        assert!(!user.path().join("service.json").exists());
    }

    /// omp's profile is its agent directory; an empty `OMP_PROFILE` keeps a
    /// named profile from overriding it, for an Ottam home and an
    /// existing setup alike.
    #[test]
    fn omp_launches_with_its_agent_directory_and_the_default_profile() {
        let root = tempfile::tempdir().expect("tempdir");
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
        )
        .with_home_policy(AgentHomePolicy {
            root: root.path().to_path_buf(),
        });
        let env = |definition: &AgentDefinition, name: &str| {
            definition
                .launch
                .environment
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        let definition = registry
            .definition(&AgentId::new(OMP_AGENT_ID))
            .expect("definition");
        let home = root.path().join(OMP_AGENT_ID).join("home");
        assert_eq!(definition.launch.command, "omp");
        assert_eq!(definition.launch.arguments, ["acp"]);
        assert_eq!(
            env(&definition, "PI_CODING_AGENT_DIR").as_deref(),
            Some(home.to_str().unwrap())
        );
        assert_eq!(env(&definition, "OMP_PROFILE").as_deref(), Some(""));
        let binary = definition.adapter.as_ref().unwrap().binary().unwrap();
        assert_eq!(binary.command, "omp");
        assert!(binary
            .artifacts
            .iter()
            .all(|artifact| artifact.archive == BinaryArchive::Raw
                && artifact.url.contains(&format!("/v{OMP_VERSION}/"))));
        assert_eq!(
            binary
                .artifacts
                .iter()
                .map(|artifact| artifact.arch)
                .collect::<Vec<_>>(),
            ["aarch64", "x86_64"]
        );

        registry.publish_instance(super::super::AgentInstance {
            id: AgentId::new("omp.cli"),
            harness_id: super::super::HarnessId(OMP_AGENT_ID.into()),
            label: "Oh-My-Pi — Your setup".into(),
            home: super::super::HomeLayout::Existing {
                path: "/fixture/.omp/agent".into(),
                environment: BTreeMap::from([("PI_CODING_AGENT_DIR".into(), None)]),
            },
            claimed: true,
        });
        let existing = registry
            .definition(&AgentId::new("omp.cli"))
            .expect("existing");
        assert_eq!(env(&existing, "PI_CODING_AGENT_DIR"), None);
        assert_eq!(env(&existing, "OMP_PROFILE").as_deref(), Some(""));
    }

    /// Only Claude's isolated home participates in refresh-lock cleanup.
    #[test]
    fn an_abandoned_refresh_lock_is_swept_from_claudes_home_only() {
        let root = tempfile::tempdir().expect("tempdir");
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
        )
        .with_home_policy(AgentHomePolicy {
            root: root.path().to_path_buf(),
        });

        for agent_id in [CLAUDE_AGENT_ID, CODEX_AGENT_ID] {
            let home = root.path().join(agent_id).join("home");
            std::fs::create_dir_all(home.join(super::super::refresh_lock::CLAUDE_REFRESH_LOCK))
                .expect("lock");
        }
        let claude = AgentId::new(CLAUDE_AGENT_ID);
        let codex = AgentId::new(CODEX_AGENT_ID);

        registry.begin_catalog_probe(&claude);
        assert!(!root
            .path()
            .join(CLAUDE_AGENT_ID)
            .join("home")
            .join(super::super::refresh_lock::CLAUDE_REFRESH_LOCK)
            .exists());
        registry.finish_catalog_probe(&claude, true);
        registry.prepare_managed_start(&codex);
        assert!(root
            .path()
            .join(CODEX_AGENT_ID)
            .join("home")
            .join(super::super::refresh_lock::CLAUDE_REFRESH_LOCK)
            .is_dir());
    }

    /// A brand-new home is created empty — never seeded from the vendor dir —
    /// so Ottam always runs a stock CLI with no user customizations. The
    /// path is answerable before the home exists; unsupported agents have no
    /// managed home at all.
    #[test]
    fn a_new_home_starts_empty_and_its_path_is_known_up_front() {
        let root = tempfile::tempdir().expect("tempdir");
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
        )
        .with_home_policy(AgentHomePolicy {
            root: root.path().to_path_buf(),
        });
        let claude = AgentId::new(CLAUDE_AGENT_ID);
        let expected = root.path().join(CLAUDE_AGENT_ID).join("home");

        // Path known before anything exists.
        assert_eq!(registry.agent_home(&claude), Some(expected.clone()));
        assert!(!expected.exists());

        registry.definition(&claude).expect("definition");
        assert!(!expected.exists());
        registry.prepare_home(&claude).expect("prepare home");
        assert!(expected.is_dir());
        assert_eq!(
            std::fs::read_dir(&expected).expect("read home").count(),
            0,
            "a new home must be empty — no settings copied from ~/.claude"
        );

        assert_eq!(
            registry.agent_home(&AgentId::new(TEST_NATIVE_AGENT_ID)),
            None
        );
    }

    #[test]
    fn existing_setup_layouts_preserve_home_and_explicit_environment_choices() {
        let root = tempfile::tempdir().unwrap();
        let shared = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
        )
        .with_home_policy(AgentHomePolicy {
            root: root.path().into(),
        });
        let id = AgentId::new("claude.custom");
        let lock = shared
            .path()
            .join(super::super::refresh_lock::CLAUDE_REFRESH_LOCK);
        std::fs::create_dir(&lock).unwrap();
        registry.publish_instance(super::super::AgentInstance {
            id: id.clone(),
            harness_id: super::super::HarnessId("claude".into()),
            label: "Shared Claude".into(),
            home: super::super::HomeLayout::Existing {
                path: shared.path().into(),
                environment: BTreeMap::from([("CLAUDE_CONFIG_DIR".into(), None)]),
            },
            claimed: false,
        });
        let definition = registry.definition(&id).unwrap();
        assert_eq!(definition.harness_id(), "claude");
        // An instance's subagent messages are its harness's.
        assert!(definition.subagent_messages.handback.is_some());
        assert!(!definition
            .launch
            .environment
            .iter()
            .any(|(key, _)| key == "CLAUDE_CONFIG_DIR" || key == "HOME"));
        assert_eq!(registry.agent_home(&id), Some(shared.path().into()));
        registry.prepare_home(&id).unwrap();
        registry.prepare_managed_start(&id);
        registry.begin_catalog_probe(&id);
        registry.finish_catalog_probe(&id, true);
        assert!(
            lock.exists(),
            "shared locks are never cleaned, regardless of instance ID"
        );
        assert!(!root.path().join(&id.0).exists());
        assert!(
            registry.shutdown_waiting_on().is_empty(),
            "a lock without a live adapter does not block quit"
        );
    }

    #[tokio::test]
    async fn shared_shutdown_waits_cancels_and_late_cancel_allows_new_turns() {
        let shared = tempfile::tempdir().unwrap();
        let registry = Arc::new(AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
        ));
        let id = AgentId::new("claude.shared");
        registry.publish_instance(super::super::AgentInstance {
            id: id.clone(),
            harness_id: super::super::HarnessId("claude".into()),
            label: "Shared Claude".into(),
            home: super::super::HomeLayout::Existing {
                path: shared.path().into(),
                environment: BTreeMap::new(),
            },
            claimed: false,
        });
        let adapter = registry.get_or_start(&id).await.unwrap();
        let lock = shared
            .path()
            .join(super::super::refresh_lock::CLAUDE_REFRESH_LOCK);
        std::fs::create_dir(&lock).unwrap();
        assert_eq!(registry.shutdown_waiting_on(), ["Shared Claude"]);
        let task_registry = registry.clone();
        let mut task = tokio::spawn(async move { task_registry.prepare_shutdown("waiting").await });
        assert!(tokio::time::timeout(Duration::from_millis(150), &mut task)
            .await
            .is_err());
        registry.cancel_shutdown_operation("waiting");
        assert_eq!(
            task.await.unwrap().unwrap_err().kind,
            HarnessErrorKind::RequestCancelled
        );
        assert_eq!(
            adapter.status().borrow().lifecycle,
            AdapterLifecycleState::Ready
        );
        assert!(lock.exists());
        std::fs::remove_dir(&lock).unwrap();
        registry.prepare_shutdown("ready").await.unwrap();
        assert!(registry.get_or_start(&id).await.is_err());
        registry.cancel_shutdown_operation("other");
        assert!(registry.get_or_start(&id).await.is_err());
        registry.cancel_shutdown_operation("ready");
        registry.get_or_start(&id).await.unwrap();
        registry.shutdown(&id).await.unwrap();
        std::fs::create_dir(&lock).unwrap();
        assert!(registry.shutdown_waiting_on().is_empty());
        tokio::time::timeout(Duration::from_secs(1), registry.shutdown_all())
            .await
            .unwrap()
            .unwrap();
        assert!(lock.exists());
    }

    #[tokio::test]
    async fn replay_retires_only_idle_codex_and_fences_new_turns_until_cleanup() {
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
        );
        let id = AgentId::new("codex");
        let (adapter, lease) = registry.get_or_start_for_conversation(&id).await.unwrap();
        assert!(registry.prepare_replay(&id).await.is_err());
        assert_eq!(
            adapter.status().borrow().lifecycle,
            AdapterLifecycleState::Ready
        );
        drop(lease);
        let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let guard_live = live.clone();
        registry.set_replay_live_work_guard(Arc::new(move |_| guard_live.load(Ordering::Acquire)));
        assert!(registry.prepare_replay(&id).await.is_err());
        assert_eq!(
            adapter.status().borrow().lifecycle,
            AdapterLifecycleState::Ready
        );
        live.store(false, Ordering::Release);
        let admission = registry.prepare_replay(&id).await.unwrap();
        assert_eq!(
            adapter.status().borrow().lifecycle,
            AdapterLifecycleState::Stopped
        );
        assert!(registry.instance_admission(&id).try_lock().is_err());
        // Replay cleanup can wait indefinitely on this profile's refresh lock
        // without occupying the global gate or another provider's admission.
        assert!(registry.instance_changes.try_lock().is_ok());
        tokio::time::timeout(
            Duration::from_secs(1),
            registry.get_or_start_for_conversation(&AgentId::new("claude")),
        )
        .await
        .unwrap()
        .unwrap();
        drop(admission);
        registry.get_or_start_for_conversation(&id).await.unwrap();
        assert_eq!(
            adapter.status().borrow().lifecycle,
            AdapterLifecycleState::Ready
        );
    }

    #[tokio::test]
    async fn shutdown_preparation_does_not_wait_for_adapter_map() {
        let registry = Arc::new(AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
        ));
        let map = registry.adapters.lock().await;
        let task_registry = registry.clone();
        let task = tokio::spawn(async move { task_registry.prepare_shutdown("busy-map").await });
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        registry.cancel_shutdown_operation("busy-map");
        assert!(!registry.shutting_down.load(Ordering::Acquire));
        drop(map);
    }

    #[tokio::test]
    async fn eof_shutdown_releases_a_start_holding_the_instance_gate() {
        let registry = Arc::new(AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory {
                hold_start: true,
                ..Default::default()
            }),
        ));
        let starting = registry.clone();
        let start =
            tokio::spawn(async move { starting.get_or_start(&AgentId::new("codex")).await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !registry.live_adapter_snapshot().iter().any(|adapter| {
                adapter.status().borrow().lifecycle == AdapterLifecycleState::Starting
            }) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), registry.shutdown_all())
            .await
            .unwrap()
            .unwrap();
        assert!(start.await.unwrap().is_err());
        assert!(registry.get_or_start(&AgentId::new("codex")).await.is_err());
    }

    #[tokio::test]
    async fn auth_recheck_preserves_an_adapter_acquired_before_session_binding() {
        let home = tempfile::tempdir().unwrap();
        let factory = Arc::new(FakeFactory::default());
        let registry = Arc::new(AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            factory.clone(),
        ));
        let id = AgentId::new("codex.custom");
        registry.publish_instance(super::super::AgentInstance {
            id: id.clone(),
            harness_id: super::super::HarnessId("codex".into()),
            label: "Shared Codex".into(),
            home: super::super::HomeLayout::Existing {
                path: home.path().into(),
                environment: BTreeMap::new(),
            },
            claimed: true,
        });
        let initial = registry.get_or_start(&id).await.unwrap();
        registry.recheck_existing_auth(&id).await.unwrap();
        let (acquired, admission) = registry.get_or_start_for_conversation(&id).await.unwrap();
        assert!(
            !Arc::ptr_eq(&initial, &acquired),
            "initialization-only adapters can recheck"
        );
        assert!(
            acquired.sessions().is_empty(),
            "the first run has not bound its session yet"
        );
        let refreshing = registry.clone();
        let refreshing_id = id.clone();
        tokio::spawn(async move { refreshing.recheck_existing_auth(&refreshing_id).await })
            .await
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(
            &acquired,
            &registry.running_adapter(&id).await.unwrap()
        ));
        assert_eq!(factory.created.load(Ordering::SeqCst), 2);
        registry.recheck_existing_auth(&id).await.unwrap();
        assert_eq!(
            acquired.status().borrow().lifecycle,
            AdapterLifecycleState::Ready
        );
        assert_eq!(factory.created.load(Ordering::SeqCst), 2);
        // An unbound run can fail or be cancelled before publishing a session.
        let (_, second_admission) = registry.get_or_start_for_conversation(&id).await.unwrap();
        drop(admission);
        registry.recheck_existing_auth(&id).await.unwrap();
        assert_eq!(
            factory.created.load(Ordering::SeqCst),
            2,
            "another pending run still owns the adapter"
        );
        drop(second_admission);
        registry.recheck_existing_auth(&id).await.unwrap();
        assert_eq!(factory.created.load(Ordering::SeqCst), 3);
        assert!(!Arc::ptr_eq(
            &acquired,
            &registry.running_adapter(&id).await.unwrap()
        ));
        registry.shutdown_all().await.unwrap();
    }

    #[tokio::test]
    async fn eof_shutdown_returns_permanent_errors_and_stops_other_adapters() {
        for lifecycle in [
            AdapterLifecycleState::Starting,
            AdapterLifecycleState::Ready,
            AdapterLifecycleState::Stopping,
        ] {
            let registry = AgentRegistry::with_components(
                built_in_definitions(),
                Arc::new(ReadyProbe),
                Arc::new(FakeFactory::default()),
            );
            let healthy = registry
                .get_or_start(&AgentId::new("claude"))
                .await
                .unwrap();
            let id = AgentId::new("codex");
            let failed = Arc::new(FakeAdapter::new(registry.definition(&id).unwrap(), 1, true));
            failed
                .status_tx
                .send_modify(|status| status.lifecycle = lifecycle);
            let adapter: Arc<dyn HarnessAdapter> = failed.clone();
            registry
                .adapters
                .lock()
                .await
                .insert(id, registry.manage(&adapter));
            // A failed start can still own this gate; reporting its permanent
            // shutdown error must not require that start to finish first.
            let _admission = registry.instance_changes.lock().await;
            let error = tokio::time::timeout(Duration::from_secs(1), registry.shutdown_all())
                .await
                .expect("permanent shutdown errors must not hang EOF teardown")
                .unwrap_err();
            assert_eq!(error.kind, HarnessErrorKind::Internal);
            assert_eq!(failed.shutdown_calls.load(Ordering::SeqCst), 1);
            assert_eq!(failed.status_tx.borrow().lifecycle, lifecycle);
            assert_eq!(
                healthy.status().borrow().lifecycle,
                AdapterLifecycleState::Stopped
            );
        }
    }

    #[test]
    fn managed_home_failure_and_missing_existing_home_fail_without_fallback() {
        let root = tempfile::tempdir().unwrap();
        let blocked = root.path().join("blocked");
        std::fs::write(&blocked, "file").unwrap();
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
        )
        .with_home_policy(AgentHomePolicy { root: blocked });
        assert!(registry.prepare_home(&AgentId::new("codex")).is_err());
        let missing = root.path().join("missing");
        let id = AgentId::new("codex.custom");
        registry.publish_instance(super::super::AgentInstance {
            id: id.clone(),
            harness_id: super::super::HarnessId("codex".into()),
            label: "Custom Codex".into(),
            home: super::super::HomeLayout::Existing {
                path: missing.clone(),
                environment: BTreeMap::from([(
                    "CODEX_HOME".into(),
                    Some(missing.to_string_lossy().into()),
                )]),
            },
            claimed: false,
        });
        assert!(registry.prepare_home(&id).is_err());
        assert!(!missing.exists());
        let definition = registry.definition(&id).unwrap();
        assert!(definition
            .launch
            .environment
            .contains(&("CODEX_HOME".into(), missing.to_string_lossy().into())));
    }

    #[test]
    fn attached_cli_aliases_cannot_be_purged_or_added_as_managed_siblings() {
        let root = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            Arc::new(FakeFactory::default()),
        )
        .with_home_policy(AgentHomePolicy {
            root: root.path().join("agents"),
        });
        let id = AgentId::new("codex");
        registry.prepare_home(&id).unwrap();
        let home = registry.agent_home(&id).unwrap();
        std::fs::write(home.join("credentials"), "keep").unwrap();
        let cli = super::super::HomeLayout::Existing {
            path: home.clone(),
            environment: BTreeMap::new(),
        };
        assert!(registry.validate_attachment(&cli).is_err());
        // Defend persisted overlaps from earlier versions too, even though new
        // attachments now reject them before storage/publication.
        registry.publish_instance(super::super::AgentInstance {
            id: AgentId::new("codex.cli"),
            harness_id: super::super::HarnessId("codex".into()),
            label: "CLI".into(),
            home: cli,
            claimed: false,
        });
        assert!(registry.purge_home(&id, &[]).is_err());
        assert!(home.join("credentials").exists());
        #[cfg(unix)]
        {
            let alias = root.path().join("alias");
            std::os::unix::fs::symlink(&home, &alias).unwrap();
            assert!(registry
                .validate_attachment(&super::super::HomeLayout::Existing {
                    path: alias.clone(),
                    environment: BTreeMap::new(),
                })
                .is_err());
            registry.publish_instance(super::super::AgentInstance {
                id: AgentId::new("codex.cli"),
                harness_id: super::super::HarnessId("codex".into()),
                label: "CLI".into(),
                home: super::super::HomeLayout::Existing {
                    path: alias,
                    environment: BTreeMap::new(),
                },
                claimed: false,
            });
            assert!(registry.purge_home(&id, &[]).is_err());
        }
        let external = tempfile::tempdir().unwrap();
        assert!(registry
            .validate_attachment(&super::super::HomeLayout::Existing {
                path: external.path().to_path_buf(),
                environment: BTreeMap::new(),
            })
            .is_ok());
        registry.remove_instance(&AgentId::new("codex.cli"));
        assert!(registry
            .purge_home(&id, std::slice::from_ref(&home))
            .is_err());
        assert!(home.join("credentials").exists());
    }

    #[test]
    fn the_probe_runs_the_resolved_binaries_rather_than_bare_names() {
        let resolved = NodeResolution::Resolved(ResolvedNodeRuntime {
            node: PathBuf::from("/opt/homebrew/bin/node"),
            npx: PathBuf::from("/opt/homebrew/bin/npx"),
            npm: PathBuf::from("/opt/homebrew/bin/npm"),
            source: NodeSource::WellKnown,
        });
        assert_eq!(
            resolved_program(&resolved, "node"),
            "/opt/homebrew/bin/node"
        );
        assert_eq!(resolved_program(&resolved, "npx"), "/opt/homebrew/bin/npx");
        assert_eq!(
            resolved_program(&NodeResolution::Unresolved, "node"),
            "node"
        );
    }

    /// A direct first start is valid protocol usage; it must not depend on an
    /// earlier engine-list call having warmed the resolver cache.
    #[tokio::test]
    async fn a_cold_first_start_uses_the_definition_resolved_by_its_probe() {
        struct ResolvingProbe {
            resolver: Arc<NodeResolver>,
        }

        impl RuntimeProbe for ResolvingProbe {
            fn probe(&self, _definition: &AgentDefinition) -> AgentRuntimeStatus {
                self.resolver
                    .store_for_test(NodeResolution::Resolved(ResolvedNodeRuntime {
                        node: PathBuf::from("/resolved/bin/node"),
                        npx: PathBuf::from("/resolved/bin/npx"),
                        npm: PathBuf::from("/resolved/bin/npm"),
                        source: NodeSource::LoginShell,
                    }));
                AgentRuntimeStatus::ready("v22.0.0", "10.0.0")
            }
        }

        let resolver = Arc::new(NodeResolver::default());
        let factory = Arc::new(FakeFactory::default());
        let registry = AgentRegistry::with_components_and_resolver(
            built_in_definitions(),
            Arc::new(ResolvingProbe {
                resolver: Arc::clone(&resolver),
            }),
            factory.clone(),
            resolver,
        );

        let adapter = registry
            .get_or_start(&AgentId::new(CODEX_AGENT_ID))
            .await
            .expect("start adapter");

        assert_eq!(adapter.definition().launch.command, "/resolved/bin/npx");
        assert!(adapter
            .definition()
            .launch
            .environment
            .iter()
            .any(|(name, value)| name == "PATH" && value.starts_with("/resolved/bin")));
        assert_eq!(factory.created.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_stopped_adapter_is_recreated_when_runtime_refresh_changes_its_launch() {
        struct SwitchingProbe {
            resolver: Arc<NodeResolver>,
            use_new_runtime: Arc<AtomicBool>,
        }

        impl RuntimeProbe for SwitchingProbe {
            fn probe(&self, _definition: &AgentDefinition) -> AgentRuntimeStatus {
                let directory = if self.use_new_runtime.load(Ordering::SeqCst) {
                    "/new/bin"
                } else {
                    "/old/bin"
                };
                self.resolver
                    .store_for_test(NodeResolution::Resolved(ResolvedNodeRuntime {
                        node: PathBuf::from(directory).join("node"),
                        npx: PathBuf::from(directory).join("npx"),
                        npm: PathBuf::from(directory).join("npm"),
                        source: NodeSource::LoginShell,
                    }));
                AgentRuntimeStatus::ready("v22.0.0", "10.0.0")
            }
        }

        let resolver = Arc::new(NodeResolver::default());
        let use_new_runtime = Arc::new(AtomicBool::new(false));
        let factory = Arc::new(FakeFactory::default());
        let registry = AgentRegistry::with_components_and_resolver(
            built_in_definitions(),
            Arc::new(SwitchingProbe {
                resolver: Arc::clone(&resolver),
                use_new_runtime: Arc::clone(&use_new_runtime),
            }),
            factory.clone(),
            resolver,
        );
        let agent_id = AgentId::new(CODEX_AGENT_ID);
        let old = registry
            .get_or_start(&agent_id)
            .await
            .expect("start old runtime");
        registry.shutdown(&agent_id).await.expect("stop adapter");
        use_new_runtime.store(true, Ordering::SeqCst);

        let new = registry
            .get_or_start(&agent_id)
            .await
            .expect("start new runtime");

        assert!(!Arc::ptr_eq(&old, &new));
        assert_eq!(new.definition().launch.command, "/new/bin/npx");
        assert_eq!(new.status().borrow().generation, 2);
        assert_eq!(factory.created.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn only_slow_not_ready_answers_are_cached_and_only_briefly() {
        let missing = AgentRuntimeStatus {
            availability: RuntimeAvailability::Missing {
                component: "node".into(),
            },
            source: None,
            node_version: None,
            npx_version: None,
        };
        let ready = AgentRuntimeStatus::ready("v22.0.0", "10.0.0");
        assert!(!worth_caching(&missing, Duration::from_millis(5)));
        assert!(worth_caching(&missing, SLOW_NOT_READY_THRESHOLD));
        assert!(worth_caching(&ready, Duration::from_millis(5)));
        assert!(status_cache_ttl(&missing) < status_cache_ttl(&ready));
    }

    /// A probe that straddles an install or "Check Again" must not leave its
    /// older answer behind for the probe queued after it.
    #[test]
    fn a_status_probed_before_an_invalidation_is_not_cached() {
        let probe = SystemRuntimeProbe::default();
        let ready = AgentRuntimeStatus::ready("v22.0.0", "10.0.0");
        let cached = |probe: &SystemRuntimeProbe| probe.cache.lock().unwrap().contains_key("codex");

        let started_at = probe.invalidations.load(Ordering::Acquire);
        probe.invalidate();
        probe.cache_unless_invalidated("codex", started_at, ready.clone());
        assert!(!cached(&probe));

        let started_at = probe.invalidations.load(Ordering::Acquire);
        probe.cache_unless_invalidated("codex", started_at, ready);
        assert!(cached(&probe));
    }

    #[test]
    fn explicit_runtime_refresh_clears_resolver_and_probe_caches() {
        struct InvalidatingProbe {
            invalidations: Arc<AtomicUsize>,
        }

        impl RuntimeProbe for InvalidatingProbe {
            fn probe(&self, _definition: &AgentDefinition) -> AgentRuntimeStatus {
                AgentRuntimeStatus::ready("v22.0.0", "10.0.0")
            }

            fn invalidate(&self) {
                self.invalidations.fetch_add(1, Ordering::SeqCst);
            }
        }

        let resolver = Arc::new(NodeResolver::default());
        resolver.store_for_test(NodeResolution::Resolved(ResolvedNodeRuntime {
            node: PathBuf::from("/old/bin/node"),
            npx: PathBuf::from("/old/bin/npx"),
            npm: PathBuf::from("/old/bin/npm"),
            source: NodeSource::LoginShell,
        }));
        let invalidations = Arc::new(AtomicUsize::new(0));
        let registry = AgentRegistry::with_components_and_resolver(
            built_in_definitions(),
            Arc::new(InvalidatingProbe {
                invalidations: Arc::clone(&invalidations),
            }),
            Arc::new(FakeFactory::default()),
            Arc::clone(&resolver),
        );

        registry.invalidate_runtime_cache();

        assert!(resolver.cached().is_none());
        assert_eq!(invalidations.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn runtime_probe_distinguishes_ready_missing_invalid_and_unsupported() {
        let definition = built_in_definitions()
            .into_iter()
            .find(|definition| definition.agent_id.0 == CODEX_AGENT_ID)
            .expect("Codex definition");
        let ready = probe_runtime_with(&definition, |program, _| match program {
            "node" => Ok(output("v22.12.0")),
            "npx" => Ok(output("10.9.0")),
            _ => unreachable!(),
        });
        assert_eq!(ready, AgentRuntimeStatus::ready("v22.12.0", "10.9.0"));

        // The floor is inclusive: Codex's own minimum is enough.
        let at_floor = probe_runtime_with(&definition, |program, _| match program {
            "node" => Ok(output("v18.0.0")),
            "npx" => Ok(output("9.8.1")),
            _ => unreachable!(),
        });
        assert_eq!(at_floor, AgentRuntimeStatus::ready("v18.0.0", "9.8.1"));

        let missing = probe_runtime_with(&definition, |_, _| {
            Err(io::Error::new(io::ErrorKind::NotFound, "missing"))
        });
        assert_eq!(
            missing.availability,
            RuntimeAvailability::Missing {
                component: "node".to_string()
            }
        );

        let invalid = probe_runtime_with(&definition, |program, _| match program {
            "node" => Ok(output("not-a-version")),
            _ => unreachable!(),
        });
        assert_eq!(
            invalid.availability,
            RuntimeAvailability::Invalid {
                component: "node".to_string()
            }
        );

        let unsupported = probe_runtime_with(&definition, |program, _| match program {
            "node" => Ok(output("v16.20.2")),
            _ => unreachable!(),
        });
        assert_eq!(
            unsupported.availability,
            RuntimeAvailability::Unsupported {
                found: 16,
                minimum: 18
            }
        );
    }

    #[test]
    fn version_parsing_accepts_node_prefix_and_rejects_malformed_values() {
        assert_eq!(parse_node_major("v18.19.1"), Some(18));
        assert_eq!(parse_node_major("22.0.0"), Some(22));
        assert_eq!(parse_node_major("vnext"), None);
        assert_eq!(parse_semver_major("10"), None);
        assert_eq!(parse_semver_major("10.beta"), None);
        assert_eq!(parse_semver_major("10.9beta.0"), None);
    }

    #[tokio::test]
    async fn cached_status_does_not_create_or_start_an_adapter() {
        let factory = Arc::new(FakeFactory::default());
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            factory.clone(),
        );
        let status = registry
            .cached_status(&AgentId::new(CODEX_AGENT_ID))
            .await
            .expect("cached status");
        assert_eq!(status.lifecycle, AdapterLifecycleState::Stopped);
        assert_eq!(status.auth_state, HarnessAuthState::Unknown);
        assert_eq!(factory.created.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_probed_sign_in_verdict_stands_in_until_a_managed_adapter_exists() {
        let factory = Arc::new(FakeFactory::default());
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            factory.clone(),
        );
        let agent_id = AgentId::new(CODEX_AGENT_ID);

        // `Unknown` is not a verdict and must not overwrite anything.
        registry.note_probed_auth(&agent_id, HarnessAuthState::Unknown);
        let status = registry.cached_status(&agent_id).await.expect("status");
        assert_eq!(status.auth_state, HarnessAuthState::Unknown);

        registry.note_probed_auth(&agent_id, HarnessAuthState::Missing);
        let status = registry.cached_status(&agent_id).await.expect("status");
        assert_eq!(status.lifecycle, AdapterLifecycleState::Stopped);
        assert_eq!(status.auth_state, HarnessAuthState::Missing);
        assert_eq!(factory.created.load(Ordering::SeqCst), 0);

        // Forgetting (an uninstall) returns to knowing nothing.
        registry.forget_probed_auth(&agent_id);
        let status = registry.cached_status(&agent_id).await.expect("status");
        assert_eq!(status.auth_state, HarnessAuthState::Unknown);

        // A running adapter with no verdict of its own yet leaves the probe's
        // standing, so Sign In stays on offer while it starts.
        registry.note_probed_auth(&agent_id, HarnessAuthState::Missing);
        registry.get_or_start(&agent_id).await.expect("start");
        let status = registry.cached_status(&agent_id).await.expect("status");
        assert_ne!(status.lifecycle, AdapterLifecycleState::Stopped);
        assert_eq!(status.auth_state, HarnessAuthState::Missing);

        // Once it has one, its own status wins.
        factory.last_adapter().status_tx.send_modify(|status| {
            status.auth_state = HarnessAuthState::Ready;
        });
        let status = registry.cached_status(&agent_id).await.expect("status");
        assert_eq!(status.auth_state, HarnessAuthState::Ready);

        // Reinstall shuts that instance down but retains it for generation
        // bookkeeping; until it starts again, the new probe verdict wins.
        registry.shutdown(&agent_id).await.expect("shutdown");
        let status = registry.cached_status(&agent_id).await.expect("status");
        assert_eq!(status.lifecycle, AdapterLifecycleState::Stopped);
        assert_eq!(status.auth_state, HarnessAuthState::Missing);
    }

    #[tokio::test]
    async fn registry_deduplicates_concurrent_callers_and_restart_advances_generation() {
        let factory = Arc::new(FakeFactory::default());
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            factory.clone(),
        );
        let agent_id = AgentId::new(CODEX_AGENT_ID);

        let (first, second) = tokio::join!(
            registry.get_or_start(&agent_id),
            registry.get_or_start(&agent_id)
        );
        let first = first.expect("first adapter");
        let second = second.expect("second adapter");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(factory.created.load(Ordering::SeqCst), 1);
        assert_eq!(first.status().borrow().generation, 1);

        registry
            .shutdown(&agent_id)
            .await
            .expect("shutdown adapter");
        let resumed = registry
            .get_or_start(&agent_id)
            .await
            .expect("resume adapter");
        assert!(Arc::ptr_eq(&first, &resumed));
        assert_eq!(resumed.status().borrow().generation, 2);

        let restarted = registry.restart(&agent_id).await.expect("restart adapter");
        assert!(!Arc::ptr_eq(&first, &restarted));
        assert_eq!(factory.created.load(Ordering::SeqCst), 2);
        assert_eq!(restarted.status().borrow().generation, 3);
    }

    #[tokio::test]
    async fn failed_restart_retains_registry_ownership_of_the_previous_adapter() {
        let factory = Arc::new(FakeFactory {
            fail_shutdown: true,
            ..FakeFactory::default()
        });
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            factory.clone(),
        );
        let agent_id = AgentId::new(CODEX_AGENT_ID);
        let original = registry
            .get_or_start(&agent_id)
            .await
            .expect("original adapter");

        let error = match registry.restart(&agent_id).await {
            Ok(_) => panic!("restart must fail when shutdown fails"),
            Err(error) => error,
        };
        assert_eq!(error.kind, HarnessErrorKind::Internal);
        assert_eq!(factory.created.load(Ordering::SeqCst), 1);

        let retained = registry
            .get_or_start(&agent_id)
            .await
            .expect("retained adapter");
        assert!(Arc::ptr_eq(&original, &retained));
        assert_eq!(factory.created.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn restarting_a_stopped_handle_reprobes_the_runtime() {
        let probe = Arc::new(ToggleProbe {
            ready: AtomicBool::new(true),
        });
        let factory = Arc::new(FakeFactory::default());
        let registry =
            AgentRegistry::with_components(built_in_definitions(), probe.clone(), factory.clone());
        let agent_id = AgentId::new(CODEX_AGENT_ID);
        registry
            .get_or_start(&agent_id)
            .await
            .expect("initial adapter");
        registry
            .shutdown(&agent_id)
            .await
            .expect("shutdown adapter");
        probe.ready.store(false, Ordering::SeqCst);

        let error = match registry.get_or_start(&agent_id).await {
            Ok(_) => panic!("runtime removal must prevent a new process start"),
            Err(error) => error,
        };
        assert_eq!(error.kind, HarnessErrorKind::RuntimeMissing);
        assert_eq!(factory.created.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn both_agents_route_through_the_same_factory() {
        let factory = Arc::new(FakeFactory::default());
        let registry = AgentRegistry::with_components(
            built_in_definitions(),
            Arc::new(ReadyProbe),
            factory.clone(),
        );
        registry
            .get_or_start(&AgentId::new(CODEX_AGENT_ID))
            .await
            .expect("Codex adapter");
        registry
            .get_or_start(&AgentId::new(CLAUDE_AGENT_ID))
            .await
            .expect("Claude adapter");

        let created_ids = factory
            .agent_ids
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(
            created_ids,
            [AgentId::new(CODEX_AGENT_ID), AgentId::new(CLAUDE_AGENT_ID)]
        );
    }

    fn output(stdout: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    #[tokio::test]
    async fn managed_auth_changes_invalidate_consumers_and_stop_on_drop() {
        let registry = AgentRegistry::with_runtime_probe(Arc::new(ReadyProbe));
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        registry.set_auth_change_sink(Arc::new(move |id| {
            let _ = sender.send(id.clone());
        }));
        let id = AgentId::new("claude");
        let fake = Arc::new(FakeAdapter::new(
            registry.definition(&id).unwrap(),
            1,
            false,
        ));
        let adapter: Arc<dyn HarnessAdapter> = fake.clone();
        let managed = registry.manage(&adapter);
        for verdict in [HarnessAuthState::Ready, HarnessAuthState::Missing] {
            fake.status_tx
                .send_modify(|status| status.auth_state = verdict);
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                    .await
                    .unwrap(),
                Some(id.clone())
            );
            fake.status_tx
                .send_modify(|status| status.last_error = Some("unrelated status update".into()));
            tokio::task::yield_now().await;
            assert!(
                receiver.try_recv().is_err(),
                "unchanged auth must not invalidate"
            );
        }
        drop(managed);
        tokio::task::yield_now().await;
        assert_eq!(
            fake.status_tx.receiver_count(),
            0,
            "replacing an adapter retires its watcher"
        );
    }

    #[tokio::test]
    async fn managed_session_catalog_changes_reach_the_catalog_sink() {
        let registry = AgentRegistry::with_runtime_probe(Arc::new(ReadyProbe));
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        registry.set_session_catalog_sink(Arc::new(
            move |definition: &AgentDefinition, status: &AdapterStatus| {
                let _ = sender.send((
                    definition.agent_id.clone(),
                    status.models.clone(),
                    status.default_model_id.clone(),
                ));
            },
        ));
        let id = AgentId::new("claude");
        let fake = Arc::new(FakeAdapter::new(
            registry.definition(&id).unwrap(),
            1,
            false,
        ));
        let adapter: Arc<dyn HarnessAdapter> = fake.clone();
        let _managed = registry.manage(&adapter);
        let models = vec![AgentModelOption {
            model_id: "sonnet".into(),
            display_name: "Sonnet 5.5".into(),
        }];
        fake.status_tx.send_modify(|status| {
            status.models = models.clone();
            status.default_model_id = Some("sonnet".into());
        });
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap(),
            Some((id.clone(), models, Some("sonnet".into())))
        );
        // A second session advertising the same catalog, or an unrelated
        // status update, has nothing new to record.
        fake.status_tx.send_modify(|status| {
            status.auth_state = HarnessAuthState::Ready;
            status.default_model_id = Some("sonnet".into());
        });
        tokio::task::yield_now().await;
        assert!(
            receiver.try_recv().is_err(),
            "an unchanged catalog must not be re-recorded"
        );
        fake.status_tx.send_modify(|status| {
            status.session_selects = Some(AgentSessionSelects::default());
        });
        assert!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn coalesced_session_catalog_and_stop_preserves_new_observation() {
        use crate::agent::model_catalog::{pinned_adapter_package, ModelCatalogService};

        for (lifecycle, models_changed, selects_changed) in [
            (AdapterLifecycleState::Stopped, true, true),
            (AdapterLifecycleState::Dead, true, true),
            (AdapterLifecycleState::Stopped, false, true),
            (AdapterLifecycleState::Dead, false, true),
            (AdapterLifecycleState::Stopped, true, false),
            (AdapterLifecycleState::Dead, true, false),
        ] {
            let registry = AgentRegistry::with_runtime_probe(Arc::new(ReadyProbe));
            let id = AgentId::new("claude");
            let definition = registry.definition(&id).unwrap();
            let catalog = Arc::new(ModelCatalogService::load(None));
            catalog.record(
                &id,
                vec![AgentModelOption {
                    model_id: "cached".into(),
                    display_name: "Cached".into(),
                }],
                Some("cached".into()),
                Some(true),
                pinned_adapter_package(&definition),
                Some(AgentSessionSelects::default()),
            );
            let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let recording = Arc::clone(&catalog);
            registry.set_session_catalog_sink(Arc::new(move |definition, status| {
                recording.record_session_change(definition, status);
                let _ = sender.send(status.clone());
            }));
            let fake = Arc::new(FakeAdapter::new(definition, 1, false));
            fake.status_tx.send_modify(|status| {
                (status.models, status.default_model_id) = catalog.catalog_for(&id);
                status.session_selects = catalog.session_selects_for(&id);
            });
            let adapter: Arc<dyn HarnessAdapter> = fake.clone();
            let _managed = registry.manage(&adapter);
            let models = if models_changed {
                vec![AgentModelOption {
                    model_id: "fresh".into(),
                    display_name: "Fresh".into(),
                }]
            } else {
                catalog.models_for(&id)
            };
            let default_model_id = models.first().map(|model| model.model_id.clone());
            let selects = if selects_changed {
                AgentSessionSelects {
                    modes: vec![crate::agent::types::AgentSelectOption {
                        id: "plan".into(),
                        display_name: "Plan".into(),
                    }],
                    default_mode_id: Some("plan".into()),
                    ..AgentSessionSelects::default()
                }
            } else {
                AgentSessionSelects::default()
            };
            // No await between updates on this current-thread runtime: the
            // watcher can only read the final stopped/dead snapshot.
            fake.status_tx.send_modify(|status| {
                status.lifecycle = AdapterLifecycleState::Ready;
                status.models = models.clone();
                status.default_model_id = default_model_id.clone();
                status.session_selects = Some(selects.clone());
                status.capabilities.prompt_image = true;
            });
            fake.status_tx
                .send_modify(|status| status.lifecycle = lifecycle);
            let observed = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(observed.lifecycle, lifecycle);
            assert_eq!(
                observed.models,
                if models_changed {
                    models.clone()
                } else {
                    Vec::new()
                }
            );
            assert_eq!(
                observed.default_model_id,
                if models_changed {
                    default_model_id.clone()
                } else {
                    None
                }
            );
            assert_eq!(
                observed.session_selects,
                if selects_changed {
                    Some(selects.clone())
                } else {
                    None
                }
            );
            assert_eq!(catalog.catalog_for(&id), (models, default_model_id));
            assert_eq!(catalog.session_selects_for(&id), Some(selects));
        }
    }

    struct ReadyProbe;

    impl RuntimeProbe for ReadyProbe {
        fn probe(&self, _definition: &AgentDefinition) -> AgentRuntimeStatus {
            AgentRuntimeStatus::ready("v22.0.0", "10.0.0")
        }
    }

    struct ToggleProbe {
        ready: AtomicBool,
    }

    impl RuntimeProbe for ToggleProbe {
        fn probe(&self, _definition: &AgentDefinition) -> AgentRuntimeStatus {
            if self.ready.load(Ordering::SeqCst) {
                AgentRuntimeStatus::ready("v22.0.0", "10.0.0")
            } else {
                missing_status("node")
            }
        }
    }

    #[derive(Default)]
    struct FakeFactory {
        created: AtomicUsize,
        agent_ids: StdMutex<Vec<AgentId>>,
        fail_shutdown: bool,
        hold_start: bool,
        adapters: StdMutex<Vec<Arc<FakeAdapter>>>,
    }

    impl FakeFactory {
        fn last_adapter(&self) -> Arc<FakeAdapter> {
            self.adapters
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .last()
                .cloned()
                .expect("an adapter was created")
        }
    }

    impl AdapterFactory for FakeFactory {
        fn create(&self, definition: AgentDefinition, generation: u64) -> Arc<dyn HarnessAdapter> {
            self.created.fetch_add(1, Ordering::SeqCst);
            self.agent_ids
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(definition.agent_id.clone());
            let mut adapter = FakeAdapter::new(definition, generation, self.fail_shutdown);
            adapter.hold_start = self.hold_start;
            let adapter = Arc::new(adapter);
            self.adapters
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(Arc::clone(&adapter));
            adapter
        }
    }

    struct FakeAdapter {
        definition: AgentDefinition,
        status_tx: watch::Sender<AdapterStatus>,
        updates_tx: HarnessUpdateSender,
        sessions: StdMutex<HashMap<HarnessSessionId, HarnessSessionSummary>>,
        fail_shutdown: bool,
        hold_start: bool,
        shutdown_calls: AtomicUsize,
    }

    impl FakeAdapter {
        fn new(definition: AgentDefinition, generation: u64, fail_shutdown: bool) -> Self {
            let mut status = AdapterStatus::stopped();
            status.generation = generation.saturating_sub(1);
            let (status_tx, _) = watch::channel(status);
            let updates_tx = HarnessUpdateSender::default();
            Self {
                definition,
                status_tx,
                updates_tx,
                sessions: StdMutex::new(HashMap::new()),
                fail_shutdown,
                hold_start: false,
                shutdown_calls: AtomicUsize::new(0),
            }
        }
    }

    impl HarnessAdapter for FakeAdapter {
        fn definition(&self) -> &AgentDefinition {
            &self.definition
        }

        fn status(&self) -> watch::Receiver<AdapterStatus> {
            self.status_tx.subscribe()
        }

        fn subscribe_updates(&self) -> HarnessUpdateReceiver {
            self.updates_tx.subscribe()
        }

        fn sessions(&self) -> Vec<HarnessSessionSummary> {
            self.sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .values()
                .cloned()
                .collect()
        }

        fn start(&self) -> HarnessFuture<'_, AdapterStatus> {
            self.status_tx.send_modify(|status| {
                if status.lifecycle != AdapterLifecycleState::Ready {
                    status.generation = status.generation.saturating_add(1);
                }
                status.lifecycle = if self.hold_start {
                    AdapterLifecycleState::Starting
                } else {
                    AdapterLifecycleState::Ready
                };
                status.auth_state = HarnessAuthState::Unknown;
            });
            let status = self.status_tx.borrow().clone();
            Box::pin(async move {
                if self.hold_start {
                    let mut updates = self.status_tx.subscribe();
                    while updates.borrow().lifecycle == AdapterLifecycleState::Starting {
                        updates.changed().await.unwrap();
                    }
                    return Err(HarnessError::new(
                        HarnessErrorKind::RequestCancelled,
                        "start stopped during core exit",
                    ));
                }
                Ok(status)
            })
        }

        fn shutdown(&self) -> HarnessFuture<'_, ()> {
            Box::pin(async move {
                self.shutdown_calls.fetch_add(1, Ordering::SeqCst);
                if self.fail_shutdown {
                    return Err(HarnessError::new(
                        HarnessErrorKind::Internal,
                        "fake shutdown failed",
                    ));
                }
                self.status_tx.send_modify(|status| {
                    status.lifecycle = AdapterLifecycleState::Stopped;
                });
                Ok(())
            })
        }

        fn authenticate(&self, _method_id: String) -> HarnessFuture<'_, ()> {
            unsupported()
        }

        fn new_session(
            &self,
            _cwd: PathBuf,
            _config: crate::agent::SessionConfigRequest,
        ) -> HarnessFuture<'_, HarnessSessionId> {
            unsupported()
        }

        fn load_session(
            &self,
            _session_id: HarnessSessionId,
            _cwd: PathBuf,
        ) -> HarnessFuture<'_, ()> {
            unsupported()
        }

        fn delete_session(&self, _session_id: HarnessSessionId) -> HarnessFuture<'_, ()> {
            unsupported()
        }

        fn prompt(
            &self,
            _session_id: HarnessSessionId,
            _prompt: HarnessPrompt,
        ) -> HarnessFuture<'_, HarnessStopReason> {
            unsupported()
        }

        fn steer(
            &self,
            _session_id: HarnessSessionId,
            _prompt: HarnessPrompt,
        ) -> HarnessFuture<'_, ()> {
            unsupported()
        }

        fn cancel(&self, _session_id: HarnessSessionId) -> HarnessFuture<'_, ()> {
            unsupported()
        }
    }

    fn unsupported<'a, T>() -> HarnessFuture<'a, T> {
        Box::pin(async {
            Err(HarnessError::new(
                HarnessErrorKind::Internal,
                "unsupported fake operation",
            ))
        })
    }
}
