use super::acp::AcpFeature;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch};

pub type HarnessFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, HarnessError>> + Send + 'a>>;
pub type HarnessPermissionFuture<'a> =
    Pin<Box<dyn Future<Output = HarnessPermissionOutcome> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AgentId(pub String);

impl AgentId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// What a sign-in grants, so the UI can word it without knowing the vendor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountKind {
    /// A vendor subscription reached through a browser login.
    Subscription,
    /// A provider API key.
    ApiKey,
    /// One login that can hold several providers at once; core cannot tell
    /// which, so the label names none. Model IDs read `provider/model`.
    MultiProvider {
        /// Providers the agent lists with no setup at all (omp's on-device
        /// `apple`), which say nothing about what the user configured.
        keyless_providers: &'static [&'static str],
    },
}

/// Everything a client needs to show an agent, owned by its definition so a
/// new agent renders without per-id switches anywhere downstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPresentation {
    /// What the UI calls the agent ("Claude Code").
    pub product_name: &'static str,
    pub vendor_name: &'static str,
    /// The vendor CLI's own config dir, for the "pull in your old setup" hint.
    pub vendor_home: Option<&'static str>,
    /// The app's icon catalog key; `None` (or a key the app lacks) renders a
    /// monogram.
    pub brand: Option<&'static str>,
    /// What signing in connects ("ChatGPT account").
    pub account_label: &'static str,
    pub account_kind: AccountKind,
}

/// The pinned coordinate an agent's adapter ships as. Carried explicitly
/// so nothing has to recover `name@version` from launch arguments — the
/// arguments describe *how to run it*, this describes *what it is*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterPackage {
    pub name: String,
    pub version: String,
    pub source: AdapterSource,
}

impl AdapterPackage {
    /// The `name@version` install spec npm and the catalog both speak.
    pub fn spec(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }

    /// The program the adapter installs as: the npm package's last name
    /// segment (`@agentclientprotocol/claude-agent-acp` →
    /// `claude-agent-acp`), or a binary's own command name.
    pub fn program(&self) -> &str {
        match &self.source {
            AdapterSource::Npm => self.name.rsplit('/').next().unwrap_or(&self.name),
            AdapterSource::Binary(binary) => &binary.command,
        }
    }

    pub fn binary(&self) -> Option<&BinaryAdapter> {
        match &self.source {
            AdapterSource::Npm => None,
            AdapterSource::Binary(binary) => Some(binary),
        }
    }
}

/// Where an adapter's files come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterSource {
    /// An npm package that npm installs and Node runs.
    Npm,
    /// A vendor-built executable, downloaded per architecture and verified
    /// against a pinned digest. Needs no Node.
    Binary(BinaryAdapter),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryAdapter {
    /// The installed executable's name, which is also how the agent's own
    /// terminal-login answer refers to itself.
    pub command: String,
    pub artifacts: Vec<BinaryArtifact>,
}

impl BinaryAdapter {
    /// The artifact for this machine's architecture.
    pub fn for_host(&self) -> Option<&BinaryArtifact> {
        self.artifacts
            .iter()
            .find(|artifact| artifact.arch == std::env::consts::ARCH)
    }
}

/// One architecture's download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryArtifact {
    /// `std::env::consts::ARCH` (`aarch64`, `x86_64`).
    pub arch: &'static str,
    pub url: String,
    /// Lowercase hex sha256 of the downloaded file.
    pub sha256: String,
    pub archive: BinaryArchive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinaryArchive {
    /// The download is the executable itself.
    Raw,
    /// A gzipped tarball; `member` is the executable's path inside it.
    Tgz { member: String },
}

