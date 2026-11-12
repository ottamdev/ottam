use super::attachments::message_prompt_images;
use super::conversations::{
    active_conversation, conversation_run_checked, run_summary, working_root_summary,
};
use super::ids::{
    parse_attachment_id, parse_conversation_id, parse_input_id, parse_project_id, parse_run_id,
    parse_source_id,
};
use super::wire::{
    domain_event_kind, domain_origin, domain_run_status, event_to_delta, input_to_wire,
    stop_reason_value, RunRequestOptions,
};
use super::ChatService;
use crate::agent::HarnessPromptImage;
use crate::core_error::CoreError;
use crate::storage::database::recorded_transaction;
use crate::storage::error::StorageError;
use crate::storage::fingerprint::{source_scope_fingerprint, ScopeRoot};
use crate::storage::models::{
    AgentSession, AgentSessionId, AgentSessionStatus, ConversationId, ConversationProjectRole,
    ConversationRun, DeliveryMode, ProjectId, ProjectSourceId, RunSourceRole,
    WorkingRootKind as DomainWorkingRootKind, WorkspaceId,
};
use crate::storage::repositories::{
    AttachmentRepository, AutonomousRunParams, ConversationRepository, CreateRunParams,
    DeliveryResolutionAction, FinalizeRunParams, InterruptFollowupParams, ProjectRepository,
    RunRepository, RunSourceSnapshot, WorkspaceRepository, JSON_PAYLOAD_MAX_BYTES,
};
use chrono::Utc;
use ottam_protocol::{
    AgentSessionID, AgentStopReason, ConversationAttachmentID, ConversationContextUsage,
    ConversationEventDeltaParams, ConversationEventKind, ConversationHistoryReplay, ConversationID,
    ConversationRunID, ConversationRunInput as WireConversationRunInput, ConversationRunInputID,
    ConversationRunStatus, ConversationRunSummary, ConversationRunUsage, EngineID, ProjectID,
    RunInputDeliveryResolution, SessionOrigin, WorkingRootAvailability, WorkingRootKind,
};
use diesel::sqlite::SqliteConnection;
use serde_json::{json, Value};
use std::path::Path;

/// The persisted state a run starts from: the queued run, its durable
/// `sending` input, and the user message text the adapter will receive.
#[derive(Debug, Clone)]
pub(in crate::server) struct BeginRunRecord {
    pub run: ConversationRunSummary,
    pub input: WireConversationRunInput,
    pub message_body: String,
    /// Images attached to the Message, resolved from the row on every
    /// delivery. Retry reuses the same `message_id`, so this is re-derived
    /// rather than remembered.
    pub images: Vec<HarnessPromptImage>,
}

#[derive(Debug, Clone)]
pub(in crate::server) struct PreparedSteerRecord {
    pub run: ConversationRunSummary,
    pub input: WireConversationRunInput,
    pub message_body: String,
    pub images: Vec<HarnessPromptImage>,
    pub admitted_position: i64,
}

#[derive(Debug, Clone)]
pub(in crate::server) struct InterruptRunRecord {
    pub interrupted_run: ConversationRunSummary,
    pub followup: BeginRunRecord,
}

struct RunSourceValue {
    root_kind: DomainWorkingRootKind,
    project_id: Option<ProjectId>,
    source_id: Option<ProjectSourceId>,
    canonical_path: String,
    role: RunSourceRole,
}

