use crate::ids::{AgentID, AgentInstallID};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const METHOD_AGENT_INSTALL: &str = "agent.install";
pub const METHOD_AGENT_INSTALL_CANCEL: &str = "agent.installCancel";
pub const METHOD_AGENT_INSTALL_EVENT: &str = "agent.installEvent";
pub const METHOD_AGENT_UNINSTALL: &str = "agent.uninstall";

/// Where an adapter install has got to. Only `Ready`, `Failed`, and
/// `Cancelled` are terminal. Distinct from `RuntimeInstallPhase`: an adapter
/// install has no byte counts (npm owns the fetch) and adds a `verifying`
/// phase — the first ACP handshake — so "installed" always means the adapter
/// actually answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentInstallPhase {
    /// Accepted, nothing has run yet.
    Starting,
    /// npm is resolving and unpacking the pinned package.
    Installing,
    /// The package is on disk; a throwaway adapter is completing its first
    /// handshake. A failure here rolls the install back.
    Verifying,
    Ready,
    Failed,
    Cancelled,
}

/// Install the agent's pinned ACP adapter into Ottam's managed store. The
/// version is pinned in the app binary, so the client names only the agent.
/// The call returns as soon as the install is accepted; progress arrives on
/// `agent.installEvent`. Calling while an install for the same agent is in
/// flight joins it rather than starting a second one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentInstallParams {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentInstallResult {
    #[serde(rename = "installID")]
    pub install_id: AgentInstallID,
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    /// The npm package being installed, split like `AgentAdapterPackage`.
    pub name: String,
    pub version: String,
    pub phase: AgentInstallPhase,
}

/// Stop an install in flight. A stale or already-finished id is a no-op, so a
/// client that races the terminal event does not get an error for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentInstallCancelParams {
    #[serde(rename = "installID")]
    pub install_id: AgentInstallID,
}

/// Remove the agent's installed adapter from Ottam's managed store — every
/// version — and optionally the agent's isolated home (its sessions and
/// sign-in). The running adapter is shut down first. Refused while an install
/// for the agent is in flight. Afterwards the agent reports `notInstalled`
/// and its engine is unusable until `agent.install` runs again; a reinstall is
/// simply `agent.install` on an installed agent, which replaces the tree and
/// re-verifies the handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentUninstallParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_key: Option<String>,
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    /// Also delete the agent's Ottam-managed home. Absent or `false`
    /// keeps it, so a plain remove-and-reinstall preserves sessions and
    /// sign-in. Mirrors the `Option<bool>` shape of `refreshRuntime`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove_home: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentInstallEventParams {
    #[serde(rename = "installID")]
    pub install_id: AgentInstallID,
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    pub name: String,
    pub version: String,
    pub phase: AgentInstallPhase,
    /// Present only on `failed`: what went wrong, one line, user-presentable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
