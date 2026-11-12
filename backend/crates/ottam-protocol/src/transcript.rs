//! Normalized local transcript. Provider envelopes never cross this interface.
use crate::{ConversationID, ConversationRunID, WorkspaceID};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const METHOD_CONVERSATION_LIST_TRANSCRIPT: &str = "conversation.listTranscript";
pub const METHOD_CONVERSATION_READ_TRANSCRIPT_BODY: &str = "conversation.readTranscriptBody";
pub const METHOD_CONVERSATION_READ_TRANSCRIPT_PAYLOAD: &str = "conversation.readTranscriptPayload";
pub const NOTIFICATION_CONVERSATION_TRANSCRIPT_DELTA: &str = "conversation.transcriptDelta";

/// Provider identities are meaningful only within their originating session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptAdapterMetadata {
    #[serde(default)]
    pub session_id: Option<String>,
    pub phase: Option<String>,
    pub parent_message_id: Option<String>,
    pub parent_tool_call_id: Option<String>,
    /// The native subagent thread core routed this update to, by the session
    /// it arrived on. Core sets it; adapters never send it, and transcript
    /// items carry it as their own `subagentThreadID` instead.
    #[serde(
        default,
        rename = "subagentThreadID",
        skip_serializing_if = "Option::is_none"
    )]
    pub subagent_thread_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TranscriptItemKind {
    Message,
    AssistantText,
    Reasoning,
    Image,
    Tool,
    Plan,
    Permission,
    Gap,
    RunBoundary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TranscriptCompletion {
    Open,
    Complete,
    Partial,
    Suppressed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptPayload {
    pub role: String,
    pub availability: String,
    pub reason: Option<String>,
    #[serde(rename = "payloadID")]
    pub payload_id: Option<String>,
    pub byte_count: Option<u64>,
    pub upstream_truncated: Option<bool>,
    pub locally_truncated: bool,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub parts: Vec<TranscriptPayloadPart>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptPayloadPart {
    pub name: String,
    pub format: String,
    pub byte_count: u64,
    pub path: Option<String>,
    pub digest: Option<String>,
    #[serde(default)]
    pub chunk_digests: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationReadTranscriptPayloadParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(rename = "payloadID")]
    pub payload_id: String,
    pub revision: u64,
    pub part: u32,
    pub offset: u64,
    pub length: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationReadTranscriptPayloadResult {
    #[serde(rename = "payloadID")]
    pub payload_id: String,
    pub revision: u64,
    pub part: u32,
    pub offset: u64,
    /// Bytes are base64 encoded so text pages need not split on UTF-8 boundaries.
    pub data: String,
    pub next_offset: Option<u64>,
    pub readable_length: u64,
    pub availability: String,
    pub reason: Option<String>,
    pub unsaved: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptTool {
    #[serde(rename = "providerToolCallID")]
    pub provider_tool_call_id: Option<String>,
    pub name: Option<String>,
    pub kind: Option<String>,
    pub title: Option<String>,
    pub status: Option<String>,
    pub arguments: Value,
    pub locations: Vec<TranscriptLocation>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub exit_code: Option<i64>,
    pub partial_metadata: bool,
    pub payloads: Vec<TranscriptPayload>,
    /// The AIR async task this call handed its work to. Tool rows stored
    /// before background tasks existed have no key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_task: Option<TranscriptBackgroundTask>,
    /// The subagent thread this control call opened. Its rows name it by
    /// `subagentThreadID`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_thread: Option<TranscriptSubagentThread>,
    /// The parent's message to a subagent that was still running, sent by
    /// this call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_follow_up: Option<TranscriptSubagentFollowUp>,
    /// What the user answered when this call asked a question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<TranscriptQuestion>,
}

/// A question an agent asked through a tool call, and what became of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptQuestion {
    #[serde(rename = "requestID")]
    pub request_id: String,
    pub state: TranscriptQuestionState,
    /// One entry per question, in order. `values` is empty until answered.
    pub answers: Vec<TranscriptQuestionAnswer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TranscriptQuestionState {
    /// Waiting for the user. Core settles it as `notAnswered` when the run
    /// ends or an earlier core stopped.
    Pending,
    Answered,
    Skipped,
    NotAnswered,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptQuestionAnswer {
    pub prompt: String,
    /// Option labels and free text, as the user gave them. Empty for a
    /// secure answer, which is never stored.
    pub values: Vec<String>,
    #[serde(default)]
    pub secure: bool,
}

/// A parent's message to one of its running subagents. A message that
/// resumes a finished subagent is the new run's `prompt` instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSubagentFollowUp {
    #[serde(rename = "threadID")]
    pub thread_id: String,
    pub message: String,
}

/// A subagent's work inside its parent conversation. The thread's lifetime is
/// not its control call's: an asynchronous launch completes the call at once
/// and the thread runs until its run ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSubagentThread {
    /// Core-assigned, stable within the conversation.
    pub id: String,
    /// The thread whose subagent opened this one; absent at the top level.
    #[serde(
        default,
        rename = "parentThreadID",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_thread_id: Option<String>,
    /// The agent type the parent asked for, such as `Explore`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The parent's short description of the work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// What this run of the subagent was asked: its first prompt, or the
    /// parent's message that resumed it. A thread with several runs has an
    /// anchor per run, each with its own state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub state: SubagentThreadState,
    pub source: SubagentThreadSource,
    /// The control call returned at launch; the thread ends with its run.
    #[serde(default)]
    pub asynchronous: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SubagentThreadState {
    Running,
    Completed,
    Failed,
    Cancelled,
    /// Core-only: the thread was running when its run ended without saying
    /// how it finished, or when an earlier core stopped. A native subagent,
    /// which may outlive its run, is disconnected once core loses it: its
    /// adapter or session restarted, or an earlier core stopped.
    Disconnected,
}

impl SubagentThreadState {
    pub fn is_live(self) -> bool {
        self == Self::Running
    }
}

/// Where a thread's membership comes from: Claude's `parentToolUseId` stamp
/// on a child's first report, or the native child session an update arrived
/// on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SubagentThreadSource {
    LegacyParentId,
    /// An AIR native subagent session, announced by `subagent_spawned`.
    NativeSession,
}

/// `id` is absent when the adapter marked the call backgrounded before any
/// lifecycle update named the task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptBackgroundTask {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub state: crate::AsyncTaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_file_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptLocation {
    pub path: String,
    pub line: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptPlanEntry {
    pub content: String,
    pub priority: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptPermission {
    #[serde(rename = "requestID")]
    pub request_id: String,
    #[serde(rename = "providerToolCallID")]
    pub provider_tool_call_id: Option<String>,
    pub title: String,
    pub outcome_label: String,
    pub allowed: bool,
    /// The plan's markdown when the request approved or declined a plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptGap {
    pub end_position: u64,
    pub dropped_updates: u64,
    pub reason: String,
}

/// Body is a bounded preview; readTranscriptBody reads the owning SQLite body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationTranscriptItem {
    #[serde(rename = "itemID")]
    pub item_id: String,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(rename = "runID")]
    pub run_id: Option<ConversationRunID>,
    pub run_status: Option<crate::ConversationRunStatus>,
    pub position: u64,
    pub revision: u64,
    /// Highest revision acknowledged by the checkpoint writer, if known.
    #[serde(default)]
    pub committed_revision: Option<u64>,
    pub kind: TranscriptItemKind,
    pub completion: TranscriptCompletion,
    pub created_at: String,
    pub updated_at: String,
    #[serde(rename = "providerMessageID")]
    pub provider_message_id: Option<String>,
    #[serde(default)]
    pub adapter_metadata: TranscriptAdapterMetadata,
    /// The subagent thread this row belongs to. Clients group by this and
    /// never join on `adapterMetadata.parentToolCallId`.
    #[serde(
        default,
        rename = "subagentThreadID",
        skip_serializing_if = "Option::is_none"
    )]
    pub subagent_thread_id: Option<String>,
    pub body_preview: String,
    pub body_byte_count: u64,
    /// Only in a live delta whose `bodyAppends` extend this item's body at the
    /// same revision: `bodyPreview` is empty and the client keeps the preview
    /// and body it already holds. Snapshots and revision changes never omit it.
    #[serde(default)]
    pub preview_omitted: bool,
    pub message: Option<crate::ConversationMessage>,
    pub tool: Option<TranscriptTool>,
    #[serde(default)]
    pub payloads: Vec<TranscriptPayload>,
    pub plan: Option<Vec<TranscriptPlanEntry>>,
    pub permission: Option<TranscriptPermission>,
    pub gap: Option<TranscriptGap>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationListTranscriptParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    /// One subagent thread's page: its own rows and its nested threads',
    /// with the thread's anchor and theirs. Cursors page within the thread.
    #[serde(
        default,
        rename = "subagentThreadID",
        skip_serializing_if = "Option::is_none"
    )]
    pub subagent_thread_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptCaptureStatus {
    #[serde(rename = "runID")]
    pub run_id: ConversationRunID,
    /// Missing means historical capture is unknown.
    pub state: Option<String>,
    pub problem: Option<String>,
    #[serde(default)]
    pub gaps: Vec<TranscriptCoverage>,
}

