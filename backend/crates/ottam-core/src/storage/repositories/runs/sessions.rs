use super::RunRepository;
use crate::storage::database::recorded_transaction;
use crate::storage::error::StorageError;
use crate::storage::models::{
    AgentSession, AgentSessionId, AgentSessionRow, AgentSessionStatus, ConversationId,
    ConversationRunId, NewAgentSessionRow, WorkspaceId,
};
use crate::storage::schema::{agent_sessions, conversation_runs};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

impl RunRepository {
    // ------------------------------------------------------------------
    // Agent sessions
    // ------------------------------------------------------------------

    pub fn create_session(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        conversation_id: &ConversationId,
        engine_id: &str,
        external_session_id: Option<&str>,
        source_scope_fingerprint: &str,
        now: DateTime<Utc>,
    ) -> Result<AgentSession, StorageError> {
        let id = AgentSessionId::new_v7();
        let id_value = id.to_string();
        let now_ms = now.timestamp_millis();
        diesel::insert_into(agent_sessions::table)
            .values(NewAgentSessionRow {
                id: &id_value,
                workspace_id: &workspace_id.to_string(),
                conversation_id: &conversation_id.to_string(),
                engine_id,
                external_session_id,
                source_scope_fingerprint,
                status: AgentSessionStatus::Active.as_storage_str(),
                created_at: now_ms,
                last_used_at: now_ms,
                ended_at: None,
                end_reason: None,
            })
            .execute(connection)?;
        Self::get_session(connection, &id)
    }

    pub fn get_session(
        connection: &mut SqliteConnection,
        session_id: &AgentSessionId,
    ) -> Result<AgentSession, StorageError> {
        Self::find_session(connection, session_id)?
            .ok_or_else(|| StorageError::not_found("AgentSession"))
    }

    pub fn find_session(
        connection: &mut SqliteConnection,
        session_id: &AgentSessionId,
    ) -> Result<Option<AgentSession>, StorageError> {
        agent_sessions::table
            .find(session_id.to_string())
            .select(AgentSessionRow::as_select())
            .first::<AgentSessionRow>(connection)
            .optional()?
            .map(TryInto::try_into)
            .transpose()
    }

    /// The active session the Conversation's latest run speaks to: that run's
    /// own session, else the most recently used active session of its engine.
    /// After an engine switch back (A → B → A) the conversation still holds an
    /// active B session, newer and possibly more recently used than A's.
    pub fn latest_active_session(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
    ) -> Result<Option<AgentSession>, StorageError> {
        let Some((engine_id, run_session_id)) = conversation_runs::table
            .filter(conversation_runs::conversation_id.eq(conversation_id.to_string()))
            .order(conversation_runs::id.desc())
            .select((
                conversation_runs::engine_id,
                conversation_runs::agent_session_id,
            ))
            .first::<(String, Option<String>)>(connection)
            .optional()?
        else {
            return Ok(None);
        };
        let active = || {
            agent_sessions::table
                .filter(agent_sessions::conversation_id.eq(conversation_id.to_string()))
                .filter(agent_sessions::status.eq(AgentSessionStatus::Active.as_storage_str()))
        };
        if let Some(run_session_id) = run_session_id {
            if let Some(row) = active()
                .filter(agent_sessions::id.eq(run_session_id))
                .select(AgentSessionRow::as_select())
                .first::<AgentSessionRow>(connection)
                .optional()?
            {
                return row.try_into().map(Some);
            }
        }
        active()
            .filter(agent_sessions::engine_id.eq(engine_id))
            .order(agent_sessions::last_used_at.desc())
            .select(AgentSessionRow::as_select())
            .first::<AgentSessionRow>(connection)
            .optional()?
            .map(TryInto::try_into)
            .transpose()
    }

    /// The resumable session for a Conversation, engine, and source scope:
    /// active and fingerprint-matched, per the spec's resume rule.
    pub fn resolve_resumable_session(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        engine_id: &str,
        fingerprint: &str,
    ) -> Result<Option<AgentSession>, StorageError> {
        agent_sessions::table
            .filter(agent_sessions::conversation_id.eq(conversation_id.to_string()))
            .filter(agent_sessions::engine_id.eq(engine_id))
            .filter(agent_sessions::source_scope_fingerprint.eq(fingerprint))
            .filter(agent_sessions::status.eq(AgentSessionStatus::Active.as_storage_str()))
            .select(AgentSessionRow::as_select())
            .first::<AgentSessionRow>(connection)
            .optional()?
            .map(TryInto::try_into)
            .transpose()
    }

    /// The active session already bound to an adapter's external session id,
    /// if one exists for this Conversation and engine.
    pub fn find_active_session_by_external(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        engine_id: &str,
        external_session_id: &str,
    ) -> Result<Option<AgentSession>, StorageError> {
        agent_sessions::table
            .filter(agent_sessions::conversation_id.eq(conversation_id.to_string()))
            .filter(agent_sessions::engine_id.eq(engine_id))
            .filter(agent_sessions::external_session_id.eq(external_session_id))
            .filter(agent_sessions::status.eq(AgentSessionStatus::Active.as_storage_str()))
            .select(AgentSessionRow::as_select())
            .first::<AgentSessionRow>(connection)
            .optional()?
            .map(TryInto::try_into)
            .transpose()
    }

