use super::tests::fixture;
use super::*;
use base64::Engine;
use diesel::{connection::SimpleConnection, Connection};
use serde_json::json;

async fn idle(chat: &ChatService, run: &ConversationRunID) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while chat.transcript_coordinator.payloads.pending(&run.0) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("payload worker finishes");
}
async fn items(
    chat: &ChatService,
    conversation: &ConversationID,
) -> Vec<ConversationTranscriptItem> {
    chat.list_transcript(ConversationListTranscriptParams {
        workspace_id: chat.workspace_id.clone(),
        conversation_id: conversation.clone(),
        limit: None,
        cursor: None,
        subagent_thread_id: None,
    })
    .await
    .unwrap()
    .items
}
async fn read(
    chat: &ChatService,
    conversation: &ConversationID,
    payload: &TranscriptPayload,
    part: u32,
) -> ConversationReadTranscriptPayloadResult {
    chat.read_transcript_payload(ConversationReadTranscriptPayloadParams {
        workspace_id: chat.workspace_id.clone(),
        conversation_id: conversation.clone(),
        payload_id: payload.payload_id.clone().unwrap(),
        revision: payload.revision,
        part,
        offset: 0,
        length: None,
    })
    .await
    .unwrap()
}
async fn update(
    chat: &ChatService,
    conversation: &ConversationID,
    run: &ConversationRunID,
    payload: Value,
) {
    chat.record_transcript_update(conversation, run, AgentUpdate::ToolCallUpdate { payload })
        .await
        .unwrap();
    idle(chat, run).await;
}

#[tokio::test]
async fn payload_capture_warning_keeps_unsupported_details_distinct_from_text_after_reload() {
    let (_home, chat, conversation, run) = fixture().await;
    chat.record_transcript_update(
        &conversation,
        &run,
        super::tests::text("Retained response", None),
    )
    .await
    .unwrap();
    update(&chat, &conversation, &run, json!({"toolCallId":"image-reference","kind":"read","status":"completed","content":[{"type":"content","content":{"type":"resource_link","name":"Image reference","uri":"https://example.invalid/image.png","mimeType":"image/png"}}]})).await;
    chat.finalize_run(
        &conversation,
        &run,
        ConversationRunStatus::Completed,
        None,
        json!({}),
    )
    .await
    .unwrap();
    chat.transcript_coordinator.drain().await;
    let restored = ChatService::new(chat.database.clone(), chat.workspace_id.clone());
    for service in [&chat, &restored] {
        let result = service
            .list_transcript(ConversationListTranscriptParams {
                workspace_id: service.workspace_id.clone(),
                conversation_id: conversation.clone(),
                limit: None,
                cursor: None,
                subagent_thread_id: None,
            })
            .await
            .unwrap();
        let capture = result
            .capture
            .iter()
            .find(|capture| capture.run_id == run)
            .unwrap();
        assert!(capture.gaps.is_empty());
        assert_eq!(
            capture.problem.as_deref(),
            Some("Some tool details could not be saved completely")
        );
        assert!(result
            .items
            .iter()
            .any(|item| item.body_preview == "Retained response"));
        assert!(result
            .items
            .iter()
            .filter_map(|item| item.tool.as_ref())
            .flat_map(|tool| &tool.payloads)
            .any(|payload| payload.locally_truncated));
    }
}

