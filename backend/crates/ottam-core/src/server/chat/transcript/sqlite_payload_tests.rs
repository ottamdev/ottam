use super::*;
use crate::storage::payload::{worker::Worker, Part, Reader, Revision, RevisionKey};
use base64::Engine;
use diesel::connection::SimpleConnection;
use diesel::Connection;
use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl};
use serde_json::json;

async fn seed(
    chat: &ChatService,
    conversation: &ConversationID,
    run: &ConversationRunID,
) -> RevisionKey {
    chat.transcript_coordinator.manual_checkpoints();
    chat.record_transcript_update(
        conversation,
        run,
        AgentUpdate::ToolCallUpdate {
            payload: json!({"toolCallId":"sqlite-tool","kind":"execute","status":"in_progress"}),
        },
    )
    .await
    .unwrap();
    let page = chat
        .list_transcript(ConversationListTranscriptParams {
            workspace_id: chat.workspace_id.clone(),
            conversation_id: conversation.clone(),
            limit: None,
            cursor: None,
            subagent_thread_id: None,
        })
        .await
        .unwrap();
    let key = RevisionKey {
        workspace_id: chat.workspace_id.0.clone(),
        conversation_id: conversation.0.clone(),
        item_id: page
            .items
            .iter()
            .find(|i| i.tool.is_some())
            .unwrap()
            .item_id
            .clone(),
        payload_id: Uuid::new_v4().to_string(),
        revision: 1,
    };
    // The manual handoff fixture owns the same publication lease as live capture.
    chat.database
        .payload_leases()
        .lock()
        .unwrap()
        .insert(key.clone());
    key
}

async fn seal(worker: &Worker, key: &RevisionKey, run: &ConversationRunID, values: &[&[u8]]) {
    let id = worker
        .begin(Revision {
            workspace_id: key.workspace_id.clone(),
            conversation_id: key.conversation_id.clone(),
            item_id: key.item_id.clone(),
            payload_id: key.payload_id.clone(),
            revision: key.revision,
            run_id: run.0.clone(),
            role: "output".into(),
            upstream_truncated: Some(true),
            parts: values
                .iter()
                .enumerate()
                .map(|(i, _)| Part {
                    name: format!("part-{i}"),
                    format: "text/plain".into(),
                    source_path: None,
                    unavailable_reason: None,
                })
                .collect(),
        })
        .unwrap();
    for (part, bytes) in values.iter().enumerate() {
        // Split inside multibyte UTF-8, without saturating the input-job limit.
        for fragment in bytes.chunks(997) {
            worker.enqueue(id, part, fragment.to_vec()).unwrap();
        }
    }
    worker.complete(id).unwrap().await.unwrap().unwrap();
}

fn request(key: &RevisionKey, part: u32, offset: u64) -> ConversationReadTranscriptPayloadParams {
    ConversationReadTranscriptPayloadParams {
        workspace_id: WorkspaceID(key.workspace_id.clone()),
        conversation_id: ConversationID(key.conversation_id.clone()),
        payload_id: key.payload_id.clone(),
        revision: key.revision,
        part,
        offset,
        length: Some(u32::MAX),
    }
}

#[tokio::test]
async fn sqlite_payload_cache_clear_protects_reads_and_reports_unavailable_recovery() {
    let (_home, chat, conversation, run) = tests::fixture().await;
    let key = seed(&chat, &conversation, &run).await;
    seal(
        chat.database.payload_worker(),
        &key,
        &run,
        &[b"cached output"],
    )
    .await;
    chat.stage_sealed_payload(key.clone()).await.unwrap();
    chat.transcript_coordinator.checkpoint().await;
    let run_id = run.0.clone();
    chat.database
        .run("test.finishCacheRun", move |c| {
            use crate::storage::schema::conversation_runs as runs;
            let internal_session = "019b0000-0000-7000-8000-000000000099";
            diesel::sql_query("INSERT INTO agent_sessions (id, workspace_id, conversation_id, engine_id, external_session_id, source_scope_fingerprint, status, created_at, last_used_at) SELECT ?, workspace_id, conversation_id, engine_id, 'provider-session', 'scope', 'closed', 0, 0 FROM conversation_runs WHERE id = ?")
                .bind::<diesel::sql_types::Text, _>(internal_session)
                .bind::<diesel::sql_types::Text, _>(&run_id)
                .execute(c)?;
            diesel::update(runs::table.find(run_id))
                .set((runs::completed_at.eq(Some(1_i64)), runs::agent_session_id.eq(Some(internal_session))))
                .execute(c)?;
            Ok(())
        })
        .await
        .unwrap();
    let (protected, _) = crate::server::storage_cache::sweep(&chat.database, true, None, true)
        .await
        .unwrap();
    assert!(protected.cache_bytes > 0);
    chat.database.payload_leases().lock().unwrap().remove(&key);
    let read = chat.database.payload_cache_read(key.clone());
    let (protected, _) = crate::server::storage_cache::sweep(&chat.database, true, None, true)
        .await
        .unwrap();
    assert!(protected.cache_bytes > 0);
    drop(read);
    let (cleared, _) = crate::server::storage_cache::sweep(&chat.database, true, None, true)
        .await
        .unwrap();
    assert_eq!(cleared.cache_bytes, 0);
    let recovery = crate::storage::payload::cache::Cache::open(chat.database.home())
        .unwrap()
        .recovery(&key)
        .unwrap()
        .unwrap();
    assert_eq!(recovery.session_id.as_deref(), Some("provider-session"));
    assert!(Reader::open(chat.database.home())
        .unwrap()
        .manifest(&key)
        .is_ok());
    let error = chat
        .read_transcript_payload(request(&key, 0, 0))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("could not be restored"));
}

