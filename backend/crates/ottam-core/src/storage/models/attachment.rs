use super::types::{
    timestamp, AttachmentKind, AttachmentMimeType, AttachmentSourceKind, ConversationAttachmentId,
    ConversationId, ConversationMessageId, WorkspaceId,
};
use crate::storage::error::StorageError;
use crate::storage::schema::conversation_attachments;
use chrono::{DateTime, Utc};
use diesel::{Identifiable, Insertable, Queryable, Selectable};

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = conversation_attachments)]
pub struct ConversationAttachmentRow {
    pub id: String,
    pub workspace_id: String,
    pub conversation_id: Option<String>,
    pub message_id: Option<String>,
    pub ordinal: Option<i64>,
    pub kind: String,
    pub mime_type: String,
    pub byte_count: i64,
    pub width: i64,
    pub height: i64,
    pub relative_path: String,
    pub idempotency_key: Option<String>,
    pub source_kind: String,
    pub origin_path: Option<String>,
    pub created_at: i64,
    pub claimed_at: Option<i64>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = conversation_attachments)]
pub struct NewConversationAttachmentRow<'a> {
    pub id: &'a str,
    pub workspace_id: &'a str,
    pub conversation_id: Option<&'a str>,
    pub message_id: Option<&'a str>,
    pub ordinal: Option<i64>,
    pub kind: &'a str,
    pub mime_type: &'a str,
    pub byte_count: i64,
    pub width: i64,
    pub height: i64,
    pub relative_path: &'a str,
    pub idempotency_key: Option<&'a str>,
    pub source_kind: &'a str,
    pub origin_path: Option<&'a str>,
    pub created_at: i64,
    pub claimed_at: Option<i64>,
}

/// Whether an attachment still belongs to a draft or has been bound to a
/// posted Message. The four claim columns are set together or not at all, so a
/// half-set row decodes as `storageCorrupt` rather than an ambiguous value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentOwnership {
    Unclaimed,
    Claimed {
        conversation_id: ConversationId,
        message_id: ConversationMessageId,
        ordinal: i64,
        claimed_at: DateTime<Utc>,
    },
}

impl AttachmentOwnership {
    pub fn message_id(&self) -> Option<ConversationMessageId> {
        match self {
            Self::Unclaimed => None,
            Self::Claimed { message_id, .. } => Some(*message_id),
        }
    }