fn conversation_run_source_values(
    connection: &mut SqliteConnection,
    home: &Path,
    workspace_id: &WorkspaceId,
    conversation_id: &ConversationId,
) -> Result<Vec<RunSourceValue>, StorageError> {
    let conversation = active_conversation(connection, workspace_id, conversation_id)?;
    let links = ConversationRepository::projects(connection, conversation_id)?;
    let root = working_root_summary(connection, home, &conversation, &links)?;
    if root.availability != WorkingRootAvailability::Available {
        return Err(StorageError::new(
            "workingRootUnavailable",
            "the Conversation's working root is unavailable",
        )
        .recoverable(true));
    }

    let mut values = vec![RunSourceValue {
        root_kind: match root.kind {
            WorkingRootKind::WorkspaceFiles => DomainWorkingRootKind::WorkspaceFiles,
            WorkingRootKind::Project => DomainWorkingRootKind::Project,
        },
        project_id: root.project_id.as_ref().map(parse_project_id).transpose()?,
        source_id: root.source_id.as_ref().map(parse_source_id).transpose()?,
        canonical_path: root.canonical_path,
        role: RunSourceRole::Primary,
    }];
    for link in links
        .iter()
        .filter(|link| link.role == ConversationProjectRole::Context && link.removed_at.is_none())
    {
        let membership = WorkspaceRepository::membership(
            connection,
            &conversation.workspace_id,
            &link.project_id,
        )?;
        let source = ProjectRepository::get_source(connection, &link.source_id)?;
        if membership.removed_at.is_some() || source.archived_at.is_some() {
            return Err(StorageError::new(
                "workingRootUnavailable",
                "a Conversation context Project is unavailable",
            )
            .recoverable(true));
        }
        values.push(RunSourceValue {
            root_kind: DomainWorkingRootKind::Project,
            project_id: Some(link.project_id),
            source_id: Some(link.source_id),
            canonical_path: source.canonical_path,
            role: RunSourceRole::Context,
        });
    }
    Ok(values)
}
/// Find (or create) the AgentSession row for an adapter's external session,
/// retiring a fingerprint-matched predecessor the adapter has replaced.
fn resolve_session_row(
    connection: &mut SqliteConnection,
    workspace_id: &WorkspaceId,
    conversation_id: &ConversationId,
    run: &ConversationRun,
    external_session_id: &str,
) -> Result<AgentSession, StorageError> {
    if let Some(session) = RunRepository::find_active_session_by_external(
        connection,
        conversation_id,
        &run.engine_id,
        external_session_id,
    )? {
        return Ok(session);
    }
    let sources = RunRepository::run_sources(connection, &run.id)?;
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
    let fingerprint = source_scope_fingerprint(&roots)?;
    let now = Utc::now();
    // A restarted adapter can reissue an external id that a stale row from
    // another Conversation still holds; the adapter is authoritative, so the
    // prior holder is lost.
    if let Some(stale) = RunRepository::find_active_session_by_external_engine_wide(
        connection,
        &run.engine_id,
        external_session_id,
    )? {
        RunRepository::end_session(
            connection,
            &stale.id,
            AgentSessionStatus::Lost,
            Some("replacedByNewExternalSession"),
            now,
        )?;
    }
    if let Some(previous) = RunRepository::resolve_resumable_session(
        connection,
        conversation_id,
        &run.engine_id,
        &fingerprint,
    )? {
        RunRepository::end_session(
            connection,
            &previous.id,
            AgentSessionStatus::Lost,
            Some("replacedByNewExternalSession"),
            now,
        )?;
    }
    RunRepository::create_session(
        connection,
        workspace_id,
        conversation_id,
        &run.engine_id,
        Some(external_session_id),
        &fingerprint,
        now,
    )
}
fn normalized_payload(payload: &Value) -> String {
    let serialized = payload.to_string();
    if serialized.len() <= JSON_PAYLOAD_MAX_BYTES {
        serialized
    } else {
        json!({
            "kind": "payloadTruncated",
            "originalBytes": serialized.len(),
        })
        .to_string()
    }
}

fn delivery_unresolved() -> StorageError {
    StorageError::new(
        "deliveryUnresolved",
        "a ConversationRunInput has unresolved delivery; resolve it before new work",
    )
    .recoverable(true)
}

impl ChatService {
    // ------------------------------------------------------------------
    // Run lifecycle (bridge-facing)
    // ------------------------------------------------------------------

    /// Reject-on-ambiguity, snapshot the working root, create the queued run
    /// naming the pending input, and durably enter `sending` before any
    /// adapter I/O — all in the persisted delivery order the spec requires.
    pub(in crate::server) async fn begin_run(
        &self,
        conversation_id: &ConversationID,
        engine_id: &EngineID,
        model_id: Option<String>,
        mode_id: Option<String>,
        thought_level_id: Option<String>,
        run_input_id: &ConversationRunInputID,
    ) -> Result<BeginRunRecord, CoreError> {
        let wire_conversation = conversation_id.clone();
        self.transcript_coordinator
            .prepare(self, conversation_id)
            .await?;
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_input_id = parse_input_id(run_input_id)?;
        let engine_id = engine_id.0.clone();
        let home = self.home.clone();
        // Recorded with the run so a retry can be launched the way the user launched
        // it. Without this a retry rides the adapter's defaults, which silently
        // widens a run the user had restricted.
        let request_options = RunRequestOptions {
            mode_id,
            thought_level_id,
        }
        .to_json();
        let record = self
            .database
            .run("conversation.startRun", move |connection| {
                if RunRepository::has_unresolved_delivery(connection, &conversation_id)? {
                    return Err(delivery_unresolved());
                }
                let source_values = conversation_run_source_values(
                    connection,
                    &home,
                    &workspace_id,
                    &conversation_id,
                )?;
                let sources = source_values
                    .iter()
                    .map(|source| RunSourceSnapshot {
                        root_kind: source.root_kind,
                        project_id: source.project_id.as_ref(),
                        source_id: source.source_id.as_ref(),
                        canonical_path: &source.canonical_path,
                        role: source.role,
                    })
                    .collect::<Vec<_>>();
                let now = Utc::now();
                let (run, input) = RunRepository::create_run_with_snapshot_and_mark_input_sending(
                    connection,
                    &CreateRunParams {
                        workspace_id: &workspace_id,
                        conversation_id: &conversation_id,
                        engine_id: &engine_id,
                        model_id: model_id.as_deref(),
                        request_options_json: request_options.as_deref(),
                        pending_input_id: &run_input_id,
                        sources: &sources,
                    },
                    DeliveryMode::Prompt,
                    now,
                )?;
                let message = ConversationRepository::get_message(
                    connection,
                    &conversation_id,
                    &input.message_id,
                )?;
                Ok(BeginRunRecord {
                    run: run_summary(connection, run)?,
                    input: input_to_wire(input),
                    images: message_prompt_images(
                        connection,
                        &home,
                        &workspace_id,
                        &conversation_id,
                        &message,
                    )?,
                    message_body: message.body,
                })
            })
            .await
            .map_err(CoreError::from)?;
        self.transcript_coordinator
            .accept_run(&wire_conversation, &record.run.run_id);
        Ok(record)
    }

