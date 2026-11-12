use crate::storage::error::StorageError;
use crate::storage::models::{
    AttachmentKind, AttachmentMimeType, AttachmentSourceKind, ConversationAttachment,
    ConversationAttachmentId, ConversationAttachmentRow, ConversationId, ConversationMessageId,
    NewConversationAttachmentRow, WorkspaceId,
};
use crate::storage::schema::conversation_attachments;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use std::collections::HashMap;

/// Product caps on what one Message may carry. The DB CHECKs mirror the
/// per-attachment byte cap; these two are per-Message and have no CHECK to
/// enforce them, so `claim` is the only gate.
pub const MAX_ATTACHMENTS_PER_MESSAGE: usize = 6;
pub const MAX_ATTACHMENT_BYTES_PER_MESSAGE: i64 = 4 * 1024 * 1024;
/// Mirrors the `byte_count <= 3145728` CHECK in the migration.
pub const MAX_ATTACHMENT_BYTES: i64 = 3 * 1024 * 1024;

pub struct AttachmentRepository;

#[derive(Debug, Clone)]
pub struct CreateAttachmentParams<'a> {
    pub workspace_id: &'a WorkspaceId,
    pub kind: AttachmentKind,
    pub mime_type: AttachmentMimeType,
    pub byte_count: i64,
    pub width: i64,
    pub height: i64,
    pub relative_path: &'a str,
    pub idempotency_key: &'a str,
    pub source_kind: AttachmentSourceKind,
    pub origin_path: Option<&'a str>,
}

impl AttachmentRepository {
    /// Record an attachment that belongs to a draft. It is claimed later, when
    /// the Message it was composed with is actually posted — or reaped if that
    /// never happens.
    pub fn create_unclaimed(
        connection: &mut SqliteConnection,
        id: &ConversationAttachmentId,
        params: &CreateAttachmentParams<'_>,
        now: DateTime<Utc>,
    ) -> Result<ConversationAttachment, StorageError> {
        let id_value = id.to_string();
        let workspace_id_value = params.workspace_id.to_string();
        diesel::insert_into(conversation_attachments::table)
            .values(NewConversationAttachmentRow {
                id: &id_value,
                workspace_id: &workspace_id_value,
                conversation_id: None,
                message_id: None,
                ordinal: None,
                kind: params.kind.as_storage_str(),
                mime_type: params.mime_type.as_storage_str(),
                byte_count: params.byte_count,
                width: params.width,
                height: params.height,
                relative_path: params.relative_path,
                idempotency_key: Some(params.idempotency_key),
                source_kind: params.source_kind.as_storage_str(),
                origin_path: params.origin_path,
                created_at: now.timestamp_millis(),
                claimed_at: None,
            })
            .execute(connection)?;
        Self::get(connection, params.workspace_id, id)
    }

    pub fn get_by_idempotency_key(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        idempotency_key: &str,
    ) -> Result<Option<ConversationAttachment>, StorageError> {
        conversation_attachments::table
            .filter(conversation_attachments::workspace_id.eq(workspace_id.to_string()))
            .filter(conversation_attachments::idempotency_key.eq(idempotency_key))
            .select(ConversationAttachmentRow::as_select())
            .first::<ConversationAttachmentRow>(connection)
            .optional()?
            .map(TryInto::try_into)
            .transpose()
    }

    pub fn get(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        id: &ConversationAttachmentId,
    ) -> Result<ConversationAttachment, StorageError> {
        conversation_attachments::table
            .find(id.to_string())
            .filter(conversation_attachments::workspace_id.eq(workspace_id.to_string()))
            .select(ConversationAttachmentRow::as_select())
            .first::<ConversationAttachmentRow>(connection)
            .optional()?
            .ok_or_else(|| StorageError::not_found("ConversationAttachment"))?
            .try_into()
    }

