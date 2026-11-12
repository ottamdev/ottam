//! SQLite paging and descriptors using the existing RPC and checkpoint boundaries.
use super::*;
use crate::storage::payload::{Manifest, Reader, RevisionKey};
use ottam_protocol::{
    ConversationReadTranscriptPayloadParams, ConversationReadTranscriptPayloadResult,
};

pub(super) fn descriptor(manifest: Manifest) -> TranscriptPayload {
    let unavailable = manifest
        .parts
        .iter()
        .find_map(|p| p.unavailable_reason.clone());
    let reason = manifest.capture_reason.or(unavailable);
    let locally_truncated = manifest.locally_truncated || reason.is_some();
    TranscriptPayload {
        role: manifest.role,
        availability: if locally_truncated {
            "partial"
        } else {
            "available"
        }
        .into(),
        reason,
        payload_id: Some(manifest.key.payload_id),
        byte_count: Some(
            manifest
                .parts
                .iter()
                .map(|p| p.committed_raw_bytes as u64)
                .sum(),
        ),
        upstream_truncated: manifest.upstream_truncated,
        locally_truncated,
        revision: manifest.key.revision,
        parts: manifest
            .parts
            .into_iter()
            .map(|p| TranscriptPayloadPart {
                name: p.name,
                format: p.format,
                path: p.source_path,
                byte_count: p.committed_raw_bytes as u64,
                digest: p
                    .final_sha256
                    .map(|bytes| bytes.iter().map(|b| format!("{b:02x}")).collect()),
                chunk_digests: Vec::new(),
            })
            .collect(),
    }
}

impl super::super::ChatService {
    /// Hands a sealed descriptor to the existing coordinator. Only its subsequent
    /// checkpoint commits the main reference; this is not a publication receipt.
    #[cfg(test)]
    pub(in crate::server::chat) async fn stage_sealed_payload(
        &self,
        key: RevisionKey,
    ) -> Result<(), crate::core_error::CoreError> {
        if key.workspace_id != self.workspace_id.0 {
            return Err(StorageError::not_found("Payload").into());
        }
        let home = self.home.clone();
        let conversation = key.conversation_id.clone();
        let item = key.item_id.clone();
        let payload = tokio::task::spawn_blocking(move || {
            Reader::open(&home)?.manifest(&key).map(descriptor)
        })
        .await
        .map_err(|_| StorageError::new("payloadReadFailed", "Payload reader stopped"))?
        .map_err(|_| {
            StorageError::new("payloadReadFailed", "Sealed payload manifest unavailable")
        })?;
        self.transcript_coordinator
            .payload_updated(&conversation, &item, payload, None);
        Ok(())
    }

