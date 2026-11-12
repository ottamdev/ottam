use super::ids::parse_attachment_id;
use super::wire::{iso, message_to_wire};
use super::{ensure_workspace_active, ChatService};
use crate::agent::HarnessPromptImage;
use crate::core_error::CoreError;
use crate::storage::error::StorageError;
use crate::storage::models::{
    AttachmentKind, AttachmentMimeType, AttachmentSourceKind, ConversationAttachment,
    ConversationAttachmentId, ConversationId, ConversationMessage, WorkspaceId,
};
use crate::storage::repositories::{
    AttachmentRepository, CreateAttachmentParams, MAX_ATTACHMENT_BYTES,
};
use crate::storage::{ensure_workspace_paths, Database};
use chrono::Utc;
use ottam_protocol::{
    ConversationAttachment as WireConversationAttachment, ConversationAttachmentID,
    ConversationAttachmentKind as WireAttachmentKind,
    ConversationAttachmentSourceKind as WireAttachmentSourceKind,
    ConversationCreateAttachmentParams, ConversationCreateAttachmentResult,
    ConversationDiscardAttachmentResult, ConversationMessage as WireConversationMessage,
};
use diesel::sqlite::SqliteConnection;
use std::path::Path;

impl ChatService {
    /// Take a client-written image file into the Workspace's attachment store.
    ///
    /// Core **copies** the source and never touches it again. Moving would make
    /// this RPC a primitive that unlinks any caller-supplied path — fatal for a
    /// drag-drop, where that path is the user's own project file — and `rename`
    /// fails across volumes anyway. The client deletes its own temp file.
    pub(in crate::server) async fn create_attachment(
        &self,
        params: ConversationCreateAttachmentParams,
    ) -> Result<ConversationCreateAttachmentResult, CoreError> {
        let workspace_id = self.workspace()?;
        let home = self.home.clone();
        self.database
            .run("conversation.createAttachment", move |connection| {
                ensure_workspace_active(connection, &workspace_id)?;
                let paths = ensure_workspace_paths(&home, &workspace_id)?;
                if let Some(existing) = AttachmentRepository::get_by_idempotency_key(
                    connection,
                    &workspace_id,
                    &params.idempotency_key,
                )? {
                    return Ok(ConversationCreateAttachmentResult {
                        attachment: attachment_to_wire(&paths.attachments, existing),
                    });
                }

                let source_path = Path::new(&params.source_path);
                if !source_path.is_absolute() {
                    return Err(attachment_unsafe("the attachment path must be absolute"));
                }
                let metadata = std::fs::symlink_metadata(source_path)
                    .map_err(|_| attachment_unsafe("the attachment file is unreadable"))?;
                if metadata.file_type().is_symlink() {
                    return Err(attachment_unsafe(
                        "the attachment path must not be a symbolic link",
                    ));
                }
                if !metadata.is_file() {
                    return Err(attachment_unsafe(
                        "the attachment path must be a regular file",
                    ));
                }
                if metadata.len() > MAX_ATTACHMENT_BYTES as u64 {
                    return Err(StorageError::new(
                        "attachmentTooLarge",
                        "the attachment exceeds the three-megabyte limit",
                    )
                    .recoverable(true));
                }

                let bytes = std::fs::read(source_path)
                    .map_err(|_| attachment_unsafe("the attachment file is unreadable"))?;
                // The declared type is a claim; the magic bytes are the fact.
                // They have to agree, or a caller could store a script under an
                // image MIME type and have Core hand it to an agent as one.
                let sniffed = sniff_image_mime(&bytes).ok_or_else(|| {
                    StorageError::new(
                        "attachmentTypeUnsupported",
                        "the attachment is not a supported image",
                    )
                    .recoverable(true)
                })?;
                let declared =
                    AttachmentMimeType::try_from(params.mime_type.as_str()).map_err(|_| {
                        StorageError::new(
                            "attachmentTypeUnsupported",
                            "the attachment is not a supported image",
                        )
                        .recoverable(true)
                    })?;
                if declared != sniffed {
                    return Err(StorageError::new(
                        "attachmentTypeUnsupported",
                        "the attachment's contents do not match its declared type",
                    )
                    .recoverable(true));
                }

                let source_kind = match params.source_kind {
                    WireAttachmentSourceKind::Pasted => AttachmentSourceKind::Pasted,
                    WireAttachmentSourceKind::Dropped => AttachmentSourceKind::Dropped,
                    WireAttachmentSourceKind::Picked => AttachmentSourceKind::Picked,
                };
                if matches!(source_kind, AttachmentSourceKind::Pasted)
                    != params.origin_path.is_none()
                {
                    return Err(StorageError::new(
                        "storageConstraint",
                        "only attachments taken from disk carry an origin path",
                    )
                    .recoverable(true));
                }

                let id = ConversationAttachmentId::new_v7();
                let relative_path = format!("{id}.{}", sniffed.file_extension());
                let stored_path = paths.attachments.join(&relative_path);
                write_attachment_file(&stored_path, &bytes)?;

                let created = AttachmentRepository::create_unclaimed(
                    connection,
                    &id,
                    &CreateAttachmentParams {
                        workspace_id: &workspace_id,
                        kind: AttachmentKind::Image,
                        mime_type: sniffed,
                        byte_count: bytes.len() as i64,
                        width: clamp_dimension(params.width),
                        height: clamp_dimension(params.height),
                        relative_path: &relative_path,
                        idempotency_key: &params.idempotency_key,
                        source_kind,
                        origin_path: params.origin_path.as_deref(),
                    },
                    Utc::now(),
                );
                let created = match created {
                    Ok(created) => created,
                    Err(error) => {
                        // The row is what makes the file reachable. Without one
                        // the bytes are unreferenced, so drop them now rather
                        // than leave them for the reconciliation sweep.
                        let _ = std::fs::remove_file(&stored_path);
                        return Err(error);
                    }
                };

                Ok(ConversationCreateAttachmentResult {
                    attachment: attachment_to_wire(&paths.attachments, created),
                })
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn discard_attachment(
        &self,
        attachment_id: &ConversationAttachmentID,
    ) -> Result<ConversationDiscardAttachmentResult, CoreError> {
        let workspace_id = self.workspace()?;
        let attachment_id = parse_attachment_id(attachment_id)?;
        let home = self.home.clone();
        let sweep_workspace_id = workspace_id;
        let removed = self
            .database
            .run("conversation.discardAttachment", move |connection| {
                AttachmentRepository::discard_unclaimed(connection, &workspace_id, &attachment_id)
            })
            .await
            .map_err(CoreError::from)?;
        let removed = removed.into_iter().collect::<Vec<_>>();
        let discarded = !removed.is_empty();
        unlink_attachment_files(&home, &sweep_workspace_id, &removed);
        Ok(ConversationDiscardAttachmentResult { discarded })
    }
}

/// Map a page of Messages, loading every Message's attachments in one query.
/// A per-Message lookup would be N+1 across a fifty-Message page.
pub(super) fn messages_to_wire(
    connection: &mut SqliteConnection,
    home: &Path,
    workspace_id: &WorkspaceId,
    conversation_id: &ConversationId,
    messages: Vec<ConversationMessage>,
) -> Result<Vec<WireConversationMessage>, StorageError> {
    let message_ids = messages
        .iter()
        .map(|message| message.id)
        .collect::<Vec<_>>();
    let mut grouped =
        AttachmentRepository::list_for_messages(connection, conversation_id, &message_ids)?;
    if grouped.is_empty() {
        return Ok(messages
            .into_iter()
            .map(|message| message_to_wire(message, Vec::new()))
            .collect());
    }
    let paths = ensure_workspace_paths(home, workspace_id)?;
    Ok(messages
        .into_iter()
        .map(|message| {
            let attachments = grouped
                .remove(&message.id)
                .unwrap_or_default()
                .into_iter()
                .map(|attachment| attachment_to_wire(&paths.attachments, attachment))
                .collect();
            message_to_wire(message, attachments)
        })
        .collect())
}

/// How long a draft attachment may sit unclaimed before the sweep takes it.
/// Long enough that a composer left open overnight survives.
const UNCLAIMED_ATTACHMENT_TTL_HOURS: i64 = 24;
/// How long a file with no row is left alone before the sweep reclaims it.
/// Long enough that a createAttachment running concurrently with the sweep
/// cannot have its bytes deleted out from under its own insert.
const STRAY_ATTACHMENT_GRACE_SECS: u64 = 3600;

/// Reap attachments a Workspace no longer needs: draft rows whose Message was
/// never posted, and files on disk with no row at all.
///
/// The second half matters because `create_attachment` writes bytes before it
/// inserts — a crash in between leaves a file nothing references. Explicit
/// discard covers the common case; this covers crashes, force-quits, and
/// discard calls that never landed.
///
/// Best effort throughout: a failure here must never block opening a Workspace.
pub(in crate::server) async fn sweep_attachments(database: &Database, workspace_id: &WorkspaceId) {
    let home = database.home().to_path_buf();
    let workspace_id = *workspace_id;
    let cutoff = Utc::now() - chrono::Duration::hours(UNCLAIMED_ATTACHMENT_TTL_HOURS);
    let swept = database
        .run("conversation.sweepAttachments", move |connection| {
            let expired = AttachmentRepository::sweep_unclaimed(connection, &workspace_id, cutoff)?;
            let live = AttachmentRepository::all_relative_paths(connection, &workspace_id)?;
            Ok((expired, live))
        })
        .await;
    let Ok((expired, live)) = swept else {
        return;
    };
    let _ = tokio::task::spawn_blocking(move || {
        unlink_attachment_files(&home, &workspace_id, &expired);
        reconcile_attachment_files(&home, &workspace_id, live);
    })
    .await;
}

fn reconcile_attachment_files(home: &Path, workspace_id: &WorkspaceId, live: Vec<String>) {
    let Ok(paths) = ensure_workspace_paths(home, workspace_id) else {
        return;
    };
    let live = live.into_iter().collect::<std::collections::HashSet<_>>();
    let Ok(entries) = std::fs::read_dir(&paths.attachments) else {
        return;
    };
    let grace =
        std::time::SystemTime::now() - std::time::Duration::from_secs(STRAY_ATTACHMENT_GRACE_SECS);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if live.contains(name) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        // `live` was read before this directory listing, so a file written by a
        // concurrent createAttachment is legitimately absent from it. Only
        // reclaim files old enough that no in-flight call could still own them.
        if metadata.modified().map(|at| at > grace).unwrap_or(true) {
            continue;
        }
        if let Err(error) = std::fs::remove_file(entry.path()) {
            tracing::warn!(
                target: "ottam::chat",
                error = %error,
                "failed to remove an unreferenced attachment file",
            );
        }
    }
}

fn attachment_unsafe(message: &'static str) -> StorageError {
    StorageError::new("attachmentPathUnsafe", message).recoverable(true)
}

/// Dimensions are presentation hints supplied by the client — Core does not
/// decode the image to verify them. Clamp so a bad value cannot violate the
/// column CHECK and turn a cosmetic mistake into a failed attachment.
fn clamp_dimension(value: u32) -> i64 {
    (value.max(1) as i64).min(20_000)
}

/// Identify an image by its magic bytes. Deliberately narrow: exactly the four
/// types the column CHECK accepts, and nothing inferred from a file name.
fn sniff_image_mime(bytes: &[u8]) -> Option<AttachmentMimeType> {
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.starts_with(PNG) {
        return Some(AttachmentMimeType::Png);
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(AttachmentMimeType::Jpeg);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(AttachmentMimeType::Gif);
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some(AttachmentMimeType::Webp);
    }
    None
}

/// Write the stored copy and fsync it, so a crash cannot leave a row pointing
/// at a truncated file.
fn write_attachment_file(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    use std::io::Write;

    let unavailable = || {
        StorageError::new(
            "workspaceFilesUnavailable",
            "failed to store the attachment file",
        )
        .recoverable(true)
    };
    let mut file = std::fs::File::create(path).map_err(|_| unavailable())?;
    file.write_all(bytes).map_err(|_| unavailable())?;
    file.sync_all().map_err(|_| unavailable())?;
    Ok(())
}

/// Resolve one Message's attachments into the paths the adapter will read.
/// Returns empty for assistant messages and for any Message with none.
pub(super) fn message_prompt_images(
    connection: &mut SqliteConnection,
    home: &Path,
    workspace_id: &WorkspaceId,
    conversation_id: &ConversationId,
    message: &ConversationMessage,
) -> Result<Vec<HarnessPromptImage>, StorageError> {
    let attachments =
        AttachmentRepository::list_for_messages(connection, conversation_id, &[message.id])?
            .remove(&message.id)
            .unwrap_or_default();
    if attachments.is_empty() {
        return Ok(Vec::new());
    }
    let paths = ensure_workspace_paths(home, workspace_id)?;
    Ok(attachments
        .into_iter()
        .map(|attachment| HarnessPromptImage {
            mime_type: attachment.mime_type.as_storage_str().to_string(),
            path: paths.attachments.join(&attachment.relative_path),
            // Only images the user attached from their own disk have a file
            // the agent could go read for itself.
            uri: attachment
                .origin_path
                .map(|origin| format!("file://{origin}")),
        })
        .collect())
}

pub(super) fn wire_attachments(
    home: &Path,
    workspace_id: &WorkspaceId,
    attachments: Vec<ConversationAttachment>,
) -> Result<Vec<WireConversationAttachment>, StorageError> {
    if attachments.is_empty() {
        return Ok(Vec::new());
    }
    let paths = ensure_workspace_paths(home, workspace_id)?;
    Ok(attachments
        .into_iter()
        .map(|attachment| attachment_to_wire(&paths.attachments, attachment))
        .collect())
}

fn attachment_to_wire(
    attachments_root: &Path,
    attachment: ConversationAttachment,
) -> WireConversationAttachment {
    WireConversationAttachment {
        attachment_id: ConversationAttachmentID(attachment.id.to_string()),
        kind: match attachment.kind {
            AttachmentKind::Image => WireAttachmentKind::Image,
        },
        mime_type: attachment.mime_type.as_storage_str().to_string(),
        byte_count: attachment.byte_count.max(0) as u64,
        width: attachment.width.max(0) as u32,
        height: attachment.height.max(0) as u32,
        // Absolute, because the app reads these bytes directly off disk rather
        // than over JSONL — image data would not fit in one frame.
        path: attachments_root
            .join(&attachment.relative_path)
            .to_string_lossy()
            .into_owned(),
        source_kind: match attachment.source_kind {
            AttachmentSourceKind::Pasted => WireAttachmentSourceKind::Pasted,
            AttachmentSourceKind::Dropped => WireAttachmentSourceKind::Dropped,
            AttachmentSourceKind::Picked => WireAttachmentSourceKind::Picked,
        },
        origin_path: attachment.origin_path,
        created_at: iso(attachment.created_at),
    }
}

/// Remove attachment bytes for rows a transaction already deleted. Best effort
/// by design: a failure leaves a stray file that the reconciliation sweep in
/// `sweep_attachments` collects on the next Workspace open.
pub(super) fn unlink_attachment_files(
    home: &Path,
    workspace_id: &WorkspaceId,
    relative_paths: &[String],
) {
    if relative_paths.is_empty() {
        return;
    }
    let Ok(paths) = ensure_workspace_paths(home, workspace_id) else {
        return;
    };
    for relative_path in relative_paths {
        let path = paths.attachments.join(relative_path);
        if !path.starts_with(&paths.attachments) {
            continue;
        }
        if let Err(error) = std::fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    target: "ottam::chat",
                    path = %path.display(),
                    "failed to remove an attachment file",
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::ConversationMessageId;
    use crate::storage::repositories::{
        ConversationRepository, ConversationRootSelection, CreateConversationParams,
        InitialMessage, WorkspaceRepository,
    };
    use diesel::connection::{Connection, InstrumentationEvent};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn create_test_attachment(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        label: &str,
        source_kind: AttachmentSourceKind,
        origin_path: Option<&str>,
    ) -> Result<ConversationAttachmentId, StorageError> {
        let id = ConversationAttachmentId::new_v7();
        AttachmentRepository::create_unclaimed(
            connection,
            &id,
            &CreateAttachmentParams {
                workspace_id,
                kind: AttachmentKind::Image,
                mime_type: AttachmentMimeType::Png,
                byte_count: 100,
                width: 10,
                height: 20,
                relative_path: &format!("{label}.png"),
                idempotency_key: &format!("attachment-{label}"),
                source_kind,
                origin_path,
            },
            Utc::now(),
        )?;
        Ok(id)
    }

    #[tokio::test]
    async fn message_attachment_projection_stays_batched_and_preserves_prompt_metadata() {
        let home = TempDir::new().expect("temporary home");
        let database = Database::open_test_home(home.path().to_path_buf()).expect("opens database");
        let projection_home = home.path().to_path_buf();
        database
            .run("test.messageAttachmentProjection", move |connection| {
                let workspace = WorkspaceRepository::create(connection, "Chat", Utc::now())?;
                let created = ConversationRepository::create(
                    connection,
                    &CreateConversationParams {
                        workspace_id: &workspace.id,
                        title: "Attachments",
                        root: ConversationRootSelection::WorkspaceFiles,
                        context_projects: &[],
                        initial_message: Some(InitialMessage {
                            body: "first",
                            idempotency_key: "message-0",
                        }),
                    },
                    Utc::now(),
                )?;
                let initial = created.initial_input.expect("initial message").message;
                let picked = create_test_attachment(
                    connection,
                    &workspace.id,
                    "picked",
                    AttachmentSourceKind::Picked,
                    Some("/tmp/picked.png"),
                )?;
                let pasted = create_test_attachment(
                    connection,
                    &workspace.id,
                    "pasted",
                    AttachmentSourceKind::Pasted,
                    None,
                )?;
                let dropped = create_test_attachment(
                    connection,
                    &workspace.id,
                    "dropped",
                    AttachmentSourceKind::Dropped,
                    Some("/tmp/dropped.png"),
                )?;
                AttachmentRepository::claim(
                    connection,
                    &workspace.id,
                    &created.conversation.id,
                    &initial.id,
                    &[picked, pasted, dropped],
                    Utc::now(),
                )?;

                let prompt_images = message_prompt_images(
                    connection,
                    &projection_home,
                    &workspace.id,
                    &created.conversation.id,
                    &initial,
                )?;
                assert_eq!(
                    prompt_images
                        .iter()
                        .map(|image| image.uri.as_deref())
                        .collect::<Vec<_>>(),
                    vec![
                        Some("file:///tmp/picked.png"),
                        None,
                        Some("file:///tmp/dropped.png"),
                    ]
                );
                assert_eq!(
                    prompt_images
                        .iter()
                        .map(|image| {
                            image
                                .path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .expect("attachment file name")
                        })
                        .collect::<Vec<_>>(),
                    vec!["picked.png", "pasted.png", "dropped.png"]
                );

                let mut messages = vec![initial];
                for index in 1..25 {
                    messages.push(
                        ConversationRepository::post_message(
                            connection,
                            &workspace.id,
                            &created.conversation.id,
                            &format!("message-{index}"),
                            &format!("message-{index}"),
                            Utc::now(),
                        )?
                        .message,
                    );
                }
                let message_ids = messages
                    .iter()
                    .map(|message| message.id)
                    .collect::<Vec<ConversationMessageId>>();
                let query_count = Arc::new(AtomicUsize::new(0));
                let observed_count = Arc::clone(&query_count);
                connection.set_instrumentation(move |event: InstrumentationEvent<'_>| {
                    if matches!(event, InstrumentationEvent::StartQuery { .. }) {
                        observed_count.fetch_add(1, Ordering::SeqCst);
                    }
                });

                let one = messages_to_wire(
                    connection,
                    &projection_home,
                    &workspace.id,
                    &created.conversation.id,
                    vec![messages[0].clone()],
                )?;
                assert_eq!(one[0].attachments.len(), 3);
                let one_query_count = query_count.swap(0, Ordering::SeqCst);

                let many = messages_to_wire(
                    connection,
                    &projection_home,
                    &workspace.id,
                    &created.conversation.id,
                    messages,
                )?;
                let many_query_count = query_count.load(Ordering::SeqCst);
                assert_eq!(many_query_count, one_query_count);
                assert!(
                    many_query_count <= 1,
                    "message projection must stay batched"
                );
                assert_eq!(many.len(), 25);
                assert_eq!(
                    many.iter()
                        .map(|message| message.message_id.0.as_str())
                        .collect::<Vec<_>>(),
                    message_ids
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                );
                Ok(())
            })
            .await
            .expect("projects message attachments");
    }
}
