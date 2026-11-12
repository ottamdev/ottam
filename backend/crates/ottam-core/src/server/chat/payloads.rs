//! Managed, bounded payload capture. Payload I/O never holds a main database
//! gateway permit or a transcript/state mutex. Checkpoints own publication of
//! the resulting immutable references.
mod normalize;
mod recovery;
pub(super) mod sqlite;
#[cfg(test)]
mod tests;

use super::TranscriptCoordinator;
use crate::storage::{Database, StorageError};
use ottam_protocol::{TranscriptPayload, TranscriptPayloadPart};
use diesel::prelude::*;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, Weak,
    },
};
use tokio::sync::Notify;

pub(super) const LIMIT: usize = 16 * 1024 * 1024;
pub(super) const PAGE: usize = 64 * 1024;

pub(super) struct Part {
    name: String,
    format: String,
    bytes: Vec<u8>,
    path: Option<String>,
    reason: Option<String>,
}
impl Part {
    fn text(name: &str, text: String, path: Option<String>) -> Self {
        Self {
            name: name.into(),
            format: "text/plain".into(),
            bytes: text.into_bytes(),
            path,
            reason: None,
        }
    }
    fn unavailable(name: &str, reason: &str) -> Self {
        Self {
            name: name.into(),
            format: "unavailable".into(),
            bytes: Vec::new(),
            path: None,
            reason: Some(reason.into()),
        }
    }
    fn json(name: &str, value: &Value) -> Self {
        struct Bounded(Vec<u8>);
        impl Write for Bounded {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let size = bytes.len().min(LIMIT.saturating_sub(self.0.len()));
                self.0.extend_from_slice(&bytes[..size]);
                if size == 0 && !bytes.is_empty() {
                    return Err(std::io::Error::other("capture limit"));
                }
                Ok(size)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = Bounded(Vec::new());
        let truncated = serde_json::to_writer_pretty(&mut writer, value).is_err();
        Self {
            name: name.into(),
            format: "application/json".into(),
            bytes: writer.0,
            path: None,
            reason: truncated.then(|| "captureLimit".into()),
        }
    }
}

pub(super) struct Update {
    role: String,
    parts: Option<Vec<Part>>,
    append: bool,
    publish: bool,
    upstream: Option<bool>,
}
pub(super) fn extract(payload: &mut Value) -> Vec<Update> {
    normalize::extract(payload)
}

/// Whether a tool call's input is a file change, whose contents are captured
/// as a payload instead of kept as arguments. Payload extraction and the
/// transcript's tool row both ask this, so they agree on where text went.
///
/// Not by the tool `name`: it is a real identifier, and a substring match
/// would read Codex's `write_stdin` or an MCP `write_note` as a file change.
/// An MCP call's input, or an Oh-My-Pi device run's (`device`), is its
/// arguments, whatever its caption says.
pub(in crate::server::chat) fn is_file_change(
    name: Option<&str>,
    kind: Option<&str>,
    title: Option<&str>,
    device: bool,
    input: &Value,
) -> bool {
    if device || name.is_some_and(|name| name.starts_with("mcp__")) {
        return false;
    }
    let label = format!("{} {}", kind.unwrap_or_default(), title.unwrap_or_default()).to_lowercase();
    ["write", "edit", "patch"].iter().any(|s| label.contains(s))
        || ["oldText", "newText", "old_string", "new_string", "patch"]
            .iter()
            .any(|key| input.get(key).is_some())
        || ((input.get("path").is_some() || input.get("file_path").is_some())
            && input.get("content").is_some())
}

/// Whether a tool payload is an Oh-My-Pi device run (`DEVICE_CALL_STAMP`).
pub(in crate::server::chat) fn is_device_call(payload: &Value) -> bool {
    payload
        .get(crate::agent::DEVICE_CALL_STAMP)
        .and_then(Value::as_bool)
        == Some(true)
}

mod live_service;
pub(in crate::server::chat) use live_service::PayloadService;

impl super::ChatService {
    async fn authorize_payload(
        &self,
        params: &ottam_protocol::ConversationReadTranscriptPayloadParams,
    ) -> Result<AuthorizedPayload, crate::core_error::CoreError> {
        let workspace = self.workspace()?;
        let conversation = super::ids::parse_conversation_id(&params.conversation_id)?;
        uuid::Uuid::parse_str(&params.payload_id)
            .map_err(|_| StorageError::not_found("Payload"))?;
        let cid = conversation.to_string();
        let pid = params.payload_id.clone();
        let saved = self.database.run("transcript.payloadDescriptor", move |connection| {
            super::ensure_workspace_active(connection, &workspace)?;
            use crate::storage::repositories::ConversationRepository;
            ConversationRepository::get(connection, &workspace, &conversation)?;
            #[derive(QueryableByName)] struct Row {
                #[diesel(sql_type = diesel::sql_types::Text)] descriptor: String,
                #[diesel(sql_type = diesel::sql_types::Text)] item_id: String,
            }
            Ok(diesel::sql_query("SELECT p.descriptor, p.item_id FROM transcript_payloads p JOIN transcript_items i ON i.id = p.item_id WHERE p.payload_id = ? AND i.conversation_id = ? ORDER BY p.revision DESC LIMIT 1")
                .bind::<diesel::sql_types::Text,_>(pid).bind::<diesel::sql_types::Text,_>(cid).get_result::<Row>(connection).optional()?.map(|r| (r.descriptor, r.item_id)))
        }).await?;
        let live = self.transcript_coordinator.payloads.live(
            &self.workspace_id.0,
            &params.conversation_id.0,
            &params.payload_id,
        );
        let item_id = self
            .transcript_coordinator
            .payloads
            .live_key(
                &self.workspace_id.0,
                &params.conversation_id.0,
                &params.payload_id,
            )
            .map(|key| key.item_id)
            .or_else(|| saved.as_ref().map(|(_, id)| id.clone()));
        let saved = saved
            .map(|(s, _)| serde_json::from_str::<TranscriptPayload>(&s))
            .transpose()
            .map_err(|_| StorageError::new("storageCorrupt", "Invalid payload descriptor"))?;
        let saved_revision = saved.as_ref().map(|p| p.revision);
        let descriptor = live
            .or(saved)
            .ok_or_else(|| StorageError::not_found("Payload"))?;
        if descriptor.revision != params.revision {
            return Err(StorageError::new(
                "stalePayload",
                "The payload changed; refresh its descriptor",
            )
            .into());
        }
        Ok(AuthorizedPayload {
            descriptor,
            saved_revision,
            item_id,
        })
    }
}