/// One environment variable that points a vendor CLI at the home Ottam
/// manages for an instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HomeVariable {
    pub name: &'static str,
    pub target: HomeTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeTarget {
    /// The home directory itself.
    Root,
    /// A directory inside the home, created with it.
    Dir(&'static str),
    /// A file inside the home, which the CLI creates.
    File(&'static str),
}

impl HomeVariable {
    pub fn value(&self, home: &std::path::Path) -> std::path::PathBuf {
        match self.target {
            HomeTarget::Root => home.to_path_buf(),
            HomeTarget::Dir(path) | HomeTarget::File(path) => home.join(path),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLaunchSpec {
    pub command: String,
    pub arguments: Vec<String>,
    pub environment: Vec<(String, String)>,
    pub minimum_node_major: u64,
}

/// When a terminal-delivered login shows its terminal to the user.
///
/// This is knowledge about the vendor's login *program*, not about a method:
/// claude's login prints a browser URL and finishes on its own, while another
/// vendor's may be a full-screen picker that waits for a human — and both
/// advertise themselves as terminal methods, so the method's shape cannot tell
/// them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalLoginReveal {
    /// Show the terminal from the first frame; the login is the interaction.
    Immediate,
    /// Keep the terminal hidden; reveal only when the login looks like it is
    /// waiting on someone to type (it goes silent).
    WhenStuck,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDefinition {
    pub agent_id: AgentId,
    pub instance: Option<super::instances::AgentInstance>,
    pub presentation: AgentPresentation,
    pub terminal_login_reveal: TerminalLoginReveal,
    /// `None` for adapters that are not installed packages (the in-process
    /// test engine); every real engine carries its pin here.
    pub adapter: Option<AdapterPackage>,
    pub launch: AgentLaunchSpec,
    /// How a managed home reaches the CLI. Empty means the CLI has no home
    /// override, so it gets no isolated home and runs against its default.
    pub home_variables: &'static [HomeVariable],
    pub subagent_messages: SubagentMessageTools,
    /// Subagent activity the agent projects onto the parent's session, tagged
    /// per child, with a control call that launched it.
    pub child_sessions: Option<ChildSessionTags>,
    /// A plugin Ottam ships for this agent and loads into every launch,
    /// whatever the home.
    pub client_plugin: Option<ClientPlugin>,
    /// JSON settings Ottam keeps in a managed home, never in an
    /// existing setup.
    pub home_settings: &'static [HomeSetting],
    /// What core answers when the user skips the agent's form.
    pub form_skip: FormSkip,
    /// What shows the agent is signed in, when it sends no auth status.
    pub sign_in_proof: SignInProof,
    /// Arguments to the agent's own command that its terminal sign-in runs
    /// in place of the one the adapter advertises. `None` runs the advertised
    /// one.
    pub terminal_login_args: Option<&'static [&'static str]>,
    pub mcp_connections: McpConnections,
    /// How the model-catalog probe keeps its session out of the agent's
    /// history. Rests on adapter internals: recheck at every pin bump.
    pub catalog_probe: super::ProbeCleanup,
}

/// What shows an agent without `auth_status` updates is signed in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SignInProof {
    /// Opening a session at all.
    #[default]
    Session,
    /// A session that offers a model. oh-my-pi opens sessions with no
    /// provider set up and lists only the models it holds credentials for
    /// (`#buildConfigOptions`).
    ModelSelector,
}

/// How an agent holds the MCP servers its sessions are sent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum McpConnections {
    /// Each session connects with its own entries.
    #[default]
    PerSession,
    /// One connection per server and session directory, shared by every
    /// session there with the headers of the one opened there last, as
    /// OpenCode does.
    PerDirectory,
}

/// How an agent hears that the user skipped its form. ACP's `decline`
/// means "the user saw it and said no"; an agent that treats `decline` as
/// a cancel would end the turn on Skip.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FormSkip {
    #[default]
    Decline,
    /// `accept` with no content. OpenCode 2.0.27 cancels its question tool
    /// on `decline`, which ends the step interrupted (`question.ts:97`);
    /// its question fields are optional, so an empty answer continues.
    EmptyAccept,
}

impl AgentDefinition {
    /// Whether the adapter is a prebuilt binary, run directly without Node.
    pub fn runs_binary(&self) -> bool {
        self.adapter
            .as_ref()
            .is_some_and(|package| package.binary().is_some())
    }
}

/// How an agent tags child-session updates it projects onto the parent's
/// session, and the tool call that launches a child. OpenCode's
/// `_meta["opencode/child-session"] {id, parentID, depth, title}` beside its
/// `subagent` tool, whose `description` is the child's title. Core rewrites
/// them into native subagent sessions (`agent/acp/child_sessions.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildSessionTags {
    pub meta_key: &'static str,
    pub control_tool: &'static str,
    /// The control call's `rawInput` fields: the thread's name (the child's
    /// title) and the prompt that opens its tab.
    pub name: &'static str,
    pub prompt: &'static str,
}

/// One top-level key of a JSON file inside a managed home, set on every
/// start; the file's other keys are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HomeSetting {
    /// Relative to the home.
    pub file: &'static str,
    pub key: &'static str,
    pub value: bool,
}