/// How many of a run's background tasks are still live in this core instance.
/// Live-only: after a core restart no task is live, and a stored running task
/// reads as `lost`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRunBackgroundTasks {
    #[serde(rename = "runID")]
    pub run_id: ConversationRunID,
    pub live: u32,
}

/// How many of a run's subagent threads are running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRunSubagentThreads {
    #[serde(rename = "runID")]
    pub run_id: ConversationRunID,
    pub live: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptCoverage {
    pub start_position: u64,
    pub end_position: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationListTranscriptResult {
    #[serde(default)]
    pub session_states: Vec<crate::ConversationSessionState>,
    #[serde(rename = "coreInstanceID")]
    pub core_instance_id: String,
    pub sequence: u64,
    pub items: Vec<ConversationTranscriptItem>,
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub capture: Vec<TranscriptCaptureStatus>,
    pub coverage: Option<TranscriptCoverage>,
    /// Runs with background tasks seen in this core instance; a run that is
    /// absent has none live.
    #[serde(default)]
    pub background_tasks: Vec<ConversationRunBackgroundTasks>,
    /// Runs this core instance holds with running subagent threads; a run
    /// that is absent has none running.
    #[serde(default)]
    pub subagent_threads: Vec<ConversationRunSubagentThreads>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationReadTranscriptBodyParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(rename = "itemID")]
    pub item_id: String,
    pub revision: u64,
    pub offset: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationReadTranscriptBodyResult {
    #[serde(rename = "itemID")]
    pub item_id: String,
    pub revision: u64,
    pub text: String,
    pub next_offset: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptBodyAppend {
    #[serde(rename = "itemID")]
    pub item_id: String,
    pub revision: u64,
    /// UTF-8 byte offset in the immutable item's append-only body.
    pub offset: u64,
    /// At most 64 KiB, ending on a UTF-8 boundary.
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationTranscriptDeltaParams {
    #[serde(default)]
    pub session_states: Vec<crate::ConversationSessionState>,
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(rename = "coreInstanceID")]
    pub core_instance_id: String,
    pub sequence: u64,
    /// Set when core merged consecutive streaming-text deltas into this one:
    /// the first sequence it covers. Absent means only `sequence`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_start: Option<u64>,
    pub items: Vec<ConversationTranscriptItem>,
    pub body_appends: Vec<TranscriptBodyAppend>,
    #[serde(default)]
    pub capture: Vec<TranscriptCaptureStatus>,
    /// The client must recover its loaded coverage; missing items are not deletions.
    #[serde(default)]
    pub recovery_required: bool,
    /// Runs whose live background-task count changed in this delta, including
    /// to zero.
    #[serde(default)]
    pub background_tasks: Vec<ConversationRunBackgroundTasks>,
    /// Runs whose running subagent-thread count this delta may have moved,
    /// including to zero.
    #[serde(default)]
    pub subagent_threads: Vec<ConversationRunSubagentThreads>,
}