    /// The active session holding an adapter's external session id anywhere on
    /// this engine. A restarted adapter can reissue an id another Conversation
    /// still holds; the caller retires that stale holder before rebinding.
    pub fn find_active_session_by_external_engine_wide(
        connection: &mut SqliteConnection,
        engine_id: &str,
        external_session_id: &str,
    ) -> Result<Option<AgentSession>, StorageError> {
        agent_sessions::table
            .filter(agent_sessions::engine_id.eq(engine_id))
            .filter(agent_sessions::external_session_id.eq(external_session_id))
            .filter(agent_sessions::status.eq(AgentSessionStatus::Active.as_storage_str()))
            .select(AgentSessionRow::as_select())
            .first::<AgentSessionRow>(connection)
            .optional()?
            .map(TryInto::try_into)
            .transpose()
    }

    /// Record the mode the agent confirmed for an active session. An ended
    /// session keeps the mode it ended with.
    pub fn set_session_mode(
        connection: &mut SqliteConnection,
        session_id: &AgentSessionId,
        mode_id: &str,
    ) -> Result<(), StorageError> {
        diesel::update(
            agent_sessions::table
                .find(session_id.to_string())
                .filter(agent_sessions::status.eq(AgentSessionStatus::Active.as_storage_str())),
        )
        .set(agent_sessions::current_mode_id.eq(mode_id))
        .execute(connection)?;
        Ok(())
    }

    /// Record the commands the agent last announced for an active session,
    /// as a JSON array. An ended session keeps the commands it ended with.
    pub fn set_session_commands(
        connection: &mut SqliteConnection,
        session_id: &AgentSessionId,
        commands: &str,
    ) -> Result<(), StorageError> {
        diesel::update(
            agent_sessions::table
                .find(session_id.to_string())
                .filter(agent_sessions::status.eq(AgentSessionStatus::Active.as_storage_str())),
        )
        .set(agent_sessions::available_commands.eq(commands))
        .execute(connection)?;
        Ok(())
    }

    /// Record how full an active session's context was when the agent last
    /// reported, as JSON. An ended session keeps its last value.
    pub fn set_session_context_usage(
        connection: &mut SqliteConnection,
        session_id: &AgentSessionId,
        context_usage: &str,
    ) -> Result<(), StorageError> {
        diesel::update(
            agent_sessions::table
                .find(session_id.to_string())
                .filter(agent_sessions::status.eq(AgentSessionStatus::Active.as_storage_str())),
        )
        .set(agent_sessions::context_usage.eq(context_usage))
        .execute(connection)?;
        Ok(())
    }

    /// Record that a session has the conversation's earlier turns, and what
    /// the run that sent them replayed. Seeds once: returns whether this call
    /// seeded it, and a second call is a no-op.
    pub fn mark_history_seeded(
        connection: &mut SqliteConnection,
        session_id: &AgentSessionId,
        run_id: &ConversationRunId,
        replay_json: Option<&str>,
    ) -> Result<bool, StorageError> {
        recorded_transaction(connection, "conversation.seedHistory", |connection| {
            let seeded = diesel::update(
                agent_sessions::table
                    .find(session_id.to_string())
                    .filter(agent_sessions::history_seeded.eq(false)),
            )
            .set(agent_sessions::history_seeded.eq(true))
            .execute(connection)?;
            if seeded == 1 {
                if let Some(replay_json) = replay_json {
                    diesel::update(conversation_runs::table.find(run_id.to_string()))
                        .set(conversation_runs::history_replay_json.eq(replay_json))
                        .execute(connection)?;
                }
            }
            Ok(seeded == 1)
        })
    }

    /// Undo [`Self::mark_history_seeded`] for a run whose prompt never
    /// reached the agent, so the session's next prompt carries the turns.
    pub fn unmark_history_seeded(
        connection: &mut SqliteConnection,
        session_id: &AgentSessionId,
        run_id: &ConversationRunId,
    ) -> Result<(), StorageError> {
        recorded_transaction(connection, "conversation.unseedHistory", |connection| {
            diesel::update(agent_sessions::table.find(session_id.to_string()))
                .set(agent_sessions::history_seeded.eq(false))
                .execute(connection)?;
            diesel::update(conversation_runs::table.find(run_id.to_string()))
                .set(conversation_runs::history_replay_json.eq(None::<String>))
                .execute(connection)?;
            Ok(())
        })
    }

    /// A missing, expired, or lost external session ends the AgentSession
    /// instead of mutating historical identity.
    pub fn end_session(
        connection: &mut SqliteConnection,
        session_id: &AgentSessionId,
        status: AgentSessionStatus,
        end_reason: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<AgentSession, StorageError> {
        if status == AgentSessionStatus::Active {
            return Err(StorageError::new(
                "storageConstraint",
                "an AgentSession cannot be ended into the active status",
            ));
        }
        let changed = diesel::update(
            agent_sessions::table
                .find(session_id.to_string())
                .filter(agent_sessions::status.eq(AgentSessionStatus::Active.as_storage_str())),
        )
        .set((
            agent_sessions::status.eq(status.as_storage_str()),
            agent_sessions::ended_at.eq(now.timestamp_millis()),
            agent_sessions::end_reason.eq(end_reason),
        ))
        .execute(connection)?;
        if changed == 0 {
            return Err(StorageError::new(
                "storageConstraint",
                "only an active AgentSession can be ended",
            )
            .recoverable(true));
        }
        Self::get_session(connection, session_id)
    }

    /// Deletion must retain every external session identity until adapter
    /// cleanup has been attempted. Unlike UI-facing history, this maintenance
    /// query is intentionally unpaged.
    pub fn all_sessions_for_cleanup(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
    ) -> Result<Vec<AgentSession>, StorageError> {
        agent_sessions::table
            .filter(agent_sessions::conversation_id.eq(conversation_id.to_string()))
            .order(agent_sessions::id.desc())
            .select(AgentSessionRow::as_select())
            .load::<AgentSessionRow>(connection)?
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }
}