struct AuthorizedPayload {
    descriptor: TranscriptPayload,
    saved_revision: Option<u64>,
    // Bind storage lookup to the authorized owning item.
    item_id: Option<String>,
}

fn page_response(
    params: ottam_protocol::ConversationReadTranscriptPayloadParams,
    descriptor: TranscriptPayload,
    size: u64,
    unsaved: bool,
    read: std::io::Result<Vec<u8>>,
) -> ottam_protocol::ConversationReadTranscriptPayloadResult {
    use base64::Engine;
    let (data, availability, reason) = match read {
        Ok(bytes) => (bytes, descriptor.availability, descriptor.reason),
        Err(e) => (
            Vec::new(),
            if e.kind() == std::io::ErrorKind::NotFound {
                "missing"
            } else {
                "failed"
            }
            .into(),
            Some("Saved detail could not be read".into()),
        ),
    };
    let end = params.offset.saturating_add(data.len() as u64);
    ottam_protocol::ConversationReadTranscriptPayloadResult {
        payload_id: params.payload_id,
        revision: params.revision,
        part: params.part,
        offset: params.offset,
        data: base64::engine::general_purpose::STANDARD.encode(&data),
        next_offset: (!data.is_empty() && end < size).then_some(end),
        readable_length: size,
        availability,
        reason,
        unsaved,
    }
}