    /// Refuses as `begin_run` does while an input's delivery is unresolved. A
    /// user start checks this before it closes the agent's own run, so a start
    /// that would be refused leaves that run alone.
    pub(in crate::server) async fn ensure_delivery_resolved(
        &self,
        conversation_id: &ConversationID,
    ) -> Result<(), CoreError> {
        let conversation_id = parse_conversation_id(conversation_id)?;
        self.database
            .run("conversation.checkDelivery", move |connection| {
                if RunRepository::has_unresolved_delivery(connection, &conversation_id)? {
                    return Err(delivery_unresolved());
                }
                Ok(())
            })
            .await
            .map_err(CoreError::from)
    }

    /// Opens an autonomous follow-up of `continues` on that run's session and
    /// admits it to the live transcript. Unlike `begin_run` it sends nothing,
    /// so an input left in `deliveryUnknown` does not block it: the agent is
    /// already working.
    pub(in crate::server) async fn begin_autonomous_run(
        &self,
        conversation_id: &ConversationID,
        continues: &ConversationRunID,
    ) -> Result<ConversationRunSummary, CoreError> {
        let wire_conversation = conversation_id.clone();
        self.transcript_coordinator
            .prepare(self, conversation_id)
            .await?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        let continues = parse_run_id(continues)?;
        let run = self
            .database
            .run("conversation.openAutonomousRun", move |connection| {
                let run = RunRepository::create_autonomous_run(
                    connection,
                    &AutonomousRunParams {
                        conversation_id: &conversation_id,
                        continues_run_id: &continues,
                    },
                    Utc::now(),
                )?;
                run_summary(connection, run)
            })
            .await
            .map_err(CoreError::from)?;
        self.transcript_coordinator
            .accept_run(&wire_conversation, &run.run_id);
        Ok(run)
    }

    /// The resumable external harness session for this conversation, engine,
    /// and current source scope, per the spec's fingerprint-matched rule, with
    /// the mode the agent last confirmed for it.
    pub(in crate::server) async fn resumable_session(
        &self,
        conversation_id: &ConversationID,
        engine_id: &EngineID,
    ) -> Result<Option<(AgentSessionID, Option<String>)>, CoreError> {
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        let engine_id = engine_id.0.clone();
        let home = self.home.clone();
        self.database
            .run("conversation.resumableSession", move |connection| {
                let sources = conversation_run_source_values(
                    connection,
                    &home,
                    &workspace_id,
                    &conversation_id,
                )?;
                let source_ids = sources
                    .iter()
                    .map(|source| source.source_id.as_ref().map(ToString::to_string))
                    .collect::<Vec<_>>();
                let roots = sources
                    .iter()
                    .zip(source_ids.iter())
                    .map(|(source, source_id)| ScopeRoot {
                        kind: source.root_kind,
                        source_id: source_id.as_deref(),
                        canonical_path: &source.canonical_path,
                    })
                    .collect::<Vec<_>>();
                let fingerprint = source_scope_fingerprint(&roots)?;
                Ok(RunRepository::resolve_resumable_session(
                    connection,
                    &conversation_id,
                    &engine_id,
                    &fingerprint,
                )?
                .and_then(|session| {
                    Some((
                        AgentSessionID(session.external_session_id?),
                        session.current_mode_id,
                    ))
                }))
            })
            .await
            .map_err(CoreError::from)
    }

