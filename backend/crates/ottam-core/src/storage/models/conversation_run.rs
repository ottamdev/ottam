use super::types::{
    optional_timestamp, timestamp, AgentSessionId, ConversationId, ConversationRunId, ProjectId,
    ProjectSourceId, RunEventId, RunEventKind, RunOrigin, RunSourceRole, RunStatus, SessionOrigin,
    WorkingRootKind, WorkspaceId,
};
use crate::storage::error::StorageError;
use crate::storage::schema::{conversation_run_sources, conversation_runs, run_events};
use chrono::{DateTime, Utc};
use diesel::{Identifiable, Insertable, Queryable, Selectable};

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = conversation_runs)]
pub struct ConversationRunRow {
    pub id: String,
    pub workspace_id: String,
    pub conversation_id: String,
    pub engine_id: String,
    pub model_id: Option<String>,
    pub agent_session_id: Option<String>,
    pub session_origin: Option<String>,
    pub status: String,
    pub request_options_json: Option<String>,
    pub event_sequence_cursor: i64,
    pub queued_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub stop_reason: Option<String>,
    pub error_json: Option<String>,
    pub origin: String,
    pub usage_json: Option<String>,
    pub history_replay_json: Option<String>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = conversation_runs)]
pub struct NewConversationRunRow<'a> {
    pub id: &'a str,
    pub workspace_id: &'a str,
    pub conversation_id: &'a str,
    pub engine_id: &'a str,
    pub model_id: Option<&'a str>,
    pub agent_session_id: Option<&'a str>,
    pub session_origin: Option<&'a str>,
    pub status: &'a str,
    pub request_options_json: Option<&'a str>,
    pub event_sequence_cursor: i64,
    pub queued_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub stop_reason: Option<&'a str>,
    pub error_json: Option<&'a str>,
    pub origin: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationRun {
    pub id: ConversationRunId,
    pub workspace_id: WorkspaceId,
    pub conversation_id: ConversationId,
    pub engine_id: String,
    pub model_id: Option<String>,
    pub agent_session_id: Option<AgentSessionId>,
    pub session_origin: Option<SessionOrigin>,
    pub status: RunStatus,
    pub request_options_json: Option<String>,
    pub event_sequence_cursor: i64,
    pub queued_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub stop_reason: Option<String>,
    pub error_json: Option<String>,
    pub origin: RunOrigin,
    /// `ConversationRunUsage` JSON: the tokens the run's turn used.
    pub usage_json: Option<String>,
    /// `ConversationHistoryReplay` JSON: what the run's prompt replayed from
    /// earlier turns.
    pub history_replay_json: Option<String>,
}

impl TryFrom<ConversationRunRow> for ConversationRun {
    type Error = StorageError;

    fn try_from(row: ConversationRunRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: ConversationRunId::try_from(row.id)?,
            workspace_id: WorkspaceId::try_from(row.workspace_id)?,
            conversation_id: ConversationId::try_from(row.conversation_id)?,
            engine_id: row.engine_id,
            model_id: row.model_id,
            agent_session_id: row
                .agent_session_id
                .map(AgentSessionId::try_from)
                .transpose()?,
            session_origin: row
                .session_origin
                .map(SessionOrigin::try_from)
                .transpose()?,
            status: RunStatus::try_from(row.status)?,
            request_options_json: row.request_options_json,
            event_sequence_cursor: row.event_sequence_cursor,
            queued_at: timestamp(row.queued_at, "ConversationRun.queued_at")?,
            started_at: optional_timestamp(row.started_at, "ConversationRun.started_at")?,
            completed_at: optional_timestamp(row.completed_at, "ConversationRun.completed_at")?,
            stop_reason: row.stop_reason,
            error_json: row.error_json,
            origin: RunOrigin::try_from(row.origin)?,
            usage_json: row.usage_json,
            history_replay_json: row.history_replay_json,
        })
    }
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = conversation_run_sources)]
#[diesel(primary_key(run_id, canonical_path))]
pub struct ConversationRunSourceRow {
    pub run_id: String,
    pub workspace_id: String,
    pub conversation_id: String,
    pub root_kind: String,
    pub project_id: Option<String>,
    pub source_id: Option<String>,
    pub canonical_path: String,
    pub role: String,
    pub created_at: i64,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = conversation_run_sources)]
pub struct NewConversationRunSourceRow<'a> {
    pub run_id: &'a str,
    pub workspace_id: &'a str,
    pub conversation_id: &'a str,
    pub root_kind: &'a str,
    pub project_id: Option<&'a str>,
    pub source_id: Option<&'a str>,
    pub canonical_path: &'a str,
    pub role: &'a str,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationRunSource {
    pub run_id: ConversationRunId,
    pub workspace_id: WorkspaceId,
    pub conversation_id: ConversationId,
    pub root_kind: WorkingRootKind,
    pub project_id: Option<ProjectId>,
    pub source_id: Option<ProjectSourceId>,
    pub canonical_path: String,
    pub role: RunSourceRole,
    pub created_at: DateTime<Utc>,
}

impl TryFrom<ConversationRunSourceRow> for ConversationRunSource {
    type Error = StorageError;

    fn try_from(row: ConversationRunSourceRow) -> Result<Self, Self::Error> {
        let root_kind = WorkingRootKind::try_from(row.root_kind)?;
        let project_id = row.project_id.map(ProjectId::try_from).transpose()?;
        let source_id = row.source_id.map(ProjectSourceId::try_from).transpose()?;
        match root_kind {
            WorkingRootKind::Project if project_id.is_none() || source_id.is_none() => {
                return Err(StorageError::new(
                    "storageCorrupt",
                    "persisted Project run source is missing its Project or ProjectSource",
                ));
            }
            WorkingRootKind::WorkspaceFiles if project_id.is_some() || source_id.is_some() => {
                return Err(StorageError::new(
                    "storageCorrupt",
                    "persisted Workspace-files run source carries Project identity",
                ));
            }
            _ => {}
        }
        Ok(Self {
            run_id: ConversationRunId::try_from(row.run_id)?,
            workspace_id: WorkspaceId::try_from(row.workspace_id)?,
            conversation_id: ConversationId::try_from(row.conversation_id)?,
            root_kind,
            project_id,
            source_id,
            canonical_path: row.canonical_path,
            role: RunSourceRole::try_from(row.role)?,
            created_at: timestamp(row.created_at, "ConversationRunSource.created_at")?,
        })
    }
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = run_events)]
pub struct RunEventRow {
    pub id: String,
    pub run_id: String,
    pub sequence: i64,
    pub kind: String,
    pub payload_json: String,
    pub created_at: i64,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = run_events)]
pub struct NewRunEventRow<'a> {
    pub id: &'a str,
    pub run_id: &'a str,
    pub sequence: i64,
    pub kind: &'a str,
    pub payload_json: &'a str,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEvent {
    pub id: RunEventId,
    pub run_id: ConversationRunId,
    pub sequence: i64,
    pub kind: RunEventKind,
    pub payload_json: String,
    pub created_at: DateTime<Utc>,
}

impl TryFrom<RunEventRow> for RunEvent {
    type Error = StorageError;

    fn try_from(row: RunEventRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: RunEventId::try_from(row.id)?,
            run_id: ConversationRunId::try_from(row.run_id)?,
            sequence: row.sequence,
            kind: RunEventKind::try_from(row.kind)?,
            payload_json: row.payload_json,
            created_at: timestamp(row.created_at, "RunEvent.created_at")?,
        })
    }
}
