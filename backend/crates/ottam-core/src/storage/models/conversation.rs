use super::types::{
    optional_timestamp, timestamp, ConversationId, ConversationMessageId, ConversationProjectRole,
    MessageRole, MessageSource, ProjectId, ProjectSourceId, WorkingRootKind, WorkspaceId,
};
use crate::storage::error::StorageError;
use crate::storage::schema::{
    conversation_messages, conversation_projects, conversations, transcript_items,
};
use chrono::{DateTime, Utc};
use diesel::{Identifiable, Insertable, Queryable, Selectable};

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = conversations)]
pub struct ConversationRow {
    pub id: String,
    pub workspace_id: String,
    pub working_root_kind: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
    pub always_allow_tool_actions: bool,
    pub parent_conversation_id: Option<String>,
    pub delegated_agent_name: Option<String>,
    pub delegation_key: Option<String>,
    pub delegated_task_title: Option<String>,
    pub delegated_avatar_seed: Option<i64>,
    pub delegated_named_agent_id: Option<String>,
    pub delegated_from_run_id: Option<String>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = conversations)]
pub struct NewConversationRow<'a> {
    pub id: &'a str,
    pub workspace_id: &'a str,
    pub working_root_kind: &'a str,
    pub title: &'a str,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
    pub always_allow_tool_actions: bool,
    pub parent_conversation_id: Option<&'a str>,
    pub delegated_agent_name: Option<&'a str>,
    pub delegation_key: Option<&'a str>,
    pub delegated_task_title: Option<&'a str>,
    pub delegated_avatar_seed: Option<i64>,
    pub delegated_named_agent_id: Option<&'a str>,
    pub delegated_from_run_id: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    pub id: ConversationId,
    pub workspace_id: WorkspaceId,
    pub working_root_kind: WorkingRootKind,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    /// Ottam answers this conversation's tool permission requests itself
    /// for agents without a bypass mode of their own.
    pub always_allow_tool_actions: bool,
    /// The conversation that delegated this one, while it exists.
    pub parent_conversation_id: Option<ConversationId>,
    /// The named agent this delegated child was started as.
    pub delegated_agent_name: Option<String>,
    /// A digest of the prompt this child was delegated with.
    pub delegation_key: Option<String>,
    /// The task this child was delegated, as its parent named it.
    pub delegated_task_title: Option<String>,
    /// The seed this child's tile is drawn from, set when it started.
    pub delegated_avatar_seed: Option<u32>,
    /// The named agent this child was started as, whose current tile it
    /// draws when it has no seed of its own.
    pub delegated_named_agent_id: Option<String>,
    /// The parent's run whose `delegate` call started this child.
    pub delegated_from_run_id: Option<String>,
}

impl TryFrom<ConversationRow> for Conversation {
    type Error = StorageError;

    fn try_from(row: ConversationRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: ConversationId::try_from(row.id)?,
            workspace_id: WorkspaceId::try_from(row.workspace_id)?,
            working_root_kind: WorkingRootKind::try_from(row.working_root_kind)?,
            title: row.title,
            created_at: timestamp(row.created_at, "Conversation.created_at")?,
            updated_at: timestamp(row.updated_at, "Conversation.updated_at")?,
            archived_at: optional_timestamp(row.archived_at, "Conversation.archived_at")?,
            always_allow_tool_actions: row.always_allow_tool_actions,
            parent_conversation_id: row
                .parent_conversation_id
                .map(ConversationId::try_from)
                .transpose()?,
            delegated_agent_name: row.delegated_agent_name,
            delegation_key: row.delegation_key,
            delegated_task_title: row.delegated_task_title,
            delegated_avatar_seed: row
                .delegated_avatar_seed
                .and_then(|seed| u32::try_from(seed).ok()),
            delegated_named_agent_id: row.delegated_named_agent_id,
            delegated_from_run_id: row.delegated_from_run_id,
        })
    }
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = conversation_projects)]
#[diesel(primary_key(conversation_id, project_id))]
pub struct ConversationProjectRow {
    pub conversation_id: String,
    pub workspace_id: String,
    pub project_id: String,
    pub source_id: String,
    pub role: String,
    pub created_at: i64,
    pub removed_at: Option<i64>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = conversation_projects)]
pub struct NewConversationProjectRow<'a> {
    pub conversation_id: &'a str,
    pub workspace_id: &'a str,
    pub project_id: &'a str,
    pub source_id: &'a str,
    pub role: &'a str,
    pub created_at: i64,
    pub removed_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationProject {
    pub conversation_id: ConversationId,
    pub workspace_id: WorkspaceId,
    pub project_id: ProjectId,
    pub source_id: ProjectSourceId,
    pub role: ConversationProjectRole,
    pub created_at: DateTime<Utc>,
    pub removed_at: Option<DateTime<Utc>>,
}

impl TryFrom<ConversationProjectRow> for ConversationProject {
    type Error = StorageError;

    fn try_from(row: ConversationProjectRow) -> Result<Self, Self::Error> {
        Ok(Self {
            conversation_id: ConversationId::try_from(row.conversation_id)?,
            workspace_id: WorkspaceId::try_from(row.workspace_id)?,
            project_id: ProjectId::try_from(row.project_id)?,
            source_id: ProjectSourceId::try_from(row.source_id)?,
            role: ConversationProjectRole::try_from(row.role)?,
            created_at: timestamp(row.created_at, "ConversationProject.created_at")?,
            removed_at: optional_timestamp(row.removed_at, "ConversationProject.removed_at")?,
        })
    }
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = conversation_messages)]
pub struct ConversationMessageRow {
    pub id: String,
    pub conversation_id: String,
    pub role: String,
    pub body: String,
    #[diesel(select_expression = transcript_items::body_byte_count)]
    #[diesel(select_expression_type = transcript_items::body_byte_count)]
    pub body_byte_count: i64,
    #[diesel(select_expression = transcript_items::position)]
    #[diesel(select_expression_type = transcript_items::position)]
    pub position: i64,
    pub created_at: i64,
    pub source: String,
    pub note_json: Option<String>,
    pub note_delivered_at: Option<i64>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = conversation_messages)]
pub struct NewConversationMessageRow<'a> {
    pub id: &'a str,
    pub conversation_id: &'a str,
    pub role: &'a str,
    pub body: &'a str,
    pub created_at: i64,
    pub source: &'a str,
    pub note_json: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationMessage {
    pub id: ConversationMessageId,
    pub conversation_id: ConversationId,
    pub role: MessageRole,
    pub body: String,
    pub body_byte_count: i64,
    pub position: i64,
    pub created_at: DateTime<Utc>,
    pub source: MessageSource,
    /// The message's delegation record: a child's note on a `system`
    /// message in its parent, or the task on a child's first message
    /// (source `delegation`).
    pub note_json: Option<String>,
    /// When a prompt to the parent carried this note.
    pub note_delivered_at: Option<DateTime<Utc>>,
}

impl TryFrom<ConversationMessageRow> for ConversationMessage {
    type Error = StorageError;

    fn try_from(row: ConversationMessageRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: ConversationMessageId::try_from(row.id)?,
            conversation_id: ConversationId::try_from(row.conversation_id)?,
            role: MessageRole::try_from(row.role)?,
            body: row.body,
            body_byte_count: row.body_byte_count,
            position: row.position,
            created_at: timestamp(row.created_at, "ConversationMessage.created_at")?,
            source: MessageSource::try_from(row.source)?,
            note_json: row.note_json,
            note_delivered_at: optional_timestamp(
                row.note_delivered_at,
                "ConversationMessage.note_delivered_at",
            )?,
        })
    }
}