#[tokio::test]
async fn sqlite_payload_partial_descriptors_and_read_failures_are_explicit() {
    let (home, chat, conversation, run) = tests::fixture().await;
    let key = seed(&chat, &conversation, &run).await;
    let worker = chat.database.payload_worker();
    let output = worker
        .begin(Revision {
            workspace_id: key.workspace_id.clone(),
            conversation_id: key.conversation_id.clone(),
            item_id: key.item_id.clone(),
            payload_id: key.payload_id.clone(),
            revision: 1,
            run_id: run.0.clone(),
            role: "output".into(),
            upstream_truncated: None,
            parts: vec![
                Part {
                    name: "text".into(),
                    format: "text/plain".into(),
                    source_path: Some("source.log".into()),
                    unavailable_reason: None,
                },
                Part {
                    name: "attachment".into(),
                    format: "unavailable".into(),
                    source_path: None,
                    unavailable_reason: Some("unsupported".into()),
                },
            ],
        })
        .unwrap();
    assert!(worker
        .enqueue(output, 0, vec![0; 16 * 1024 * 1024 + 1])
        .is_err());
    worker.enqueue(output, 0, b"retained".to_vec()).unwrap();
    worker.complete(output).unwrap().await.unwrap().unwrap();
    chat.stage_sealed_payload(key.clone()).await.unwrap();
    chat.transcript_coordinator.checkpoint().await;
    let result = chat
        .read_transcript_payload(request(&key, 0, 0))
        .await
        .unwrap();
    assert_eq!(result.availability, "partial");
    assert_eq!(result.reason.as_deref(), Some("captureLimit"));
    assert_eq!(
        chat.read_transcript_payload(request(&key, 1, 0))
            .await
            .unwrap()
            .readable_length,
        0
    );
    // Validate protocol metadata through the same list path used by consumers.
    let page = chat
        .list_transcript(ConversationListTranscriptParams {
            workspace_id: chat.workspace_id.clone(),
            conversation_id: conversation.clone(),
            limit: None,
            cursor: None,
            subagent_thread_id: None,
        })
        .await
        .unwrap();
    let descriptor = &page
        .items
        .iter()
        .find_map(|i| i.tool.as_ref())
        .unwrap()
        .payloads[0];
    assert!(descriptor.locally_truncated);
    assert_eq!(descriptor.parts[0].path.as_deref(), Some("source.log"));
    assert_eq!(descriptor.parts[0].digest.as_ref().unwrap().len(), 64);
    assert!(descriptor.parts.iter().all(|p| p.chunk_digests.is_empty()));
    let mut connection =
        SqliteConnection::establish(home.path().join("payloads.sqlite").to_str().unwrap()).unwrap();
    connection
        .batch_execute("UPDATE payload_chunks SET raw_sha256 = zeroblob(32)")
        .unwrap();
    let corrupt = chat
        .read_transcript_payload(request(&key, 0, 0))
        .await
        .unwrap();
    assert_eq!(corrupt.availability, "failed");
    assert!(corrupt.data.is_empty());
    assert_eq!(corrupt.next_offset, None);
    connection
        .batch_execute("PRAGMA foreign_keys=ON; DELETE FROM payload_revisions")
        .unwrap();
    assert_eq!(
        chat.read_transcript_payload(request(&key, 0, 0))
            .await
            .unwrap()
            .availability,
        "missing"
    );
    drop(connection);
    std::fs::remove_file(home.path().join("payloads.sqlite")).unwrap();
    assert_eq!(
        chat.read_transcript_payload(request(&key, 0, 0))
            .await
            .unwrap()
            .availability,
        "missing"
    );
}

