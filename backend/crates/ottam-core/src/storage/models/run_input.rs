use super::types::{
    optional_timestamp, timestamp, ConversationId, ConversationMessageId, ConversationRunId,
    ConversationRunInputId, DeliveryEvidence, DeliveryMode, InputResolution, RunInputKind,
    RunInputStatus, WorkspaceId,
};
use crate::storage::error::StorageError;
use crate::storage::schema::conversation_run_inputs;
use chrono::{DateTime, Utc};
use diesel::{Identifiable, Insertable, Queryable, Selectable};

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = conversation_run_inputs)]
pub struct ConversationRunInputRow {
    pub id: String,
    pub workspace_id: String,
    pub conversation_id: String,
    pub message_id: String,
    pub requested_against_run_id: Option<String>,
    pub target_run_id: Option<String>,
    pub delivery_sequence: Option<i64>,
    pub kind: String,
    pub retry_of_input_id: Option<String>,
    pub idempotency_key: String,
    pub status: String,
    pub delivery_mode: Option<String>,
    pub delivery_evidence: Option<String>,
    pub requested_at: i64,
    pub delivery_attempted_at: Option<i64>,
    pub delivered_at: Option<i64>,
    pub ambiguity_detected_at: Option<i64>,
    pub failed_at: Option<i64>,
    pub failure_reason: Option<String>,
    pub resolved_at: Option<i64>,
    pub resolution: Option<String>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = conversation_run_inputs)]
pub struct NewConversationRunInputRow<'a> {
    pub id: &'a str,
    pub workspace_id: &'a str,
    pub conversation_id: &'a str,
    pub message_id: &'a str,
    pub requested_against_run_id: Option<&'a str>,
    pub target_run_id: Option<&'a str>,
    pub delivery_sequence: Option<i64>,
    pub kind: &'a str,
    pub retry_of_input_id: Option<&'a str>,
    pub idempotency_key: &'a str,
    pub status: &'a str,
    pub delivery_mode: Option<&'a str>,
    pub delivery_evidence: Option<&'a str>,
    pub requested_at: i64,
    pub delivery_attempted_at: Option<i64>,
    pub delivered_at: Option<i64>,
    pub ambiguity_detected_at: Option<i64>,
    pub failed_at: Option<i64>,
    pub failure_reason: Option<&'a str>,
    pub resolved_at: Option<i64>,
    pub resolution: Option<&'a str>,
}

/// Delivery assignment shared by every post-`pending` state: the target Run,
/// its input order, the mode used, and when Core durably entered `sending`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssignedDelivery {
    pub target_run_id: ConversationRunId,
    pub delivery_sequence: i64,
    pub delivery_mode: DeliveryMode,
    pub delivery_attempted_at: DateTime<Utc>,
}

/// Validated per-status delivery state. Decoding enforces the spec's
/// per-state required fields so a half-valid row surfaces as
/// `storageCorrupt` instead of an inconsistent domain value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunInputDelivery {
    Pending,
    Sending {
        assigned: AssignedDelivery,
    },
    Delivered {
        assigned: AssignedDelivery,
        evidence: DeliveryEvidence,
        delivered_at: DateTime<Utc>,
        resolution: Option<(InputResolution, DateTime<Utc>)>,
    },
    DeliveryUnknown {
        assigned: AssignedDelivery,
        ambiguity_detected_at: DateTime<Utc>,
    },
    Failed {
        assigned: Option<AssignedDelivery>,
        failed_at: DateTime<Utc>,
        failure_reason: String,
    },
    Superseded {
        assigned: Option<AssignedDelivery>,
        ambiguity_detected_at: Option<DateTime<Utc>>,
        resolution: InputResolution,
        resolved_at: DateTime<Utc>,
    },
}

