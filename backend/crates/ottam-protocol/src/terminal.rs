use crate::ids::{TerminalID, TerminalSessionID, WorkspaceID};
use crate::{WorkingRootSelection, WorkingRootSummary};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const METHOD_TERMINAL_OPEN: &str = "terminal.open";
pub const METHOD_TERMINAL_WRITE: &str = "terminal.write";
pub const METHOD_TERMINAL_RESIZE: &str = "terminal.resize";
pub const METHOD_TERMINAL_CLOSE: &str = "terminal.close";
pub const METHOD_TERMINAL_LIST: &str = "terminal.list";
pub const METHOD_TERMINAL_DELETE: &str = "terminal.delete";
pub const METHOD_TERMINAL_UPDATE_METADATA: &str = "terminal.updateMetadata";
pub const METHOD_TERMINAL_REPLAY: &str = "terminal.replay";
pub const METHOD_TERMINAL_OUTPUT_DELTA: &str = "terminal.outputDelta";
pub const METHOD_TERMINAL_EXITED: &str = "terminal.exited";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOpenParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    pub working_root: WorkingRootSelection,
    pub rows: u16,
    pub cols: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionSummary {
    #[serde(rename = "terminalID")]
    pub terminal_id: TerminalID,
    #[serde(rename = "sessionID")]
    pub session_id: TerminalSessionID,
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    pub working_root: WorkingRootSummary,
    pub cwd: String,
    pub shell: String,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalWriteParams {
    #[serde(rename = "sessionID")]
    pub session_id: TerminalSessionID,
    pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalResizeParams {
    #[serde(rename = "sessionID")]
    pub session_id: TerminalSessionID,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCloseParams {
    #[serde(rename = "sessionID")]
    pub session_id: TerminalSessionID,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalEmptyResult {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordSummary {
    #[serde(rename = "terminalID")]
    pub terminal_id: TerminalID,
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    pub working_root: WorkingRootSummary,
    #[serde(rename = "runtimeSessionID", skip_serializing_if = "Option::is_none")]
    pub runtime_session_id: Option<TerminalSessionID>,
    pub title: String,
    pub cwd: String,
    pub shell: String,
    pub rows: u16,
    pub cols: u16,
    pub status: String,
    pub last_sequence: u64,
    pub last_byte_offset: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalListParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalListResult {
    pub terminals: Vec<TerminalRecordSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalDeleteParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "terminalID")]
    pub terminal_id: TerminalID,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalUpdateMetadataParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "terminalID")]
    pub terminal_id: TerminalID,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalReplayParams {
    /// A scoping assertion, still enforced when present: the terminal must
    /// belong to this workspace. Omit it for a terminal that belongs to none,
    /// such as a sign-in login terminal.
    #[serde(rename = "workspaceID", skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceID>,
    #[serde(rename = "terminalID")]
    pub terminal_id: TerminalID,
    #[serde(default)]
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalReplayChunk {
    pub sequence_start: u64,
    pub sequence_end: u64,
    pub byte_start: u64,
    pub byte_end: u64,
    pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalReplayResult {
    #[serde(rename = "terminalID")]
    pub terminal_id: TerminalID,
    pub chunks: Vec<TerminalReplayChunk>,
    pub truncated: bool,
    #[serde(default)]
    pub response_truncated: bool,
    #[serde(default)]
    pub retention_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_byte_start: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_byte_end: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_sequence_start: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_sequence_end: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TerminalOutputDeliveryState {
    Normal,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOutputDeltaParams {
    #[serde(rename = "sessionID")]
    pub session_id: TerminalSessionID,
    /// Absent for a terminal that belongs to no workspace (a sign-in login).
    #[serde(rename = "workspaceID", skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceID>,
    #[serde(rename = "terminalID", skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<TerminalID>,
    pub sequence: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_start: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_end: Option<u64>,
    pub data_base64: String,
    pub cap_reached: bool,
    #[serde(default)]
    pub replay: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_state: Option<TerminalOutputDeliveryState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped_chunks: Option<u64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub replay_suggested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalExitedParams {
    #[serde(rename = "sessionID")]
    pub session_id: TerminalSessionID,
    /// Absent for a terminal that belongs to no workspace (a sign-in login).
    #[serde(rename = "workspaceID", skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceID>,
    #[serde(rename = "terminalID")]
    pub terminal_id: TerminalID,
    pub exit_status: Option<i32>,
}

fn is_false(value: &bool) -> bool {
    !*value
}