    /// The active session the conversation's latest run speaks to. A named
    /// session that is not that one is stale: after an engine switch the
    /// other engine's session can still be active.
    pub(in crate::server) async fn active_agent_session(
        &self,
        conversation_id: &ConversationID,
        agent_session_id: Option<&AgentSessionID>,
    ) -> Result<Option<AgentSession>, CoreError> {
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        let session_id = match agent_session_id {
            Some(id) => match AgentSessionId::try_from(id.0.as_str()) {
                Ok(session_id) => Some(session_id),
                Err(_) => return Ok(None),
            },
            None => None,
        };
        self.database
            .run("conversation.activeAgentSession", move |connection| {
                active_conversation(connection, &workspace_id, &conversation_id)?;
                let target = RunRepository::latest_active_session(connection, &conversation_id)?;
                Ok(target.filter(|session| session_id.as_ref().is_none_or(|id| session.id == *id)))
            })
            .await
            .map_err(CoreError::from)
    }

    /// Persist a mode the agent acknowledged outside a run.
    pub(in crate::server) async fn record_session_mode(
        &self,
        session_id: AgentSessionId,
        mode_id: &str,
    ) -> Result<(), CoreError> {
        let mode_id = mode_id.to_string();
        self.database
            .run("conversation.recordSessionMode", move |connection| {
                RunRepository::set_session_mode(connection, &session_id, &mode_id)
            })
            .await
            .map_err(CoreError::from)
    }

    /// Whether the adapter session `external_session_id` that `run_id` is
    /// about to bind has yet to be sent the conversation's earlier turns.
    /// Read before the run binds it, so a session with no row yet is a new
    /// one. It looks the row up as binding does, by the run's engine.
    pub(in crate::server) async fn session_needs_history(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
        external_session_id: &AgentSessionID,
    ) -> Result<bool, CoreError> {
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_id = parse_run_id(run_id)?;
        let external = external_session_id.0.clone();
        self.database
            .run("conversation.sessionNeedsHistory", move |connection| {
                let run = conversation_run_checked(connection, &conversation_id, &run_id)?;
                Ok(RunRepository::find_active_session_by_external(
                    connection,
                    &conversation_id,
                    &run.engine_id,
                    &external,
                )?
                .is_none_or(|session| !session.history_seeded))
            })
            .await
            .map_err(CoreError::from)
    }

    /// Record that the bound run's session has the conversation's earlier
    /// turns, and what the run replayed to give them to it. Returns whether
    /// this run seeded it.
    pub(in crate::server) async fn mark_run_history_seeded(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
        replay: Option<&ConversationHistoryReplay>,
    ) -> Result<bool, CoreError> {
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_id = parse_run_id(run_id)?;
        let replay_json =
            replay.map(|replay| serde_json::to_string(replay).expect("history replay serializes"));
        self.database
            .run("conversation.seedHistory", move |connection| {
                let run = conversation_run_checked(connection, &conversation_id, &run_id)?;
                let Some(session_id) = run.agent_session_id else {
                    return Ok(false);
                };
                RunRepository::mark_history_seeded(
                    connection,
                    &session_id,
                    &run_id,
                    replay_json.as_deref(),
                )
            })
            .await
            .map_err(CoreError::from)
    }

    /// Undo the run's seed: its prompt never reached the agent.
    pub(in crate::server) async fn unmark_run_history_seeded(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
    ) -> Result<(), CoreError> {
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_id = parse_run_id(run_id)?;
        self.database
            .run("conversation.unseedHistory", move |connection| {
                let run = conversation_run_checked(connection, &conversation_id, &run_id)?;
                let Some(session_id) = run.agent_session_id else {
                    return Ok(());
                };
                RunRepository::unmark_history_seeded(connection, &session_id, &run_id)
            })
            .await
            .map_err(CoreError::from)
    }

    /// Bind the adapter's external session to a persisted AgentSession row
    /// (creating one and retiring a replaced scope-mate when needed) and mark
    /// the queued run running on it.
    pub(in crate::server) async fn mark_run_running(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
        external_session_id: &AgentSessionID,
        origin: SessionOrigin,
    ) -> Result<ConversationRunSummary, CoreError> {
        self.bind_run_session(conversation_id, run_id, external_session_id, origin, true)
            .await
    }

    /// Re-bind a running run onto a recovered session (the prompt-retry path
    /// after a Continued session vanished).
    pub(in crate::server) async fn update_run_session(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
        external_session_id: &AgentSessionID,
        origin: SessionOrigin,
    ) -> Result<ConversationRunSummary, CoreError> {
        self.bind_run_session(conversation_id, run_id, external_session_id, origin, false)
            .await
    }

