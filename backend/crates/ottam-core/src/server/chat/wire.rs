use crate::storage::error::StorageError;
use crate::storage::models::{
    AgentSession, AgentSessionStatus, ConversationMessage, ConversationProject,
    ConversationProjectRole, ConversationRun, ConversationRunInput, DeliveryEvidence, MessageRole,
    MessageSource, RunEvent, RunEventKind, RunInputDelivery, RunInputKind,
    RunOrigin as DomainRunOrigin, RunStatus, SessionOrigin as DomainSessionOrigin,
};
use crate::storage::repositories::MessageWithInput;
use chrono::{DateTime, SecondsFormat, Utc};
use ottam_protocol::{
    AgentSessionID, AgentSessionState, AgentSessionSummary, AgentStopReason,
    ConversationAttachment as WireConversationAttachment, ConversationEventDeltaParams,
    ConversationEventID, ConversationEventKind, ConversationID,
    ConversationMessage as WireConversationMessage, ConversationMessageID,
    ConversationPostMessageResult, ConversationProjectLink,
    ConversationProjectRole as WireConversationProjectRole, ConversationRole, ConversationRunID,
    ConversationRunInput as WireConversationRunInput, ConversationRunInputID,
    ConversationRunStatus, ConversationRunSummary, EngineID, MessageSource as WireMessageSource,
    ProjectID, ProjectSourceID, RunInputDeliveryEvidence, RunInputKind as WireRunInputKind,
    RunInputStatus as WireRunInputStatus, RunOrigin, SessionOrigin, WorkspaceID,
};
use serde_json::{json, Value};

/// The selections a run was started with, beyond the model, stored as the run's
/// `request_options_json`. Kept in one JSON column rather than a column apiece so
/// later ACP config categories need no migration; every field is optional so an
/// older row (or one that requested nothing) reads back as "no selection".
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RunRequestOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) mode_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) thought_level_id: Option<String>,
}

impl RunRequestOptions {
    fn is_empty(&self) -> bool {
        self.mode_id.is_none() && self.thought_level_id.is_none()
    }

    /// Serialized form for storage, or None when there is nothing worth writing.
    pub(super) fn to_json(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        serde_json::to_string(self).ok()
    }
}

pub(super) fn iso(timestamp: DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(super) fn message_with_input_to_wire(
    record: MessageWithInput,
    attachments: Vec<WireConversationAttachment>,
) -> ConversationPostMessageResult {
    ConversationPostMessageResult {
        message: message_to_wire(record.message, attachments),
        input: input_to_wire(record.input),
    }
}

pub(super) fn message_to_wire(
    message: ConversationMessage,
    attachments: Vec<WireConversationAttachment>,
) -> WireConversationMessage {
    WireConversationMessage {
        message_id: ConversationMessageID(message.id.to_string()),
        conversation_id: ConversationID(message.conversation_id.to_string()),
        role: match message.role {
            MessageRole::User => ConversationRole::User,
            MessageRole::System => ConversationRole::System,
        },
        body: message.body,
        attachments,
        position: message.position.max(0) as u64,
        created_at: iso(message.created_at),
        source: match message.source {
            MessageSource::User => WireMessageSource::User,
            MessageSource::Delegation => WireMessageSource::Delegation,
            MessageSource::NoteDelivery => WireMessageSource::NoteDelivery,
        },
        // A record that no longer reads is shown as a plain message.
        note: (message.source != MessageSource::Delegation)
            .then_some(message.note_json.as_deref())
            .flatten()
            .and_then(|json| serde_json::from_str(json).ok()),
        task: (message.source == MessageSource::Delegation)
            .then_some(message.note_json.as_deref())
            .flatten()
            .and_then(|json| serde_json::from_str(json).ok()),
        note_delivered_at: message.note_delivered_at.map(iso),
    }
}

pub(super) fn link_to_wire(link: &ConversationProject) -> ConversationProjectLink {
    ConversationProjectLink {
        project_id: ProjectID(link.project_id.to_string()),
        project_source_id: ProjectSourceID(link.source_id.to_string()),
        role: match link.role {
            ConversationProjectRole::Primary => WireConversationProjectRole::Primary,
            ConversationProjectRole::Context => WireConversationProjectRole::Context,
        },
    }
}

pub(super) fn run_to_wire(
    run: ConversationRun,
    external_session_id: Option<String>,
) -> ConversationRunSummary {
    // A row written before options were recorded, or unparseable after a rollback,
    // reads as no selection — the same as a run that asked for nothing.
    let options: RunRequestOptions = run
        .request_options_json
        .as_deref()
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_default();
    ConversationRunSummary {
        run_id: ConversationRunID(run.id.to_string()),
        conversation_id: ConversationID(run.conversation_id.to_string()),
        engine_id: EngineID(run.engine_id),
        model_id: run.model_id,
        mode_id: options.mode_id,
        thought_level_id: options.thought_level_id,
        status: wire_run_status(run.status),
        started_at: iso(run.started_at.unwrap_or(run.queued_at)),
        completed_at: run.completed_at.map(iso),
        agent_session_id: external_session_id.map(AgentSessionID),
        session_origin: run.session_origin.map(wire_origin),
        origin: wire_run_origin(run.origin),
        stop_reason: run.stop_reason.as_deref().map(stop_reason_from_value),
        cost: json!({}),
        // Unreadable usage reads as unreported, never as zero.
        usage: run
            .usage_json
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok()),
        // SQL NULL, not the JSON `null` of a run from before usage.
        usage_unavailable: run.usage_json.is_none()
            && !matches!(run.status, RunStatus::Queued | RunStatus::Running),
        history_replay: run
            .history_replay_json
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok()),
        error: run
            .error_json
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_else(|| json!({})),
    }
}