    /// Bind draft attachments to a posted Message. `ids` carries the order the
    /// user arranged in the composer, and that order becomes `ordinal` — not
    /// `created_at`, which reflects paste order instead.
    ///
    /// Runs on the caller's connection so a failure here rolls the Message back
    /// with it.
    pub fn claim(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        conversation_id: &ConversationId,
        message_id: &ConversationMessageId,
        ids: &[ConversationAttachmentId],
        now: DateTime<Utc>,
    ) -> Result<Vec<ConversationAttachment>, StorageError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        if ids.len() > MAX_ATTACHMENTS_PER_MESSAGE {
            return Err(StorageError::new(
                "storageConstraint",
                "a Message may not carry more than six attachments",
            )
            .recoverable(true));
        }
        let mut seen = std::collections::HashSet::new();
        for id in ids {
            if !seen.insert(*id) {
                return Err(StorageError::new(
                    "storageConstraint",
                    "the same attachment was claimed twice by one Message",
                )
                .recoverable(true));
            }
        }

        let mut total_bytes: i64 = 0;
        let mut claimed = Vec::with_capacity(ids.len());
        for (ordinal, id) in ids.iter().enumerate() {
            let attachment = Self::get(connection, workspace_id, id)?;
            // Counted before the ownership check so a replay weighs the same as
            // the original claim — these bytes are on the Message either way.
            total_bytes = total_bytes.saturating_add(attachment.byte_count);
            if total_bytes > MAX_ATTACHMENT_BYTES_PER_MESSAGE {
                return Err(StorageError::new(
                    "storageConstraint",
                    "the Message's attachments exceed the four-megabyte total",
                )
                .recoverable(true));
            }
            match attachment.ownership.message_id() {
                // Already ours. A retry carrying the same idempotency key
                // replays the Message rather than inserting a second one, and
                // this claim runs again against the ids it claimed the first
                // time — treating that as a conflict would fail the very retry
                // the key exists to make safe.
                Some(owner) if owner == *message_id => {
                    claimed.push(attachment);
                    continue;
                }
                Some(_) => {
                    return Err(StorageError::new(
                        "storageConstraint",
                        "the attachment already belongs to another Message",
                    )
                    .recoverable(true));
                }
                None => {}
            }
            diesel::update(
                conversation_attachments::table
                    .find(id.to_string())
                    .filter(conversation_attachments::workspace_id.eq(workspace_id.to_string())),
            )
            .set((
                conversation_attachments::conversation_id.eq(conversation_id.to_string()),
                conversation_attachments::message_id.eq(message_id.to_string()),
                conversation_attachments::ordinal.eq(ordinal as i64),
                conversation_attachments::claimed_at.eq(now.timestamp_millis()),
            ))
            .execute(connection)?;
            claimed.push(Self::get(connection, workspace_id, id)?);
        }
        Ok(claimed)
    }

    /// Load attachments for a page of Messages in one query, keyed by Message.
    /// A per-Message query would be N+1 across a fifty-Message page.
    pub fn list_for_messages(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        message_ids: &[ConversationMessageId],
    ) -> Result<HashMap<ConversationMessageId, Vec<ConversationAttachment>>, StorageError> {
        if message_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let values = message_ids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let rows = conversation_attachments::table
            .filter(conversation_attachments::conversation_id.eq(conversation_id.to_string()))
            .filter(conversation_attachments::message_id.eq_any(&values))
            .order((
                conversation_attachments::message_id.asc(),
                conversation_attachments::ordinal.asc(),
            ))
            .select(ConversationAttachmentRow::as_select())
            .load::<ConversationAttachmentRow>(connection)?;
        let mut grouped: HashMap<ConversationMessageId, Vec<ConversationAttachment>> =
            HashMap::new();
        for row in rows {
            let attachment = ConversationAttachment::try_from(row)?;
            if let Some(message_id) = attachment.ownership.message_id() {
                grouped.entry(message_id).or_default().push(attachment);
            }
        }
        Ok(grouped)
    }

    /// Return the attachment set already committed for an idempotent Message
    /// replay, rejecting any attempt to rewrite durable input evidence.
    pub fn validate_replay(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        message_id: &ConversationMessageId,
        ids: &[ConversationAttachmentId],
    ) -> Result<Vec<ConversationAttachment>, StorageError> {
        let persisted = Self::list_for_messages(connection, conversation_id, &[*message_id])?
            .remove(message_id)
            .unwrap_or_default();
        let persisted_ids = persisted.iter().map(|item| item.id).collect::<Vec<_>>();
        if persisted_ids != ids {
            return Err(StorageError::new(
                "idempotencyConflict",
                "the idempotency key was already used with different attachments",
            )
            .recoverable(true));
        }
        Ok(persisted)
    }

    /// Drop one draft attachment — the composer's ✕. Returns the relative path
    /// to unlink, or `None` when the row was already gone.
    pub fn discard_unclaimed(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        id: &ConversationAttachmentId,
    ) -> Result<Option<String>, StorageError> {
        let Some(row) = conversation_attachments::table
            .find(id.to_string())
            .filter(conversation_attachments::workspace_id.eq(workspace_id.to_string()))
            .select(ConversationAttachmentRow::as_select())
            .first::<ConversationAttachmentRow>(connection)
            .optional()?
        else {
            return Ok(None);
        };
        if row.message_id.is_some() {
            return Err(StorageError::new(
                "storageConstraint",
                "an attachment already sent with a Message cannot be discarded",
            )
            .recoverable(true));
        }
        let relative_path = row.relative_path.clone();
        // The `message_id IS NULL` filter belongs on the DELETE, not just the
        // read above: `claim` runs on a different pooled connection, so it can
        // land between the two statements. Without it, discarding a draft the
        // user has just sent deletes the row — and the caller then unlinks the
        // bytes out from under a committed Message. Matching zero rows means we
        // lost that race, and the path must not be reported as removed.
        let deleted = diesel::delete(
            conversation_attachments::table
                .find(id.to_string())
                .filter(conversation_attachments::message_id.is_null()),
        )
        .execute(connection)?;
        Ok((deleted > 0).then_some(relative_path))
    }

    /// The safety net for drafts that were never posted: crashes, force-quits,
    /// abandoned composers, and discard RPCs that never landed. Returns the
    /// relative paths to unlink.
    pub fn sweep_unclaimed(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        created_before: DateTime<Utc>,
    ) -> Result<Vec<String>, StorageError> {
        let stale = conversation_attachments::table
            .filter(conversation_attachments::workspace_id.eq(workspace_id.to_string()))
            .filter(conversation_attachments::message_id.is_null())
            .filter(conversation_attachments::created_at.lt(created_before.timestamp_millis()))
            .select((
                conversation_attachments::id,
                conversation_attachments::relative_path,
            ))
            .load::<(String, String)>(connection)?;
        if stale.is_empty() {
            return Ok(Vec::new());
        }
        // Deleted one at a time, each re-checking `message_id IS NULL`, so only
        // rows that were *still* unclaimed at delete time are reported for
        // unlinking. A batch delete would report every path it selected,
        // including one a concurrent `claim` had just attached to a Message —
        // and the caller would unlink a live attachment's bytes.
        let mut removed = Vec::with_capacity(stale.len());
        for (id, relative_path) in stale {
            let deleted = diesel::delete(
                conversation_attachments::table
                    .find(id)
                    .filter(conversation_attachments::message_id.is_null()),
            )
            .execute(connection)?;
            if deleted > 0 {
                removed.push(relative_path);
            }
        }
        Ok(removed)
    }

    /// Every relative path still recorded for a Workspace. The sweep uses this
    /// to tell a live attachment from a file left behind by a crash between
    /// writing the bytes and inserting the row.
    pub fn all_relative_paths(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
    ) -> Result<Vec<String>, StorageError> {
        Ok(conversation_attachments::table
            .filter(conversation_attachments::workspace_id.eq(workspace_id.to_string()))
            .select(conversation_attachments::relative_path)
            .load::<String>(connection)?)
    }

    /// Runs before the Message delete in `ConversationRepository::delete` —
    /// the FK is RESTRICT, so skipping this blocks the whole cascade. Returns
    /// the relative paths to unlink after the transaction commits.
    pub fn delete_for_conversation(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
    ) -> Result<Vec<String>, StorageError> {
        let paths = conversation_attachments::table
            .filter(conversation_attachments::conversation_id.eq(conversation_id.to_string()))
            .select(conversation_attachments::relative_path)
            .load::<String>(connection)?;
        diesel::delete(
            conversation_attachments::table
                .filter(conversation_attachments::conversation_id.eq(conversation_id.to_string())),
        )
        .execute(connection)?;
        Ok(paths)
    }
}