impl RunInputDelivery {
    pub fn status(&self) -> RunInputStatus {
        match self {
            Self::Pending => RunInputStatus::Pending,
            Self::Sending { .. } => RunInputStatus::Sending,
            Self::Delivered { .. } => RunInputStatus::Delivered,
            Self::DeliveryUnknown { .. } => RunInputStatus::DeliveryUnknown,
            Self::Failed { .. } => RunInputStatus::Failed,
            Self::Superseded { .. } => RunInputStatus::Superseded,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationRunInput {
    pub id: ConversationRunInputId,
    pub workspace_id: WorkspaceId,
    pub conversation_id: ConversationId,
    pub message_id: ConversationMessageId,
    pub requested_against_run_id: Option<ConversationRunId>,
    pub kind: RunInputKind,
    pub retry_of_input_id: Option<ConversationRunInputId>,
    pub idempotency_key: String,
    pub requested_at: DateTime<Utc>,
    pub delivery: RunInputDelivery,
}

impl ConversationRunInput {
    pub fn status(&self) -> RunInputStatus {
        self.delivery.status()
    }

    pub fn target_run_id(&self) -> Option<ConversationRunId> {
        match &self.delivery {
            RunInputDelivery::Pending => None,
            RunInputDelivery::Sending { assigned }
            | RunInputDelivery::Delivered { assigned, .. }
            | RunInputDelivery::DeliveryUnknown { assigned, .. } => Some(assigned.target_run_id),
            RunInputDelivery::Failed { assigned, .. }
            | RunInputDelivery::Superseded { assigned, .. } => {
                assigned.map(|assigned| assigned.target_run_id)
            }
        }
    }
}

fn corrupt(message: &'static str) -> StorageError {
    StorageError::new("storageCorrupt", message)
}

fn assigned_delivery(
    row: &ConversationRunInputRow,
) -> Result<Option<AssignedDelivery>, StorageError> {
    match (
        row.target_run_id.as_deref(),
        row.delivery_sequence,
        row.delivery_mode.as_deref(),
        row.delivery_attempted_at,
    ) {
        (None, None, None, None) => Ok(None),
        (Some(target_run_id), Some(delivery_sequence), Some(delivery_mode), Some(attempted_at)) => {
            Ok(Some(AssignedDelivery {
                target_run_id: ConversationRunId::try_from(target_run_id)?,
                delivery_sequence,
                delivery_mode: DeliveryMode::try_from(delivery_mode)?,
                delivery_attempted_at: timestamp(
                    attempted_at,
                    "ConversationRunInput.delivery_attempted_at",
                )?,
            }))
        }
        _ => Err(corrupt(
            "persisted ConversationRunInput delivery assignment is incomplete",
        )),
    }
}

fn delivery_from_row(row: &ConversationRunInputRow) -> Result<RunInputDelivery, StorageError> {
    let status = RunInputStatus::try_from(row.status.as_str())?;
    let assigned = assigned_delivery(row)?;
    let evidence = row
        .delivery_evidence
        .as_deref()
        .map(DeliveryEvidence::try_from)
        .transpose()?;
    let resolution = row
        .resolution
        .as_deref()
        .map(InputResolution::try_from)
        .transpose()?;
    let delivered_at = optional_timestamp(row.delivered_at, "ConversationRunInput.delivered_at")?;
    let ambiguity_detected_at = optional_timestamp(
        row.ambiguity_detected_at,
        "ConversationRunInput.ambiguity_detected_at",
    )?;
    let failed_at = optional_timestamp(row.failed_at, "ConversationRunInput.failed_at")?;
    let resolved_at = optional_timestamp(row.resolved_at, "ConversationRunInput.resolved_at")?;

    match status {
        RunInputStatus::Pending => Ok(RunInputDelivery::Pending),
        RunInputStatus::Sending => Ok(RunInputDelivery::Sending {
            assigned: assigned.ok_or_else(|| {
                corrupt("persisted sending ConversationRunInput lacks assignment")
            })?,
        }),
        RunInputStatus::Delivered => {
            Ok(RunInputDelivery::Delivered {
                assigned: assigned.ok_or_else(|| {
                    corrupt("persisted delivered ConversationRunInput lacks assignment")
                })?,
                evidence: evidence.ok_or_else(|| {
                    corrupt("persisted delivered ConversationRunInput lacks delivery evidence")
                })?,
                delivered_at: delivered_at.ok_or_else(|| {
                    corrupt("persisted delivered ConversationRunInput lacks delivered_at")
                })?,
                resolution: match (resolution, resolved_at) {
                    (None, None) => None,
                    (Some(resolution), Some(resolved_at)) => Some((resolution, resolved_at)),
                    _ => return Err(corrupt(
                        "persisted delivered ConversationRunInput has a half-recorded resolution",
                    )),
                },
            })
        }
        RunInputStatus::DeliveryUnknown => Ok(RunInputDelivery::DeliveryUnknown {
            assigned: assigned.ok_or_else(|| {
                corrupt("persisted deliveryUnknown ConversationRunInput lacks assignment")
            })?,
            ambiguity_detected_at: ambiguity_detected_at.ok_or_else(|| {
                corrupt(
                    "persisted deliveryUnknown ConversationRunInput lacks ambiguity_detected_at",
                )
            })?,
        }),
        RunInputStatus::Failed => Ok(RunInputDelivery::Failed {
            assigned,
            failed_at: failed_at
                .ok_or_else(|| corrupt("persisted failed ConversationRunInput lacks failed_at"))?,
            failure_reason: row.failure_reason.clone().ok_or_else(|| {
                corrupt("persisted failed ConversationRunInput lacks a failure reason")
            })?,
        }),
        RunInputStatus::Superseded => Ok(RunInputDelivery::Superseded {
            assigned,
            ambiguity_detected_at,
            resolution: resolution.ok_or_else(|| {
                corrupt("persisted superseded ConversationRunInput lacks a resolution")
            })?,
            resolved_at: resolved_at.ok_or_else(|| {
                corrupt("persisted superseded ConversationRunInput lacks resolved_at")
            })?,
        }),
    }
}

fn validate_kind(row: &ConversationRunInputRow) -> Result<(), StorageError> {
    let kind = RunInputKind::try_from(row.kind.as_str())?;
    match (kind, row.retry_of_input_id.is_some()) {
        (RunInputKind::Retry, false) => Err(corrupt(
            "persisted retry ConversationRunInput lacks its prior input reference",
        )),
        (RunInputKind::Initial | RunInputKind::Steer, true) => Err(corrupt(
            "persisted non-retry ConversationRunInput carries a retry reference",
        )),
        _ => Ok(()),
    }
}

impl TryFrom<ConversationRunInputRow> for ConversationRunInput {
    type Error = StorageError;

    fn try_from(row: ConversationRunInputRow) -> Result<Self, Self::Error> {
        let delivery = delivery_from_row(&row)?;
        validate_kind(&row)?;
        Ok(Self {
            id: ConversationRunInputId::try_from(row.id)?,
            workspace_id: WorkspaceId::try_from(row.workspace_id)?,
            conversation_id: ConversationId::try_from(row.conversation_id)?,
            message_id: ConversationMessageId::try_from(row.message_id)?,
            requested_against_run_id: row
                .requested_against_run_id
                .map(ConversationRunId::try_from)
                .transpose()?,
            kind: RunInputKind::try_from(row.kind)?,
            retry_of_input_id: row
                .retry_of_input_id
                .map(ConversationRunInputId::try_from)
                .transpose()?,
            idempotency_key: row.idempotency_key,
            requested_at: timestamp(row.requested_at, "ConversationRunInput.requested_at")?,
            delivery,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn base_row() -> ConversationRunInputRow {
        ConversationRunInputRow {
            id: Uuid::now_v7().to_string(),
            workspace_id: Uuid::now_v7().to_string(),
            conversation_id: Uuid::now_v7().to_string(),
            message_id: Uuid::now_v7().to_string(),
            requested_against_run_id: None,
            target_run_id: None,
            delivery_sequence: None,
            kind: "initial".to_string(),
            retry_of_input_id: None,
            idempotency_key: "key-1".to_string(),
            status: "pending".to_string(),
            delivery_mode: None,
            delivery_evidence: None,
            requested_at: 1_000,
            delivery_attempted_at: None,
            delivered_at: None,
            ambiguity_detected_at: None,
            failed_at: None,
            failure_reason: None,
            resolved_at: None,
            resolution: None,
        }
    }

    fn assign(row: &mut ConversationRunInputRow, mode: &str) {
        row.target_run_id = Some(Uuid::now_v7().to_string());
        row.delivery_sequence = Some(1);
        row.delivery_mode = Some(mode.to_string());
        row.delivery_attempted_at = Some(2_000);
    }

    #[test]
    fn pending_row_decodes_without_assignment() {
        let input = ConversationRunInput::try_from(base_row()).expect("decode");
        assert_eq!(input.status(), RunInputStatus::Pending);
        assert_eq!(input.target_run_id(), None);
    }

    #[test]
    fn sending_row_requires_full_assignment() {
        let mut row = base_row();
        row.status = "sending".to_string();
        let error = ConversationRunInput::try_from(row.clone()).expect_err("missing assignment");
        assert_eq!(error.kind, "storageCorrupt");

        assign(&mut row, "prompt");
        row.delivery_sequence = None;
        let error = ConversationRunInput::try_from(row.clone()).expect_err("half assignment");
        assert_eq!(error.kind, "storageCorrupt");

        row.delivery_sequence = Some(1);
        let input = ConversationRunInput::try_from(row).expect("decode");
        assert_eq!(input.status(), RunInputStatus::Sending);
        assert!(input.target_run_id().is_some());
    }

    #[test]
    fn delivered_row_requires_evidence_and_timestamp() {
        let mut row = base_row();
        row.status = "delivered".to_string();
        assign(&mut row, "nativeSteer");
        let error = ConversationRunInput::try_from(row.clone()).expect_err("missing evidence");
        assert_eq!(error.kind, "storageCorrupt");

        row.delivery_evidence = Some("adapterConfirmed".to_string());
        row.delivered_at = Some(3_000);
        let input = ConversationRunInput::try_from(row.clone()).expect("decode");
        match input.delivery {
            RunInputDelivery::Delivered {
                evidence,
                resolution,
                ..
            } => {
                assert_eq!(evidence, DeliveryEvidence::AdapterConfirmed);
                assert_eq!(resolution, None);
            }
            other => panic!("unexpected delivery: {other:?}"),
        }

        row.resolution = Some("markedDelivered".to_string());
        let error = ConversationRunInput::try_from(row.clone()).expect_err("half resolution");
        assert_eq!(error.kind, "storageCorrupt");

        row.resolved_at = Some(4_000);
        row.delivery_evidence = Some("userMarkedDelivered".to_string());
        let input = ConversationRunInput::try_from(row).expect("decode");
        match input.delivery {
            RunInputDelivery::Delivered {
                evidence,
                resolution,
                ..
            } => {
                assert_eq!(evidence, DeliveryEvidence::UserMarkedDelivered);
                assert_eq!(
                    resolution.map(|(resolution, _)| resolution),
                    Some(InputResolution::MarkedDelivered)
                );
            }
            other => panic!("unexpected delivery: {other:?}"),
        }
    }

    #[test]
    fn delivery_unknown_row_retains_assignment_and_detection_time() {
        let mut row = base_row();
        row.status = "deliveryUnknown".to_string();
        assign(&mut row, "interruptThenPrompt");
        let error = ConversationRunInput::try_from(row.clone()).expect_err("missing detection");
        assert_eq!(error.kind, "storageCorrupt");

        row.ambiguity_detected_at = Some(5_000);
        let input = ConversationRunInput::try_from(row).expect("decode");
        assert_eq!(input.status(), RunInputStatus::DeliveryUnknown);
    }

    #[test]
    fn failed_row_requires_reason_and_timestamp() {
        let mut row = base_row();
        row.status = "failed".to_string();
        row.failed_at = Some(6_000);
        let error = ConversationRunInput::try_from(row.clone()).expect_err("missing reason");
        assert_eq!(error.kind, "storageCorrupt");

        row.failure_reason = Some("adapterRejected".to_string());
        let input = ConversationRunInput::try_from(row).expect("decode");
        assert_eq!(input.status(), RunInputStatus::Failed);
    }

    #[test]
    fn superseded_row_requires_resolution() {
        let mut row = base_row();
        row.status = "superseded".to_string();
        assign(&mut row, "prompt");
        row.ambiguity_detected_at = Some(5_000);
        row.resolved_at = Some(7_000);
        let error = ConversationRunInput::try_from(row.clone()).expect_err("missing resolution");
        assert_eq!(error.kind, "storageCorrupt");

        row.resolution = Some("retryCreated".to_string());
        let input = ConversationRunInput::try_from(row).expect("decode");
        assert_eq!(input.status(), RunInputStatus::Superseded);
    }

    #[test]
    fn retry_kind_requires_prior_input_reference() {
        let mut row = base_row();
        row.kind = "retry".to_string();
        let error = ConversationRunInput::try_from(row.clone()).expect_err("missing reference");
        assert_eq!(error.kind, "storageCorrupt");

        row.retry_of_input_id = Some(Uuid::now_v7().to_string());
        let input = ConversationRunInput::try_from(row.clone()).expect("decode");
        assert_eq!(input.kind, RunInputKind::Retry);

        row.kind = "initial".to_string();
        let error = ConversationRunInput::try_from(row).expect_err("stray reference");
        assert_eq!(error.kind, "storageCorrupt");
    }
}