    pub fn ordinal(&self) -> Option<i64> {
        match self {
            Self::Unclaimed => None,
            Self::Claimed { ordinal, .. } => Some(*ordinal),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationAttachment {
    pub id: ConversationAttachmentId,
    pub workspace_id: WorkspaceId,
    pub ownership: AttachmentOwnership,
    pub kind: AttachmentKind,
    pub mime_type: AttachmentMimeType,
    pub byte_count: i64,
    pub width: i64,
    pub height: i64,
    /// File name inside `workspaces/<workspace_id>/attachments/`. Relative so
    /// the row survives a OTTAM_HOME that moves.
    pub relative_path: String,
    pub idempotency_key: Option<String>,
    pub source_kind: AttachmentSourceKind,
    /// Where the file came from on the user's own disk, for picked and dropped
    /// images. Pasted images have no such origin.
    pub origin_path: Option<String>,
    pub created_at: DateTime<Utc>,
}

fn corrupt(message: &'static str) -> StorageError {
    StorageError::new("storageCorrupt", message)
}

fn ownership_from_row(
    row: &ConversationAttachmentRow,
) -> Result<AttachmentOwnership, StorageError> {
    match (
        row.conversation_id.as_deref(),
        row.message_id.as_deref(),
        row.ordinal,
        row.claimed_at,
    ) {
        (None, None, None, None) => Ok(AttachmentOwnership::Unclaimed),
        (Some(conversation_id), Some(message_id), Some(ordinal), Some(claimed_at)) => {
            Ok(AttachmentOwnership::Claimed {
                conversation_id: ConversationId::try_from(conversation_id)?,
                message_id: ConversationMessageId::try_from(message_id)?,
                ordinal,
                claimed_at: timestamp(claimed_at, "ConversationAttachment.claimed_at")?,
            })
        }
        _ => Err(corrupt(
            "persisted ConversationAttachment claim state is incomplete",
        )),
    }
}

fn validate_origin(row: &ConversationAttachmentRow) -> Result<(), StorageError> {
    let source_kind = AttachmentSourceKind::try_from(row.source_kind.as_str())?;
    match (source_kind, row.origin_path.is_some()) {
        (AttachmentSourceKind::Pasted, true) => Err(corrupt(
            "persisted pasted ConversationAttachment carries an origin path",
        )),
        (AttachmentSourceKind::Dropped | AttachmentSourceKind::Picked, false) => Err(corrupt(
            "persisted ConversationAttachment lacks the origin path its source kind requires",
        )),
        _ => Ok(()),
    }
}

impl TryFrom<ConversationAttachmentRow> for ConversationAttachment {
    type Error = StorageError;

    fn try_from(row: ConversationAttachmentRow) -> Result<Self, Self::Error> {
        let ownership = ownership_from_row(&row)?;
        validate_origin(&row)?;
        Ok(Self {
            id: ConversationAttachmentId::try_from(row.id)?,
            workspace_id: WorkspaceId::try_from(row.workspace_id)?,
            ownership,
            kind: AttachmentKind::try_from(row.kind)?,
            mime_type: AttachmentMimeType::try_from(row.mime_type)?,
            byte_count: row.byte_count,
            width: row.width,
            height: row.height,
            relative_path: row.relative_path,
            idempotency_key: row.idempotency_key,
            source_kind: AttachmentSourceKind::try_from(row.source_kind)?,
            origin_path: row.origin_path,
            created_at: timestamp(row.created_at, "ConversationAttachment.created_at")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn base_row() -> ConversationAttachmentRow {
        ConversationAttachmentRow {
            id: Uuid::now_v7().to_string(),
            workspace_id: Uuid::now_v7().to_string(),
            conversation_id: None,
            message_id: None,
            ordinal: None,
            kind: "image".to_string(),
            mime_type: "image/png".to_string(),
            byte_count: 1_024,
            width: 800,
            height: 600,
            relative_path: "019b0000-0000-7000-8000-000000000001.png".to_string(),
            idempotency_key: Some("attachment-key".to_string()),
            source_kind: "pasted".to_string(),
            origin_path: None,
            created_at: 1_000,
            claimed_at: None,
        }
    }

    #[test]
    fn unclaimed_row_decodes_as_a_draft_attachment() {
        let attachment = ConversationAttachment::try_from(base_row()).expect("decode");
        assert_eq!(attachment.ownership, AttachmentOwnership::Unclaimed);
        assert_eq!(attachment.ownership.message_id(), None);
    }

    #[test]
    fn claim_state_is_all_or_none() {
        let mut row = base_row();
        row.message_id = Some(Uuid::now_v7().to_string());
        let error = ConversationAttachment::try_from(row.clone()).expect_err("half claim");
        assert_eq!(error.kind, "storageCorrupt");

        row.conversation_id = Some(Uuid::now_v7().to_string());
        row.ordinal = Some(0);
        row.claimed_at = Some(2_000);
        let attachment = ConversationAttachment::try_from(row).expect("decode");
        assert!(attachment.ownership.message_id().is_some());
        assert_eq!(attachment.ownership.ordinal(), Some(0));
    }

    #[test]
    fn pasted_attachments_reject_an_origin_path() {
        let mut row = base_row();
        row.origin_path = Some("/Users/someone/shot.png".to_string());
        let error = ConversationAttachment::try_from(row).expect_err("stray origin");
        assert_eq!(error.kind, "storageCorrupt");
    }

    #[test]
    fn picked_attachments_require_an_origin_path() {
        let mut row = base_row();
        row.source_kind = "picked".to_string();
        let error = ConversationAttachment::try_from(row.clone()).expect_err("missing origin");
        assert_eq!(error.kind, "storageCorrupt");

        row.origin_path = Some("/Users/someone/shot.png".to_string());
        let attachment = ConversationAttachment::try_from(row).expect("decode");
        assert_eq!(attachment.source_kind, AttachmentSourceKind::Picked);
    }
}
