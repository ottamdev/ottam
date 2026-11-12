use crate::agent::AgentStopReason;
use crate::ids::{
    AgentSessionID, ConversationAttachmentID, ConversationEventID, ConversationID,
    ConversationMessageID, ConversationRunID, ConversationRunInputID, EngineID, ProjectID,
    ProjectSourceID, WorkspaceID,
};
use crate::WorkingRootSelection;
use crate::WorkingRootSummary;
use crate::WorkspaceSummary;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const METHOD_CONVERSATION_CREATE: &str = "conversation.create";
pub const METHOD_CONVERSATION_DELETE: &str = "conversation.delete";
pub const METHOD_CONVERSATION_LIST: &str = "conversation.list";
pub const METHOD_CONVERSATION_GET: &str = "conversation.get";
pub const METHOD_CONVERSATION_LIST_MESSAGES: &str = "conversation.listMessages";
pub const METHOD_CONVERSATION_LIST_RUNS: &str = "conversation.listRuns";
pub const METHOD_CONVERSATION_LIST_RUN_INPUTS: &str = "conversation.listRunInputs";
pub const METHOD_CONVERSATION_LIST_AGENT_SESSIONS: &str = "conversation.listAgentSessions";
pub const METHOD_CONVERSATION_LIST_EVENTS: &str = "conversation.listEvents";
pub const METHOD_CONVERSATION_POST_MESSAGE: &str = "conversation.postMessage";
pub const METHOD_CONVERSATION_CREATE_ATTACHMENT: &str = "conversation.createAttachment";
pub const METHOD_CONVERSATION_DISCARD_ATTACHMENT: &str = "conversation.discardAttachment";
pub const METHOD_CONVERSATION_START_RUN: &str = "conversation.startRun";
pub const METHOD_CONVERSATION_CANCEL_RUN: &str = "conversation.cancelRun";
pub const METHOD_CONVERSATION_STEER_RUN: &str = "conversation.steerRun";
pub const METHOD_CONVERSATION_STOP_BACKGROUND_TASK: &str = "conversation.stopBackgroundTask";
pub const METHOD_CONVERSATION_SET_MODE: &str = "conversation.setMode";
pub const METHOD_CONVERSATION_SET_ALWAYS_ALLOW_TOOL_ACTIONS: &str =
    "conversation.setAlwaysAllowToolActions";
pub const METHOD_TOOL_PERMISSIONS_GET: &str = "toolPermissions.get";
pub const METHOD_TOOL_PERMISSIONS_SET: &str = "toolPermissions.set";
pub const METHOD_CONVERSATION_RESOLVE_RUN_INPUT_DELIVERY: &str =
    "conversation.resolveRunInputDelivery";
pub const METHOD_CONVERSATION_EVENT_DELTA: &str = "conversation.eventDelta";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConversationRole {
    User,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConversationRunStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Canceled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConversationEventKind {
    RunStatus,
    PermissionRequested,
    PermissionResolved,
    SteerRequested,
    SteerDelivered,
    RunInterrupted,
    Usage,
    SessionMetadata,
}

/// Typed session state, separate from transcript content and provider envelopes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSessionMetadata {
    pub title: Option<String>,
    pub current_mode_id: Option<String>,
    pub commands: Option<Vec<ConversationSessionCommand>>,
    pub configuration: Option<Vec<ConversationSessionConfiguration>>,
    pub usage: Option<ConversationSessionUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<ConversationSessionSignal>,
}