    async fn bind_run_session(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
        external_session_id: &AgentSessionID,
        origin: SessionOrigin,
        mark_running: bool,
    ) -> Result<ConversationRunSummary, CoreError> {
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_id = parse_run_id(run_id)?;
        let external = external_session_id.0.clone();
        let origin = domain_origin(origin);
        self.database
            .run("conversation.bindRunSession", move |connection| {
                let run = conversation_run_checked(connection, &conversation_id, &run_id)?;
                let session = resolve_session_row(
                    connection,
                    &workspace_id,
                    &conversation_id,
                    &run,
                    &external,
                )?;
                let run = if mark_running {
                    RunRepository::mark_run_running(
                        connection,
                        &run_id,
                        &session.id,
                        origin,
                        Utc::now(),
                    )?
                } else {
                    RunRepository::update_run_session(connection, &run_id, &session.id, origin)?
                };
                run_summary(connection, run)
            })
            .await
            .map_err(CoreError::from)
    }

    /// Records the tokens a run's turn used, once, and a `usage` event that
    /// carries them to clients. Works on a closed run: a stopped run's answer
    /// arrives after it ends. Returns the event deltas to publish, none when
    /// the run already had its usage.
    pub(in crate::server) async fn record_turn_usage(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
        project_id: Option<ProjectID>,
        usage: ConversationRunUsage,
    ) -> Result<Vec<ConversationEventDeltaParams>, CoreError> {
        let run = parse_run_id(run_id)?;
        let usage_json = serde_json::to_string(&usage).expect("run usage serializes");
        let written = self
            .database
            .run("conversation.recordTurnUsage", move |connection| {
                RunRepository::record_run_usage(connection, &run, &usage_json)
            })
            .await
            .map_err(CoreError::from)?;
        if !written {
            return Ok(Vec::new());
        }
        self.record_run_events(
            conversation_id,
            run_id,
            project_id,
            vec![(
                ConversationEventKind::Usage,
                serde_json::to_value(&usage).expect("run usage serializes"),
            )],
        )
        .await
    }