pub(super) fn session_to_wire(session: AgentSession) -> AgentSessionSummary {
    AgentSessionSummary {
        agent_session_id: AgentSessionID(session.id.to_string()),
        conversation_id: ConversationID(session.conversation_id.to_string()),
        engine_id: EngineID(session.engine_id),
        state: match session.status {
            AgentSessionStatus::Active => AgentSessionState::Active,
            AgentSessionStatus::Closed => AgentSessionState::Closed,
            AgentSessionStatus::Lost => AgentSessionState::Lost,
        },
        source_scope_fingerprint: session.source_scope_fingerprint,
        created_at: iso(session.created_at),
        last_used_at: iso(session.last_used_at),
        ended_at: session.ended_at.map(iso),
        end_reason: session.end_reason,
        current_mode_id: session.current_mode_id,
        commands: session
            .available_commands
            .and_then(|commands| serde_json::from_str(&commands).ok()),
        context_usage: session
            .context_usage
            .and_then(|usage| serde_json::from_str(&usage).ok()),
    }
}

pub(super) fn input_to_wire(input: ConversationRunInput) -> WireConversationRunInput {
    let status = match input.status() {
        crate::storage::models::RunInputStatus::Pending => WireRunInputStatus::Pending,
        crate::storage::models::RunInputStatus::Sending => WireRunInputStatus::Sending,
        crate::storage::models::RunInputStatus::Delivered => WireRunInputStatus::Delivered,
        crate::storage::models::RunInputStatus::DeliveryUnknown => {
            WireRunInputStatus::DeliveryUnknown
        }
        crate::storage::models::RunInputStatus::Failed => WireRunInputStatus::Failed,
        crate::storage::models::RunInputStatus::Superseded => WireRunInputStatus::Superseded,
    };
    let (assigned, delivery_evidence) = match &input.delivery {
        RunInputDelivery::Pending => (None, None),
        RunInputDelivery::Sending { assigned }
        | RunInputDelivery::DeliveryUnknown { assigned, .. } => (Some(assigned), None),
        RunInputDelivery::Delivered {
            assigned, evidence, ..
        } => (Some(assigned), Some(wire_evidence(*evidence))),
        RunInputDelivery::Failed { assigned, .. }
        | RunInputDelivery::Superseded { assigned, .. } => (assigned.as_ref(), None),
    };
    let target_run_id =
        assigned.map(|assigned| ConversationRunID(assigned.target_run_id.to_string()));
    WireConversationRunInput {
        run_input_id: ConversationRunInputID(input.id.to_string()),
        conversation_id: ConversationID(input.conversation_id.to_string()),
        message_id: ConversationMessageID(input.message_id.to_string()),
        requested_against_run_id: input
            .requested_against_run_id
            .map(|id| ConversationRunID(id.to_string())),
        target_run_id,
        kind: match input.kind {
            RunInputKind::Initial => WireRunInputKind::Initial,
            RunInputKind::Steer => WireRunInputKind::Steer,
            RunInputKind::Retry => WireRunInputKind::Retry,
        },
        retry_of_input_id: input
            .retry_of_input_id
            .map(|id| ConversationRunInputID(id.to_string())),
        idempotency_key: input.idempotency_key,
        status,
        delivery_evidence,
    }
}