/// A bounded owned observation, never a provider envelope. Activity is live-only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSessionSignal {
    pub session_id: String,
    pub activity: Option<String>,
    pub failure: Option<ConversationSessionFailure>,
    pub limit: Option<ConversationSessionLimit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSessionFailure {
    pub id: String,
    pub revision: Option<u64>,
    pub category: String,
    pub severity: String,
    pub message: String,
    pub retryable: Option<bool>,
    pub retry_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSessionLimit {
    pub status: String,
    pub kind: Option<String>,
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSessionState {
    #[serde(rename = "runID")]
    pub run_id: ConversationRunID,
    pub signal: ConversationSessionSignal,
    /// False for database snapshots; saved warnings do not imply live activity.
    pub live: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSessionUsage {
    pub used: u64,
    pub size: u64,
    pub cost: Option<ConversationSessionCost>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSessionCost {
    pub amount: f64,
    pub currency: String,
}

/// How full the agent's context window was when it last reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationContextUsage {
    pub used: u64,
    pub size: u64,
}

/// What a run's token counts cover, which differs by agent. Totals never mix
/// scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConversationRunUsageScope {
    /// Every model call in the turn.
    Turn,
    /// The main agent's model calls, without subagents.
    MainAgent,
    /// Only the turn's last model request (Codex).
    LastRequest,
}

/// The tokens a run's turn used, as the agent reported them when it
/// answered the prompt. Absent fields were not reported, never zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRunUsage {
    pub scope: ConversationRunUsageScope,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_write_tokens: Option<u64>,
    /// The adapter package that reported it, and its version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSessionCommand {
    pub name: String,
    pub description: String,
    /// What to type after the command, from ACP's `input.hint`. Absent when
    /// the command takes no input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSessionConfiguration {
    pub id: String,
    pub name: String,
    pub category: Option<String>,
    pub current_value: String,
    pub value_kind: SessionConfigurationValueKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionConfigurationValueKind {
    Text,
    Boolean,
    Number,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RunInputKind {
    Initial,
    Steer,
    Retry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RunInputStatus {
    Pending,
    Sending,
    Delivered,
    DeliveryUnknown,
    Failed,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RunInputDeliveryEvidence {
    AdapterConfirmed,
    UserMarkedDelivered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentSessionState {
    Active,
    Closed,
    Lost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConversationProjectRole {
    Primary,
    Context,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSummary {
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    pub working_root: WorkingRootSummary,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    /// Ottam answers this conversation's tool permission requests itself
    /// (allow once) for agents without a bypass mode of their own.
    #[serde(default)]
    pub always_allow_tool_actions: bool,
    /// The conversation that delegated this one. Absent for a top-level
    /// conversation, and for a child whose parent was deleted.
    #[serde(
        rename = "parentConversationID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_conversation_id: Option<ConversationID>,
    /// The agent a delegated child was started as: a named agent's name, or
    /// an installed agent's display name for a child started ad hoc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_agent_name: Option<String>,
    /// The task a delegated child was given, as its parent named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_task_title: Option<String>,
    /// The seed a delegated child's tile is drawn from: its own for a
    /// child started ad hoc, or its named agent's current one. Absent for
    /// a named agent without one, whose tile clients draw from
    /// `delegatedAgentName`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_avatar_seed: Option<u32>,
    /// The first 16 hex digits of the SHA-256 of the delegated prompt,
    /// trimmed, so the parent's `delegate` tool call finds this child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_key: Option<String>,
    /// The parent's run whose `delegate` call started this child. A client
    /// shows the child's card in that run when no tool call in it is
    /// recognizably `delegate`, as when OpenCode runs MCP tools inside its
    /// code-mode `execute`. Absent for children from before it was kept.
    #[serde(
        rename = "delegatedFromRunID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub delegated_from_run_id: Option<crate::ConversationRunID>,
    /// A delegated child's latest turn, for its card in the parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_activity: Option<crate::DelegationActivity>,
    /// A delegated child's latest turn whose report waits in its parent
    /// without waking it, because the user drove it; that turn offers Send
    /// to Parent.
    #[serde(
        rename = "deferredReportRunID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub deferred_report_run_id: Option<ConversationRunID>,
}

/// A Project participating in a Conversation, with the ProjectSource pinned at
/// link time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationProjectLink {
    #[serde(rename = "projectID")]
    pub project_id: ProjectID,
    #[serde(rename = "projectSourceID")]
    pub project_source_id: ProjectSourceID,
    pub role: ConversationProjectRole,
}

/// What an attachment carries. Only images are accepted today; the enum exists
/// so a later non-image attachment does not have to reshape the wire type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConversationAttachmentKind {
    Image,
}

/// How the user produced the attachment. Pasted content has no meaningful path
/// on the user's disk; dropped and picked files do, and that path is forwarded
/// to the agent so it can act on the real file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConversationAttachmentSourceKind {
    Pasted,
    Dropped,
    Picked,
}

/// An image stored in the Workspace attachment store and attached to a user
/// Message. `path` is Core-owned and canonical; the app reads it directly to
/// render thumbnails, which is why attachment bytes never cross the protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationAttachment {
    #[serde(rename = "attachmentID")]
    pub attachment_id: ConversationAttachmentID,
    pub kind: ConversationAttachmentKind,
    pub mime_type: String,
    pub byte_count: u64,
    pub width: u32,
    pub height: u32,
    pub path: String,
    pub source_kind: ConversationAttachmentSourceKind,
    /// The file the user attached from, when they attached one from disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_path: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessage {
    #[serde(rename = "messageID")]
    pub message_id: ConversationMessageID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    pub role: ConversationRole,
    pub body: String,
    pub position: u64,
    pub created_at: String,
    /// Attachments claimed by this Message, in the order the user arranged them
    /// in the composer. Absent on every Message that predates attachments.
    #[serde(default)]
    pub attachments: Vec<ConversationAttachment>,
    /// Who wrote the message. Absent on every Message from before delegation.
    #[serde(default)]
    pub source: MessageSource,
    /// A delegated child's report, on a `system` message in its parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<crate::DelegationNote>,
    /// The task a delegated child was given, on its first message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<crate::DelegationTask>,
    /// When a turn of the parent carried this note. Absent while it waits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_delivered_at: Option<String>,
}

/// Who wrote a Message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MessageSource {
    /// The user, or a child's note on a `system` message.
    #[default]
    User,
    /// Ottam, starting a delegated child: the named agent's instructions
    /// and the parent's prompt. Shown as the child's task.
    Delegation,
    /// Ottam, delivering waiting notes to a parent. Not shown; the
    /// notes' "Received message" rows stand for it.
    NoteDelivery,
}

/// How a run's harness session came to be: freshly created, continued on a live
/// session held by the adapter, or restored via ACP `session/load`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionOrigin {
    New,
    Continued,
    Restored,
}

/// Who started a run: the user, or the agent continuing on its own after a
/// turn ended, such as when a background task's completion wakes it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RunOrigin {
    #[default]
    User,
    Autonomous,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRunSummary {
    #[serde(rename = "runID")]
    pub run_id: ConversationRunID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(rename = "engineID")]
    pub engine_id: EngineID,
    #[serde(rename = "modelID", default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// The mode this run was started with, so a retry can be launched the same way
    /// rather than falling back to the adapter's default — which is how a run the
    /// user restricted comes back more permissive than they chose. None for runs
    /// started before this was recorded, and for runs that requested no mode.
    #[serde(rename = "modeID", default, skip_serializing_if = "Option::is_none")]
    pub mode_id: Option<String>,
    /// The thought/effort level this run was started with. Recorded for the same
    /// reason as `mode_id`.
    #[serde(
        rename = "thoughtLevelID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub thought_level_id: Option<String>,
    pub status: ConversationRunStatus,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(rename = "agentSessionID", skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<AgentSessionID>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_origin: Option<SessionOrigin>,
    /// Who started the run. A core that predates autonomous runs omits it;
    /// all its runs are the user's.
    #[serde(default)]
    pub origin: RunOrigin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<AgentStopReason>,
    #[serde(default)]
    pub cost: Value,
    /// Tokens the run's turn used. Absent for runs before this was recorded,
    /// and for a turn whose answer never came.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ConversationRunUsage>,
    /// A finished run whose answer has reported no usage. False for runs from
    /// before usage was recorded. A stopped run's late answer can still clear
    /// it, with a `usage` event.
    #[serde(default)]
    pub usage_unavailable: bool,
    /// The earlier turns this run's prompt carried to a session that had not
    /// seen them, such as one opened after switching agents. Absent when it
    /// carried none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_replay: Option<ConversationHistoryReplay>,
    #[serde(default)]
    pub error: Value,
}

/// Earlier turns a prompt replayed: messages in full, tool calls as one line
/// each without their output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationHistoryReplay {
    /// Entries the replay carried.
    pub entries: u32,
    /// Whether older entries were left out to fit the budget. The agent can
    /// read them with the history tools.
    pub truncated: bool,
}

/// A user input intent and its authoritative delivery state. The Message body
/// is immutable and shared across retries; delivery state lives here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRunInput {
    #[serde(rename = "runInputID")]
    pub run_input_id: ConversationRunInputID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(rename = "messageID")]
    pub message_id: ConversationMessageID,
    #[serde(
        rename = "requestedAgainstRunID",
        skip_serializing_if = "Option::is_none"
    )]
    pub requested_against_run_id: Option<ConversationRunID>,
    #[serde(rename = "targetRunID", skip_serializing_if = "Option::is_none")]
    pub target_run_id: Option<ConversationRunID>,
    pub kind: RunInputKind,
    #[serde(rename = "retryOfInputID", skip_serializing_if = "Option::is_none")]
    pub retry_of_input_id: Option<ConversationRunInputID>,
    pub idempotency_key: String,
    pub status: RunInputStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_evidence: Option<RunInputDeliveryEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionSummary {
    #[serde(rename = "agentSessionID")]
    pub agent_session_id: AgentSessionID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(rename = "engineID")]
    pub engine_id: EngineID,
    pub state: AgentSessionState,
    pub source_scope_fingerprint: String,
    pub created_at: String,
    pub last_used_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<String>,
    /// The permission mode the agent last confirmed for this session.
    #[serde(rename = "currentModeID", skip_serializing_if = "Option::is_none")]
    pub current_mode_id: Option<String>,
    /// The slash commands the agent last announced for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<ConversationSessionCommand>>,
    /// How full the session's context was when the agent last reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<ConversationContextUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessagePage {
    pub messages: Vec<ConversationMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRunPage {
    pub runs: Vec<ConversationRunSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRunInputPage {
    pub run_inputs: Vec<ConversationRunInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionPage {
    pub sessions: Vec<AgentSessionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Bounded conversation snapshot: metadata, links, and the latest page of each
/// collection. History beyond these pages is fetched with the cursor-paged
/// list methods.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationDetail {
    pub conversation: ConversationSummary,
    pub workspace: WorkspaceSummary,
    pub projects: Vec<ConversationProjectLink>,
    pub latest_message_page: ConversationMessagePage,
    pub latest_run_page: ConversationRunPage,
    pub latest_run_input_page: ConversationRunInputPage,
    pub latest_agent_session_page: AgentSessionPage,
}

/// First user message submitted together with conversation creation; carries
/// its own idempotency key like any other message submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationInitialMessage {
    pub body: String,
    pub idempotency_key: String,
    /// Unclaimed attachments to claim for this Message, in composer order.
    #[serde(rename = "attachmentIDs", default)]
    pub attachment_ids: Vec<ConversationAttachmentID>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationCreateParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    pub working_root: WorkingRootSelection,
    #[serde(rename = "contextProjectIDs", default)]
    pub context_project_ids: Vec<ProjectID>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<ConversationInitialMessage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationCreateResult {
    pub conversation: ConversationDetail,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_input: Option<ConversationRunInput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum HarnessSessionCleanupStatus {
    Deleted,
    Unsupported,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessSessionCleanup {
    #[serde(rename = "engineID")]
    pub engine_id: EngineID,
    #[serde(rename = "agentSessionID")]
    pub agent_session_id: AgentSessionID,
    pub status: HarnessSessionCleanupStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationDeleteParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    /// What becomes of the conversation's delegated children. Kept
    /// children stop working and become top-level conversations.
    #[serde(default)]
    pub children: ConversationDeleteChildren,
}

/// What deleting a parent does to its delegated children.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConversationDeleteChildren {
    #[default]
    Keep,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationDeleteResult {
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    pub harness_session_cleanup: Vec<HarnessSessionCleanup>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationListParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationListResult {
    pub conversations: Vec<ConversationSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationGetParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationGetResult {
    pub conversation: ConversationDetail,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationListMessagesParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationListMessagesResult {
    pub page: ConversationMessagePage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationListRunsParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationListRunsResult {
    pub page: ConversationRunPage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationListRunInputsParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationListRunInputsResult {
    pub page: ConversationRunInputPage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationListAgentSessionsParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationListAgentSessionsResult {
    pub page: AgentSessionPage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationListEventsParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(rename = "runID", default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<ConversationRunID>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Persisted run events replayed in insertion order; entries reuse the live
/// delta shape so clients rebuild historical run timelines with the same code
/// path that consumes `conversation.eventDelta` notifications.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationListEventsResult {
    pub events: Vec<ConversationEventDeltaParams>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationPostMessageParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    pub message: String,
    pub idempotency_key: String,
    /// Unclaimed attachments to claim for this Message, in composer order.
    #[serde(rename = "attachmentIDs", default)]
    pub attachment_ids: Vec<ConversationAttachmentID>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPostMessageResult {
    pub message: ConversationMessage,
    pub input: ConversationRunInput,
}

/// Registers an already-normalized image with the Workspace attachment store.
/// The client writes the bytes to `source_path` — a temporary file it owns and
/// deletes — because the JSONL frame cap makes base64 payloads unshippable.
/// Core copies the file into the store and never modifies or removes the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationCreateAttachmentParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    /// Absolute path to a regular file the client wrote. Core rejects symbolic
    /// links and re-validates the declared type against the file's magic bytes.
    pub source_path: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub source_kind: ConversationAttachmentSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_path: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationCreateAttachmentResult {
    pub attachment: ConversationAttachment,
}

/// Drops an attachment the user removed from the composer before sending.
/// Only unclaimed attachments can be discarded; once a Message claims one it
/// lives and dies with that Message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationDiscardAttachmentParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "attachmentID")]
    pub attachment_id: ConversationAttachmentID,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationDiscardAttachmentResult {
    /// False when the attachment was already gone, so a retried discard is not
    /// an error.
    pub discarded: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationStartRunParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(rename = "engineID")]
    pub engine_id: EngineID,
    #[serde(rename = "modelID", default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Adapter-defined mode id (ACP `category: mode`); nil runs the engine's
    /// default mode and omits the key on the wire, like `modelID`.
    #[serde(rename = "modeID", default, skip_serializing_if = "Option::is_none")]
    pub mode_id: Option<String>,
    /// Adapter-defined thought/effort level id (ACP `category: thought_level`).
    #[serde(
        rename = "thoughtLevelID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub thought_level_id: Option<String>,
    #[serde(rename = "runInputID")]
    pub run_input_id: ConversationRunInputID,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationStartRunResult {
    pub run: ConversationRunSummary,
    pub input: ConversationRunInput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationCancelRunParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(rename = "runID")]
    pub run_id: ConversationRunID,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationCancelRunResult {
    pub run: ConversationRunSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationSteerRunParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    /// The run the user believes is active; steering is rejected with a typed
    /// error when a different run is active.
    #[serde(rename = "runID")]
    pub run_id: ConversationRunID,
    pub message: String,
    pub idempotency_key: String,
    /// Unclaimed attachments to claim for the steer Message, in composer order.
    #[serde(rename = "attachmentIDs", default)]
    pub attachment_ids: Vec<ConversationAttachmentID>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSteerRunResult {
    /// Authoritative input state after routing; during interrupt-and-continue
    /// the target run differs from the requested-against run.
    pub input: ConversationRunInput,
    pub run: ConversationRunSummary,
}

/// Switches the permission mode of a conversation's agent session now, while
/// idle or mid-turn, rather than with the next message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationSetModeParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    /// The session the client believes is current; a different or ended
    /// session is rejected so the change never lands on a replacement.
    /// Current is the latest run's session, else its engine's most recently
    /// used active one. Omitted, Core targets that session.
    #[serde(
        rename = "agentSessionID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub agent_session_id: Option<AgentSessionID>,
    #[serde(rename = "modeID")]
    pub mode_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConversationSetModeOutcome {
    /// The agent acknowledged the mode; it is the session's confirmed mode.
    Applied,
    /// The session is not open in a running agent. Nothing was sent; the mode
    /// applies when the next message opens it.
    Deferred,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSetModeResult {
    pub outcome: ConversationSetModeOutcome,
    /// The session's confirmed mode after the call.
    #[serde(rename = "currentModeID", skip_serializing_if = "Option::is_none")]
    pub current_mode_id: Option<String>,
}

/// Turns Ottam's auto-approval on or off for one conversation. The value
/// also becomes the default for new conversations. It applies from the next
/// permission request, mid-turn included.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationSetAlwaysAllowToolActionsParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    pub enabled: bool,
}

/// The always-allow value new conversations start from: the one last set in
/// any conversation. `toolPermissions.get` takes no parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolPermissionsState {
    pub always_allow_tool_actions: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolPermissionsGetParams {}

/// Asks the agent to stop a background task that a run started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationStopBackgroundTaskParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    /// The run whose tool started the task. Its agent session is the one the
    /// task id belongs to.
    #[serde(rename = "runID")]
    pub run_id: ConversationRunID,
    /// The adapter's task id, from the tool's `backgroundTask.id`.
    #[serde(rename = "asyncTaskID")]
    pub async_task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationStopBackgroundTaskResult {
    /// Whether the agent stopped the task. False when the task had already
    /// ended or its session is no longer live; the task's state arrives as a
    /// transcript revision either way.
    pub stopped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RunInputDeliveryResolution {
    /// Supersede an unsent input — `deliveryUnknown` or `failed` — and create a
    /// linked retry RunInput with a new idempotency key. Retrying a
    /// `deliveryUnknown` input may duplicate work the agent already accepted;
    /// retrying a `failed` one cannot, since the adapter rejected it before
    /// acceptance.
    Retry {
        #[serde(rename = "idempotencyKey")]
        #[schemars(rename = "idempotencyKey")]
        idempotency_key: String,
    },
    /// Record that the user observed the input take effect. Does not claim an
    /// adapter receipt exists.
    MarkDelivered,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationResolveRunInputDeliveryParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(rename = "runInputID")]
    pub run_input_id: ConversationRunInputID,
    pub resolution: RunInputDeliveryResolution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationResolveRunInputDeliveryResult {
    pub resolved_input: ConversationRunInput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_input: Option<ConversationRunInput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationEventDeltaParams {
    #[serde(rename = "eventID")]
    pub event_id: ConversationEventID,
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "projectID", skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectID>,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    #[serde(rename = "runID")]
    pub run_id: ConversationRunID,
    pub sequence: u64,
    pub event_kind: ConversationEventKind,
    #[serde(default)]
    pub payload: Value,
    pub created_at: String,
}
