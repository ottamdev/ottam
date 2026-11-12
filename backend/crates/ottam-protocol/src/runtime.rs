use crate::ids::RuntimeInstallID;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const METHOD_RUNTIME_INSTALL: &str = "runtime.install";
pub const METHOD_RUNTIME_INSTALL_CANCEL: &str = "runtime.installCancel";
pub const METHOD_RUNTIME_INSTALL_EVENT: &str = "runtime.installEvent";

/// Where an install has got to. Only `Ready`, `Failed`, and `Cancelled` are
/// terminal; a session in any other phase is still live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeInstallPhase {
    /// Accepted, nothing has been fetched yet.
    Starting,
    /// Bytes are arriving. `bytesReceived` advances; `bytesTotal` is present
    /// unless the server declined to give a content length.
    Downloading,
    /// Checking the payload against the pinned digest.
    Verifying,
    Extracting,
    Ready,
    Failed,
    Cancelled,
}

/// Install the runtime Ottam manages on the user's behalf — today the
/// pinned Node the ACP adapters run on.
///
/// No parameters: the runtime is app-scoped rather than per-agent, and the
/// version is pinned in the app binary, so there is nothing for the client to
/// choose. The call returns as soon as the install is accepted; progress
/// arrives on `runtime.installEvent`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeInstallParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInstallResult {
    #[serde(rename = "installID")]
    pub install_id: RuntimeInstallID,
    pub version: String,
    pub phase: RuntimeInstallPhase,
}

/// Stop an install in flight. A stale or already-finished id is a no-op, so a
/// client that races the terminal event does not get an error for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeInstallCancelParams {
    #[serde(rename = "installID")]
    pub install_id: RuntimeInstallID,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInstallEventParams {
    #[serde(rename = "installID")]
    pub install_id: RuntimeInstallID,
    pub version: String,
    pub phase: RuntimeInstallPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_received: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeEmptyResult {}