#[tokio::test]
async fn payload_terminal_chunks_append_and_survive_local_reload() {
    let (_home, chat, conversation, run) = fixture().await;
    update(&chat, &conversation, &run, json!({"toolCallId":"shell","kind":"execute","status":"in_progress","_meta":{"terminal_output":{"terminal_id":"shell","data":"first "}}})).await;
    let first = items(&chat, &conversation).await;
    let payload = &first.iter().find_map(|i| i.tool.as_ref()).unwrap().payloads[0];
    assert!(read(&chat, &conversation, payload, 0).await.unsaved);
    // The AIR contract: the completion carries only what changed, so the
    // last chunk rides with the exit and no output snapshot follows.
    update(&chat, &conversation, &run, json!({"toolCallId":"shell","status":"completed","_meta":{"terminal_output":{"terminal_id":"shell","data":"final"},"terminal_exit":{"terminal_id":"shell","exit_code":7,"signal":null}}})).await;
    chat.transcript_coordinator.drain().await;
    let saved = items(&chat, &conversation).await;
    let tool = saved.iter().find_map(|i| i.tool.as_ref()).unwrap();
    assert_eq!(tool.exit_code, Some(7));
    let response = read(&chat, &conversation, &tool.payloads[0], 0).await;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(response.data)
            .unwrap(),
        b"first final"
    );
    assert!(!response.unsaved);
    let restored = ChatService::new(chat.database.clone(), chat.workspace_id.clone());
    assert_eq!(
        read(&restored, &conversation, &tool.payloads[0], 0)
            .await
            .availability,
        "available"
    );
}

/// Both pinned adapters' recorded tool calls, decoded by the SDK as core
/// receives them. AIR sends each fact once: the command's bytes only as a
/// terminal chunk, and Claude's edit only as a diff block.
#[tokio::test]
async fn payload_recorded_air_tool_calls_keep_terminal_bytes_and_diffs() {
    use agent_client_protocol::schema::v1::{SessionNotification, SessionUpdate};
    // Claude 0.94 and later drop the command's trailing newline.
    for (recording, listed) in [
        (
            include_str!("../../../agent/fixtures/codex-acp-2.5.0.json"),
            &b"README.md\nnotes.txt\nsrc\nhello-from-shell\n"[..],
        ),
        (
            include_str!("../../../agent/fixtures/claude-agent-acp-0.100.0.json"),
            &b"README.md\nnotes.txt\nsrc\nhello-from-shell"[..],
        ),
    ] {
        let (_home, chat, conversation, run) = fixture().await;
        for message in serde_json::from_str::<Vec<Value>>(recording).unwrap() {
            if message["method"] != "session/update" {
                continue;
            }
            let notification: SessionNotification =
                serde_json::from_value(message["params"].clone()).unwrap();
            let payload = match notification.update {
                SessionUpdate::ToolCall(call) => serde_json::to_value(call).unwrap(),
                SessionUpdate::ToolCallUpdate(update) => serde_json::to_value(update).unwrap(),
                _ => continue,
            };
            update(&chat, &conversation, &run, payload).await;
        }
        chat.transcript_coordinator.drain().await;
        let saved = items(&chat, &conversation).await;
        let tools: Vec<_> = saved.iter().filter_map(|i| i.tool.as_ref()).collect();
        // Adapters may run more commands than asked (Codex reads the file
        // before patching it), so find the listing among the clean exits.
        let mut outputs = Vec::new();
        for tool in tools.iter().filter(|tool| tool.exit_code == Some(0)) {
            let output = tool.payloads.iter().find(|p| p.role == "output").unwrap();
            let response = read(&chat, &conversation, output, 0).await;
            outputs.push(
                base64::engine::general_purpose::STANDARD
                    .decode(response.data)
                    .unwrap(),
            );
        }
        assert!(
            outputs.iter().any(|output| output == listed),
            "the listing command keeps its exit code and bytes: {outputs:?}"
        );
        let edit = tools
            .iter()
            .flat_map(|tool| &tool.payloads)
            .find(|p| p.role == "fileChangeInput" && !p.parts.is_empty())
            .expect("the edit keeps its diff");
        assert_eq!(
            edit.parts
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["Before", "After"]
        );
        assert_eq!(edit.parts[0].path.as_deref(), Some("/work/repo/notes.txt"));
    }
}