    pub(in crate::server) async fn read_transcript_payload(
        &self,
        params: ConversationReadTranscriptPayloadParams,
    ) -> Result<ConversationReadTranscriptPayloadResult, crate::core_error::CoreError> {
        let AuthorizedPayload {
            descriptor,
            saved_revision,
            item_id,
        } = self.authorize_payload(&params).await?;
        if !matches!(
            descriptor.availability.as_str(),
            "available" | "partial" | "pending"
        ) {
            return Ok(page_response(params, descriptor, 0, false, Ok(Vec::new())));
        }
        let part = descriptor
            .parts
            .get(params.part as usize)
            .ok_or_else(|| StorageError::not_found("PayloadPart"))?;
        let size = part.byte_count;
        let key = RevisionKey {
            workspace_id: self.workspace_id.0.clone(),
            conversation_id: params.conversation_id.0.clone(),
            item_id: item_id.ok_or_else(|| StorageError::not_found("Payload"))?,
            payload_id: params.payload_id.clone(),
            revision: params.revision,
        };
        let home = self.home.clone();
        let index = params.part;
        let offset = params.offset;
        let length = params.length.unwrap_or(PAGE as u32).clamp(1, PAGE as u32) as usize;
        let pending = descriptor.availability == "pending";
        let lease_database = self.database.clone();
        let lease_key = key.clone();
        let _read_lease =
            tokio::task::spawn_blocking(move || lease_database.payload_cache_read(lease_key))
                .await
                .map_err(|_| {
                    StorageError::new("payloadReadFailed", "Cache read admission stopped")
                })?;
        let restored = if !pending {
            let home = home.clone();
            let lookup_key = key.clone();
            let recovery = tokio::task::spawn_blocking(move || {
                crate::storage::payload::cache::Cache::open(&home)?.recovery(&lookup_key)
            })
            .await
            .map_err(|_| StorageError::new("payloadReadFailed", "Cache lookup stopped"))?
            .map_err(|_| StorageError::new("payloadReadFailed", "Cache lookup failed"))?;
            if let Some(recovery) = recovery {
                let outcome = async {
                    let registry = self
                        .registry
                        .clone()
                        .ok_or_else(|| "Harness runtime is unavailable".to_owned())?;
                    let agent = recovery
                        .agent_id
                        .ok_or_else(|| "Original agent identifier is unavailable".to_owned())?;
                    if registry
                        .definition(&crate::agent::AgentId(agent.clone()))
                        .is_err()
                    {
                        return Err("This agent was removed".into());
                    }
                    let session = recovery
                        .session_id
                        .ok_or_else(|| "Original session identifier is unavailable".to_owned())?;
                    let tool = recovery
                        .tool_call_id
                        .ok_or_else(|| "Original tool identifier is unavailable".to_owned())?;
                    let cwd = match recovery.cwd {
                        Some(cwd) => std::path::PathBuf::from(cwd),
                        None => self
                            .working_root(&params.conversation_id)
                            .await
                            .map_err(|e| e.to_string())?
                            .canonical_path
                            .into(),
                    };
                    super::recovery::load(registry, self.home.clone(), agent, session, tool, cwd)
                        .await
                }
                .await;
                match outcome {
                    Ok(bodies) => Some(bodies),
                    Err(reason) => {
                        if reason == "This agent was removed" {
                            return Err(StorageError::new("agentRemoved", "This agent was removed. Uncached tool results cannot be recovered.").into());
                        }
                        tracing::warn!(payload_id = %key.payload_id, reason = %reason, "evicted tool result recovery failed");
                        return Err(StorageError::new("payloadUnavailable", format!("We tried loading the saved session, but this tool result could not be restored. {reason}. You can retry.")).into());
                    }
                }
            } else {
                None
            }
        } else {
            None
        };
        let database = self.database.clone();
        let attempted_recovery = restored.is_some();
        let read = tokio::task::spawn_blocking(move || {
            let _gate = database.payload_cache_gate().lock().unwrap();
            let mut cache =
                crate::storage::payload::cache::Cache::open(&home).map_err(read_error)?;
            if let Some(bodies) = restored {
                if cache.recovery(&key).map_err(read_error)?.is_some() {
                    database.payload_worker().check_storage_space(bodies.iter().map(|body| body.len() as u64).sum()).map_err(read_error)?;
                    cache.restore(&key, &bodies).map_err(|error| {
                        tracing::warn!(payload_id = %key.payload_id, reason = %error, "evicted tool result restoration failed");
                        read_error(error)
                    })?;
                }
            }
            cache.touch(&key).map_err(read_error)?;
            let mut reader = Reader::open(&home).map_err(read_error)?;
            if pending {
                reader
                    .read_active(&key, index, size, offset, length)
                    .map_err(read_error)
            } else {
                reader
                    .read_sealed(&key, index, size, offset, length)
                    .map_err(read_error)
            }
        })
        .await
        .map_err(|_| StorageError::new("payloadReadFailed", "Payload reader stopped"))?;
        if attempted_recovery && read.is_err() {
            return Err(StorageError::new("payloadUnavailable", "We tried loading the saved session, but this tool result could not be restored. You can retry.").into());
        }
        let unsaved = pending || saved_revision != Some(descriptor.revision);
        Ok(page_response(params, descriptor, size, unsaved, read))
    }
}

fn read_error(error: Box<dyn std::error::Error + Send + Sync>) -> std::io::Error {
    if error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    {
        std::io::Error::new(std::io::ErrorKind::NotFound, error)
    } else {
        std::io::Error::other(error)
    }
}
