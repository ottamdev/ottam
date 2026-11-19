use crate::ids::{ConversationID, ConversationRunID, WorkspaceID};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const METHOD_ATTENTION_LIST: &str = "attention.list";
pub const METHOD_ATTENTION_SET_FOCUS: &str = "attention.setFocus";
pub const METHOD_ATTENTION_CHANGED: &str = "attention.changed";

/// Read every conversation that wants the user: one waiting on an answer, or
/// one whose run ended while nobody was looking. Later changes arrive on
/// `attention.changed`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttentionListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttentionListResult {
    /// The revision of the last `attention.changed` core sent before this
    /// answer. An entry a client stamped at or below it is superseded.
    pub revision: u64,
    /// Entries whose state is not `none`.
    pub entries: Vec<AttentionEntry>,
}

/// The conversation this connection's user is looking at, or none. Core
/// treats a focused conversation as seen and does not notify for it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttentionSetFocusParams {
    #[serde(
        rename = "conversationID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub conversation_id: Option<ConversationID>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttentionSetFocusResult {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AttentionState {
    /// The agent waits on the user: a permission, a question, or a plan.
    NeedsInput,
    /// A run ended while nobody looked at the conversation.
    Unseen,
    /// Nothing; sent when an earlier state ends.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AttentionReason {
    Permission,
    Question,
    Plan,
    Completed,
    Failed,
}

/// One conversation's attention. Copy for the user stays in the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttentionEntry {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "conversationID")]
    pub conversation_id: ConversationID,
    pub state: AttentionState,
    /// Why the state holds; for `none`, why the state that ended held.
    pub reason: AttentionReason,
    #[serde(rename = "runID", default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<ConversationRunID>,
    /// The conversation's title when the entry was made.
    pub title: String,
    /// The asking tool's title for an ask; the failure message for a failed
    /// run; for a completed run, the start of its last message, as
    /// markdown. A completed run restored after a restart has none: it never
    /// alerts again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One conversation's attention changed. `notify` is true at most once per
/// state a conversation enters, when a client should alert its user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttentionChangedParams {
    pub revision: u64,
    pub entry: AttentionEntry,
    pub notify: bool,
}
