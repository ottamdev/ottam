use super::tests::fixture;
use super::*;
use serde_json::json;

const RECORDING: &str = include_str!("../../../agent/fixtures/claude-subagents-0.85.1.json");
const OUTER: &str = "toolu_01B7bGspZr9HB4ctXRrxemcW";
const FAILED: &str = "toolu_018oDuABh8ft9pADCzi9jv5H";
const NESTED: &str = "toolu_01SvuHrKZ1tbgaM9bUsFEoMT";
const SYNC: &str = "toolu_01BGNedJuiMVrGUsmRegzkKY";

/// Feeds a recorded turn as the runner does: the SDK's typed decode, then
/// session-stamped chunks.
async fn replay(
    chat: &ChatService,
    conversation: &ConversationID,
    run: &ConversationRunID,
    turn: &str,
) {
    use agent_client_protocol::schema::v1::{ContentBlock, SessionNotification, SessionUpdate};
    let recording: Value = serde_json::from_str(RECORDING).unwrap();
    for message in recording[turn].as_array().unwrap() {
        if message["method"] != "session/update" {
            continue;
        }
        let notification: SessionNotification =
            serde_json::from_value(message["params"].clone()).unwrap();
        let update = match notification.update {
            SessionUpdate::ToolCall(call) => AgentUpdate::ToolCall {
                payload: serde_json::to_value(call).unwrap(),
            },
            SessionUpdate::ToolCallUpdate(update) => AgentUpdate::ToolCallUpdate {
                payload: serde_json::to_value(update).unwrap(),
            },
            SessionUpdate::AgentMessageChunk(chunk) => {
                let ContentBlock::Text(text) = chunk.content else {
                    continue;
                };
                AgentUpdate::RichMessageChunk {
                    text: text.text,
                    message_id: chunk.message_id.map(|id| id.0.to_string()),
                    reasoning: false,
                    metadata: TranscriptAdapterMetadata {
                        session_id: Some("claude:session".into()),
                        ..Default::default()
                    },
                }
            }
            _ => continue,
        };
        chat.record_transcript_update(conversation, run, update)
            .await
            .unwrap();
    }
}

async fn page(
    chat: &ChatService,
    conversation: &ConversationID,
) -> ConversationListTranscriptResult {
    chat.list_transcript(ConversationListTranscriptParams {
        workspace_id: chat.workspace_id.clone(),
        conversation_id: conversation.clone(),
        limit: Some(200),
        cursor: None,
        subagent_thread_id: None,
    })
    .await
    .unwrap()
}

async fn finish(
    chat: &ChatService,
    conversation: &ConversationID,
    run: &ConversationRunID,
    status: ConversationRunStatus,
) {
    let reason = match status {
        ConversationRunStatus::Canceled => AgentStopReason::Cancelled,
        _ => AgentStopReason::EndTurn,
    };
    chat.finalize_run(conversation, run, status, Some(reason), Value::Null)
        .await
        .unwrap();
}

fn tool<'a>(items: &'a [ConversationTranscriptItem], call: &str) -> &'a ConversationTranscriptItem {
    items
        .iter()
        .find(|item| {
            item.tool
                .as_ref()
                .and_then(|tool| tool.provider_tool_call_id.as_deref())
                == Some(call)
        })
        .unwrap_or_else(|| panic!("no tool {call}"))
}

fn thread(item: &ConversationTranscriptItem) -> &TranscriptSubagentThread {
    item.tool
        .as_ref()
        .and_then(|tool| tool.subagent_thread.as_ref())
        .expect("a control call owns a thread")
}

fn live(page: &ConversationListTranscriptResult) -> Vec<u32> {
    page.subagent_threads
        .iter()
        .map(|count| count.live)
        .collect()
}