/// Files Ottam writes under `<agents root>/<harness>/plugins/<name>/`,
/// and the variable that loads them: `"{dir}"` (quotes included) in
/// `template` becomes that directory's path as a JSON string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientPlugin {
    pub name: &'static str,
    pub files: &'static [(&'static str, &'static str)],
    pub variable: &'static str,
    pub template: &'static str,
    /// For an Ottam profile, the variable that carries the user's own
    /// values of the home variables as a JSON object (`null` when unset), so
    /// the plugin can give them back to the shell commands the agent runs.
    pub shell_environment: Option<&'static str>,
}

/// Tool calls that carry messages between a native subagent and its parent
/// which the adapter forwards only as the call's input. Core shows each
/// message in the subagent's thread; the call itself stays a tool row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubagentMessageTools {
    /// The subagent's final report, as a call from the subagent's session.
    pub handback: Option<SubagentHandbackTool>,
    /// The parent's message to a subagent that is still running.
    pub follow_up: Option<SubagentFollowUpTool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentHandbackTool {
    pub tool: &'static str,
    /// The `rawInput` field holding the report.
    pub message: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentFollowUpTool {
    pub tool: &'static str,
    /// The `rawInput` field naming the subagent by its first run's session id.
    pub target: &'static str,
    /// The `rawInput` field holding the message.
    pub message: &'static str,
}

#[cfg(test)]
impl AgentDefinition {
    /// A neutral definition for unit tests; override what a test is about with
    /// struct-update syntax so definition fields change in one place.
    pub(crate) fn for_test(agent_id: &str) -> Self {
        Self {
            agent_id: AgentId::new(agent_id),
            instance: None,
            presentation: AgentPresentation {
                product_name: "Test Agent",
                vendor_name: "Test Vendor",
                vendor_home: None,
                brand: None,
                account_label: "Test account",
                account_kind: AccountKind::Subscription,
            },
            terminal_login_reveal: TerminalLoginReveal::Immediate,
            adapter: None,
            launch: AgentLaunchSpec {
                command: "npx".to_string(),
                arguments: Vec::new(),
                environment: Vec::new(),
                minimum_node_major: 18,
            },
            home_variables: &[],
            subagent_messages: SubagentMessageTools::default(),
            child_sessions: None,
            client_plugin: None,
            home_settings: &[],
            form_skip: FormSkip::Decline,
            sign_in_proof: SignInProof::Session,
            terminal_login_args: None,
            mcp_connections: McpConnections::PerSession,
            catalog_probe: super::ProbeCleanup::Nothing,
        }
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelOption {
    #[serde(rename = "modelID")]
    pub model_id: String,
    pub display_name: String,
}

/// One choice in an agent-advertised session select (mode, thought level).
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "camelCase")]
pub struct AgentSelectOption {
    pub id: String,
    pub display_name: String,
}

/// The mode and thought-level selectors an agent advertised on `session/new`,
/// observed as a unit so "advertises none" (empty lists) stays distinguishable
/// from "no session has been created yet" (the whole struct absent).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionSelects {
    #[serde(default)]
    pub modes: Vec<AgentSelectOption>,
    #[serde(
        rename = "defaultModeID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub default_mode_id: Option<String>,
    #[serde(default)]
    pub thought_levels: Vec<AgentSelectOption>,
    #[serde(
        rename = "defaultThoughtLevelID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub default_thought_level_id: Option<String>,
}

/// The thought-level select one model brings with it. Agents change both the
/// levels and the current value when the model changes (omp applies each
/// model's own default, OpenCode drops the select for models without
/// variants), so one engine-wide default misdescribes every other model.
/// Observed right after `session/new` or a model switch, before Ottam
/// applies an explicit level: the effort a run gets when it picks none.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelSelects {
    #[serde(default)]
    pub thought_levels: Vec<AgentSelectOption>,
    #[serde(
        rename = "defaultThoughtLevelID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub default_thought_level_id: Option<String>,
}

/// Per-session configuration requested at `session/new`. All fields optional;
/// `None` leaves that selector on the agent's default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionConfigRequest {
    pub model_id: Option<String>,
    pub mode_id: Option<String>,
    pub thought_level_id: Option<String>,
    /// MCP servers the session reaches through core's proxy, sent on
    /// `session/new` and on the `session/load` that restores it.
    pub mcp_servers: Vec<McpServerEntry>,
    /// `_meta` for `session/new`. Only the model-catalog probe sets it.
    pub meta: Option<serde_json::Map<String, serde_json::Value>>,
}