    pub(in crate::server) async fn record_run_events(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
        project_id: Option<ProjectID>,
        events: Vec<(ConversationEventKind, Value)>,
    ) -> Result<Vec<ConversationEventDeltaParams>, CoreError> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        for (kind, payload) in &events {
            if *kind == ConversationEventKind::PermissionResolved {
                self.capture_permission(conversation_id, run_id, payload)
                    .await?;
            }
        }
        let wire_workspace_id = self.workspace_id.clone();
        let wire_conversation_id = conversation_id.clone();
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_id = parse_run_id(run_id)?;
        let persisted = self
            .database
            .run_transcript(
                conversation_id.to_string(),
                "conversation.appendEvents",
                events.len(),
                move |connection, _| {
                    recorded_transaction(connection, "conversation.appendEvents", |connection| {
                        let run = conversation_run_checked(connection, &conversation_id, &run_id)?;
                        // A mode the agent announced (leaving plan mode, say)
                        // becomes the session's confirmed mode, so the next
                        // prompt and a later restore keep it.
                        let announced_mode = events
                            .iter()
                            .rev()
                            .filter(|(kind, _)| *kind == ConversationEventKind::SessionMetadata)
                            .find_map(|(_, payload)| payload.get("currentModeId")?.as_str());
                        if let (Some(session_id), Some(mode_id)) =
                            (run.agent_session_id.as_ref(), announced_mode)
                        {
                            RunRepository::set_session_mode(connection, session_id, mode_id)?;
                        }
                        // Likewise the commands it announced, so `/` lists them
                        // after a relaunch. The recorder already drops oversized
                        // metadata; the bound covers payloads it coalesced.
                        let announced_commands = events
                            .iter()
                            .rev()
                            .filter(|(kind, _)| *kind == ConversationEventKind::SessionMetadata)
                            .find_map(|(_, payload)| payload.get("commands")?.as_array())
                            .map(|commands| Value::from(commands.clone()).to_string())
                            .filter(|commands| commands.len() <= JSON_PAYLOAD_MAX_BYTES);
                        if let (Some(session_id), Some(commands)) =
                            (run.agent_session_id.as_ref(), announced_commands)
                        {
                            RunRepository::set_session_commands(connection, session_id, &commands)?;
                        }
                        // And how full its context was, so the chat's gauge
                        // shows it after a relaunch. Cost stays on the event.
                        let reported_context = events
                            .iter()
                            .rev()
                            .filter(|(kind, _)| *kind == ConversationEventKind::SessionMetadata)
                            .find_map(|(_, payload)| {
                                serde_json::from_value::<ConversationContextUsage>(
                                    payload.get("usage")?.clone(),
                                )
                                .ok()
                            });
                        if let (Some(session_id), Some(context)) =
                            (run.agent_session_id.as_ref(), reported_context)
                        {
                            RunRepository::set_session_context_usage(
                                connection,
                                session_id,
                                &serde_json::to_string(&context).expect("context usage serializes"),
                            )?;
                        }
                        let batch = events
                            .iter()
                            .map(|(kind, payload)| {
                                (domain_event_kind(*kind), normalized_payload(payload))
                            })
                            .collect::<Vec<_>>();
                        let persisted = RunRepository::append_event_batch_in_transaction(
                            connection,
                            &run.id,
                            &batch,
                            Utc::now(),
                        )?;
                        Ok(persisted
                            .into_iter()
                            .map(|event| {
                                event_to_delta(
                                    event,
                                    &wire_workspace_id,
                                    &wire_conversation_id,
                                    project_id.clone(),
                                )
                            })
                            .collect::<Vec<_>>())
                    })
                },
            )
            .await
            .map_err(CoreError::from)?;
        Ok(persisted)
    }

    pub(in crate::server) async fn finalize_run(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
        status: ConversationRunStatus,
        stop_reason: Option<AgentStopReason>,
        error: Value,
    ) -> Result<ConversationRunSummary, CoreError> {
        let publish_conversation = conversation_id.clone();
        let _terminal_handoff = self.transcript_coordinator.terminal_handoff.write().await;
        let publish_run = run_id.clone();
        let final_status = status.clone();
        let failure = error.get("kind").and_then(Value::as_str).map(str::to_owned);
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_id = parse_run_id(run_id)?;
        let result = self
            .database
            .run_transcript(
                conversation_id.to_string(),
                "conversation.finalizeRun",
                0,
                move |connection, _| {
                    let _ = conversation_run_checked(connection, &conversation_id, &run_id)?;
                    let error_json = (!error.is_null()
                        && error.as_object().is_none_or(|object| !object.is_empty()))
                    .then(|| error.to_string());
                    let run = RunRepository::finalize_run_with_live_capture(
                        connection,
                        &FinalizeRunParams {
                            run_id: &run_id,
                            status: domain_run_status(&status)?,
                            stop_reason: stop_reason.as_ref().map(stop_reason_value),
                            error_json: error_json.as_deref(),
                        },
                        Utc::now(),
                    )?;
                    run_summary(connection, run)
                },
            )
            .await
            .map_err(CoreError::from);
        if result.is_ok() {
            #[cfg(test)]
            {
                let gate = self
                    .transcript_coordinator
                    .terminal_commit_gate
                    .lock()
                    .expect("terminal gate")
                    .take();
                if let Some((entered, release)) = gate {
                    let _ = entered.send(());
                    let _ = release.await;
                }
            }
            self.transcript_coordinator
                .finish(
                    self,
                    &publish_conversation,
                    &publish_run,
                    final_status,
                    failure.as_deref(),
                )
                .await?;
        }
        result
    }

    pub(in crate::server) async fn mark_input_delivered(
        &self,
        conversation_id: &ConversationID,
        run_input_id: &ConversationRunInputID,
    ) -> Result<WireConversationRunInput, CoreError> {
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_input_id = parse_input_id(run_input_id)?;
        self.database
            .run("conversation.inputDelivered", move |connection| {
                RunRepository::mark_input_delivered(
                    connection,
                    &conversation_id,
                    &run_input_id,
                    Utc::now(),
                )
                .map(input_to_wire)
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn prepare_steer(
        &self,
        conversation_id: &ConversationID,
        expected_run_id: &ConversationRunID,
        message: String,
        attachment_ids: Vec<ConversationAttachmentID>,
        idempotency_key: String,
    ) -> Result<PreparedSteerRecord, CoreError> {
        let publish_conversation = conversation_id.clone();
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        let expected_run_id = parse_run_id(expected_run_id)?;
        let home = self.home.clone();
        let result = self
            .database
            .run_transcript(
                conversation_id.to_string(),
                "conversation.prepareSteer",
                1,
                move |connection, position| {
                    let attachment_ids = attachment_ids
                        .iter()
                        .map(parse_attachment_id)
                        .collect::<Result<Vec<_>, _>>()?;
                    // One transaction over both, for the same reason as
                    // `post_message`: a rejected claim must not leave a steering
                    // Message the user never successfully sent.
                    let now = Utc::now();
                    let posted = recorded_transaction(
                        connection,
                        "conversation.steerMessage",
                        |connection| {
                            let posted =
                                ConversationRepository::append_steer_message_in_transaction(
                                    connection,
                                    &workspace_id,
                                    &conversation_id,
                                    &expected_run_id,
                                    message.trim(),
                                    &idempotency_key,
                                    position,
                                    now,
                                )?;
                            if posted.replayed {
                                AttachmentRepository::validate_replay(
                                    connection,
                                    &conversation_id,
                                    &posted.message.id,
                                    &attachment_ids,
                                )?;
                            } else {
                                AttachmentRepository::claim(
                                    connection,
                                    &workspace_id,
                                    &conversation_id,
                                    &posted.message.id,
                                    &attachment_ids,
                                    now,
                                )?;
                            }
                            Ok(posted)
                        },
                    )?;
                    let target_run_id = posted.input.target_run_id();
                    let run_id = target_run_id
                        .as_ref()
                        .or(posted.input.requested_against_run_id.as_ref())
                        .ok_or_else(|| {
                            StorageError::new(
                                "storageCorrupt",
                                "a steering input has no associated run",
                            )
                        })?;
                    let run = conversation_run_checked(connection, &conversation_id, run_id)?;
                    Ok(PreparedSteerRecord {
                        admitted_position: posted.message.position,
                        run: run_summary(connection, run)?,
                        input: input_to_wire(posted.input),
                        images: message_prompt_images(
                            connection,
                            &home,
                            &workspace_id,
                            &conversation_id,
                            &posted.message,
                        )?,
                        message_body: posted.message.body,
                    })
                },
            )
            .await
            .map_err(CoreError::from);
        if let Ok(record) = &result {
            self.transcript_coordinator
                .admitted(&publish_conversation, record.admitted_position)?;
            self.publish_transcript_boundary(&publish_conversation)
                .await;
        }
        result
    }

    pub(in crate::server) async fn mark_input_sending(
        &self,
        conversation_id: &ConversationID,
        run_input_id: &ConversationRunInputID,
        run_id: &ConversationRunID,
        mode: DeliveryMode,
    ) -> Result<WireConversationRunInput, CoreError> {
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_input_id = parse_input_id(run_input_id)?;
        let run_id = parse_run_id(run_id)?;
        self.database
            .run("conversation.steerSending", move |connection| {
                RunRepository::mark_input_sending(
                    connection,
                    &conversation_id,
                    &run_input_id,
                    &run_id,
                    mode,
                    Utc::now(),
                )
                .map(input_to_wire)
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn interrupt_after_cancel(
        &self,
        conversation_id: &ConversationID,
        interrupted_run_id: &ConversationRunID,
        pending_input_id: &ConversationRunInputID,
    ) -> Result<InterruptRunRecord, CoreError> {
        let publish_conversation = conversation_id.clone();
        let _terminal_handoff = self.transcript_coordinator.terminal_handoff.write().await;
        let publish_run = interrupted_run_id.clone();
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        let interrupted_run_id = parse_run_id(interrupted_run_id)?;
        let pending_input_id = parse_input_id(pending_input_id)?;
        let home = self.home.clone();
        let result = self
            .database
            .run_transcript(
                conversation_id.to_string(),
                "conversation.interruptFollowup",
                0,
                move |connection, _| {
                    let record = RunRepository::interrupt_with_live_capture(
                        connection,
                        &InterruptFollowupParams {
                            workspace_id: &workspace_id,
                            conversation_id: &conversation_id,
                            interrupted_run_id: &interrupted_run_id,
                            pending_input_id: &pending_input_id,
                            retain_session: true,
                        },
                        Utc::now(),
                    )?;
                    let message = ConversationRepository::get_message(
                        connection,
                        &conversation_id,
                        &record.input.message_id,
                    )?;
                    Ok(InterruptRunRecord {
                        interrupted_run: run_summary(connection, record.interrupted_run)?,
                        followup: BeginRunRecord {
                            run: run_summary(connection, record.followup_run)?,
                            input: input_to_wire(record.input),
                            images: message_prompt_images(
                                connection,
                                &home,
                                &workspace_id,
                                &conversation_id,
                                &message,
                            )?,
                            message_body: message.body,
                        },
                    })
                },
            )
            .await
            .map_err(CoreError::from);
        if result.is_ok() {
            self.transcript_coordinator
                .finish(
                    self,
                    &publish_conversation,
                    &publish_run,
                    ConversationRunStatus::Interrupted,
                    None,
                )
                .await?;
            if let Ok(record) = &result {
                self.transcript_coordinator
                    .accept_run(&publish_conversation, &record.followup.run.run_id);
            }
        }
        result
    }

    pub(in crate::server) async fn resolve_run_input_delivery(
        &self,
        conversation_id: &ConversationID,
        run_input_id: &ConversationRunInputID,
        resolution: RunInputDeliveryResolution,
    ) -> Result<(WireConversationRunInput, Option<WireConversationRunInput>), CoreError> {
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_input_id = parse_input_id(run_input_id)?;
        self.database
            .run("conversation.resolveRunInputDelivery", move |connection| {
                let _ = active_conversation(connection, &workspace_id, &conversation_id)?;
                let action = match &resolution {
                    RunInputDeliveryResolution::Retry { idempotency_key } => {
                        DeliveryResolutionAction::Retry { idempotency_key }
                    }
                    RunInputDeliveryResolution::MarkDelivered => {
                        DeliveryResolutionAction::MarkDelivered
                    }
                };
                let record = RunRepository::resolve_undelivered_input(
                    connection,
                    &workspace_id,
                    &conversation_id,
                    &run_input_id,
                    action,
                    Utc::now(),
                )?;
                Ok((
                    input_to_wire(record.resolved_input),
                    record.retry.map(|retry| input_to_wire(retry.input)),
                ))
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn mark_input_failed(
        &self,
        conversation_id: &ConversationID,
        run_input_id: &ConversationRunInputID,
        failure_reason: String,
    ) -> Result<WireConversationRunInput, CoreError> {
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_input_id = parse_input_id(run_input_id)?;
        self.database
            .run("conversation.inputFailed", move |connection| {
                RunRepository::mark_input_failed(
                    connection,
                    &conversation_id,
                    &run_input_id,
                    &failure_reason,
                    Utc::now(),
                )
                .map(input_to_wire)
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn resolve_input_after_launch_failure(
        &self,
        conversation_id: &ConversationID,
        run_input_id: &ConversationRunInputID,
        failure_reason: String,
    ) -> Result<bool, CoreError> {
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_input_id = parse_input_id(run_input_id)?;
        let now = Utc::now();
        self.database
            .run("conversation.launchFailureResolved", move |connection| {
                RunRepository::resolve_input_after_launch_failure(
                    connection,
                    &conversation_id,
                    &run_input_id,
                    &failure_reason,
                    now,
                )
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn mark_input_undelivered(
        &self,
        conversation_id: &ConversationID,
        run_input_id: &ConversationRunInputID,
    ) -> Result<WireConversationRunInput, CoreError> {
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_input_id = parse_input_id(run_input_id)?;
        self.database
            .run("conversation.inputUndelivered", move |connection| {
                RunRepository::mark_input_undelivered(connection, &conversation_id, &run_input_id)
                    .map(input_to_wire)
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn mark_input_delivery_unknown(
        &self,
        conversation_id: &ConversationID,
        run_input_id: &ConversationRunInputID,
    ) -> Result<WireConversationRunInput, CoreError> {
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_input_id = parse_input_id(run_input_id)?;
        self.database
            .run("conversation.inputUnknown", move |connection| {
                RunRepository::mark_input_delivery_unknown(
                    connection,
                    &conversation_id,
                    &run_input_id,
                    Utc::now(),
                )
                .map(input_to_wire)
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn run_input(
        &self,
        conversation_id: &ConversationID,
        run_input_id: &ConversationRunInputID,
    ) -> Result<WireConversationRunInput, CoreError> {
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_input_id = parse_input_id(run_input_id)?;
        self.database
            .run("conversation.runInput", move |connection| {
                let _ = active_conversation(connection, &workspace_id, &conversation_id)?;
                Ok(input_to_wire(ConversationRepository::get_input(
                    connection,
                    &conversation_id,
                    &run_input_id,
                )?))
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn conversation_run(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
    ) -> Result<ConversationRunSummary, CoreError> {
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_id = parse_run_id(run_id)?;
        self.database
            .run("conversation.run", move |connection| {
                let run = conversation_run_checked(connection, &conversation_id, &run_id)?;
                run_summary(connection, run)
            })
            .await
            .map_err(CoreError::from)
    }

    /// Distinct (engine, external session id) pairs for adapter-side cleanup
    /// before a Conversation is deleted.
    pub(in crate::server) async fn harness_sessions(
        &self,
        conversation_id: &ConversationID,
    ) -> Result<Vec<(EngineID, AgentSessionID)>, CoreError> {
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        self.database
            .run("conversation.harnessSessions", move |connection| {
                let _ = active_conversation(connection, &workspace_id, &conversation_id)?;
                let mut pairs =
                    RunRepository::all_sessions_for_cleanup(connection, &conversation_id)?
                        .into_iter()
                        .filter_map(|session| {
                            session
                                .external_session_id
                                .map(|external| (session.engine_id, external))
                        })
                        .collect::<Vec<_>>();
                pairs.sort();
                pairs.dedup();
                Ok(pairs
                    .into_iter()
                    .map(|(engine, external)| (EngineID(engine), AgentSessionID(external)))
                    .collect())
            })
            .await
            .map_err(CoreError::from)
    }
}
