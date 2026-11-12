mod events;
mod queries;
mod sessions;

use crate::storage::database::recorded_transaction;
use crate::storage::error::StorageError;
use crate::storage::fingerprint::{source_scope_fingerprint, ScopeRoot};
use crate::storage::models::{
    AgentSessionId, AgentSessionStatus, ConversationId, ConversationRun,
    ConversationRunId, ConversationRunInput, ConversationRunInputId, ConversationRunRow,
    ConversationRunSource, ConversationRunSourceRow, DeliveryEvidence, DeliveryMode,
    InputResolution, NewConversationRunRow, NewConversationRunSourceRow, ProjectId,
    ProjectSourceId, RunInputKind, RunInputStatus, RunOrigin, RunSourceRole, RunStatus,
    SessionOrigin, WorkingRootKind, WorkspaceId,
};
use crate::storage::repositories::conversations::{
    ConversationRepository, MessageWithInput, JSON_PAYLOAD_MAX_BYTES,
};
use crate::storage::schema::{
    agent_sessions, conversation_run_inputs, conversation_run_sources, conversation_runs,
};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

pub struct RunRepository;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunSourceSnapshot<'a> {
    pub root_kind: WorkingRootKind,
    pub project_id: Option<&'a ProjectId>,
    pub source_id: Option<&'a ProjectSourceId>,
    pub canonical_path: &'a str,
    pub role: RunSourceRole,
}

#[derive(Debug, Clone)]
pub struct CreateRunParams<'a> {
    pub workspace_id: &'a WorkspaceId,
    pub conversation_id: &'a ConversationId,
    pub engine_id: &'a str,
    pub model_id: Option<&'a str>,
    pub request_options_json: Option<&'a str>,
    pub pending_input_id: &'a ConversationRunInputId,
    pub sources: &'a [RunSourceSnapshot<'a>],
}

/// An autonomous follow-up: work the agent started on its own, after
/// `continues_run_id` ended, on that run's session.
#[derive(Debug, Clone)]
pub struct AutonomousRunParams<'a> {
    pub conversation_id: &'a ConversationId,
    pub continues_run_id: &'a ConversationRunId,
}

/// How a run made from another starts; everything else is copied.
struct RunStart<'a> {
    status: RunStatus,
    started_at: Option<i64>,
    agent_session_id: Option<&'a str>,
    session_origin: Option<SessionOrigin>,
    origin: RunOrigin,
}

