use crate::ids::{
    AgentAuthSessionID, AgentID, AgentSessionID, ConversationID, ConversationRunID, ProjectID,
    TerminalID, TerminalSessionID, WorkspaceID,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const METHOD_AGENT_LIST: &str = "agent.list";
pub const METHOD_AGENT_DETECT_SETUPS: &str = "agent.detectSetups";
pub const METHOD_AGENT_CREATE: &str = "agent.create";
pub const METHOD_AGENT_RENAME: &str = "agent.rename";
pub const METHOD_AGENT_DELETE: &str = "agent.delete";
pub const METHOD_AGENT_SET_LAYOUT: &str = "agent.setLayout";
pub const METHOD_AGENT_SHUTDOWN_READINESS: &str = "agent.shutdownReadiness";
pub const METHOD_AGENT_PREPARE_SHUTDOWN: &str = "agent.prepareShutdown";
pub const METHOD_AGENT_CANCEL_SHUTDOWN: &str = "agent.cancelShutdown";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentShutdownOperationParams {
    pub operation_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentHomeKind {
    Managed,
    Existing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentCreateParams {
    #[serde(rename = "harnessID")]
    pub harness_id: String,
    pub layout: AgentHomeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRenameParams {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentDeleteParams {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    #[serde(default)]
    pub purge: bool,
    pub operation_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSetLayoutParams {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    pub layout: AgentHomeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentInstanceResult {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentDetectedSetup {
    #[serde(rename = "harnessID")]
    pub harness_id: String,
    pub display_name: String,
    pub path: String,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugins: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentDetectSetupsResult {
    pub setups: Vec<AgentDetectedSetup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentShutdownReadinessResult {
    pub ready: bool,
    pub waiting_on: Vec<String>,
    pub refresh_lock_paths: Vec<String>,
    /// Ownership remains relevant even when no shared adapter is running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_shared_homes: Option<bool>,
}
pub const METHOD_AGENT_HOME_INFO: &str = "agent.homeInfo";
pub const METHOD_AGENT_AUTH_STATE: &str = "agent.authState";
pub const METHOD_AGENT_SIGN_IN: &str = "agent.signIn";
pub const METHOD_AGENT_SIGN_IN_CANCEL: &str = "agent.signInCancel";
pub const METHOD_AGENT_SIGN_IN_FINISH: &str = "agent.signInFinish";
pub const METHOD_AGENT_SIGN_IN_EVENT: &str = "agent.signInEvent";
pub const METHOD_AGENT_REQUEST_PERMISSION: &str = "agent.requestPermission";
pub const METHOD_AGENT_ASK_QUESTION: &str = "agent.askQuestion";
/// Core withdraws a core→client request it no longer waits for: the agent
/// withdrew it, the run stopped, or core is shutting down. The client drops
/// whatever it shows for that request; an answer sent afterwards is ignored.
pub const NOTIFICATION_CANCEL_REQUEST: &str = "$/cancelRequest";

/// What signing in to an agent grants, so clients can word sign-in without
/// knowing the vendor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentAccountKind {
    /// A vendor subscription reached through a browser login.
    Subscription,
    /// A provider API key.
    ApiKey,
    /// One login that can hold several providers at once.
    MultiProvider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentRuntimeAvailability {
    Ready,
    Missing,
    Invalid,
    Unsupported,
    /// The probe outran the list's budget and is still running. Not a verdict:
    /// `engine.changed` follows once it lands.
    Checking,
}

/// Which Node the adapters will actually run on. Absent when nothing was
/// resolved, in which case the bare `node`/`npx` names are used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentRuntimeSource {
    /// The user's own toolchain — Homebrew, a version manager, wherever their
    /// login shell points.
    System,
    /// The runtime Ottam downloaded and owns.
    Managed,
    /// `OTTAM_NODE`.
    Override,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeStatus {
    pub availability: AgentRuntimeAvailability,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<AgentRuntimeSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found_major: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_major: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub npx_version: Option<String>,
    /// The managed runtime is being replaced by an app update's newer pin.
    /// Present only while this agent runs on the managed runtime and the pin
    /// has not landed; `nodeVersion` is the older runtime still in use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_update: Option<AgentUpdateStatus>,
}

/// An app update moved a pin past what is installed. The installed version
/// keeps running throughout; this says how the move to `version` is going.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentUpdateStatus {
    /// The pin being installed.
    pub version: String,
    pub state: AgentUpdateState,
    /// Why the last attempt failed, for `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentUpdateState {
    /// Not running: not started yet, or the user cancelled it. Core retries
    /// at the next launch; the client may start it sooner.
    Pending,
    Updating,
    /// The last attempt failed. Core retries at the next launch and the next
    /// run start; the client may retry sooner.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentLifecycleState {
    Stopped,
    Starting,
    Ready,
    Stopping,
    Dead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentAuthState {
    Unknown,
    Authenticating,
    Ready,
    Missing,
}

/// Who performs the sign-in for a given method.
///
/// Mirrors the ACP `AuthMethod` variants. It decides the mechanism, never the
/// presentation: both end with the user signing in, and only `Terminal` puts a
/// command in Ottam's hands to run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentAuthMethodKind {
    /// The agent authenticates itself when asked. ACP's default when a method
    /// carries no `type`, and so the default here too.
    #[default]
    Agent,
    /// The client runs the method's command in a terminal.
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthMethod {
    #[serde(rename = "methodID")]
    pub method_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub kind: AgentAuthMethodKind,
}

/// Which side of ACP a feature lives on; the sheet groups rows by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentFeatureGroup {
    AgentCapability,
    AgentMethod,
    UpdateKind,
    ClientRequest,
    ClientCapability,
    Extension,
}

/// Whether the pinned ACP SDK defines the surface and core compiles it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentFeatureSdkStatus {
    Stable,
    /// Behind an SDK cargo feature core enables.
    UnstableOn,
    /// Behind an SDK cargo feature core leaves off.
    UnstableOff,
    /// Behind a schema-crate feature the pinned SDK cannot enable.
    NotInBuild,
    /// Not in the SDK: an adapter or AIR convention.
    Extension,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentFeatureClientSupport {
    Supported,
    Partial,
    Unsupported,
}

/// The join of the agent and client columns. `Unknown` is any row the agent
/// side could not answer from `initialize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentFeatureVerdict {
    Works,
    AgentOnly,
    ClientOnly,
    Neither,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentFeatureCompatibility {
    /// The ACP wire name, e.g. `loadSession` or `session/cancel`.
    pub feature: String,
    pub group: AgentFeatureGroup,
    pub sdk_status: AgentFeatureSdkStatus,
    /// The SDK cargo feature or extension vendor behind `sdk_status`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_detail: Option<String>,
    /// What the agent said on `initialize`; absent when not judgeable there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_advertises: Option<bool>,
    pub client_support: AgentFeatureClientSupport,
    /// Which part is handled, for `Partial` rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_detail: Option<String>,
    pub verdict: AgentFeatureVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentAdapterStatus {
    pub lifecycle: AgentLifecycleState,
    pub auth_state: AgentAuthState,
    #[serde(default)]
    pub auth_methods: Vec<AgentAuthMethod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,
    #[serde(default)]
    pub steering_capability: crate::engine::SteeringCapability,
    /// Mirrors the ACP agent's `promptCapabilities.image` from `initialize`.
    #[serde(default)]
    pub supports_image_input: bool,
    /// One row per ACP surface core knows, joining what the SDK defines,
    /// what this agent advertised on `initialize`, and what Ottam's
    /// client implements. Rows are always present; the agent column is
    /// absent until the agent has been probed once.
    #[serde(default)]
    pub compatibility: Vec<AgentFeatureCompatibility>,
    /// This adapter generation was seen authenticated at least once, so a later
    /// `authState: missing` means credentials lapsed rather than were never
    /// present. Consumers use it only to word the difference.
    #[serde(default)]
    pub credentials_seen: bool,
    pub generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentSummary {
    /// Advisory snapshot; layout changes still enforce claiming transactionally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_set_layout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "harnessID")]
    pub harness_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_kind: Option<AgentHomeKind>,
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    /// The product name ("Claude Code"); `engine.list` uses the same one.
    pub display_name: String,
    pub vendor_name: String,
    /// The vendor CLI's own config dir ("~/.claude"), for a hint about
    /// carrying an existing setup over. Absent when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_home: Option<String>,
    /// The client's icon key. Absent, or a key the client does not know,
    /// means a monogram.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand: Option<String>,
    /// What signing in connects ("ChatGPT account").
    pub account_label: String,
    pub account_kind: AgentAccountKind,
    /// A `multiProvider` agent's providers the user set up, sorted, read off
    /// its last-seen model IDs (`provider/model`). Providers the agent lists
    /// with no setup (omp's on-device `apple`) are left out, so an agent can
    /// be signed in with none. Absent for other account kinds and before the
    /// agent's models were ever seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_providers: Option<Vec<String>>,
    pub runtime: AgentRuntimeStatus,
    pub adapter: AgentAdapterStatus,
    /// The pinned npm package this agent's adapter runs at. Statically known,
    /// so it is the one version a client can show before the adapter has ever
    /// started — `adapter.agent_version` is the ACP agent's self-report from
    /// `initialize` and is absent until then. `None` for agents launched some
    /// other way (the in-process test agent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_package: Option<AgentAdapterPackage>,
}

/// A pinned `name@version`, split so clients need no npm spec parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentAdapterPackage {
    pub name: String,
    pub version: String,
    /// How the pin is installed. Absent means `npm` (a server predating
    /// binary adapters).
    #[serde(default)]
    pub source: AgentAdapterSource,
    /// Whether a launchable version is installed in Ottam's managed
    /// store: this pin, or an older one standing in while an app update's
    /// pin installs (see `installedVersion`, `update`). Install is mandatory:
    /// a `notInstalled` adapter's engine is unusable until `agent.install`
    /// completes. Defaults to `installed` when absent (a server predating
    /// managed installs launched via npx on demand).
    #[serde(default = "AgentAdapterInstallState::installed")]
    pub install_state: AgentAdapterInstallState,
    /// The version that launches now. Equals `version` once the pin is
    /// installed; an older version while `update` is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    /// Present while an older installed version stands in for this pin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<AgentUpdateStatus>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentAdapterSource {
    /// An npm package that runs under Node.
    #[default]
    Npm,
    /// A vendor-built executable; needs no Node.
    Binary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentAdapterInstallState {
    NotInstalled,
    Installing,
    Installed,
}

impl AgentAdapterInstallState {
    fn installed() -> Self {
        Self::Installed
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentListParams {
    /// Invalidate the cached Node runtime resolution before probing, so a
    /// just-installed toolchain is noticed. Mirrors `EngineListParams`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_runtime: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentListResult {
    pub agents: Vec<AgentSummary>,
}

/// The isolated home Ottam gives one agent. Errors for agents whose CLI
/// has no config-dir override (nothing is managed for them).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentHomeInfoParams {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentHomeInfoResult {
    /// Absolute path of the agent's home, e.g.
    /// `/Users/example/.ottam/agents/claude/home`. Reported even before the
    /// directory exists — it is created on the agent's first start.
    pub path: String,
    /// Rough recursive size of the home in bytes; 0 when it does not exist
    /// yet. Computed on request, so clients cache it and refresh on revisit.
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentAuthStateParams {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthStateResult {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    pub adapter: AgentAdapterStatus,
}

/// How a sign-in reaches the user. One flow, two mechanisms: either the adapter
/// opens the browser itself, or Ottam runs a command that does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentAuthDelivery {
    /// The adapter handles the whole exchange and answers when it is finished.
    AdapterBrowser,
    /// Ottam runs the method's command in a terminal it owns. The terminal
    /// stays hidden unless something goes wrong.
    ClientTerminal,
}

/// Where a sign-in has got to. Only `Succeeded`, `Failed`, and `Cancelled` are
/// terminal; a session in any other phase is still live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentAuthPhase {
    /// Accepted, nothing has happened yet.
    Starting,
    /// Handed off — the browser is open, or the command is running.
    AwaitingUser,
    /// The flow wants something typed. Only `clientTerminal` reaches this, and
    /// it always arrives with `revealTerminal` set.
    NeedsInput,
    Succeeded,
    Failed,
    Cancelled,
}

/// The terminal running a `clientTerminal` sign-in, addressed the same way any
/// other terminal is, so a revealing client can attach and replay it in place
/// rather than starting the login over.
///
/// No workspace: a login terminal need not belong to one, and `terminal.replay`
/// addresses it by terminal id alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthTerminal {
    #[serde(rename = "sessionID")]
    pub session_id: TerminalSessionID,
    #[serde(rename = "terminalID")]
    pub terminal_id: TerminalID,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSignInParams {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    /// Whose root the login command runs in. Omit it — as Settings does, where
    /// there may be no workspace at all — and the login runs in the agent's own
    /// isolated home instead. The login writes to the CLI's own config
    /// directory either way, so this only picks a working directory.
    #[serde(rename = "workspaceID", skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceID>,
    /// Overrides the method core would have picked. Omit it in the ordinary
    /// case — the client should not have to know one agent's method ids.
    #[serde(rename = "methodID", skip_serializing_if = "Option::is_none")]
    pub method_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentSignInResult {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    #[serde(rename = "authSessionID")]
    pub auth_session_id: AgentAuthSessionID,
    pub delivery: AgentAuthDelivery,
    pub phase: AgentAuthPhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSignInCancelParams {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    #[serde(rename = "authSessionID")]
    pub auth_session_id: AgentAuthSessionID,
}

/// The user declares a terminal login complete — pressing Done on a login
/// whose program never exits on its own (a full-screen picker). Core closes
/// the login terminal, restarts the adapter, and reports the outcome through
/// `agent.signInEvent`, exactly as if the command had exited cleanly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSignInFinishParams {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    #[serde(rename = "authSessionID")]
    pub auth_session_id: AgentAuthSessionID,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentSignInEventParams {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    #[serde(rename = "authSessionID")]
    pub auth_session_id: AgentAuthSessionID,
    pub delivery: AgentAuthDelivery,
    pub phase: AgentAuthPhase,
    /// The login needs to be seen. Latches on: once core has asked for the
    /// terminal to be shown, later events keep asking, so a client that missed
    /// one event still ends up showing it.
    #[serde(default)]
    pub reveal_terminal: bool,
    /// Present for `clientTerminal` from the first event, whether or not
    /// `revealTerminal` is set yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<AgentAuthTerminal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentStopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentEmptyResult {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AgentUpdate {
    SessionSignal {
        signal: crate::ConversationSessionSignal,
    },
    RichMessageChunk {
        text: String,
        #[serde(rename = "messageID")]
        message_id: Option<String>,
        reasoning: bool,
        metadata: crate::TranscriptAdapterMetadata,
    },
    UserMessageChunk {
        text: String,
    },
    AgentMessageChunk {
        text: String,
        #[serde(default, rename = "messageID")]
        message_id: Option<String>,
    },
    AgentThoughtChunk {
        text: String,
        #[serde(default, rename = "messageID")]
        message_id: Option<String>,
    },
    AgentImageChunk {
        content: Value,
        #[serde(default, rename = "messageID")]
        message_id: Option<String>,
    },
    ToolCall {
        payload: Value,
    },
    ToolCallUpdate {
        payload: Value,
    },
    Plan {
        payload: Value,
    },
    StreamGap {
        dropped: u64,
    },
    /// AIR `async_task_spawned`. Only the task id is guaranteed; Codex omits
    /// `description`, and Claude can spawn before it knows the tool call.
    AsyncTaskSpawned {
        #[serde(rename = "asyncTaskID")]
        async_task_id: String,
        #[serde(default, rename = "providerToolCallID")]
        provider_tool_call_id: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default, rename = "taskType")]
        task_type: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default, rename = "outputFilePath")]
        output_file_path: Option<String>,
        #[serde(default, rename = "showInTranscript")]
        show_in_transcript: Option<bool>,
        #[serde(default, rename = "canStop")]
        can_stop: Option<bool>,
    },
    /// AIR `async_task_progress`: changed fields only.
    AsyncTaskProgress {
        #[serde(rename = "asyncTaskID")]
        async_task_id: String,
        #[serde(default, rename = "providerToolCallID")]
        provider_tool_call_id: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        summary: Option<String>,
        #[serde(default, rename = "lastToolName")]
        last_tool_name: Option<String>,
        #[serde(default)]
        usage: Option<AsyncTaskUsage>,
        #[serde(default, rename = "outputFilePath")]
        output_file_path: Option<String>,
    },
    /// AIR `async_task_state_update`.
    AsyncTaskStateUpdate {
        #[serde(rename = "asyncTaskID")]
        async_task_id: String,
        state: AsyncTaskState,
        #[serde(default, rename = "providerToolCallID")]
        provider_tool_call_id: Option<String>,
        #[serde(default)]
        summary: Option<String>,
        #[serde(default, rename = "outputFilePath")]
        output_file_path: Option<String>,
    },
    /// AIR native subagent session announced on its immediate parent's
    /// session (the pins' draft of ACP's subagent RFD).
    SubagentSpawned {
        #[serde(rename = "subagentSessionID")]
        subagent_session_id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        task: Option<String>,
        /// The run's prompt: the first task, or the parent's message that
        /// resumed the subagent as a new generation.
        #[serde(default)]
        prompt: Option<String>,
    },
    /// AIR native subagent's terminal state, sent on its parent's session.
    SubagentStateUpdate {
        #[serde(rename = "subagentSessionID")]
        subagent_session_id: String,
        state: crate::SubagentThreadState,
    },
    Other {
        #[serde(rename = "updateKind")]
        update_kind: String,
        payload: Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AsyncTaskState {
    Running,
    Paused,
    Completed,
    Failed,
    Stopped,
    /// Core-only: the task's adapter or session died, or core restarted,
    /// before it reported an end. Never accepted from an adapter.
    Lost,
}

impl AsyncTaskState {
    /// Running or paused: the task can still report an end.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Running | Self::Paused)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AsyncTaskUsage {
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub tool_uses: Option<u64>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentPermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentPermissionOption {
    #[serde(rename = "optionID")]
    pub option_id: String,
    pub name: String,
    pub kind: AgentPermissionOptionKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentRequestPermissionParams {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    #[serde(rename = "sessionID")]
    pub session_id: AgentSessionID,
    #[serde(rename = "workspaceID", skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceID>,
    #[serde(rename = "projectID", skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectID>,
    #[serde(rename = "conversationID", skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<ConversationID>,
    #[serde(rename = "runID", skip_serializing_if = "Option::is_none")]
    pub run_id: Option<ConversationRunID>,
    /// The subagent thread whose session asked, when a subagent asked.
    #[serde(
        default,
        rename = "subagentThreadID",
        skip_serializing_if = "Option::is_none"
    )]
    pub subagent_thread_id: Option<String>,
    /// That subagent's name, for the prompt's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_name: Option<String>,
    /// The plan's markdown when the request asks to approve a plan and
    /// leave plan mode. Such a request waits for the user with no timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub tool_call: Value,
    pub options: Vec<AgentPermissionOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum AgentPermissionOutcome {
    Cancelled,
    Selected {
        #[serde(rename = "optionID")]
        option_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentRequestPermissionResult {
    #[serde(flatten)]
    pub outcome: AgentPermissionOutcome,
}

/// An agent's form, as questions. Core normalizes every adapter's
/// elicitation schema into this shape, so clients never read adapter keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentAskQuestionParams {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    #[serde(rename = "sessionID")]
    pub session_id: AgentSessionID,
    #[serde(rename = "workspaceID", skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceID>,
    #[serde(rename = "projectID", skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectID>,
    #[serde(rename = "conversationID", skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<ConversationID>,
    #[serde(rename = "runID", skip_serializing_if = "Option::is_none")]
    pub run_id: Option<ConversationRunID>,
    /// The subagent thread whose session asked, when a subagent asked.
    #[serde(
        default,
        rename = "subagentThreadID",
        skip_serializing_if = "Option::is_none"
    )]
    pub subagent_thread_id: Option<String>,
    /// That subagent's name, for the panel's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_name: Option<String>,
    /// The tool call that asked; absent for a form no tool call owns, such
    /// as an MCP server's.
    #[serde(
        default,
        rename = "providerToolCallID",
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_tool_call_id: Option<String>,
    /// The agent's message for the whole form.
    pub title: String,
    pub questions: Vec<AgentQuestion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentQuestion {
    /// Core-assigned within the request; answers name it.
    pub id: String,
    /// A short label, such as Claude's question header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub prompt: String,
    pub required: bool,
    pub kind: AgentQuestionKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentQuestionKind {
    /// Pick one. `allowsOther` adds a free-text answer.
    #[serde(rename_all = "camelCase")]
    SingleSelect {
        options: Vec<AgentQuestionOption>,
        allows_other: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<String>,
    },
    /// Pick any. `allowsOther` adds a free-text answer.
    #[serde(rename_all = "camelCase")]
    MultiSelect {
        options: Vec<AgentQuestionOption>,
        allows_other: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_selected: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_selected: Option<u32>,
        #[serde(default)]
        default: Vec<String>,
    },
    /// Free text. A secure question's answer is never stored or logged.
    #[serde(rename_all = "camelCase")]
    Text {
        secure: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Number {
        integer: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minimum: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<f64>,
    },
    #[serde(rename_all = "camelCase")]
    Boolean {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<bool>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentQuestionOption {
    /// What the answer carries back.
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One question's answer. Selections carry option values; text, number, and
/// boolean answers carry their single value as text (`"true"`, `"3.5"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentQuestionAnswer {
    #[serde(rename = "questionID")]
    pub question_id: String,
    #[serde(default)]
    pub values: Vec<String>,
    /// The free-text answer of a select that allows one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum AgentQuestionOutcome {
    Submitted { answers: Vec<AgentQuestionAnswer> },
    /// The user declined to answer.
    Skipped,
    /// Nobody answered: the request was withdrawn or the client went away.
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentAskQuestionResult {
    #[serde(flatten)]
    pub outcome: AgentQuestionOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CancelRequestParams {
    pub id: crate::RequestID,
}