fn wire_evidence(evidence: DeliveryEvidence) -> RunInputDeliveryEvidence {
    match evidence {
        DeliveryEvidence::AdapterConfirmed => RunInputDeliveryEvidence::AdapterConfirmed,
        DeliveryEvidence::UserMarkedDelivered => RunInputDeliveryEvidence::UserMarkedDelivered,
    }
}

fn wire_run_status(status: RunStatus) -> ConversationRunStatus {
    match status {
        RunStatus::Queued => ConversationRunStatus::Queued,
        RunStatus::Running => ConversationRunStatus::Running,
        RunStatus::Completed => ConversationRunStatus::Completed,
        RunStatus::Failed => ConversationRunStatus::Failed,
        RunStatus::Canceled => ConversationRunStatus::Canceled,
        RunStatus::Interrupted => ConversationRunStatus::Interrupted,
    }
}

pub(super) fn domain_run_status(status: &ConversationRunStatus) -> Result<RunStatus, StorageError> {
    match status {
        ConversationRunStatus::Queued => Ok(RunStatus::Queued),
        ConversationRunStatus::Running => Ok(RunStatus::Running),
        ConversationRunStatus::Completed => Ok(RunStatus::Completed),
        ConversationRunStatus::Failed => Ok(RunStatus::Failed),
        ConversationRunStatus::Canceled => Ok(RunStatus::Canceled),
        ConversationRunStatus::Interrupted => Ok(RunStatus::Interrupted),
    }
}

fn wire_origin(origin: DomainSessionOrigin) -> SessionOrigin {
    match origin {
        DomainSessionOrigin::New => SessionOrigin::New,
        DomainSessionOrigin::Continued => SessionOrigin::Continued,
        DomainSessionOrigin::Restored => SessionOrigin::Restored,
    }
}

fn wire_run_origin(origin: DomainRunOrigin) -> RunOrigin {
    match origin {
        DomainRunOrigin::User => RunOrigin::User,
        DomainRunOrigin::Autonomous => RunOrigin::Autonomous,
    }
}

pub(super) fn domain_origin(origin: SessionOrigin) -> DomainSessionOrigin {
    match origin {
        SessionOrigin::New => DomainSessionOrigin::New,
        SessionOrigin::Continued => DomainSessionOrigin::Continued,
        SessionOrigin::Restored => DomainSessionOrigin::Restored,
    }
}

pub(super) fn stop_reason_value(reason: &AgentStopReason) -> &'static str {
    match reason {
        AgentStopReason::EndTurn => "endTurn",
        AgentStopReason::MaxTokens => "maxTokens",
        AgentStopReason::MaxTurnRequests => "maxTurnRequests",
        AgentStopReason::Refusal => "refusal",
        AgentStopReason::Cancelled => "cancelled",
        AgentStopReason::Other => "other",
    }
}

fn stop_reason_from_value(value: &str) -> AgentStopReason {
    match value {
        "endTurn" => AgentStopReason::EndTurn,
        "maxTokens" => AgentStopReason::MaxTokens,
        "maxTurnRequests" => AgentStopReason::MaxTurnRequests,
        "refusal" => AgentStopReason::Refusal,
        "cancelled" | "interruptedForSteering" => AgentStopReason::Cancelled,
        _ => AgentStopReason::Other,
    }
}

pub(super) fn domain_event_kind(kind: ConversationEventKind) -> RunEventKind {
    match kind {
        ConversationEventKind::RunStatus => RunEventKind::RunStatus,
        ConversationEventKind::PermissionRequested => RunEventKind::PermissionRequested,
        ConversationEventKind::PermissionResolved => RunEventKind::PermissionResolved,
        ConversationEventKind::SteerRequested => RunEventKind::SteerRequested,
        ConversationEventKind::SteerDelivered => RunEventKind::SteerDelivered,
        ConversationEventKind::RunInterrupted => RunEventKind::RunInterrupted,
        ConversationEventKind::Usage => RunEventKind::Usage,
        ConversationEventKind::SessionMetadata => RunEventKind::SessionMetadata,
    }
}