/// A model and/or thought level to switch a held session to before its next
/// prompt. A `None` field is left alone, though a model switch can move the
/// level on its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelSwitch {
    pub model_id: Option<String>,
    pub thought_level_id: Option<String>,
    /// The session was just restored. A loaded session can list concrete
    /// model ids rather than the picker aliases a run saves, so a value the
    /// load does not list keeps the restored one instead of failing the run.
    pub restored: bool,
}

/// One Streamable HTTP MCP server an agent session is given: a path on core's
/// loopback proxy and the headers that identify the session to it.
#[derive(Clone, PartialEq, Eq)]
pub struct McpServerEntry {
    pub name: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
}

// Headers carry the session's bearer token, which never reaches a log.
impl std::fmt::Debug for McpServerEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpServerEntry")
            .field("name", &self.name)
            .field("url", &self.url)
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeAvailability {
    Ready,
    Missing { component: String },
    Invalid { component: String },
    Unsupported { found: u64, minimum: u64 },
}

/// Which toolchain the adapters will actually run on. Reported alongside
/// availability so the UI can say "using Ottam's Node" rather than leaving
/// the user guessing which of their several Node installs is in play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSource {
    System,
    Managed,
    Override,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRuntimeStatus {
    pub availability: RuntimeAvailability,
    /// None when nothing was resolved and the bare `node`/`npx` names are used.
    pub source: Option<RuntimeSource>,
    pub node_version: Option<String>,
    pub npx_version: Option<String>,
}

impl AgentRuntimeStatus {
    pub fn ready(node_version: impl Into<String>, npx_version: impl Into<String>) -> Self {
        Self {
            availability: RuntimeAvailability::Ready,
            source: None,
            node_version: Some(node_version.into()),
            npx_version: Some(npx_version.into()),
        }
    }