#[derive(Debug, Clone)]
pub struct FinalizeRunParams<'a> {
    pub run_id: &'a ConversationRunId,
    pub status: RunStatus,
    pub stop_reason: Option<&'a str>,
    pub error_json: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct InterruptFollowupParams<'a> {
    pub workspace_id: &'a WorkspaceId,
    pub conversation_id: &'a ConversationId,
    pub interrupted_run_id: &'a ConversationRunId,
    pub pending_input_id: &'a ConversationRunInputId,
    pub retain_session: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptFollowupRecord {
    pub interrupted_run: ConversationRun,
    pub followup_run: ConversationRun,
    pub input: ConversationRunInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryResolutionAction<'a> {
    Retry { idempotency_key: &'a str },
    MarkDelivered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryResolutionRecord {
    pub resolved_input: ConversationRunInput,
    pub retry: Option<MessageWithInput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryCounts {
    pub failed_runs: usize,
    pub ambiguous_inputs: usize,
}

fn ensure_json_cap(value: Option<&str>, label: &'static str) -> Result<(), StorageError> {
    if value.is_some_and(|value| value.len() > JSON_PAYLOAD_MAX_BYTES) {
        return Err(StorageError::new(
            "payloadTooLarge",
            format!("{label} exceeds the 64 KiB cap"),
        )
        .recoverable(true));
    }
    Ok(())
}

impl RunRepository {
    // ------------------------------------------------------------------
    // Runs
    // ------------------------------------------------------------------

    /// Pending RunInput validation plus Run creation, one-active-run
    /// enforcement, and the immutable source snapshot in one transaction.
    pub fn create_run_with_snapshot(
        connection: &mut SqliteConnection,
        params: &CreateRunParams<'_>,
        now: DateTime<Utc>,
    ) -> Result<ConversationRun, StorageError> {
        recorded_transaction(connection, "conversation.startRun", |connection| {
            Self::create_run_with_snapshot_in_transaction(connection, params, now)
        })
    }

    /// Run creation, immutable source snapshot, and the first delivery
    /// transition share one commit. A Core exit can therefore observe either
    /// the original pending input or a queued run with a sending input, never a
    /// queued run whose input was left pending.
    pub fn create_run_with_snapshot_and_mark_input_sending(
        connection: &mut SqliteConnection,
        params: &CreateRunParams<'_>,
        mode: DeliveryMode,
        now: DateTime<Utc>,
    ) -> Result<(ConversationRun, ConversationRunInput), StorageError> {
        recorded_transaction(connection, "conversation.startRun", |connection| {
            let run = Self::create_run_with_snapshot_in_transaction(connection, params, now)?;
            let input = Self::mark_input_sending_in_transaction(
                connection,
                params.conversation_id,
                params.pending_input_id,
                &run.id,
                mode,
                now,
            )?;
            Ok((run, input))
        })
    }

    fn create_run_with_snapshot_in_transaction(
        connection: &mut SqliteConnection,
        params: &CreateRunParams<'_>,
        now: DateTime<Utc>,
    ) -> Result<ConversationRun, StorageError> {
        ensure_json_cap(
            params.request_options_json,
            "ConversationRun request options",
        )?;
        let primary_count = params
            .sources
            .iter()
            .filter(|source| source.role == RunSourceRole::Primary)
            .count();
        if primary_count != 1 {
            return Err(StorageError::new(
                "storageConstraint",
                "a run requires exactly one primary source root",
            ));
        }

        let pending = ConversationRepository::get_input(
            connection,
            params.conversation_id,
            params.pending_input_id,
        )?;
        if pending.status() != RunInputStatus::Pending {
            return Err(StorageError::new(
                "inputTransitionInvalid",
                "the named ConversationRunInput is not pending",
            )
            .recoverable(true));
        }

        if Self::active_run(connection, params.conversation_id)?.is_some() {
            return Err(StorageError::new(
                "runAlreadyActive",
                "the Conversation already has an active run",
            )
            .recoverable(true));
        }

        let run_id = ConversationRunId::new_v7();
        let run_id_value = run_id.to_string();
        let workspace_id_value = params.workspace_id.to_string();
        let conversation_id_value = params.conversation_id.to_string();
        let now_ms = now.timestamp_millis();
        diesel::insert_into(conversation_runs::table)
            .values(NewConversationRunRow {
                id: &run_id_value,
                workspace_id: &workspace_id_value,
                conversation_id: &conversation_id_value,
                engine_id: params.engine_id,
                model_id: params.model_id,
                agent_session_id: None,
                session_origin: None,
                status: RunStatus::Queued.as_storage_str(),
                request_options_json: params.request_options_json,
                event_sequence_cursor: 0,
                queued_at: now_ms,
                started_at: None,
                completed_at: None,
                stop_reason: None,
                error_json: None,
                origin: RunOrigin::User.as_storage_str(),
            })
            .execute(connection)?;
        Self::insert_sources(
            connection,
            &run_id,
            params.workspace_id,
            params.conversation_id,
            params.sources,
            now,
        )?;
        Self::get_run(connection, &run_id)
    }

    /// Opens an autonomous follow-up of a run whose session the agent kept
    /// using after it ended. The new run starts `running` on that session,
    /// with the continued run's engine, model, request options, and sources,
    /// so it resumes the same scope. It is refused while the session is not
    /// active or the Conversation already has an active run.
    pub fn create_autonomous_run(
        connection: &mut SqliteConnection,
        params: &AutonomousRunParams<'_>,
        now: DateTime<Utc>,
    ) -> Result<ConversationRun, StorageError> {
        recorded_transaction(connection, "conversation.openAutonomousRun", |connection| {
            let continued = Self::get_run(connection, params.continues_run_id)?;
            if continued.conversation_id != *params.conversation_id {
                return Err(StorageError::new(
                    "storageConstraint",
                    "the continued run belongs to another Conversation",
                ));
            }
            let session = match &continued.agent_session_id {
                Some(session_id) => Some(Self::get_session(connection, session_id)?),
                None => None,
            };
            let Some(session) =
                session.filter(|session| session.status == AgentSessionStatus::Active)
            else {
                return Err(StorageError::new(
                    "sessionNotFound",
                    "the continued run's session is no longer active",
                )
                .recoverable(true));
            };
            if Self::active_run(connection, params.conversation_id)?.is_some() {
                return Err(StorageError::new(
                    "runAlreadyActive",
                    "the Conversation already has an active run",
                )
                .recoverable(true));
            }
            let session_id = session.id.to_string();
            let run_id = Self::insert_run_like(
                connection,
                &continued,
                RunStart {
                    status: RunStatus::Running,
                    started_at: Some(now.timestamp_millis()),
                    agent_session_id: Some(&session_id),
                    session_origin: Some(SessionOrigin::Continued),
                    origin: RunOrigin::Autonomous,
                },
                now,
            )?;
            Self::get_run(connection, &run_id)
        })
    }

    /// Inserts a run that repeats `template`'s engine, model, request options,
    /// and source snapshot.
    fn insert_run_like(
        connection: &mut SqliteConnection,
        template: &ConversationRun,
        start: RunStart<'_>,
        now: DateTime<Utc>,
    ) -> Result<ConversationRunId, StorageError> {
        let run_id = ConversationRunId::new_v7();
        let run_id_value = run_id.to_string();
        let workspace_id = template.workspace_id.to_string();
        let conversation_id = template.conversation_id.to_string();
        let now_ms = now.timestamp_millis();
        diesel::insert_into(conversation_runs::table)
            .values(NewConversationRunRow {
                id: &run_id_value,
                workspace_id: &workspace_id,
                conversation_id: &conversation_id,
                engine_id: &template.engine_id,
                model_id: template.model_id.as_deref(),
                agent_session_id: start.agent_session_id,
                session_origin: start.session_origin.map(SessionOrigin::as_storage_str),
                status: start.status.as_storage_str(),
                request_options_json: template.request_options_json.as_deref(),
                event_sequence_cursor: 0,
                queued_at: now_ms,
                started_at: start.started_at,
                completed_at: None,
                stop_reason: None,
                error_json: None,
                origin: start.origin.as_storage_str(),
            })
            .execute(connection)?;
        for source in Self::run_sources(connection, &template.id)? {
            diesel::insert_into(conversation_run_sources::table)
                .values(NewConversationRunSourceRow {
                    run_id: &run_id_value,
                    workspace_id: &workspace_id,
                    conversation_id: &conversation_id,
                    root_kind: source.root_kind.as_storage_str(),
                    project_id: source.project_id.map(|id| id.to_string()).as_deref(),
                    source_id: source.source_id.map(|id| id.to_string()).as_deref(),
                    canonical_path: &source.canonical_path,
                    role: source.role.as_storage_str(),
                    created_at: now_ms,
                })
                .execute(connection)?;
        }
        Ok(run_id)
    }

    fn insert_sources(
        connection: &mut SqliteConnection,
        run_id: &ConversationRunId,
        workspace_id: &WorkspaceId,
        conversation_id: &ConversationId,
        sources: &[RunSourceSnapshot<'_>],
        now: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        let run_id_value = run_id.to_string();
        let workspace_id_value = workspace_id.to_string();
        let conversation_id_value = conversation_id.to_string();
        let now_ms = now.timestamp_millis();
        for source in sources {
            let project_id = source.project_id.map(ToString::to_string);
            let source_id = source.source_id.map(ToString::to_string);
            match source.root_kind {
                WorkingRootKind::Project if project_id.is_none() || source_id.is_none() => {
                    return Err(StorageError::new(
                        "storageConstraint",
                        "a Project run source requires Project and ProjectSource identity",
                    ));
                }
                WorkingRootKind::WorkspaceFiles if project_id.is_some() || source_id.is_some() => {
                    return Err(StorageError::new(
                        "storageConstraint",
                        "a Workspace-files run source cannot carry Project identity",
                    ));
                }
                _ => {}
            }
            diesel::insert_into(conversation_run_sources::table)
                .values(NewConversationRunSourceRow {
                    run_id: &run_id_value,
                    workspace_id: &workspace_id_value,
                    conversation_id: &conversation_id_value,
                    root_kind: source.root_kind.as_storage_str(),
                    project_id: project_id.as_deref(),
                    source_id: source_id.as_deref(),
                    canonical_path: source.canonical_path,
                    role: source.role.as_storage_str(),
                    created_at: now_ms,
                })
                .execute(connection)?;
        }
        Ok(())
    }

    pub fn get_run(
        connection: &mut SqliteConnection,
        run_id: &ConversationRunId,
    ) -> Result<ConversationRun, StorageError> {
        conversation_runs::table
            .find(run_id.to_string())
            .select(ConversationRunRow::as_select())
            .first::<ConversationRunRow>(connection)
            .optional()?
            .ok_or_else(|| StorageError::not_found("ConversationRun"))?
            .try_into()
    }

    pub fn active_run(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
    ) -> Result<Option<ConversationRun>, StorageError> {
        conversation_runs::table
            .filter(conversation_runs::conversation_id.eq(conversation_id.to_string()))
            .filter(conversation_runs::status.eq_any([
                RunStatus::Queued.as_storage_str(),
                RunStatus::Running.as_storage_str(),
            ]))
            .select(ConversationRunRow::as_select())
            .first::<ConversationRunRow>(connection)
            .optional()?
            .map(TryInto::try_into)
            .transpose()
    }

    pub fn run_sources(
        connection: &mut SqliteConnection,
        run_id: &ConversationRunId,
    ) -> Result<Vec<ConversationRunSource>, StorageError> {
        conversation_run_sources::table
            .filter(conversation_run_sources::run_id.eq(run_id.to_string()))
            .order(conversation_run_sources::canonical_path.asc())
            .select(ConversationRunSourceRow::as_select())
            .load::<ConversationRunSourceRow>(connection)?
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }

    fn run_scope_fingerprint(
        connection: &mut SqliteConnection,
        run_id: &ConversationRunId,
    ) -> Result<String, StorageError> {
        let sources = Self::run_sources(connection, run_id)?;
        let source_ids: Vec<Option<String>> = sources
            .iter()
            .map(|source| source.source_id.map(|id| id.to_string()))
            .collect();
        let roots: Vec<ScopeRoot<'_>> = sources
            .iter()
            .zip(source_ids.iter())
            .map(|(source, source_id)| ScopeRoot {
                kind: source.root_kind,
                source_id: source_id.as_deref(),
                canonical_path: &source.canonical_path,
            })
            .collect();
        source_scope_fingerprint(&roots)
    }

    /// A run is resumed only when its engine and source-scope fingerprint
    /// match the AgentSession; mismatch is rejected instead of silently
    /// creating cross-scope continuity.
    pub fn mark_run_running(
        connection: &mut SqliteConnection,
        run_id: &ConversationRunId,
        session_id: &AgentSessionId,
        origin: SessionOrigin,
        now: DateTime<Utc>,
    ) -> Result<ConversationRun, StorageError> {
        recorded_transaction(connection, "conversation.markRunRunning", |connection| {
            let run = Self::get_run(connection, run_id)?;
            if run.status != RunStatus::Queued {
                return Err(StorageError::new(
                    "storageConstraint",
                    "only a queued run can start running",
                )
                .recoverable(true));
            }
            let session = Self::get_session(connection, session_id)?;
            if session.engine_id != run.engine_id || session.conversation_id != run.conversation_id
            {
                return Err(StorageError::new(
                    "storageConstraint",
                    "the AgentSession does not belong to the run's Conversation and engine",
                ));
            }
            let run_fingerprint = Self::run_scope_fingerprint(connection, run_id)?;
            if session.source_scope_fingerprint != run_fingerprint {
                return Err(StorageError::new(
                    "sessionScopeMismatch",
                    "the AgentSession source scope does not match the run's sources",
                )
                .recoverable(true));
            }
            diesel::update(conversation_runs::table.find(run_id.to_string()))
                .set((
                    conversation_runs::status.eq(RunStatus::Running.as_storage_str()),
                    conversation_runs::started_at.eq(now.timestamp_millis()),
                    conversation_runs::agent_session_id.eq(session_id.to_string()),
                    conversation_runs::session_origin.eq(origin.as_storage_str()),
                ))
                .execute(connection)?;
            Self::get_run(connection, run_id)
        })
    }

    /// Re-bind a running run onto a different AgentSession (the prompt-retry
    /// path after the previously continued session was found to be gone).
    pub fn update_run_session(
        connection: &mut SqliteConnection,
        run_id: &ConversationRunId,
        session_id: &AgentSessionId,
        origin: SessionOrigin,
    ) -> Result<ConversationRun, StorageError> {
        recorded_transaction(connection, "conversation.updateRunSession", |connection| {
            let run = Self::get_run(connection, run_id)?;
            if run.status != RunStatus::Running {
                return Err(StorageError::new(
                    "storageConstraint",
                    "only a running run can be re-bound to a new AgentSession",
                )
                .recoverable(true));
            }
            let session = Self::get_session(connection, session_id)?;
            if session.engine_id != run.engine_id || session.conversation_id != run.conversation_id
            {
                return Err(StorageError::new(
                    "storageConstraint",
                    "the AgentSession does not belong to the run's Conversation and engine",
                ));
            }
            let run_fingerprint = Self::run_scope_fingerprint(connection, run_id)?;
            if session.source_scope_fingerprint != run_fingerprint {
                return Err(StorageError::new(
                    "sessionScopeMismatch",
                    "the AgentSession source scope does not match the run's sources",
                )
                .recoverable(true));
            }
            diesel::update(conversation_runs::table.find(run_id.to_string()))
                .set((
                    conversation_runs::agent_session_id.eq(session_id.to_string()),
                    conversation_runs::session_origin.eq(origin.as_storage_str()),
                ))
                .execute(connection)?;
            Self::get_run(connection, run_id)
        })
    }

    // ------------------------------------------------------------------
    // Run input delivery state machine
    // ------------------------------------------------------------------

    /// Atomically assign the target Run, next delivery sequence, and mode,
    /// and durably enter `sending` before any adapter I/O.
    pub fn mark_input_sending(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        input_id: &ConversationRunInputId,
        target_run_id: &ConversationRunId,
        mode: DeliveryMode,
        now: DateTime<Utc>,
    ) -> Result<ConversationRunInput, StorageError> {
        recorded_transaction(connection, "conversation.inputSending", |connection| {
            Self::mark_input_sending_in_transaction(
                connection,
                conversation_id,
                input_id,
                target_run_id,
                mode,
                now,
            )
        })
    }

    /// Adapter-confirmed acceptance; also the AgentSession's
    /// last-successfully-delivered-input timestamp.
    pub fn mark_input_delivered(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        input_id: &ConversationRunInputId,
        now: DateTime<Utc>,
    ) -> Result<ConversationRunInput, StorageError> {
        recorded_transaction(connection, "conversation.inputDelivered", |connection| {
            let input = ConversationRepository::get_input(connection, conversation_id, input_id)?;
            if input.status() != RunInputStatus::Sending {
                return Err(StorageError::new(
                    "inputTransitionInvalid",
                    "only a sending ConversationRunInput can be adapter-confirmed",
                )
                .recoverable(true));
            }
            diesel::update(conversation_run_inputs::table.find(input_id.to_string()))
                .set((
                    conversation_run_inputs::status.eq(RunInputStatus::Delivered.as_storage_str()),
                    conversation_run_inputs::delivery_evidence
                        .eq(DeliveryEvidence::AdapterConfirmed.as_storage_str()),
                    conversation_run_inputs::delivered_at.eq(now.timestamp_millis()),
                ))
                .execute(connection)?;
            if let Some(target_run_id) = input.target_run_id() {
                let run = Self::get_run(connection, &target_run_id)?;
                if let Some(session_id) = run.agent_session_id {
                    diesel::update(agent_sessions::table.find(session_id.to_string()))
                        .set(agent_sessions::last_used_at.eq(now.timestamp_millis()))
                        .execute(connection)?;
                }
            }
            ConversationRepository::get_input(connection, conversation_id, input_id)
        })
    }

    /// The adapter definitively did not take the input: it answered, and the
    /// answer was that there was nothing to deliver into. Returns the input to
    /// `pending` with its assignment cleared, so it is indistinguishable from
    /// one that was never attempted and any delivery path can pick it up.
    ///
    /// Distinct from `mark_input_delivery_unknown`, which is for an answer that
    /// never came. Here the message is known not to have been delivered.
    pub fn mark_input_undelivered(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        input_id: &ConversationRunInputId,
    ) -> Result<ConversationRunInput, StorageError> {
        recorded_transaction(connection, "conversation.inputUndelivered", |connection| {
            let input = ConversationRepository::get_input(connection, conversation_id, input_id)?;
            if input.status() != RunInputStatus::Sending {
                return Err(StorageError::new(
                    "inputTransitionInvalid",
                    "only a sending ConversationRunInput can return to pending",
                )
                .recoverable(true));
            }
            diesel::update(conversation_run_inputs::table.find(input_id.to_string()))
                .set((
                    conversation_run_inputs::status.eq(RunInputStatus::Pending.as_storage_str()),
                    conversation_run_inputs::target_run_id.eq(Option::<String>::None),
                    conversation_run_inputs::delivery_sequence.eq(Option::<i64>::None),
                    conversation_run_inputs::delivery_mode.eq(Option::<String>::None),
                    conversation_run_inputs::delivery_attempted_at.eq(Option::<i64>::None),
                ))
                .execute(connection)?;
            ConversationRepository::get_input(connection, conversation_id, input_id)
        })
    }

    /// A definitive adapter rejection before acceptance. Transport errors
    /// without a durable negative acknowledgement must use
    /// `mark_input_delivery_unknown` instead.
    pub fn mark_input_failed(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        input_id: &ConversationRunInputId,
        failure_reason: &str,
        now: DateTime<Utc>,
    ) -> Result<ConversationRunInput, StorageError> {
        recorded_transaction(connection, "conversation.inputFailed", |connection| {
            let input = ConversationRepository::get_input(connection, conversation_id, input_id)?;
            if !matches!(
                input.status(),
                RunInputStatus::Pending | RunInputStatus::Sending
            ) {
                return Err(StorageError::new(
                    "inputTransitionInvalid",
                    "only a pending or sending ConversationRunInput can fail",
                )
                .recoverable(true));
            }
            diesel::update(conversation_run_inputs::table.find(input_id.to_string()))
                .set((
                    conversation_run_inputs::status.eq(RunInputStatus::Failed.as_storage_str()),
                    conversation_run_inputs::failed_at.eq(now.timestamp_millis()),
                    conversation_run_inputs::failure_reason.eq(failure_reason),
                ))
                .execute(connection)?;
            ConversationRepository::get_input(connection, conversation_id, input_id)
        })
    }

    /// Resolve a definitive adapter launch failure in one transaction. A retry
    /// was never offered to the agent and returns to `pending`; an ordinary
    /// input becomes `failed`.
    ///
    /// Returns `true` when a retry was requeued and `false` when a non-retry
    /// input was failed.
    pub fn resolve_input_after_launch_failure(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        input_id: &ConversationRunInputId,
        failure_reason: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, StorageError> {
        recorded_transaction(
            connection,
            "conversation.launchFailureResolved",
            |connection| {
                let input =
                    ConversationRepository::get_input(connection, conversation_id, input_id)?;
                if !matches!(
                    input.status(),
                    RunInputStatus::Pending | RunInputStatus::Sending
                ) {
                    return Err(StorageError::new(
                        "inputTransitionInvalid",
                        "only a pending or sending input can resolve an adapter launch failure",
                    )
                    .recoverable(true));
                }
                if input.kind == RunInputKind::Retry {
                    diesel::update(conversation_run_inputs::table.find(input_id.to_string()))
                        .set((
                            conversation_run_inputs::status
                                .eq(RunInputStatus::Pending.as_storage_str()),
                            conversation_run_inputs::target_run_id.eq(Option::<String>::None),
                            conversation_run_inputs::delivery_sequence.eq(Option::<i64>::None),
                            conversation_run_inputs::delivery_mode.eq(Option::<String>::None),
                            conversation_run_inputs::delivery_attempted_at.eq(Option::<i64>::None),
                        ))
                        .execute(connection)?;
                    return Ok(true);
                }
                diesel::update(conversation_run_inputs::table.find(input_id.to_string()))
                    .set((
                        conversation_run_inputs::status.eq(RunInputStatus::Failed.as_storage_str()),
                        conversation_run_inputs::failed_at.eq(now.timestamp_millis()),
                        conversation_run_inputs::failure_reason.eq(failure_reason),
                    ))
                    .execute(connection)?;
                Ok(false)
            },
        )
    }

    /// A transport error after `sending` without a durable negative
    /// acknowledgement: honest ambiguity, never assumed delivered or failed.
    pub fn mark_input_delivery_unknown(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        input_id: &ConversationRunInputId,
        now: DateTime<Utc>,
    ) -> Result<ConversationRunInput, StorageError> {
        recorded_transaction(connection, "conversation.inputUnknown", |connection| {
            let input = ConversationRepository::get_input(connection, conversation_id, input_id)?;
            if input.status() != RunInputStatus::Sending {
                return Err(StorageError::new(
                    "inputTransitionInvalid",
                    "only a sending ConversationRunInput can become deliveryUnknown",
                )
                .recoverable(true));
            }
            diesel::update(conversation_run_inputs::table.find(input_id.to_string()))
                .set((
                    conversation_run_inputs::status
                        .eq(RunInputStatus::DeliveryUnknown.as_storage_str()),
                    conversation_run_inputs::ambiguity_detected_at.eq(now.timestamp_millis()),
                ))
                .execute(connection)?;
            ConversationRepository::get_input(connection, conversation_id, input_id)
        })
    }

    /// Explicit user resolution of an input that never reached the agent.
    ///
    /// Retry atomically supersedes the unresolved row and creates a linked
    /// retry input reusing the immutable user Message; Mark Delivered records
    /// the user's resolution without claiming an adapter receipt.
    ///
    /// Retry accepts a `failed` input as well as a `deliveryUnknown` one: a
    /// definitive pre-acceptance rejection — an expired credential, most of all
    /// — is the case where re-sending is *most* obviously safe, since the
    /// message provably never reached the agent. Mark Delivered stays
    /// `deliveryUnknown`-only; there is no ambiguity to resolve on a failure the
    /// adapter itself reported.
    pub fn resolve_undelivered_input(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        conversation_id: &ConversationId,
        input_id: &ConversationRunInputId,
        action: DeliveryResolutionAction<'_>,
        now: DateTime<Utc>,
    ) -> Result<DeliveryResolutionRecord, StorageError> {
        recorded_transaction(connection, "conversation.resolveInput", |connection| {
            let input = ConversationRepository::get_input(connection, conversation_id, input_id)?;
            let resolvable = match action {
                DeliveryResolutionAction::Retry { .. } => matches!(
                    input.status(),
                    RunInputStatus::DeliveryUnknown | RunInputStatus::Failed
                ),
                DeliveryResolutionAction::MarkDelivered => {
                    input.status() == RunInputStatus::DeliveryUnknown
                }
            };
            if !resolvable {
                return Err(StorageError::new(
                    "inputTransitionInvalid",
                    match action {
                        DeliveryResolutionAction::Retry { .. } => {
                            "only a deliveryUnknown or failed ConversationRunInput can be retried"
                        }
                        DeliveryResolutionAction::MarkDelivered => {
                            "only a deliveryUnknown ConversationRunInput can be marked delivered"
                        }
                    },
                )
                .recoverable(true));
            }
            // A retry has nowhere to run while the Conversation is busy, and
            // creating one anyway is worse than refusing: the original is
            // superseded, the retry cannot start, and the Conversation is left
            // holding a pending input that blocks every further send until
            // someone starts it by hand. Refusing keeps the failed row intact
            // and re-offerable once the run in the way finishes.
            //
            // Only `failed` is checked. A `deliveryUnknown` input already
            // blocks new runs through `has_unresolved_delivery`, so no *later*
            // run can be in its way; the run it belongs to is its own case and
            // keeps the resolution semantics it has always had.
            //
            // An autonomous run is not in the way: the retry's start preempts it.
            if matches!(action, DeliveryResolutionAction::Retry { .. })
                && input.status() == RunInputStatus::Failed
                && Self::active_run(connection, conversation_id)?
                    .is_some_and(|run| run.origin == RunOrigin::User)
            {
                return Err(StorageError::new(
                    "runAlreadyActive",
                    "the conversation has an active run; retry it once that run finishes",
                )
                .recoverable(true));
            }
            let now_ms = now.timestamp_millis();
            match action {
                DeliveryResolutionAction::MarkDelivered => {
                    diesel::update(conversation_run_inputs::table.find(input_id.to_string()))
                        .set((
                            conversation_run_inputs::status
                                .eq(RunInputStatus::Delivered.as_storage_str()),
                            conversation_run_inputs::delivery_evidence
                                .eq(DeliveryEvidence::UserMarkedDelivered.as_storage_str()),
                            conversation_run_inputs::delivered_at.eq(now_ms),
                            conversation_run_inputs::resolution
                                .eq(InputResolution::MarkedDelivered.as_storage_str()),
                            conversation_run_inputs::resolved_at.eq(now_ms),
                        ))
                        .execute(connection)?;
                    Ok(DeliveryResolutionRecord {
                        resolved_input: ConversationRepository::get_input(
                            connection,
                            conversation_id,
                            input_id,
                        )?,
                        retry: None,
                    })
                }
                DeliveryResolutionAction::Retry { idempotency_key } => {
                    let retry_input_id = ConversationRunInputId::new_v7();
                    let message = ConversationRepository::get_message(
                        connection,
                        conversation_id,
                        &input.message_id,
                    )?;
                    diesel::insert_into(conversation_run_inputs::table)
                        .values((
                            conversation_run_inputs::id.eq(retry_input_id.to_string()),
                            conversation_run_inputs::workspace_id.eq(workspace_id.to_string()),
                            conversation_run_inputs::conversation_id
                                .eq(conversation_id.to_string()),
                            conversation_run_inputs::message_id.eq(message.id.to_string()),
                            conversation_run_inputs::kind.eq(RunInputKind::Retry.as_storage_str()),
                            conversation_run_inputs::retry_of_input_id.eq(input_id.to_string()),
                            conversation_run_inputs::idempotency_key
                                .eq(idempotency_key.to_string()),
                            conversation_run_inputs::status
                                .eq(RunInputStatus::Pending.as_storage_str()),
                            conversation_run_inputs::requested_at.eq(now_ms),
                        ))
                        .execute(connection)?;
                    diesel::update(conversation_run_inputs::table.find(input_id.to_string()))
                        .set((
                            conversation_run_inputs::status
                                .eq(RunInputStatus::Superseded.as_storage_str()),
                            conversation_run_inputs::resolution
                                .eq(InputResolution::RetryCreated.as_storage_str()),
                            conversation_run_inputs::resolved_at.eq(now_ms),
                        ))
                        .execute(connection)?;
                    Ok(DeliveryResolutionRecord {
                        resolved_input: ConversationRepository::get_input(
                            connection,
                            conversation_id,
                            input_id,
                        )?,
                        retry: Some(MessageWithInput {
                            message,
                            input: ConversationRepository::get_input(
                                connection,
                                conversation_id,
                                &retry_input_id,
                            )?,
                            replayed: false,
                        }),
                    })
                }
            }
        })
    }

    pub fn has_unresolved_delivery(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
    ) -> Result<bool, StorageError> {
        let count: i64 = conversation_run_inputs::table
            .filter(conversation_run_inputs::conversation_id.eq(conversation_id.to_string()))
            .filter(
                conversation_run_inputs::status
                    .eq(RunInputStatus::DeliveryUnknown.as_storage_str()),
            )
            .count()
            .get_result(connection)?;
        Ok(count > 0)
    }

    // ------------------------------------------------------------------
    // Finalization and recovery
    // ------------------------------------------------------------------

    /// Run finalization plus optional assistant Message plus Conversation
    /// timestamp update in one transaction. Interrupt, failure,
    /// cancellation, or output-limit finalization persists non-empty
    /// accumulated output as `partial`.
    pub fn finalize_run(
        connection: &mut SqliteConnection,
        params: &FinalizeRunParams<'_>,
        now: DateTime<Utc>,
    ) -> Result<ConversationRun, StorageError> {
        recorded_transaction(connection, "conversation.finalizeRun", |connection| {
            Self::finalize_run_in_transaction(connection, params, now, true)
        })
    }

    /// Records the tokens a run's turn used, as JSON, unless it already has
    /// them. Open or closed alike: a stopped run's answer arrives after it
    /// ends. Returns whether this call wrote them.
    pub fn record_run_usage(
        connection: &mut SqliteConnection,
        run_id: &ConversationRunId,
        usage_json: &str,
    ) -> Result<bool, StorageError> {
        let written = diesel::update(
            conversation_runs::table
                .find(run_id.to_string())
                .filter(conversation_runs::usage_json.is_null()),
        )
        .set(conversation_runs::usage_json.eq(usage_json))
        .execute(connection)?;
        Ok(written == 1)
    }

    /// Live capture owns transcript revisions; execution still commits through
    /// the existing lifecycle transaction.
    pub fn finalize_run_with_live_capture(
        connection: &mut SqliteConnection,
        params: &FinalizeRunParams<'_>,
        now: DateTime<Utc>,
    ) -> Result<ConversationRun, StorageError> {
        recorded_transaction(connection, "conversation.finalizeRun", |connection| {
            Self::finalize_run_in_transaction(connection, params, now, false)
        })
    }

    fn finalize_run_in_transaction(
        connection: &mut SqliteConnection,
        params: &FinalizeRunParams<'_>,
        now: DateTime<Utc>,
        finalize_transcript: bool,
    ) -> Result<ConversationRun, StorageError> {
        if !matches!(
            params.status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Canceled | RunStatus::Interrupted
        ) {
            return Err(StorageError::new(
                "storageConstraint",
                "run finalization requires a terminal status",
            ));
        }
        ensure_json_cap(params.error_json, "ConversationRun error")?;
        let run = Self::get_run(connection, params.run_id)?;
        if !matches!(run.status, RunStatus::Queued | RunStatus::Running) {
            return Err(StorageError::new(
                "storageConstraint",
                "only an active run can be finalized",
            )
            .recoverable(true));
        }
        let now_ms = now.timestamp_millis();

        let status = params.status;
        let error_json = params.error_json.map(str::to_owned);
        let failure_kind = params
            .error_json
            .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
            .and_then(|value| {
                value
                    .get("kind")
                    .and_then(|kind| kind.as_str())
                    .map(str::to_owned)
            });
        if finalize_transcript
            && status == RunStatus::Failed
            && matches!(
                failure_kind.as_deref(),
                Some(
                    "authRequired"
                        | "authRefreshBusy"
                        | "invalidAuthMethod"
                        | "selectionUnavailable"
                        | "agentNotFound"
                        | "adapterNotInstalled"
                        | "runtimeMissing"
                        | "runtimeInvalid"
                        | "runtimeUnsupported"
                )
            )
        {
            super::transcript::TranscriptRepository::suppress_pre_agent_output(
                connection,
                &params.run_id.to_string(),
                now_ms,
            )?;
        }
        if finalize_transcript {
            super::transcript::TranscriptRepository::finish_run(
                connection,
                &params.run_id.to_string(),
                if status == RunStatus::Completed {
                    "complete"
                } else {
                    "partial"
                },
                now_ms,
            )?;
        }

        diesel::update(conversation_runs::table.find(params.run_id.to_string()))
            .set((
                conversation_runs::status.eq(status.as_storage_str()),
                conversation_runs::completed_at.eq(now_ms),
                conversation_runs::stop_reason.eq(params.stop_reason),
                conversation_runs::error_json.eq(error_json.as_deref()),
            ))
            .execute(connection)?;
        ConversationRepository::touch(connection, &run.conversation_id, now)?;
        Self::get_run(connection, params.run_id)
    }

    /// Interrupt finalization plus partial assistant Message plus follow-up
    /// Run, source snapshot, and pending-input assignment in one
    /// transaction, invoked only after adapter cancellation has resolved.
    pub fn interrupt_and_create_followup(
        connection: &mut SqliteConnection,
        params: &InterruptFollowupParams<'_>,
        now: DateTime<Utc>,
    ) -> Result<InterruptFollowupRecord, StorageError> {
        Self::interrupt_followup(connection, params, now, true)
    }

    pub fn interrupt_with_live_capture(
        connection: &mut SqliteConnection,
        params: &InterruptFollowupParams<'_>,
        now: DateTime<Utc>,
    ) -> Result<InterruptFollowupRecord, StorageError> {
        Self::interrupt_followup(connection, params, now, false)
    }

    fn interrupt_followup(
        connection: &mut SqliteConnection,
        params: &InterruptFollowupParams<'_>,
        now: DateTime<Utc>,
        finalize_transcript: bool,
    ) -> Result<InterruptFollowupRecord, StorageError> {
        recorded_transaction(connection, "conversation.interruptFollowup", |connection| {
            let old_run = Self::get_run(connection, params.interrupted_run_id)?;
            if old_run.conversation_id != *params.conversation_id {
                return Err(StorageError::new(
                    "storageConstraint",
                    "the interrupted run belongs to another Conversation",
                ));
            }
            let interrupted = match old_run.status {
                RunStatus::Queued | RunStatus::Running => Self::finalize_run_in_transaction(
                    connection,
                    &FinalizeRunParams {
                        run_id: params.interrupted_run_id,
                        status: RunStatus::Interrupted,
                        stop_reason: Some("interruptedForSteering"),
                        error_json: None,
                    },
                    now,
                    finalize_transcript,
                )?,
                RunStatus::Canceled => {
                    // The bridge waits for adapter cancellation to resolve
                    // before this transaction. Its ordinary worker may have
                    // already persisted the partial assistant Message while
                    // closing as canceled; reclassify that terminal run
                    // without inserting the output twice.
                    diesel::update(
                        conversation_runs::table.find(params.interrupted_run_id.to_string()),
                    )
                    .set((
                        conversation_runs::status.eq(RunStatus::Interrupted.as_storage_str()),
                        conversation_runs::stop_reason.eq("interruptedForSteering"),
                        conversation_runs::error_json.eq::<Option<String>>(None),
                    ))
                    .execute(connection)?;
                    // A prior cancellation checkpoint cannot certify the new
                    // interrupted boundary. Keep known loss, reopen otherwise.
                    diesel::sql_query("UPDATE transcript_capture SET state = CASE WHEN state = 'partial' THEN 'partial' ELSE 'open' END WHERE run_id = ?")
                        .bind::<diesel::sql_types::Text, _>(params.interrupted_run_id.to_string())
                        .execute(connection)?;
                    Self::get_run(connection, params.interrupted_run_id)?
                }
                _ => {
                    return Err(StorageError::new(
                        "runNoLongerActive",
                        "the expected run completed before steering delivery",
                    )
                    .recoverable(true));
                }
            };

            let followup_id = Self::insert_run_like(
                connection,
                &old_run,
                RunStart {
                    status: RunStatus::Queued,
                    started_at: None,
                    agent_session_id: None,
                    session_origin: None,
                    origin: RunOrigin::User,
                },
                now,
            )?;
            let _ = params.retain_session;

            let input = Self::mark_input_sending_in_transaction(
                connection,
                params.conversation_id,
                params.pending_input_id,
                &followup_id,
                DeliveryMode::InterruptThenPrompt,
                now,
            )?;

            Ok(InterruptFollowupRecord {
                interrupted_run: interrupted,
                followup_run: Self::get_run(connection, &followup_id)?,
                input,
            })
        })
    }

    /// Startup recovery in one transaction: active runs fail with a
    /// `coreRestarted` error and surviving `sending` inputs become visible
    /// `deliveryUnknown`; nothing is retried automatically.
    pub fn recover_on_startup(
        connection: &mut SqliteConnection,
        now: DateTime<Utc>,
    ) -> Result<RecoveryCounts, StorageError> {
        recorded_transaction(connection, "storage.recoverOnStartup", |connection| {
            let now_ms = now.timestamp_millis();
            let active = conversation_runs::table
                .filter(conversation_runs::status.eq_any(["queued", "running"]))
                .select(conversation_runs::id)
                .load::<String>(connection)?;
            for run in active {
                super::transcript::TranscriptRepository::finish_run(
                    connection, &run, "partial", now_ms,
                )?;
            }
            let failed_runs = diesel::update(
                conversation_runs::table.filter(conversation_runs::status.eq_any([
                    RunStatus::Queued.as_storage_str(),
                    RunStatus::Running.as_storage_str(),
                ])),
            )
            .set((
                conversation_runs::status.eq(RunStatus::Failed.as_storage_str()),
                conversation_runs::completed_at.eq(now_ms),
                conversation_runs::error_json.eq(
                    "{\"kind\":\"coreRestarted\",\"message\":\"the Core process restarted while the run was active\"}",
                ),
            ))
            .execute(connection)?;
            let ambiguous_inputs = diesel::update(conversation_run_inputs::table.filter(
                conversation_run_inputs::status.eq(RunInputStatus::Sending.as_storage_str()),
            ))
            .set((
                conversation_run_inputs::status
                    .eq(RunInputStatus::DeliveryUnknown.as_storage_str()),
                conversation_run_inputs::ambiguity_detected_at.eq(now_ms),
            ))
            .execute(connection)?;
            Ok(RecoveryCounts {
                failed_runs,
                ambiguous_inputs,
            })
        })
    }

    fn mark_input_sending_in_transaction(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        input_id: &ConversationRunInputId,
        target_run_id: &ConversationRunId,
        mode: DeliveryMode,
        now: DateTime<Utc>,
    ) -> Result<ConversationRunInput, StorageError> {
        let input = ConversationRepository::get_input(connection, conversation_id, input_id)?;
        if input.status() != RunInputStatus::Pending {
            return Err(StorageError::new(
                "inputTransitionInvalid",
                "only a pending ConversationRunInput can enter sending",
            )
            .recoverable(true));
        }
        let run = Self::get_run(connection, target_run_id)?;
        if run.conversation_id != *conversation_id {
            return Err(StorageError::new(
                "storageConstraint",
                "the target run belongs to another Conversation",
            ));
        }
        if !matches!(run.status, RunStatus::Queued | RunStatus::Running) {
            return Err(StorageError::new(
                "inputTransitionInvalid",
                "the target run is no longer active",
            )
            .recoverable(true));
        }
        let next_sequence: i64 = conversation_run_inputs::table
            .filter(conversation_run_inputs::target_run_id.eq(target_run_id.to_string()))
            .select(diesel::dsl::max(conversation_run_inputs::delivery_sequence))
            .first::<Option<i64>>(connection)?
            .map_or(1, |max| max + 1);
        diesel::update(conversation_run_inputs::table.find(input_id.to_string()))
            .set((
                conversation_run_inputs::status.eq(RunInputStatus::Sending.as_storage_str()),
                conversation_run_inputs::target_run_id.eq(target_run_id.to_string()),
                conversation_run_inputs::delivery_sequence.eq(next_sequence),
                conversation_run_inputs::delivery_mode.eq(mode.as_storage_str()),
                conversation_run_inputs::delivery_attempted_at.eq(now.timestamp_millis()),
            ))
            .execute(connection)?;
        ConversationRepository::get_input(connection, conversation_id, input_id)
    }
}
