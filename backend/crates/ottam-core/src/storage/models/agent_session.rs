use super::types::{
    optional_timestamp, timestamp, AgentSessionId, AgentSessionStatus, ConversationId, WorkspaceId,
};
use crate::storage::error::StorageError;
use crate::storage::schema::agent_sessions;
use chrono::{DateTime, Utc};
use diesel::{Identifiable, Insertable, Queryable, Selectable};

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = agent_sessions)]
pub struct AgentSessionRow {
    pub id: String,
    pub workspace_id: String,
    pub conversation_id: String,
    pub engine_id: String,
    pub external_session_id: Option<String>,
    pub source_scope_fingerprint: String,
    pub status: String,
    pub created_at: i64,
    pub last_used_at: i64,
    pub ended_at: Option<i64>,
    pub end_reason: Option<String>,
    pub current_mode_id: Option<String>,
    pub available_commands: Option<String>,
    pub context_usage: Option<String>,
    pub history_seeded: bool,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = agent_sessions)]
pub struct NewAgentSessionRow<'a> {
    pub id: &'a str,
    pub workspace_id: &'a str,
    pub conversation_id: &'a str,
    pub engine_id: &'a str,
    pub external_session_id: Option<&'a str>,
    pub source_scope_fingerprint: &'a str,
    pub status: &'a str,
    pub created_at: i64,
    pub last_used_at: i64,
    pub ended_at: Option<i64>,
    pub end_reason: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSession {
    pub id: AgentSessionId,
    pub workspace_id: WorkspaceId,
    pub conversation_id: ConversationId,
    pub engine_id: String,
    pub external_session_id: Option<String>,
    pub source_scope_fingerprint: String,
    pub status: AgentSessionStatus,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub end_reason: Option<String>,
    /// The permission mode the agent last confirmed for this session.
    pub current_mode_id: Option<String>,
    /// The commands the agent last announced, as a JSON array of
    /// `ConversationSessionCommand`.
    pub available_commands: Option<String>,
    /// How full the context was when the agent last reported, as
    /// `ConversationContextUsage` JSON.
    pub context_usage: Option<String>,
    /// Whether the session has been sent the conversation's earlier turns,
    /// or had none to be sent.
    pub history_seeded: bool,
}

impl TryFrom<AgentSessionRow> for AgentSession {
    type Error = StorageError;

    fn try_from(row: AgentSessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: AgentSessionId::try_from(row.id)?,
            workspace_id: WorkspaceId::try_from(row.workspace_id)?,
            conversation_id: ConversationId::try_from(row.conversation_id)?,
            engine_id: row.engine_id,
            external_session_id: row.external_session_id,
            source_scope_fingerprint: row.source_scope_fingerprint,
            status: AgentSessionStatus::try_from(row.status)?,
            created_at: timestamp(row.created_at, "AgentSession.created_at")?,
            last_used_at: timestamp(row.last_used_at, "AgentSession.last_used_at")?,
            ended_at: optional_timestamp(row.ended_at, "AgentSession.ended_at")?,
            end_reason: row.end_reason,
            current_mode_id: row.current_mode_id,
            available_commands: row.available_commands,
            context_usage: row.context_usage,
            history_seeded: row.history_seeded,
        })
    }
}