    /// A binary-lane adapter runs without Node: ready, with no toolchain to
    /// report.
    pub fn not_required() -> Self {
        Self {
            availability: RuntimeAvailability::Ready,
            source: None,
            node_version: None,
            npx_version: None,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.availability == RuntimeAvailability::Ready
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterLifecycleState {
    Stopped,
    Starting,
    Ready,
    Stopping,
    Dead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HarnessAuthState {
    Unknown,
    Authenticating,
    Ready,
    Missing,
}

/// Who performs a sign-in, mirroring the ACP `AuthMethod` variants.
///
/// Only `Terminal` hands Ottam a command to run. It is keyed off the
/// method's own variant rather than the agent id so it survives either adapter
/// changing its mind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HarnessAuthMethodKind {
    /// The agent authenticates itself. ACP's default when no `type` is given.
    #[default]
    Agent,
    /// The client runs the method's command in a terminal.
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessAuthMethod {
    pub method_id: String,
    pub name: String,
    pub description: Option<String>,
    pub kind: HarnessAuthMethodKind,
    /// Set exactly for [`HarnessAuthMethodKind::Terminal`]; `None` otherwise.
    pub terminal_command: Option<HarnessAuthTerminalCommand>,
}

/// The command line for a terminal auth method, as the adapter described it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessAuthTerminalCommand {
    /// The adapter's own absolute interpreter path, from
    /// `_meta["terminal-auth"].command`. When it is absent the adapter expects
    /// the client to re-run the agent's own launch command with `args` — which
    /// means resolving through `npx` a second time, so prefer this when offered.
    pub program: Option<String>,
    /// Complete argument list for `program`; otherwise the arguments to append
    /// to the agent's launch command.
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// The adapter's own name for this login, if it gave one.
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SteeringCapability {
    Native,
    InterruptThenPrompt,
    #[default]
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HarnessCapabilities {
    /// Sign-in state arrives through the connection's auth-status extension;
    /// successful session creation/loading is not authentication evidence.
    pub auth_status: bool,
    pub load_session: bool,
    pub delete_session: bool,
    pub steering: SteeringCapability,
    /// The agent accepts image content blocks in a prompt. Individual models
    /// behind the agent may still refuse them — ACP has no way to say which.
    pub prompt_image: bool,
    /// Everything the `initialize` response said about the agent, keyed by
    /// ACP surface. The named bits above are views onto the same answer and
    /// stay so callers do not change. A feature with no entry could not be
    /// judged from `initialize` (update kinds, client requests, and AIR
    /// extensions are only observed once a session runs), which is distinct
    /// from `false`, an explicit "not advertised".
    pub agent_advertises: BTreeMap<AcpFeature, bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterStatus {
    pub lifecycle: AdapterLifecycleState,
    pub auth_state: HarnessAuthState,
    pub auth_methods: Vec<HarnessAuthMethod>,
    pub agent_name: Option<String>,
    pub agent_version: Option<String>,
    pub capabilities: HarnessCapabilities,
    /// Model choices advertised by the agent's `session/new` config options.
    /// Empty until the adapter has created at least one session.
    pub models: Vec<AgentModelOption>,
    /// The model select's current value on the most recent `session/new` —
    /// the model the agent runs when the client requests none.
    pub default_model_id: Option<String>,
    /// Mode and thought-level selects observed on the most recent
    /// `session/new`; `None` until a session has been created this generation.
    pub session_selects: Option<AgentSessionSelects>,
    /// Per-model thought-level selects observed this generation, keyed by
    /// model id. Accumulates across sessions; see [`AgentModelSelects`].
    pub model_selects: BTreeMap<String, AgentModelSelects>,
    pub generation: u64,
    pub last_error: Option<String>,
    /// This adapter generation was observed authenticated at least once. It is
    /// what separates credentials that *lapsed* from credentials that were never
    /// there: ACP reports both as "not authenticated", but only the first should
    /// be described to the user as expired. Reset whenever the adapter restarts,
    /// since a fresh process has observed nothing yet.
    pub credentials_seen: bool,
}

impl AdapterStatus {
    pub fn stopped() -> Self {
        Self {
            lifecycle: AdapterLifecycleState::Stopped,
            auth_state: HarnessAuthState::Unknown,
            auth_methods: Vec::new(),
            credentials_seen: false,
            agent_name: None,
            agent_version: None,
            capabilities: HarnessCapabilities::default(),
            models: Vec::new(),
            default_model_id: None,
            session_selects: None,
            model_selects: BTreeMap::new(),
            generation: 0,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HarnessSessionId(pub String);

impl HarnessSessionId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for HarnessSessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessSessionState {
    Active,
    Prompting,
    Dead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSessionSummary {
    pub session_id: HarnessSessionId,
    pub cwd: PathBuf,
    pub state: HarnessSessionState,
    pub last_error: Option<String>,
    /// The permission mode the agent last confirmed: its acknowledgment at
    /// new/load/apply, or a mode change it announced. `None` when unknown.
    pub current_mode_id: Option<String>,
    /// The model and thought level the agent last confirmed, on the same
    /// terms as the mode: its acknowledgment at new/load/apply, or a change it
    /// announced. `None` when unknown or not advertised.
    pub current_model_id: Option<String>,
    pub current_thought_level_id: Option<String>,
    /// The agent's last `available_commands_update` payload, kept even when it
    /// arrived before any run could record it (as while `session/new` is in
    /// flight). `None` until the agent announces commands.
    pub announced_commands: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HarnessUpdate {
    pub session_id: HarnessSessionId,
    pub sequence: u64,
    pub update: HarnessUpdateKind,
}

pub type HarnessUpdateReceiver = mpsc::Receiver<HarnessUpdate>;

/// Updates one subscriber may have queued before newer ones are dropped. Far
/// above any burst a responsive subscriber sees (a long `session/load` replay
/// is a few thousand), so only a stalled subscriber reaches it, and then it
/// loses updates, which it sees as a sequence gap, instead of holding Core's
/// memory without limit.
pub const UPDATE_LANE_CAPACITY: usize = 1 << 16;

/// Fans an adapter's updates out to every subscriber, each on its own lane,
/// so one subscriber falling behind never costs another its updates.
///
/// Sending never waits for room. The ACP SDK already reads the agent's stdout
/// into an unbounded queue, so waiting would not bound memory; it would hold
/// the SDK's dispatch loop, and with it the replies to core's own requests. A
/// full lane drops the newest update instead.
#[derive(Clone)]
pub struct HarnessUpdateSender {
    lanes: Arc<Mutex<Vec<UpdateLane>>>,
    capacity: usize,
}

struct UpdateLane {
    tx: mpsc::Sender<HarnessUpdate>,
    /// Whether the lane is dropping updates, so it logs once per stall.
    full: bool,
}

impl Default for HarnessUpdateSender {
    fn default() -> Self {
        Self::with_capacity(UPDATE_LANE_CAPACITY)
    }
}

impl HarnessUpdateSender {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            lanes: Arc::default(),
            capacity,
        }
    }

    pub fn subscribe(&self) -> HarnessUpdateReceiver {
        let (tx, rx) = mpsc::channel(self.capacity);
        self.lanes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(UpdateLane { tx, full: false });
        rx
    }

    /// Queues `update` for every live subscriber, forgets dropped ones, and
    /// returns how many are live. The last subscriber takes the update itself,
    /// so the usual single subscriber costs no copy of its payload.
    pub fn send(&self, update: HarnessUpdate) -> usize {
        let mut lanes = self
            .lanes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        lanes.retain(|lane| !lane.tx.is_closed());
        let live = lanes.len();
        let Some((last, others)) = lanes.split_last_mut() else {
            return 0;
        };
        for lane in others {
            lane.offer(update.clone());
        }
        last.offer(update);
        live
    }
}

impl UpdateLane {
    fn offer(&mut self, update: HarnessUpdate) {
        match self.tx.try_send(update) {
            Ok(()) => self.full = false,
            Err(mpsc::error::TrySendError::Full(update)) => {
                if !self.full {
                    self.full = true;
                    tracing::warn!(
                        session_id = %update.session_id.0,
                        sequence = update.sequence,
                        capacity = self.tx.max_capacity(),
                        "agent update subscriber stalled; dropping updates until it catches up"
                    );
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum HarnessUpdateKind {
    SessionSignal(ottam_protocol::ConversationSessionSignal),
    RichMessageChunk {
        text: String,
        message_id: Option<String>,
        reasoning: bool,
        metadata: ottam_protocol::TranscriptAdapterMetadata,
    },
    UserMessageChunk(String),
    AgentMessageChunk(String, Option<String>),
    AgentThoughtChunk(String, Option<String>),
    AgentImageChunk(Value, Option<String>),
    ToolCall(Value),
    ToolCallUpdate(Value),
    Plan(Value),
    AsyncTaskSpawned(AsyncTaskUpdate),
    AsyncTaskProgress(AsyncTaskUpdate),
    AsyncTaskStateUpdate(ottam_protocol::AsyncTaskState, AsyncTaskUpdate),
    /// AIR native subagent session `child`, announced on its parent's session.
    SubagentSpawned {
        child: String,
        name: Option<String>,
        task: Option<String>,
        /// The run's prompt; a resumed generation's is the parent's message.
        prompt: Option<String>,
    },
    /// A native subagent session's terminal state.
    SubagentStateUpdate {
        child: String,
        state: ottam_protocol::SubagentThreadState,
    },
    Other {
        kind: String,
        payload: Value,
    },
    /// The tokens the agent reported in its answer to the session's prompt.
    /// Sent just before [`Self::PromptSettled`], also when no one awaits the
    /// answer any more, so a stopped run still gets its turn's usage.
    TurnUsage(HarnessTurnUsage),
    /// Core's own marker, not an agent update: the agent answered the
    /// session's prompt, and everything after it on the session is outside
    /// that turn. Sent before the connection dispatches anything the agent
    /// sent after its answer.
    PromptSettled,
}

/// A prompt answer's token counts, as ACP's `PromptResponse.usage` gives
/// them. What they cover depends on the agent
/// ([`AgentDefinition::turn_usage_scope`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessTurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub thought_tokens: Option<u64>,
    pub cached_read_tokens: Option<u64>,
    pub cached_write_tokens: Option<u64>,
    /// The answer was to a cancelled prompt.
    pub cancelled: bool,
}

/// Fields of an AIR `async_task_*` update, named as the adapters send them.
/// Only the task id is guaranteed: Codex omits `description`, and Claude
/// sends progress as changed fields only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AsyncTaskUpdate {
    pub async_task_id: String,
    pub tool_call_id: Option<String>,
    pub name: Option<String>,
    pub task_type: Option<String>,
    pub description: Option<String>,
    pub summary: Option<String>,
    pub last_tool_name: Option<String>,
    pub usage: Option<ottam_protocol::AsyncTaskUsage>,
    pub output_file_path: Option<String>,
    pub show_in_transcript: Option<bool>,
    pub can_stop: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessStopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessPermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessPermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: HarnessPermissionOptionKind,
}

#[derive(Debug, Clone)]
pub struct HarnessPermissionRequest {
    pub agent_id: AgentId,
    pub session_id: HarnessSessionId,
    pub tool_call: Value,
    pub options: Vec<HarnessPermissionOption>,
    /// Cancelled when the agent withdraws the request (`$/cancel_request`).
    pub withdrawn: tokio_util::sync::CancellationToken,
    /// The agent has no bypass mode of its own, so a conversation's "Always
    /// allow tool actions" may answer this request.
    pub auto_approvable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessPermissionOutcome {
    Cancelled,
    Selected { option_id: String },
}

pub trait HarnessPermissionBroker: Send + Sync {
    fn request_permission(&self, request: HarnessPermissionRequest) -> HarnessPermissionFuture<'_>;

    /// Puts an agent's form in front of the user. A broker with no one to
    /// ask answers `Cancelled`.
    fn ask_question(&self, _request: HarnessQuestionRequest) -> HarnessQuestionFuture<'_> {
        Box::pin(async { HarnessQuestionOutcome::Cancelled })
    }
}

pub type HarnessQuestionFuture<'a> =
    Pin<Box<dyn Future<Output = HarnessQuestionOutcome> + Send + 'a>>;

/// An agent's form, normalized into questions (`agent/acp/elicitation.rs`).
#[derive(Debug, Clone)]
pub struct HarnessQuestionRequest {
    pub agent_id: AgentId,
    pub session_id: HarnessSessionId,
    pub provider_tool_call_id: Option<String>,
    pub title: String,
    pub questions: Vec<ottam_protocol::AgentQuestion>,
    /// Cancelled when the agent withdraws the request (`$/cancel_request`).
    pub withdrawn: tokio_util::sync::CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessQuestionOutcome {
    Submitted(Vec<ottam_protocol::AgentQuestionAnswer>),
    Skipped,
    /// Nobody answered: withdrawn, stopped, or no client to ask.
    Cancelled,
}

#[derive(Debug, Default)]
pub struct CancelPermissionBroker;

impl HarnessPermissionBroker for CancelPermissionBroker {
    fn request_permission(
        &self,
        _request: HarnessPermissionRequest,
    ) -> HarnessPermissionFuture<'_> {
        Box::pin(async { HarnessPermissionOutcome::Cancelled })
    }
}

/// One image to send alongside a prompt. Carries a path rather than bytes:
/// the encoded payload is read and base64'd once, at the ACP boundary, so
/// megabytes of image never ride along through the run pipeline — which clones
/// its inputs across the retry path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessPromptImage {
    pub mime_type: String,
    pub path: PathBuf,
    /// Where the image came from on the user's disk, when it came from there
    /// at all. Pasted images have no such origin.
    pub uri: Option<String>,
}

/// What the adapter sends: the user's text plus any images attached to it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HarnessPrompt {
    pub text: String,
    pub images: Vec<HarnessPromptImage>,
}

impl HarnessPrompt {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessErrorKind {
    AgentNotFound,
    AdapterNotInstalled,
    RuntimeMissing,
    RuntimeInvalid,
    RuntimeUnsupported,
    InvalidAuthMethod,
    NotRunning,
    AdapterDead,
    AdapterExited,
    SessionNotFound,
    SessionBusy,
    /// The agent itself rejected a prompt because its session was still busy
    /// (oh-my-pi's `-32003` `session_busy`); it took nothing, so the runner
    /// may resend. Core's own active-prompt check stays [`Self::SessionBusy`].
    RemoteSessionBusy,
    SteeringUnsupported,
    /// The agent answered a steer by saying it had no turn to steer into — the
    /// turn settled between the local `Prompting` check and the request landing.
    /// Nothing was delivered, so the caller is free to send the message another
    /// way.
    NoActivePrompt,
    /// A requested model/mode/thought level that the agent advertises a
    /// selector for but no longer offers — typically an id that rotted in the
    /// catalog cache. The session was refused rather than run on the agent's
    /// default, so the run record never claims a selection it didn't get.
    SelectionUnavailable,
    AuthRequired,
    /// A token refresh was blocked by a lock, so the request never reached the
    /// agent. Distinct from [`Self::AuthRequired`] because signing in again
    /// fixes nothing — the lock has to go. When its owner record proves the
    /// holder exited, core removes it and resends the request once; what
    /// reaches the user is a live holder or a lock core cannot account for.
    /// It must never be reported as a sign-in problem or as output the agent
    /// produced.
    AuthRefreshBusy,
    RequestCancelled,
    PromptExecution,
    Protocol,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessError {
    pub kind: HarnessErrorKind,
    pub message: String,
    /// A typed terminal prompt response proves input delivery even when the
    /// adapter reports end_turn. Keep this separate from pre-admission errors.
    pub terminal_signal: Option<Box<ottam_protocol::ConversationSessionSignal>>,
    /// A native steer found the turn settled and the agent started a detached
    /// turn for it. Whatever that turn emits belongs to no run.
    pub detached_turn: bool,
}

impl HarnessError {
    pub fn new(kind: HarnessErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            terminal_signal: None,
            detached_turn: false,
        }
    }
}

impl fmt::Display for HarnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HarnessError {}

pub trait HarnessAdapter: Send + Sync {
    fn definition(&self) -> &AgentDefinition;
    fn status(&self) -> watch::Receiver<AdapterStatus>;
    fn subscribe_updates(&self) -> HarnessUpdateReceiver;
    fn sessions(&self) -> Vec<HarnessSessionSummary>;
    /// The sequence of the session's latest [`HarnessUpdateKind::PromptSettled`],
    /// so a subscriber that missed updates (sent before it subscribed, or
    /// dropped from its full lane) can tell whether the marker was among them.
    fn prompt_settled_at(&self, _session_id: &HarnessSessionId) -> Option<u64> {
        None
    }
    fn start(&self) -> HarnessFuture<'_, AdapterStatus>;
    fn shutdown(&self) -> HarnessFuture<'_, ()>;
    fn authenticate(&self, method_id: String) -> HarnessFuture<'_, ()>;
    /// Abandon an `authenticate` still in flight, failing its caller with
    /// [`HarnessErrorKind::RequestCancelled`]. Idempotent: cancelling a method
    /// with nothing outstanding succeeds. The default suits adapters whose
    /// `authenticate` resolves immediately and so is never outstanding.
    fn cancel_authenticate(&self, _method_id: String) -> HarnessFuture<'_, ()> {
        Box::pin(std::future::ready(Ok(())))
    }
    fn new_session(
        &self,
        cwd: PathBuf,
        config: SessionConfigRequest,
    ) -> HarnessFuture<'_, HarnessSessionId>;
    fn load_session(&self, session_id: HarnessSessionId, cwd: PathBuf) -> HarnessFuture<'_, ()>;
    /// Restore history and reapply an explicitly selected permission mode.
    fn load_session_with_mode(
        &self,
        session_id: HarnessSessionId,
        cwd: PathBuf,
        _mode_id: Option<String>,
    ) -> HarnessFuture<'_, ()> {
        self.load_session(session_id, cwd)
    }
    /// Restore a session with its mode and the MCP servers it is given now.
    /// The default suits adapters with no MCP support: it drops the servers.
    fn load_session_with_config(
        &self,
        session_id: HarnessSessionId,
        cwd: PathBuf,
        mode_id: Option<String>,
        _mcp_servers: Vec<McpServerEntry>,
    ) -> HarnessFuture<'_, ()> {
        self.load_session_with_mode(session_id, cwd, mode_id)
    }
    /// Switch a live session's permission mode before its next prompt,
    /// failing with [`HarnessErrorKind::SelectionUnavailable`] unless the
    /// agent acknowledges it. The default suits adapters without modes.
    fn apply_mode(&self, _session_id: HarnessSessionId, _mode_id: String) -> HarnessFuture<'_, ()> {
        Box::pin(std::future::ready(Ok(())))
    }
    /// Switch a held session's model and/or thought level before its next
    /// prompt, failing with [`HarnessErrorKind::SelectionUnavailable`] when
    /// the agent refuses or no longer offers the value. The default suits
    /// adapters without these selects.
    fn apply_model(
        &self,
        _session_id: HarnessSessionId,
        _switch: ModelSwitch,
    ) -> HarnessFuture<'_, ()> {
        Box::pin(std::future::ready(Ok(())))
    }
    fn delete_session(&self, session_id: HarnessSessionId) -> HarnessFuture<'_, ()>;
    fn prompt(
        &self,
        session_id: HarnessSessionId,
        prompt: HarnessPrompt,
    ) -> HarnessFuture<'_, HarnessStopReason>;
    fn steer(&self, session_id: HarnessSessionId, prompt: HarnessPrompt) -> HarnessFuture<'_, ()>;
    fn cancel(&self, session_id: HarnessSessionId) -> HarnessFuture<'_, ()>;
    /// Ask the agent to stop work it is doing on its own in a session with no
    /// prompt in flight, such as a cycle a background task woke. Never
    /// cancels a prompt: that is [`Self::cancel`]'s. Best effort.
    fn cancel_out_of_turn(&self, _session_id: &HarnessSessionId) -> HarnessFuture<'_, ()> {
        Box::pin(std::future::ready(Ok(())))
    }
    /// Ask the agent to stop a background task it started in the session.
    /// Answers whether it did; an agent without background tasks has none
    /// to stop.
    fn stop_background_task(
        &self,
        _session_id: &HarnessSessionId,
        _async_task_id: &str,
    ) -> HarnessFuture<'_, bool> {
        Box::pin(std::future::ready(Ok(false)))
    }
}