#[tokio::test]
async fn payload_file_inputs_are_separate_and_body_reads_are_bounded() {
    let (_home, chat, conversation, run) = fixture().await;
    update(&chat, &conversation, &run, json!({"toolCallId":"edit","kind":"edit","name":"Edit","rawInput":{"file_path":"a.swift","old_string":"before","new_string":"x".repeat(100_000),"replace_all":true}})).await;
    let saved = items(&chat, &conversation).await;
    let tool = saved.iter().find_map(|i| i.tool.as_ref()).unwrap();
    let input = tool
        .payloads
        .iter()
        .find(|p| p.role == "fileChangeInput")
        .unwrap();
    assert_eq!(input.parts.len(), 2);
    assert_eq!(input.parts[0].name, "Before");
    let response = read(&chat, &conversation, input, 1).await;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(response.data)
            .unwrap()
            .len(),
        65536
    );
    assert_eq!(response.next_offset, Some(65536));
    assert!(!saved
        .iter()
        .find(|i| i.tool.is_some())
        .unwrap()
        .body_preview
        .contains("before"));
}

#[tokio::test]
async fn payload_unfinished_output_is_never_saved_as_complete() {
    let (_home, chat, conversation, run) = fixture().await;
    update(
        &chat,
        &conversation,
        &run,
        json!({"toolCallId":"shell","status":"in_progress","rawOutput":"unfinished"}),
    )
    .await;
    chat.finalize_run(
        &conversation,
        &run,
        ConversationRunStatus::Canceled,
        None,
        json!({}),
    )
    .await
    .unwrap();
    let saved = items(&chat, &conversation).await;
    let tool = saved.iter().find_map(|i| i.tool.as_ref()).unwrap();
    assert_eq!(tool.payloads[0].availability, "unavailable");
    assert_eq!(tool.payloads[0].reason.as_deref(), Some("unfinishedOutput"));
}

#[tokio::test]
async fn payload_cancel_with_queued_bytes_cannot_replace_unfinished_status_with_staging() {
    let (_home, chat, conversation, run) = fixture().await;
    let (entered, release) = chat.transcript_coordinator.payloads.block_next_write();
    chat.record_transcript_update(
        &conversation,
        &run,
        AgentUpdate::ToolCallUpdate {
            payload: json!({"toolCallId":"shell","status":"in_progress","rawOutput":"unfinished"}),
        },
    )
    .await
    .unwrap();
    entered.await.unwrap();
    chat.finalize_run(
        &conversation,
        &run,
        ConversationRunStatus::Canceled,
        None,
        json!({}),
    )
    .await
    .unwrap();
    assert!(!chat.transcript_coordinator.payloads.pending(&run.0));
    release.send(()).unwrap();
    chat.transcript_coordinator.drain().await;
    let saved = items(&chat, &conversation).await;
    let tool = saved.iter().find_map(|i| i.tool.as_ref()).unwrap();
    assert_eq!(tool.payloads[0].availability, "unavailable");
    assert_eq!(tool.payloads[0].reason.as_deref(), Some("unfinishedOutput"));
}

#[tokio::test]
async fn payload_assistant_image_preserves_text_order_and_unavailable_bytes() {
    let (_home, chat, conversation, run) = fixture().await;
    chat.record_transcript_update(
        &conversation,
        &run,
        AgentUpdate::AgentMessageChunk {
            text: "before".into(),
            message_id: None,
        },
    )
    .await
    .unwrap();
    chat.record_transcript_update(&conversation, &run, AgentUpdate::AgentImageChunk { content: json!({"type":"image","mimeType":"image/png","data":"","uri":"https://example.invalid/image.png"}), message_id: None }).await.unwrap();
    idle(&chat, &run).await;
    chat.record_transcript_update(
        &conversation,
        &run,
        AgentUpdate::AgentMessageChunk {
            text: "after".into(),
            message_id: None,
        },
    )
    .await
    .unwrap();
    let items = items(&chat, &conversation).await;
    let kinds: Vec<_> = items
        .iter()
        .filter(|i| i.kind != TranscriptItemKind::Message)
        .map(|i| i.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            TranscriptItemKind::AssistantText,
            TranscriptItemKind::Image,
            TranscriptItemKind::AssistantText
        ]
    );
    let image = items
        .iter()
        .find(|i| i.kind == TranscriptItemKind::Image)
        .unwrap();
    assert!(image.payloads[0].locally_truncated);
    assert_eq!(
        image.payloads[0].reason.as_deref(),
        Some("bytesUnavailable")
    );
}

