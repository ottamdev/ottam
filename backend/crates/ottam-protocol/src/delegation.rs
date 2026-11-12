//! Named agents and delegation: a conversation hands a task to a named agent
//! through Ottam's own MCP tools, and the child's report comes back to
//! it as a note. See `docs/architecture/backend/mcp.md` § Delegation.

use crate::{
    ConversationID, ConversationRunID, ConversationRunStatus, ConversationRunSummary, EngineID,
    NamedAgentID, WorkspaceID,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const METHOD_NAMED_AGENT_LIST: &str = "namedAgent.list";
pub const METHOD_NAMED_AGENT_CREATE: &str = "namedAgent.create";
pub const METHOD_NAMED_AGENT_UPDATE: &str = "namedAgent.update";
pub const METHOD_NAMED_AGENT_DELETE: &str = "namedAgent.delete";
pub const METHOD_CONVERSATION_CONTINUE_DELEGATION: &str = "conversation.continueDelegation";
pub const METHOD_CONVERSATION_SEND_TO_PARENT: &str = "conversation.sendToParent";
pub const METHOD_CONVERSATION_LIST_CHANGED: &str = "conversation.listChanged";

/// What a parent woken by its children's notes replies when none of them
/// needs the user. A reply that is only this line shows as nothing; its
/// notes show as acknowledged. Written as a leaf directive, the shape
/// Codex's own client directives use.
pub const DELEGATION_ACK_DIRECTIVE: &str = "::ottam-ack{}";

/// Whether `text` is an acknowledgement and nothing else, allowing the
/// backticks or code fence an agent may quote it in.
pub fn is_delegation_ack(text: &str) -> bool {
    text.trim().trim_matches('`').trim() == DELEGATION_ACK_DIRECTIVE
}

/// An agent instance with the model, mode and instructions a delegated child
/// starts with. Names are unique, ignoring case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NamedAgent {
    #[serde(rename = "namedAgentID")]
    pub named_agent_id: NamedAgentID,
    pub name: String,
    #[serde(rename = "engineID")]
    pub engine_id: EngineID,
    #[serde(rename = "modelID", default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(rename = "modeID", default, skip_serializing_if = "Option::is_none")]
    pub mode_id: Option<String>,
    pub instructions: String,
    /// The seed its tile is drawn from. Absent, clients draw from the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_seed: Option<u32>,
    pub created_at: String,
    pub updated_at: String,
}