/// The recorded 0.85.1 turn: an asynchronous subagent that launches a nested
/// one, beside a launch that fails. Claude streams the parent's next message
/// while the child works, and stamps each child call only on its first report.
#[tokio::test]
async fn subagent_thread_recorded_async_turn_groups_children_and_keeps_parent_text_whole() {
    let (_home, chat, conversation, run) = fixture().await;
    replay(&chat, &conversation, &run, "claudeAsyncParallel").await;

    let running = page(&chat, &conversation).await;
    let outer = thread(tool(&running.items, OUTER));
    assert_eq!(outer.id, format!("subagent:{OUTER}"));
    assert_eq!(outer.name.as_deref(), Some("general-purpose"));
    assert_eq!(
        outer.task.as_deref(),
        Some("Find transcript_metadata definition")
    );
    assert!(outer.asynchronous);
    // The launch completed the call, not the thread.
    assert_eq!(
        tool(&running.items, OUTER)
            .tool
            .as_ref()
            .unwrap()
            .status
            .as_deref(),
        Some("completed")
    );
    assert_eq!(outer.state, SubagentThreadState::Running);
    assert_eq!(
        thread(tool(&running.items, FAILED)).state,
        SubagentThreadState::Failed
    );
    assert_eq!(live(&running), [1]);

    finish(&chat, &conversation, &run, ConversationRunStatus::Completed).await;
    chat.transcript_coordinator.drain().await;
    for items in [
        page(&chat, &conversation).await.items,
        page(
            &ChatService::new(chat.database.clone(), chat.workspace_id.clone()),
            &conversation,
        )
        .await
        .items,
    ] {
        let outer_id = format!("subagent:{OUTER}");
        assert_eq!(
            thread(tool(&items, OUTER)).state,
            SubagentThreadState::Completed
        );
        assert_eq!(tool(&items, OUTER).subagent_thread_id, None);
        // The nested launch is a member of its parent's thread and owns its own.
        let nested = tool(&items, NESTED);
        assert_eq!(
            nested.subagent_thread_id.as_deref(),
            Some(outer_id.as_str())
        );
        assert_eq!(
            thread(nested).parent_thread_id.as_deref(),
            Some(outer_id.as_str())
        );
        assert_eq!(thread(nested).state, SubagentThreadState::Completed);
        assert!(!thread(nested).asynchronous);
        // Both child commands joined, though their later reports carried no parent.
        let children: Vec<_> = items
            .iter()
            .filter(|item| item.subagent_thread_id.as_deref() == Some(outer_id.as_str()))
            .filter_map(|item| item.tool.as_ref()?.title.clone())
            .collect();
        assert_eq!(children.len(), 3, "{children:?}");
        // One parent text row per message, each whole.
        let text: Vec<_> = items
            .iter()
            .filter(|item| item.kind == TranscriptItemKind::AssistantText)
            .collect();
        assert_eq!(text.len(), 3);
        assert!(text.iter().all(|item| item.subagent_thread_id.is_none()));
        assert!(text[1]
            .body_preview
            .starts_with("The first subagent is still running"));
        assert!(text[1].body_preview.ends_with("reports back."));
    }
    assert_eq!(live(&page(&chat, &conversation).await), [0]);
}

/// A synchronous subagent completes with its call; a cancelled run then has
/// nothing left running.
#[tokio::test]
async fn subagent_thread_recorded_sync_turn_completes_with_its_call() {
    let (_home, chat, conversation, run) = fixture().await;
    replay(&chat, &conversation, &run, "claudeSyncCancelled").await;
    finish(&chat, &conversation, &run, ConversationRunStatus::Canceled).await;
    let items = page(&chat, &conversation).await.items;
    let control = thread(tool(&items, SYNC));
    assert_eq!(control.state, SubagentThreadState::Completed);
    assert!(!control.asynchronous);
    let members = items
        .iter()
        .filter(|item| item.subagent_thread_id.as_deref() == Some(control.id.as_str()))
        .count();
    assert_eq!(members, 2);
}

fn call(id: &str, meta: Value, status: &str) -> AgentUpdate {
    AgentUpdate::ToolCall {
        payload: json!({"toolCallId": id, "title": id, "kind": "execute", "status": status, "_meta": meta}),
    }
}

fn agent_call(id: &str) -> AgentUpdate {
    call(
        id,
        json!({"claudeCode": {"toolName": "Agent"}, "jetbrains": {"air": {"version": 1, "subagent": true}}}),
        "pending",
    )
}

fn chunk(text: &str) -> AgentUpdate {
    AgentUpdate::RichMessageChunk {
        text: text.into(),
        message_id: Some("parent".into()),
        reasoning: false,
        metadata: TranscriptAdapterMetadata {
            session_id: Some("claude:session".into()),
            ..Default::default()
        },
    }
}

/// Parent chunk, tagged child call, untagged child update, parent chunk: one
/// parent text row, and the child in its thread.
#[tokio::test]
async fn subagent_thread_child_calls_do_not_split_parent_text() {
    let (_home, chat, conversation, run) = fixture().await;
    for update in [
        agent_call("agent"),
        chunk("Working "),
        call(
            "child",
            json!({"claudeCode": {"toolName": "Bash", "parentToolUseId": "agent"}}),
            "pending",
        ),
        AgentUpdate::ToolCallUpdate {
            payload: json!({"toolCallId": "child", "status": "completed"}),
        },
        chunk("on it."),
    ] {
        chat.record_transcript_update(&conversation, &run, update)
            .await
            .unwrap();
    }
    let items = page(&chat, &conversation).await.items;
    let text: Vec<_> = items
        .iter()
        .filter(|item| item.kind == TranscriptItemKind::AssistantText)
        .collect();
    assert_eq!(text.len(), 1);
    assert_eq!(text[0].body_preview, "Working on it.");
    assert_eq!(
        tool(&items, "child").subagent_thread_id.as_deref(),
        Some("subagent:agent")
    );
}