#[tokio::test]
async fn payload_corruption_and_missing_chunks_are_local() {
    let (_home, chat, conversation, run) = fixture().await;
    update(
        &chat,
        &conversation,
        &run,
        json!({"toolCallId":"tool","status":"completed","rawOutput":"saved"}),
    )
    .await;
    chat.transcript_coordinator.drain().await;
    let saved = items(&chat, &conversation).await;
    let descriptor = saved.iter().find_map(|i| i.tool.as_ref()).unwrap().payloads[0].clone();
    let mut connection =
        diesel::SqliteConnection::establish(chat.home.join("payloads.sqlite").to_str().unwrap())
            .unwrap();
    connection
        .batch_execute(
            "PRAGMA busy_timeout=5000; UPDATE payload_chunks SET raw_sha256 = zeroblob(32);",
        )
        .unwrap();
    let corrupt = read(&chat, &conversation, &descriptor, 0).await;
    assert_eq!(corrupt.availability, "failed");
    assert!(corrupt.data.is_empty());
    connection
        .batch_execute("PRAGMA foreign_keys=ON; DELETE FROM payload_revisions;")
        .unwrap();
    let missing = read(&chat, &conversation, &descriptor, 0).await;
    assert_eq!(missing.availability, "missing");
    assert!(missing.data.is_empty());
    assert!(items(&chat, &conversation)
        .await
        .iter()
        .any(|i| i.tool.is_some()));
}

#[tokio::test]
async fn payload_failed_replacement_preserves_committed_version() {
    let (_home, chat, conversation, run) = fixture().await;
    update(
        &chat,
        &conversation,
        &run,
        json!({"toolCallId":"tool","status":"completed","rawOutput":"saved"}),
    )
    .await;
    chat.transcript_coordinator.drain().await;
    let saved = items(&chat, &conversation).await;
    let descriptor = saved.iter().find_map(|i| i.tool.as_ref()).unwrap().payloads[0].clone();
    let mut connection =
        diesel::SqliteConnection::establish(chat.home.join("payloads.sqlite").to_str().unwrap())
            .unwrap();
    connection.batch_execute("PRAGMA busy_timeout=5000; CREATE TRIGGER fail_replacement BEFORE INSERT ON payload_revisions BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;").unwrap();
    update(
        &chat,
        &conversation,
        &run,
        json!({"toolCallId":"tool","status":"completed","rawOutput":"replacement"}),
    )
    .await;
    let current = items(&chat, &conversation).await;
    let payload = &current
        .iter()
        .find_map(|i| i.tool.as_ref())
        .unwrap()
        .payloads[0];
    assert_eq!(payload.revision, descriptor.revision);
    assert_eq!(payload.reason.as_deref(), Some("writeFailed"));
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(read(&chat, &conversation, payload, 0).await.data)
            .unwrap(),
        b"saved"
    );
    connection
        .batch_execute("DROP TRIGGER fail_replacement;")
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            update(&chat, &conversation, &run, json!({"toolCallId":"tool","status":"completed","rawOutput":"recovered replacement"})).await;
            let current = items(&chat, &conversation).await;
            let payload = &current.iter().find_map(|i| i.tool.as_ref()).unwrap().payloads[0];
            if payload.availability == "available" {
                chat.transcript_coordinator.drain().await;
                let response = read(&chat, &conversation, payload, 0).await;
                assert!(!response.unsaved);
                assert_eq!(base64::engine::general_purpose::STANDARD.decode(response.data).unwrap(), b"recovered replacement");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("same Core resumes capture after storage recovery");
}