#[tokio::test]
async fn sqlite_payload_checkpoint_reopen_paging_and_replacements() {
    let (home, chat, conversation, run) = tests::fixture().await;
    let mut key = seed(&chat, &conversation, &run).await;
    let worker = chat.database.payload_worker();
    let bytes = "日本語✓".repeat(20000).into_bytes();
    seal(worker, &key, &run, &[&bytes, b"second"]).await;
    // Sealing alone publishes nothing.
    assert!(chat
        .read_transcript_payload(request(&key, 0, 0))
        .await
        .is_err());
    chat.stage_sealed_payload(key.clone()).await.unwrap();
    chat.transcript_coordinator.checkpoint().await;
    let restored = ChatService::new(chat.database.clone(), chat.workspace_id.clone());
    for offset in [0, 65535, 131072, bytes.len()] {
        let response = restored
            .read_transcript_payload(request(&key, 0, offset as u64))
            .await
            .unwrap();
        assert_eq!(response.availability, "available");
        assert!(!response.unsaved);
        assert_eq!(response.readable_length, bytes.len() as u64);
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(response.data)
                .unwrap(),
            bytes[offset..(offset + 65536).min(bytes.len())]
        );
        assert_eq!(
            response.next_offset,
            ((offset + 65536) < bytes.len()).then_some((offset + 65536) as u64)
        );
    }
    let manifest = Reader::open(home.path()).unwrap().manifest(&key).unwrap();
    assert_eq!(manifest.upstream_truncated, Some(true));
    assert_eq!(
        restored
            .read_transcript_payload(request(&key, 1, 0))
            .await
            .unwrap()
            .readable_length,
        6
    );
    assert!(restored
        .read_transcript_payload(request(&key, 2, 0))
        .await
        .is_err());
    assert_eq!(
        restored
            .read_transcript_payload(request(&key, 0, u64::MAX))
            .await
            .unwrap()
            .availability,
        "failed"
    );
    let mut wrong = key.clone();
    wrong.conversation_id = Uuid::new_v4().to_string();
    assert!(restored
        .read_transcript_payload(request(&wrong, 0, 0))
        .await
        .is_err());
    let old = key.clone();
    for replacement in [b"short".as_slice(), b""] {
        key.revision += 1;
        chat.database
            .payload_leases()
            .lock()
            .unwrap()
            .insert(key.clone());
        seal(worker, &key, &run, &[replacement]).await;
        chat.stage_sealed_payload(key.clone()).await.unwrap();
        // A late predecessor must not replace the newer descriptor.
        chat.stage_sealed_payload(old.clone()).await.unwrap();
        chat.transcript_coordinator.checkpoint().await;
        let page = restored
            .read_transcript_payload(request(&key, 0, 0))
            .await
            .unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(page.data)
                .unwrap(),
            replacement
        );
        assert!(restored
            .read_transcript_payload(request(&old, 0, 0))
            .await
            .is_err());
        assert!(Reader::open(home.path()).unwrap().manifest(&old).is_ok());
    }
}

#[tokio::test]
async fn sqlite_payload_publication_rollback_and_deleted_conversation() {
    let (home, chat, conversation, run) = tests::fixture().await;
    let mut key = seed(&chat, &conversation, &run).await;
    let worker = chat.database.payload_worker();
    seal(worker, &key, &run, &[b"predecessor"]).await;
    chat.stage_sealed_payload(key.clone()).await.unwrap();
    chat.transcript_coordinator.checkpoint().await;
    let old = key.clone();
    key.revision += 1;
    chat.database
        .payload_leases()
        .lock()
        .unwrap()
        .insert(key.clone());
    seal(worker, &key, &run, &[b"replacement"]).await;
    chat.database.run("test.rejectPayloadPublication", |c| {
        c.batch_execute("CREATE TRIGGER reject_payload BEFORE INSERT ON transcript_payloads BEGIN SELECT RAISE(ABORT, 'injected publication failure'); END;")?;
        Ok(())
    }).await.unwrap();
    chat.stage_sealed_payload(key.clone()).await.unwrap();
    chat.transcript_coordinator.checkpoint().await;
    let restored = ChatService::new(chat.database.clone(), chat.workspace_id.clone());
    assert_eq!(
        restored
            .read_transcript_payload(request(&old, 0, 0))
            .await
            .unwrap()
            .availability,
        "available"
    );
    assert!(restored
        .read_transcript_payload(request(&key, 0, 0))
        .await
        .is_err());
    assert!(Reader::open(home.path()).unwrap().manifest(&key).is_ok());
    chat.database
        .run("test.allowPayloadPublication", |c| {
            c.batch_execute("DROP TRIGGER reject_payload")?;
            Ok(())
        })
        .await
        .unwrap();
    chat.delete_conversation(&conversation).await.unwrap();
    assert!(chat.stage_sealed_payload(key.clone()).await.is_err());
    chat.transcript_coordinator.checkpoint().await;
    assert!(restored
        .read_transcript_payload(request(&key, 0, 0))
        .await
        .is_err());
    assert!(Reader::open(home.path()).unwrap().manifest(&old).is_err());
}