/// A child can report before its control call; it joins the thread the call
/// opens later. A run cancelled under a running thread cancels it.
#[tokio::test]
async fn subagent_thread_attaches_children_seen_before_the_spawn() {
    let (_home, chat, conversation, run) = fixture().await;
    for update in [
        call(
            "early",
            json!({"claudeCode": {"toolName": "Read", "parentToolUseId": "late"}}),
            "pending",
        ),
        agent_call("late"),
    ] {
        chat.record_transcript_update(&conversation, &run, update)
            .await
            .unwrap();
    }
    let items = page(&chat, &conversation).await.items;
    let opened = thread(tool(&items, "late"));
    assert_eq!(
        tool(&items, "early").subagent_thread_id.as_deref(),
        Some(opened.id.as_str())
    );
    finish(&chat, &conversation, &run, ConversationRunStatus::Canceled).await;
    let items = page(&chat, &conversation).await.items;
    assert_eq!(
        thread(tool(&items, "late")).state,
        SubagentThreadState::Cancelled
    );
}

/// A stored running thread under an ended run was left by a core that
/// stopped first.
#[test]
fn subagent_thread_running_under_an_ended_run_reads_disconnected() {
    let mut item = conversation_transcript_subagent_control_item_fixture();
    subagents::read_disconnected(&mut item);
    assert_eq!(thread(&item).state, SubagentThreadState::Running);
    item.run_status = Some(ConversationRunStatus::Interrupted);
    subagents::read_disconnected(&mut item);
    assert_eq!(thread(&item).state, SubagentThreadState::Disconnected);
}

/// A child can report in a later run, as after a steer: it joins the thread
/// its earlier launch opened, and that thread keeps the state its own run
/// gave it.
#[tokio::test]
async fn subagent_thread_child_in_a_later_run_joins_its_thread() {
    let (_home, chat, conversation, first) = fixture().await;
    chat.record_transcript_update(&conversation, &first, agent_call("agent"))
        .await
        .unwrap();
    finish(
        &chat,
        &conversation,
        &first,
        ConversationRunStatus::Completed,
    )
    .await;
    let second = next_run(&chat, &conversation).await;
    chat.record_transcript_update(
        &conversation,
        &second,
        call(
            "late-child",
            json!({"claudeCode": {"toolName": "Read", "parentToolUseId": "agent"}}),
            "pending",
        ),
    )
    .await
    .unwrap();
    let items = page(&chat, &conversation).await.items;
    let child = tool(&items, "late-child");
    assert_eq!(child.run_id.as_ref(), Some(&second));
    assert_eq!(child.subagent_thread_id.as_deref(), Some("subagent:agent"));
    let control = tool(&items, "agent");
    assert_eq!(control.run_id.as_ref(), Some(&first));
    assert_eq!(thread(control).state, SubagentThreadState::Completed);
}

/// Claude backgrounds an agent unless the request says otherwise, so the
/// request alone marks it asynchronous: a completion arriving before the
/// launch receipt does not complete the thread.
#[tokio::test]
async fn subagent_thread_request_marks_it_asynchronous_before_the_receipt() {
    let (_home, chat, conversation, run) = fixture().await;
    for update in [
        agent_call("agent"),
        AgentUpdate::ToolCallUpdate {
            payload: json!({"toolCallId": "agent", "rawInput": {"description": "Look", "subagent_type": "Explore", "prompt": "p"}}),
        },
        AgentUpdate::ToolCallUpdate {
            payload: json!({"toolCallId": "agent", "status": "completed"}),
        },
    ] {
        chat.record_transcript_update(&conversation, &run, update)
            .await
            .unwrap();
    }
    let items = page(&chat, &conversation).await.items;
    let opened = thread(tool(&items, "agent"));
    assert!(opened.asynchronous);
    assert_eq!(opened.state, SubagentThreadState::Running);
}

fn native_anchor(id: &str) -> AgentUpdate {
    AgentUpdate::ToolCall {
        payload: json!({
            "toolCallId": id, "kind": "other", "title": "Reviewer", "status": "in_progress",
            NATIVE_ANCHOR_KEY: {"id": id, "name": "Reviewer"},
        }),
    }
}

fn native_end(id: &str) -> AgentUpdate {
    AgentUpdate::ToolCallUpdate {
        payload: json!({
            "toolCallId": id, "status": "completed",
            NATIVE_ANCHOR_KEY: {"state": "completed"},
        }),
    }
}