#[tokio::test]
async fn payload_failed_streaming_capture_does_not_stay_pending_after_worker_recovery() {
    let (_home, chat, conversation, run) = fixture().await;
    let mut connection =
        diesel::SqliteConnection::establish(chat.home.join("payloads.sqlite").to_str().unwrap())
            .unwrap();
    connection.batch_execute("PRAGMA busy_timeout=5000; CREATE TRIGGER fail_stream BEFORE INSERT ON payload_chunks BEGIN SELECT RAISE(ABORT, 'injected streaming failure'); END;").unwrap();
    update(
        &chat,
        &conversation,
        &run,
        json!({"toolCallId":"tool","status":"in_progress","rawOutput":"x".repeat(64 * 1024)}),
    )
    .await;
    let current = items(&chat, &conversation).await;
    let payload = &current
        .iter()
        .find_map(|i| i.tool.as_ref())
        .unwrap()
        .payloads[0];
    assert_eq!(payload.availability, "failed");
    assert_eq!(payload.reason.as_deref(), Some("writeFailed"));
    assert!(!chat.transcript_coordinator.payloads.pending(&run.0));
    connection
        .batch_execute("DROP TRIGGER fail_stream;")
        .unwrap();
}

#[tokio::test]
async fn payload_blocked_io_leaves_database_available_and_deletion_drains_writers() {
    let (_home, chat, conversation, run) = fixture().await;
    let (entered, release) = chat.transcript_coordinator.payloads.block_next_write();
    chat.record_transcript_update(
        &conversation,
        &run,
        AgentUpdate::ToolCall {
            payload: json!({"toolCallId":"tool","status":"completed","rawOutput":"bytes"}),
        },
    )
    .await
    .unwrap();
    entered.await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        chat.database.run("payload.testDatabaseFree", |_| Ok(())),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(chat.transcript_coordinator.payloads.pending(&run.0));
    let deletion = chat.delete_conversation(&conversation);
    tokio::pin!(deletion);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut deletion)
            .await
            .is_err()
    );
    release.send(()).unwrap();
    deletion.await.unwrap();
    let mut maintenance =
        crate::storage::payload::maintenance::Maintenance::open(&chat.home).unwrap();
    assert!(maintenance.candidates(0).unwrap().is_empty());
}

#[tokio::test]
async fn payload_writer_notifies_the_changed_descriptor_without_body_bytes() {
    let (_home, chat, conversation, run) = fixture().await;
    let (outbound, mut notifications) = crate::server::outbound::test_sink();
    let chat = chat.with_transcript_delivery(Some(outbound), "payload-core".into());
    update(
        &chat,
        &conversation,
        &run,
        json!({"toolCallId":"shell","status":"in_progress","rawOutput":"body not in descriptor"}),
    )
    .await;
    let mut found = false;
    while let Ok(value) = notifications.try_recv() {
        let text = serde_json::to_string(&value.frame).unwrap();
        assert!(!text.contains("body not in descriptor"));
        if text.contains("text/plain") && text.contains("byteCount") {
            found = true;
        }
    }
    assert!(
        found,
        "the payload writer must publish the populated descriptor"
    );
}