/// What a named agent is made of; `namedAgent.create` and
/// `namedAgent.update` take the whole of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NamedAgentConfig {
    pub name: String,
    #[serde(rename = "engineID")]
    pub engine_id: EngineID,
    #[serde(rename = "modelID", default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// The mode children start in; omitted, the agent's own read-only mode.
    #[serde(rename = "modeID", default, skip_serializing_if = "Option::is_none")]
    pub mode_id: Option<String>,
    #[serde(default)]
    pub instructions: String,
    /// The seed its tile is drawn from; absent draws from the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_seed: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NamedAgentListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NamedAgentListResult {
    pub named_agents: Vec<NamedAgent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NamedAgentCreateParams {
    pub config: NamedAgentConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NamedAgentUpdateParams {
    #[serde(rename = "namedAgentID")]
    pub named_agent_id: NamedAgentID,
    pub config: NamedAgentConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NamedAgentResult {
    pub named_agent: NamedAgent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NamedAgentDeleteParams {
    #[serde(rename = "namedAgentID")]
    pub named_agent_id: NamedAgentID,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NamedAgentDeleteResult {}

/// How a child's turn ended, as its note says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DelegationNoteState {
    Completed,
    Failed,
    Stopped,
}

/// Why a child's turn was stopped, so the parent can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DelegationStopCause {
    /// The user stopped it in the child's own chat.
    User,
    /// The user stopped the parent's turn, and Stop reached its children.
    ParentStopped,
    /// Ottam quit while it worked.
    Quit,
    /// The parent's agent stopped it with `stop_child`.
    ParentAgent,
}

/// What a note carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DelegationNoteKind {
    /// A child's turn ended; the note holds its final message.
    #[default]
    Report,
    /// A child wrote to its parent with `message_parent`.
    ChildMessage,
    /// A parent wrote to its child with `message_child`. The note sits in
    /// the child.
    ParentMessage,
}

/// What one side of a delegation left for the other: a child's report or
/// message in its parent, or a parent's message in its child. It rides on a
/// `system` message, and waits there until a prompt carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DelegationNote {
    /// The child the note is about. A `parentMessage` names the child it
    /// sits in.
    #[serde(rename = "childConversationID")]
    pub child_conversation_id: ConversationID,
    /// Who wrote it: the child's named agent, or for a `parentMessage` the
    /// parent's agent.
    pub child_name: String,
    #[serde(default)]
    pub kind: DelegationNoteKind,
    /// How a report's turn ended; a message is `completed`.
    pub state: DelegationNoteState,
    /// About 400 characters of the child's final message, or of the
    /// message.
    pub excerpt: String,
    /// The child reached its cap of automatic follow-ups, so the note waits
    /// for the user's next message or Continue.
    #[serde(default)]
    pub waits_for_user: bool,
    /// A report of a turn the user drove in the child. It wakes no one and
    /// rides on the parent's next run, unless the user sends it on.
    #[serde(default)]
    pub deferred: bool,
    /// The child's turn a report is of.
    #[serde(rename = "runID", default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<ConversationRunID>,
    /// Why a `stopped` turn stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_cause: Option<DelegationStopCause>,
}

/// The task a delegated child was given, on its first message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DelegationTask {
    /// A short name for the task: the `delegate` call's title, or the
    /// prompt's first line.
    pub title: String,
    /// The parent's prompt, as written.
    pub prompt: String,
    /// The named agent's instructions when the child started.
    #[serde(default)]
    pub instructions: String,
    /// The agent of the conversation that delegated.
    #[serde(
        rename = "startedByEngineID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub started_by_engine_id: Option<EngineID>,
}

/// What a delegated child's latest turn has done, for its card in the parent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DelegationActivity {
    /// The latest turn's status; absent before the first turn starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_run_status: Option<ConversationRunStatus>,
    #[serde(default)]
    pub commands: u32,
    #[serde(default)]
    pub reads: u32,
    #[serde(default)]
    pub edits: u32,
    #[serde(default)]
    pub searches: u32,
    #[serde(default)]
    pub fetches: u32,
    /// Other tool calls.
    #[serde(default)]
    pub other: u32,
    #[serde(default)]
    pub subagents: u32,
}

/// Delivers a parent's waiting notes now, as Continue on the waiting row does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationContinueDelegationParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationContinueDelegationResult {
    /// The run that carries the notes; none when no note was waiting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<ConversationRunSummary>,
}

/// Send to Parent in a delegated child: its deferred reports wake the
/// parent now. A working parent refuses with `conversationBusy`, and the
/// reports stay deferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationSendToParentParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    /// The child.
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSendToParentResult {
    /// The parent's run that carries the reports; none when nothing was
    /// waiting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<ConversationRunSummary>,
}

/// Core created or relinked a conversation in the workspace on its own, as
/// when an agent delegates; clients list the workspace's conversations again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationListChangedParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
}

#[cfg(test)]
mod tests {
    use super::is_delegation_ack;

    #[test]
    fn an_acknowledgement_may_be_quoted_but_not_added_to() {
        assert!(is_delegation_ack("  ::ottam-ack{}\n"));
        assert!(is_delegation_ack("`::ottam-ack{}`"));
        assert!(is_delegation_ack("```\n::ottam-ack{}\n```"));
        assert!(!is_delegation_ack("::ottam-ack{} Thanks."));
        assert!(!is_delegation_ack("::"));
    }
}