/// A native subagent outlives its run: the run's end leaves it running, its
/// own end revises its anchor in that closed run, in memory or in storage,
/// and an end sent to a later run makes no anchor there. Once core loses it,
/// or never saw it live, it reads as disconnected; Stop still cancels it.
#[tokio::test]
async fn native_subagent_outlives_its_run() {
    let (_home, chat, conversation, first) = fixture().await;
    chat.transcript_coordinator.manual_checkpoints();
    for id in ["subagent:a", "subagent:b", "subagent:c"] {
        chat.record_transcript_update(&conversation, &first, native_anchor(id))
            .await
            .unwrap();
    }
    finish(
        &chat,
        &conversation,
        &first,
        ConversationRunStatus::Completed,
    )
    .await;
    let items = page(&chat, &conversation).await.items;
    for id in ["subagent:a", "subagent:b", "subagent:c"] {
        assert_eq!(thread(tool(&items, id)).state, SubagentThreadState::Running);
    }

    // `a` ends while memory holds its run; `b` once only storage does.
    chat.record_task_lifecycle(&conversation, &first, native_end("subagent:a"))
        .await
        .unwrap();
    chat.transcript_coordinator.drain().await;
    let stored = tool(&items, "subagent:b").item_id.clone();
    chat.transcript_coordinator
        .conversations
        .lock()
        .unwrap()
        .get_mut(&conversation.0)
        .unwrap()
        .items
        .retain(|_, item| item.record.row.id != stored);
    chat.record_task_lifecycle(&conversation, &first, native_end("subagent:b"))
        .await
        .unwrap();
    let second = next_run(&chat, &conversation).await;
    chat.record_transcript_update(&conversation, &second, native_end("subagent:c"))
        .await
        .unwrap();
    let items = page(&chat, &conversation).await.items;
    for (id, state) in [
        ("subagent:a", SubagentThreadState::Completed),
        ("subagent:b", SubagentThreadState::Completed),
        ("subagent:c", SubagentThreadState::Running),
    ] {
        let anchor = tool(&items, id);
        assert_eq!(anchor.run_id.as_ref(), Some(&first));
        assert_eq!(thread(anchor).state, state, "{id}");
    }
    assert_eq!(
        items
            .iter()
            .filter(|item| item.run_id.as_ref() == Some(&second) && item.tool.is_some())
            .count(),
        0,
        "an end without its anchor makes none"
    );

    // A thread this core never saw live was left by an earlier one.
    let mut unseen = tool(&items, "subagent:c").clone();
    unseen.item_id = "unseen".into();
    let mut read = [tool(&items, "subagent:c").clone(), unseen];
    chat.transcript_coordinator.read_lost(&mut read);
    assert_eq!(thread(&read[0]).state, SubagentThreadState::Running);
    assert_eq!(thread(&read[1]).state, SubagentThreadState::Disconnected);

    chat.record_task_lost(&conversation, &first, Some("subagent:c".into()), None)
        .await
        .unwrap();
    let items = page(&chat, &conversation).await.items;
    assert_eq!(
        thread(tool(&items, "subagent:c")).state,
        SubagentThreadState::Disconnected
    );

    chat.record_transcript_update(&conversation, &second, native_anchor("subagent:d"))
        .await
        .unwrap();
    finish(
        &chat,
        &conversation,
        &second,
        ConversationRunStatus::Canceled,
    )
    .await;
    let items = page(&chat, &conversation).await.items;
    assert_eq!(
        thread(tool(&items, "subagent:d")).state,
        SubagentThreadState::Cancelled
    );
}

/// The user's next turn: a message and the run that delivers it.
async fn next_run(chat: &ChatService, conversation: &ConversationID) -> ConversationRunID {
    use crate::server::chat::ids::{parse_conversation_id, parse_workspace_id};
    use crate::storage::models::{RunSourceRole, WorkingRootKind};
    use crate::storage::repositories::{
        ConversationRepository, CreateRunParams, RunRepository, RunSourceSnapshot,
    };
    let workspace = parse_workspace_id(&chat.workspace_id).unwrap();
    let conversation_id = parse_conversation_id(conversation).unwrap();
    let run = chat
        .database
        .run("test.nextRun", move |connection| {
            let now = chrono::Utc::now();
            let posted = ConversationRepository::post_message(
                connection,
                &workspace,
                &conversation_id,
                "again",
                "again",
                now,
            )?;
            RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &workspace,
                    conversation_id: &conversation_id,
                    engine_id: "engine",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &posted.input.id,
                    sources: &[RunSourceSnapshot {
                        root_kind: WorkingRootKind::WorkspaceFiles,
                        project_id: None,
                        source_id: None,
                        canonical_path: "/tmp/transcript-test",
                        role: RunSourceRole::Primary,
                    }],
                },
                now,
            )
        })
        .await
        .unwrap();
    let run = ConversationRunID(run.id.to_string());
    chat.transcript_coordinator.accept_run(conversation, &run);
    run
}