#[tokio::test]
async fn payload_replacement_leases_release_only_after_successful_checkpoint() {
    let (_home, chat, conversation, run) = fixture().await;
    chat.transcript_coordinator.manual_checkpoints();
    for revision in 1..=5 {
        update(
            &chat,
            &conversation,
            &run,
            json!({"toolCallId":"tool","status":"completed","rawOutput":format!("snapshot {revision}")}),
        )
        .await;
        let rows = items(&chat, &conversation).await;
        let tool_item = rows.iter().find(|i| i.tool.is_some()).unwrap();
        let payload = &tool_item.tool.as_ref().unwrap().payloads[0];
        assert_eq!(payload.revision, revision);
        let leases = || chat.database.payload_leases().lock().unwrap().clone();
        assert_eq!(leases().len(), if revision == 1 { 1 } else { 2 });
        if revision == 2 {
            chat.database.run("payload.failLeaseCheckpoint", |connection| {
                connection.batch_execute("CREATE TRIGGER fail_lease_checkpoint BEFORE INSERT ON transcript_payloads BEGIN SELECT RAISE(FAIL, 'injected'); END")?;
                Ok(())
            }).await.unwrap();
            chat.transcript_coordinator.checkpoint().await;
            assert_eq!(
                leases().len(),
                2,
                "rollback must retain the saved predecessor"
            );
            chat.database
                .run("payload.restoreLeaseCheckpoint", |connection| {
                    connection.batch_execute("DROP TRIGGER fail_lease_checkpoint")?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        if revision > 1 {
            // An older immutable checkpoint finishing while this newer capture
            // is live cannot release the newer publication's lease.
            let mut older = payload.clone();
            older.revision -= 1;
            chat.transcript_coordinator.payloads.checkpointed(
                &conversation.0,
                &tool_item.item_id,
                &older,
            );
            assert_eq!(leases().len(), 2);
        }
        chat.transcript_coordinator.checkpoint().await;
        assert_eq!(leases().len(), 1, "an open Run must not pin every snapshot");
        assert_eq!(leases().iter().next().unwrap().revision, revision);
        assert!(!read(&chat, &conversation, payload, 0).await.unsaved);
    }
}

#[tokio::test]
async fn payload_sql_failure_after_sealing_stays_unsaved_until_checkpoint_recovers() {
    use diesel::RunQueryDsl;
    let (_home, chat, conversation, run) = fixture().await;
    chat.database.run("payload.injectCheckpointFailure", |connection| {
        diesel::sql_query("CREATE TRIGGER fail_payload BEFORE INSERT ON transcript_payloads BEGIN SELECT RAISE(FAIL, 'injected'); END").execute(connection)?;
        Ok(())
    }).await.unwrap();
    update(
        &chat,
        &conversation,
        &run,
        json!({"toolCallId":"tool","status":"completed","rawOutput":"saved file"}),
    )
    .await;
    let rows = items(&chat, &conversation).await;
    let payload = &rows.iter().find_map(|i| i.tool.as_ref()).unwrap().payloads[0];
    let response = read(&chat, &conversation, payload, 0).await;
    assert!(response.unsaved);
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(response.data)
            .unwrap(),
        b"saved file"
    );
    chat.database
        .run("payload.clearCheckpointFailure", |connection| {
            diesel::sql_query("DROP TRIGGER fail_payload").execute(connection)?;
            Ok(())
        })
        .await
        .unwrap();
    chat.transcript_coordinator.drain().await;
    assert!(!read(&chat, &conversation, payload, 0).await.unsaved);
}

#[tokio::test]
async fn payload_pending_queue_is_bounded_and_execution_is_not_blocked() {
    let (_home, chat, conversation, run) = fixture().await;
    let (entered, release) = chat.transcript_coordinator.payloads.block_next_write();
    for id in ["first", "second", "rejected"] {
        let bytes = if id == "rejected" {
            "last".into()
        } else {
            "x".repeat(16 * 1024 * 1024)
        };
        chat.record_transcript_update(
            &conversation,
            &run,
            AgentUpdate::ToolCall {
                payload: json!({"toolCallId":id,"status":"completed","rawOutput":bytes}),
            },
        )
        .await
        .unwrap();
    }
    entered.await.unwrap();
    let rows = items(&chat, &conversation).await;
    assert!(rows
        .iter()
        .filter_map(|i| i.tool.as_ref())
        .flat_map(|t| &t.payloads)
        .any(|p| p.reason.as_deref() == Some("queueFull")));
    release.send(()).unwrap();
    idle(&chat, &run).await;
}
