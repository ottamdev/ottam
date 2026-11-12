use crate::{ConversationID, McpServerID, WorkspaceID};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const METHOD_MCP_LIST: &str = "mcp.list";
pub const METHOD_MCP_ADD: &str = "mcp.add";
pub const METHOD_MCP_UPDATE: &str = "mcp.update";
pub const METHOD_MCP_REMOVE: &str = "mcp.remove";
pub const METHOD_MCP_SET_WORKSPACE_ENABLED: &str = "mcp.setWorkspaceEnabled";
pub const METHOD_MCP_SERVER_STATE_CHANGED: &str = "mcp.serverStateChanged";
pub const METHOD_MCP_IMPORT_CANDIDATES: &str = "mcp.importCandidates";
pub const METHOD_MCP_SIGN_IN: &str = "mcp.signIn";
pub const METHOD_MCP_CANCEL_SIGN_IN: &str = "mcp.cancelSignIn";
pub const METHOD_MCP_SIGN_OUT: &str = "mcp.signOut";
pub const METHOD_MCP_SIGN_IN_FINISHED: &str = "mcp.signInFinished";
pub const METHOD_MCP_SIGN_IN_NEEDED: &str = "mcp.signInNeeded";
pub const METHOD_MCP_SERVERS_CHANGED: &str = "mcp.serversChanged";

/// One environment variable or HTTP header of a server. A secret's value is
/// kept in the Keychain and never returned: listed secrets have no `value`,
/// and an update that leaves a secret's `value` out keeps the stored one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpConfigValue {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default)]
    pub secret: bool,
}

/// How core reaches the server. Agents always reach core's proxy over
/// Streamable HTTP, whichever this is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum McpServerTransport {
    /// A local process core starts, one per workspace that uses it.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: Vec<McpConfigValue>,
    },
    /// A remote server core connects to over Streamable HTTP.
    Http {
        url: String,
        #[serde(default)]
        headers: Vec<McpConfigValue>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerConfig {
    /// The name agents see the server and its tools under, and the proxy
    /// path it is served at. Letters, digits, `-` and `_`.
    pub name: String,
    pub transport: McpServerTransport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum McpServerStatus {
    /// No session has needed it yet, or it stopped while idle.
    NotStarted,
    Starting,
    Running,
    /// It failed to start, crashed, or its connection failed; `error` says
    /// how. The next call tries again.
    Failed,
    /// It needs a sign-in before core can reach it.
    SignedOut,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerState {
    pub status: McpServerStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerSummary {
    #[serde(rename = "serverID")]
    pub server_id: McpServerID,
    pub config: McpServerConfig,
    /// Workspaces whose new sessions are not given this server.
    #[serde(rename = "disabledWorkspaceIDs")]
    pub disabled_workspace_ids: Vec<WorkspaceID>,
    pub state: McpServerState,
    /// A remote server core holds OAuth tokens for. They may still have
    /// expired; `state` turns signed out once the server says so.
    #[serde(default)]
    pub signed_in: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpListResult {
    pub servers: Vec<McpServerSummary>,
    /// Where sign-ins return to. A server that cannot register Ottam
    /// itself needs it registered by hand, for a client ID to sign in with.
    #[serde(rename = "redirectURI")]
    pub redirect_uri: String,
}

/// Adds a server for every workspace. Sessions opened from now on get it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpAddParams {
    pub config: McpServerConfig,
}

/// Replaces a server's config. Sessions that already have the server keep
/// their entry and reach the new config behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpUpdateParams {
    #[serde(rename = "serverID")]
    pub server_id: McpServerID,
    pub config: McpServerConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpRemoveParams {
    #[serde(rename = "serverID")]
    pub server_id: McpServerID,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpRemoveResult {}

/// Turns a server on or off for one workspace's new sessions. Sessions
/// already open keep it until they end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSetWorkspaceEnabledParams {
    #[serde(rename = "serverID")]
    pub server_id: McpServerID,
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerResult {
    pub server: McpServerSummary,
}

/// A server's runtime state changed: it started, stopped, failed, or was
/// signed in or out. Config changes are answered by the request that made
/// them; clients re-list for anything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStateChangedParams {
    #[serde(rename = "serverID")]
    pub server_id: McpServerID,
    pub state: McpServerState,
}

/// Starts signing in to a remote server, replacing any sign-in still
/// waiting. Core registers itself with the server's authorization server,
/// or uses `clientID`, and listens for the browser's return at
/// `redirectURI`. The client opens `authorizationURL` in the browser; the
/// outcome comes as `mcp.signInFinished`. Fails with `mcpClientIDRequired`
/// when the server needs a client ID and none was given or saved, and with
/// `mcpRedirectPortInUse` when another app holds the redirect port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSignInParams {
    #[serde(rename = "serverID")]
    pub server_id: McpServerID,
    #[serde(rename = "clientID", default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpSignInResult {
    #[serde(rename = "authorizationURL")]
    pub authorization_url: String,
    #[serde(rename = "redirectURI")]
    pub redirect_uri: String,
}

/// Stops waiting for a server's sign-in; it finishes as cancelled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCancelSignInParams {
    #[serde(rename = "serverID")]
    pub server_id: McpServerID,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpCancelSignInResult {}

/// Forgets a remote server's tokens and stops its connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSignOutParams {
    #[serde(rename = "serverID")]
    pub server_id: McpServerID,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum McpSignInOutcome {
    SignedIn,
    Cancelled,
    /// `error` says why: denied in the browser, timed out, or refused.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpSignInFinishedParams {
    #[serde(rename = "serverID")]
    pub server_id: McpServerID,
    pub outcome: McpSignInOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A chat's agent called a server that needs a sign-in. The agent was told
/// to ask the user to sign in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpSignInNeededParams {
    #[serde(rename = "serverID")]
    pub server_id: McpServerID,
    pub server_name: String,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
}

/// A server was added, changed, or removed; clients re-list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServersChangedParams {}

/// Where an import reads server definitions from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum McpImportSource {
    /// A detected Claude Code or Codex setup's own servers: Claude Code's
    /// user scope in `.claude.json`, Codex's `config.toml`.
    Setup {
        #[serde(rename = "harnessID")]
        harness_id: String,
    },
    /// A repository's `.mcp.json`: the file, or the directory holding it.
    File { path: String },
}

/// Reads the servers a source defines without saving or starting any, so
/// each command can be approved first. Approved servers are added with
/// `mcp.add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpImportCandidatesParams {
    pub source: McpImportSource,
}

/// A server as it would be added. Values come as the source wrote them;
/// header values and environment values whose names look like credentials
/// are marked secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpImportCandidate {
    pub config: McpServerConfig,
    /// The name the source used, when it had to change to be valid here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_name: Option<String>,
    /// A configured server already using this name.
    #[serde(
        rename = "existingServerID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub existing_server_id: Option<McpServerID>,
    /// What does not carry over as written, such as a `${VAR}` reference
    /// core does not expand.
    #[serde(default)]
    pub notes: Vec<String>,
    /// Turned off where it was read, so it is not chosen by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled_in_source: bool,
}

/// A server the source defines that Ottam cannot run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpImportSkipped {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpImportCandidatesResult {
    /// The file read.
    pub path: String,
    pub candidates: Vec<McpImportCandidate>,
    pub skipped: Vec<McpImportSkipped>,
}