fn wire_event_kind(kind: RunEventKind) -> ConversationEventKind {
    match kind {
        RunEventKind::RunStatus => ConversationEventKind::RunStatus,
        RunEventKind::PermissionRequested => ConversationEventKind::PermissionRequested,
        RunEventKind::PermissionResolved => ConversationEventKind::PermissionResolved,
        RunEventKind::SteerRequested => ConversationEventKind::SteerRequested,
        RunEventKind::SteerDelivered => ConversationEventKind::SteerDelivered,
        RunEventKind::RunInterrupted => ConversationEventKind::RunInterrupted,
        RunEventKind::Usage => ConversationEventKind::Usage,
        RunEventKind::SessionMetadata => ConversationEventKind::SessionMetadata,
    }
}

pub(super) fn event_to_delta(
    event: RunEvent,
    workspace_id: &WorkspaceID,
    conversation_id: &ConversationID,
    project_id: Option<ProjectID>,
) -> ConversationEventDeltaParams {
    let event_kind = wire_event_kind(event.kind);
    let payload: Value = serde_json::from_str(&event.payload_json).unwrap_or(Value::Null);
    ConversationEventDeltaParams {
        event_id: ConversationEventID(event.id.to_string()),
        workspace_id: workspace_id.clone(),
        project_id,
        conversation_id: conversation_id.clone(),
        run_id: ConversationRunID(event.run_id.to_string()),
        sequence: event.sequence.max(0) as u64,
        event_kind,
        payload,
        created_at: iso(event.created_at),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{
        AgentSessionId, AssignedDelivery, ConversationId, ConversationMessageId, ConversationRunId,
        ConversationRunInputId, DeliveryMode, InputResolution, RunEventId, WorkspaceId,
    };

    fn at(milliseconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(milliseconds).expect("test timestamp")
    }

    #[test]
    fn enum_conversions_keep_every_wire_variant_and_legacy_stop_reason() {
        let statuses = [
            (RunStatus::Queued, ConversationRunStatus::Queued),
            (RunStatus::Running, ConversationRunStatus::Running),
            (RunStatus::Completed, ConversationRunStatus::Completed),
            (RunStatus::Failed, ConversationRunStatus::Failed),
            (RunStatus::Canceled, ConversationRunStatus::Canceled),
            (RunStatus::Interrupted, ConversationRunStatus::Interrupted),
        ];
        for (domain, wire) in statuses {
            assert_eq!(wire_run_status(domain), wire);
            assert_eq!(domain_run_status(&wire).expect("wire status"), domain);
        }

        let origins = [
            (DomainSessionOrigin::New, SessionOrigin::New),
            (DomainSessionOrigin::Continued, SessionOrigin::Continued),
            (DomainSessionOrigin::Restored, SessionOrigin::Restored),
        ];
        for (domain, wire) in origins {
            assert_eq!(wire_origin(domain), wire);
            assert_eq!(domain_origin(wire), domain);
        }
        assert_eq!(wire_run_origin(DomainRunOrigin::User), RunOrigin::User);
        assert_eq!(
            wire_run_origin(DomainRunOrigin::Autonomous),
            RunOrigin::Autonomous
        );

        let stop_reasons = [
            (AgentStopReason::EndTurn, "endTurn"),
            (AgentStopReason::MaxTokens, "maxTokens"),
            (AgentStopReason::MaxTurnRequests, "maxTurnRequests"),
            (AgentStopReason::Refusal, "refusal"),
            (AgentStopReason::Cancelled, "cancelled"),
            (AgentStopReason::Other, "other"),
        ];
        for (reason, stored) in stop_reasons {
            assert_eq!(stop_reason_value(&reason), stored);
            assert_eq!(stop_reason_from_value(stored), reason);
        }
        assert_eq!(
            stop_reason_from_value("interruptedForSteering"),
            AgentStopReason::Cancelled
        );
        assert_eq!(
            stop_reason_from_value("futureReason"),
            AgentStopReason::Other
        );

        let event_kinds = [
            (RunEventKind::RunStatus, ConversationEventKind::RunStatus),
            (
                RunEventKind::PermissionRequested,
                ConversationEventKind::PermissionRequested,
            ),
            (
                RunEventKind::PermissionResolved,
                ConversationEventKind::PermissionResolved,
            ),
            (
                RunEventKind::SteerRequested,
                ConversationEventKind::SteerRequested,
            ),
            (
                RunEventKind::SteerDelivered,
                ConversationEventKind::SteerDelivered,
            ),
            (
                RunEventKind::RunInterrupted,
                ConversationEventKind::RunInterrupted,
            ),
            (RunEventKind::Usage, ConversationEventKind::Usage),
        ];
        for (domain, wire) in event_kinds {
            assert_eq!(wire_event_kind(domain), wire);
            assert_eq!(domain_event_kind(wire), domain);
        }
    }

    #[test]
    fn session_status_matrix_keeps_wire_state() {
        let statuses = [
            (AgentSessionStatus::Active, AgentSessionState::Active),
            (AgentSessionStatus::Closed, AgentSessionState::Closed),
            (AgentSessionStatus::Lost, AgentSessionState::Lost),
        ];
        for (domain, wire) in statuses {
            let session = AgentSession {
                id: AgentSessionId::new_v7(),
                workspace_id: WorkspaceId::new_v7(),
                conversation_id: ConversationId::new_v7(),
                engine_id: "engine".to_string(),
                external_session_id: Some("external".to_string()),
                source_scope_fingerprint: "scope".to_string(),
                status: domain,
                created_at: at(1_000),
                last_used_at: at(2_000),
                ended_at: None,
                end_reason: None,
                current_mode_id: None,
                available_commands: None,
                context_usage: None,
                history_seeded: true,
            };
            assert_eq!(session_to_wire(session).state, wire);
        }
    }

    fn run(options: Option<&str>, error: Option<&str>) -> ConversationRun {
        ConversationRun {
            id: ConversationRunId::new_v7(),
            workspace_id: WorkspaceId::new_v7(),
            conversation_id: ConversationId::new_v7(),
            engine_id: "engine".to_string(),
            model_id: Some("model".to_string()),
            agent_session_id: Some(AgentSessionId::new_v7()),
            session_origin: Some(DomainSessionOrigin::Restored),
            status: RunStatus::Running,
            request_options_json: options.map(str::to_string),
            event_sequence_cursor: 0,
            queued_at: at(1_000),
            started_at: None,
            completed_at: None,
            stop_reason: Some("interruptedForSteering".to_string()),
            error_json: error.map(str::to_string),
            origin: DomainRunOrigin::User,
            usage_json: None,
            history_replay_json: None,
        }
    }

    #[test]
    fn run_projection_preserves_options_and_malformed_json_fallbacks() {
        let valid = run_to_wire(
            run(
                Some(r#"{"modeId":"plan","thoughtLevelId":"high"}"#),
                Some(r#"{"kind":"boom"}"#),
            ),
            Some("external".to_string()),
        );
        assert_eq!(valid.mode_id.as_deref(), Some("plan"));
        assert_eq!(valid.thought_level_id.as_deref(), Some("high"));
        assert_eq!(valid.started_at, iso(at(1_000)));
        assert_eq!(
            valid.agent_session_id,
            Some(AgentSessionID("external".into()))
        );
        assert_eq!(valid.session_origin, Some(SessionOrigin::Restored));
        assert_eq!(valid.stop_reason, Some(AgentStopReason::Cancelled));
        assert_eq!(valid.error, json!({"kind": "boom"}));

        let malformed = run_to_wire(run(Some("{bad"), Some("{bad")), None);
        assert_eq!(malformed.mode_id, None);
        assert_eq!(malformed.thought_level_id, None);
        assert_eq!(malformed.error, json!({}));
    }

    #[derive(Clone, Copy)]
    struct ExpectedDelivery {
        kind: WireRunInputKind,
        status: WireRunInputStatus,
        evidence: Option<RunInputDeliveryEvidence>,
        targeted: bool,
    }

    fn assigned(mode: DeliveryMode, sequence: i64) -> AssignedDelivery {
        AssignedDelivery {
            target_run_id: ConversationRunId::new_v7(),
            delivery_sequence: sequence,
            delivery_mode: mode,
            delivery_attempted_at: at(2_000),
        }
    }

    fn input(kind: RunInputKind, delivery: RunInputDelivery) -> ConversationRunInput {
        ConversationRunInput {
            id: ConversationRunInputId::new_v7(),
            workspace_id: WorkspaceId::new_v7(),
            conversation_id: ConversationId::new_v7(),
            message_id: ConversationMessageId::new_v7(),
            requested_against_run_id: None,
            kind,
            retry_of_input_id: None,
            idempotency_key: "key".to_string(),
            requested_at: at(1_000),
            delivery,
        }
    }

    #[test]
    fn input_delivery_matrix_preserves_status_target_and_evidence() {
        let prompt = assigned(DeliveryMode::Prompt, -1);
        let native = assigned(DeliveryMode::NativeSteer, 2);
        let interrupt = assigned(DeliveryMode::InterruptThenPrompt, 3);
        let cases = [
            (
                input(RunInputKind::Initial, RunInputDelivery::Pending),
                ExpectedDelivery {
                    kind: WireRunInputKind::Initial,
                    status: WireRunInputStatus::Pending,
                    evidence: None,
                    targeted: false,
                },
            ),
            (
                input(
                    RunInputKind::Steer,
                    RunInputDelivery::Sending { assigned: prompt },
                ),
                ExpectedDelivery {
                    kind: WireRunInputKind::Steer,
                    status: WireRunInputStatus::Sending,
                    evidence: None,
                    targeted: true,
                },
            ),
            (
                input(
                    RunInputKind::Retry,
                    RunInputDelivery::Delivered {
                        assigned: native,
                        evidence: DeliveryEvidence::AdapterConfirmed,
                        delivered_at: at(3_000),
                        resolution: None,
                    },
                ),
                ExpectedDelivery {
                    kind: WireRunInputKind::Retry,
                    status: WireRunInputStatus::Delivered,
                    evidence: Some(RunInputDeliveryEvidence::AdapterConfirmed),
                    targeted: true,
                },
            ),
            (
                input(
                    RunInputKind::Initial,
                    RunInputDelivery::Delivered {
                        assigned: interrupt,
                        evidence: DeliveryEvidence::UserMarkedDelivered,
                        delivered_at: at(3_000),
                        resolution: Some((InputResolution::MarkedDelivered, at(4_000))),
                    },
                ),
                ExpectedDelivery {
                    kind: WireRunInputKind::Initial,
                    status: WireRunInputStatus::Delivered,
                    evidence: Some(RunInputDeliveryEvidence::UserMarkedDelivered),
                    targeted: true,
                },
            ),
            (
                input(
                    RunInputKind::Steer,
                    RunInputDelivery::DeliveryUnknown {
                        assigned: native,
                        ambiguity_detected_at: at(4_000),
                    },
                ),
                ExpectedDelivery {
                    kind: WireRunInputKind::Steer,
                    status: WireRunInputStatus::DeliveryUnknown,
                    evidence: None,
                    targeted: true,
                },
            ),
            (
                input(
                    RunInputKind::Initial,
                    RunInputDelivery::Failed {
                        assigned: None,
                        failed_at: at(4_000),
                        failure_reason: "failed".to_string(),
                    },
                ),
                ExpectedDelivery {
                    kind: WireRunInputKind::Initial,
                    status: WireRunInputStatus::Failed,
                    evidence: None,
                    targeted: false,
                },
            ),
            (
                input(
                    RunInputKind::Retry,
                    RunInputDelivery::Superseded {
                        assigned: Some(interrupt),
                        ambiguity_detected_at: Some(at(4_000)),
                        resolution: InputResolution::RetryCreated,
                        resolved_at: at(5_000),
                    },
                ),
                ExpectedDelivery {
                    kind: WireRunInputKind::Retry,
                    status: WireRunInputStatus::Superseded,
                    evidence: None,
                    targeted: true,
                },
            ),
        ];

        for (input, expected) in cases {
            let wire = input_to_wire(input);
            assert_eq!(wire.kind, expected.kind);
            assert_eq!(wire.status, expected.status);
            assert_eq!(wire.delivery_evidence, expected.evidence);
            assert_eq!(wire.target_run_id.is_some(), expected.targeted);
        }
    }

    #[test]
    fn malformed_event_payload_falls_back_to_null_and_clamps_sequence() {
        let event = RunEvent {
            id: RunEventId::new_v7(),
            run_id: ConversationRunId::new_v7(),
            sequence: -1,
            kind: RunEventKind::Usage,
            payload_json: "{bad".to_string(),
            created_at: at(1_000),
        };
        let delta = event_to_delta(
            event,
            &WorkspaceID("workspace".to_string()),
            &ConversationID("conversation".to_string()),
            None,
        );
        assert_eq!(delta.sequence, 0);
        assert_eq!(delta.event_kind, ConversationEventKind::Usage);
        assert_eq!(delta.payload, Value::Null);
    }
}
