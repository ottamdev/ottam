use super::*;
use crate::server::outbound::{test_sink, OutboundFrame};
use tokio::sync::mpsc::UnboundedReceiver;

fn conversation(id: &str) -> ConversationID {
    ConversationID(id.to_owned())
}

fn workspace() -> WorkspaceID {
    WorkspaceID("workspace".to_owned())
}

fn ended(run: &str, reason: AttentionReason) -> Ended {
    Ended {
        workspace_id: workspace(),
        run_id: ConversationRunID(run.to_owned()),
        reason,
        title: "Task".to_owned(),
        detail: None,
    }
}

fn ask(conversation_id: &str, reason: AttentionReason) -> AskInfo {
    AskInfo {
        workspace_id: workspace(),
        conversation_id: conversation(conversation_id),
        run_id: ConversationRunID("run".to_owned()),
        reason,
        detail: Some("Run tests".to_owned()),
        title: Box::pin(async { "Task".to_owned() }),
    }
}

/// Every `attention.changed` sent so far, as (state, notify).
fn drain(rx: &mut UnboundedReceiver<OutboundFrame>) -> Vec<(String, bool, u64)> {
    let mut changes = Vec::new();
    while let Ok(frame) = rx.try_recv() {
        if frame.frame["method"] == METHOD_ATTENTION_CHANGED {
            let params = &frame.frame["params"];
            changes.push((
                params["entry"]["state"].as_str().unwrap().to_owned(),
                params["notify"].as_bool().unwrap(),
                params["revision"].as_u64().unwrap(),
            ));
        }
    }
    changes
}

fn states(changes: &[(String, bool, u64)]) -> Vec<(&str, bool)> {
    changes
        .iter()
        .map(|(state, notify, _)| (state.as_str(), *notify))
        .collect()
}

async fn settle_idle(attention: &Attention, id: &str) {
    attention.settle(&conversation(id), || Box::pin(async { Settle::Idle }));
    tokio::time::sleep(SETTLE_DELAY + Duration::from_millis(10)).await;
}

#[tokio::test(start_paused = true)]
async fn an_unanswered_ask_alerts_once_and_ends_with_none() {
    let (sink, mut rx) = test_sink();
    let attention = Attention::new(sink);
    let guard = attention.begin_ask(ask("a", AttentionReason::Permission));
    tokio::task::yield_now().await;
    assert_eq!(states(&drain(&mut rx)), [("needsInput", false)]);
    assert_eq!(attention.list().entries.len(), 1);

    tokio::time::sleep(ASK_NOTIFY_DELAY + Duration::from_millis(10)).await;
    assert_eq!(states(&drain(&mut rx)), [("needsInput", true)]);

    drop(guard);
    let changes = drain(&mut rx);
    assert_eq!(states(&changes), [("none", false)]);
    assert!(attention.list().entries.is_empty());
    assert_eq!(attention.list().revision, changes[0].2);
}

#[tokio::test(start_paused = true)]
async fn an_ask_answered_quickly_or_focused_never_alerts() {
    let (sink, mut rx) = test_sink();
    let attention = Attention::new(sink);
    let guard = attention.begin_ask(ask("a", AttentionReason::Question));
    tokio::task::yield_now().await;
    drop(guard);
    tokio::time::sleep(ASK_NOTIFY_DELAY * 2).await;
    assert_eq!(states(&drain(&mut rx)), [("needsInput", false), ("none", false)]);

    attention.set_focus(ConnectionId::Primary, Some(conversation("b")));
    let _guard = attention.begin_ask(ask("b", AttentionReason::Plan));
    tokio::time::sleep(ASK_NOTIFY_DELAY * 2).await;
    assert_eq!(states(&drain(&mut rx)), [("needsInput", false)]);
}

#[tokio::test(start_paused = true)]
async fn an_ask_dropped_before_its_title_is_read_sends_nothing() {
    let (sink, mut rx) = test_sink();
    let attention = Attention::new(sink);
    let guard = attention.begin_ask(ask("a", AttentionReason::Permission));
    drop(guard);
    tokio::time::sleep(ASK_NOTIFY_DELAY * 2).await;
    assert!(drain(&mut rx).is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_older_ask_alerts_when_the_newer_one_it_deferred_to_ends_first() {
    let (sink, mut rx) = test_sink();
    let attention = Attention::new(sink);
    let _older = attention.begin_ask(ask("a", AttentionReason::Permission));
    tokio::time::sleep(ASK_NOTIFY_DELAY / 2).await;
    let newer = attention.begin_ask(ask("a", AttentionReason::Question));
    // The older ask's deadline passes while the newer one is the latest.
    tokio::time::sleep(ASK_NOTIFY_DELAY / 2 + Duration::from_millis(10)).await;
    assert_eq!(
        states(&drain(&mut rx)),
        [("needsInput", false), ("needsInput", false)]
    );

    drop(newer);
    assert_eq!(states(&drain(&mut rx)), [("needsInput", true)]);
    // The newer ask's deadline, now past, sends nothing more.
    tokio::time::sleep(ASK_NOTIFY_DELAY).await;
    assert!(drain(&mut rx).is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_unfocused_end_becomes_unseen_and_focus_clears_it() {
    let (sink, mut rx) = test_sink();
    let attention = Attention::new(sink);
    attention.run_ended(&conversation("a"), Some(ended("r1", AttentionReason::Completed)));
    settle_idle(&attention, "a").await;
    assert_eq!(states(&drain(&mut rx)), [("unseen", true)]);
    assert_eq!(attention.list().entries[0].state, AttentionState::Unseen);

    attention.set_focus(ConnectionId::Primary, Some(conversation("a")));
    tokio::task::yield_now().await;
    assert_eq!(states(&drain(&mut rx)), [("none", false)]);
    assert!(attention.list().entries.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_focused_end_is_seen() {
    let (sink, mut rx) = test_sink();
    let attention = Attention::new(sink);
    attention.connection_opened(ConnectionId::Attached(4));
    attention.set_focus(ConnectionId::Attached(4), Some(conversation("a")));
    attention.run_ended(&conversation("a"), Some(ended("r1", AttentionReason::Completed)));
    settle_idle(&attention, "a").await;
    assert!(drain(&mut rx).is_empty());
    assert!(!attention.has_candidate(&conversation("a")));
}

#[tokio::test(start_paused = true)]
async fn a_glance_during_the_settle_delay_counts_as_seen() {
    let (sink, mut rx) = test_sink();
    let attention = Attention::new(sink);
    attention.run_ended(&conversation("a"), Some(ended("r1", AttentionReason::Failed)));
    attention.settle(&conversation("a"), || Box::pin(async { Settle::Idle }));
    tokio::time::sleep(SETTLE_DELAY / 2).await;
    attention.set_focus(ConnectionId::Primary, Some(conversation("a")));
    attention.set_focus(ConnectionId::Primary, Some(conversation("b")));
    tokio::time::sleep(SETTLE_DELAY).await;
    assert!(drain(&mut rx).is_empty());
    assert!(!attention.has_candidate(&conversation("a")));
}

#[tokio::test(start_paused = true)]
async fn seeing_clears_only_the_run_focus_saw() {
    let (sink, mut rx) = test_sink();
    let attention = Attention::new(sink);
    attention.run_ended(&conversation("a"), Some(ended("r2", AttentionReason::Completed)));
    settle_idle(&attention, "a").await;
    drain(&mut rx);
    let run = |id: &str| ConversationRunID(id.to_owned());
    attention.mark_seen(&conversation("a"), &run("r1")).await;
    assert!(drain(&mut rx).is_empty(), "a newer unseen run stays");
    attention.mark_seen(&conversation("a"), &run("r2")).await;
    assert_eq!(states(&drain(&mut rx)), [("none", false)]);
}

#[tokio::test(start_paused = true)]
async fn a_second_end_while_unseen_updates_silently() {
    let (sink, mut rx) = test_sink();
    let attention = Attention::new(sink);
    attention.run_ended(&conversation("a"), Some(ended("r1", AttentionReason::Completed)));
    settle_idle(&attention, "a").await;
    attention.run_ended(&conversation("a"), Some(ended("r2", AttentionReason::Failed)));
    settle_idle(&attention, "a").await;
    assert_eq!(states(&drain(&mut rx)), [("unseen", true), ("unseen", false)]);
    let entry = &attention.list().entries[0];
    assert_eq!(entry.run_id.as_ref().unwrap().0, "r2");
    assert_eq!(entry.reason, AttentionReason::Failed);
}

#[tokio::test(start_paused = true)]
async fn a_stopped_run_or_working_again_drops_the_end() {
    let (sink, mut rx) = test_sink();
    let attention = Attention::new(sink);
    attention.run_ended(&conversation("a"), Some(ended("r1", AttentionReason::Completed)));
    attention.settle(&conversation("a"), || Box::pin(async { Settle::Drop }));
    tokio::time::sleep(SETTLE_DELAY * 2).await;
    assert!(!attention.has_candidate(&conversation("a")));

    attention.run_ended(&conversation("b"), Some(ended("r1", AttentionReason::Completed)));
    attention.run_ended(&conversation("b"), None);
    settle_idle(&attention, "b").await;
    assert!(drain(&mut rx).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_waiting_tree_keeps_the_end_for_the_next_settle() {
    let (sink, mut rx) = test_sink();
    let attention = Attention::new(sink);
    attention.run_ended(&conversation("a"), Some(ended("r1", AttentionReason::Completed)));
    attention.settle(&conversation("a"), || Box::pin(async { Settle::Waiting }));
    tokio::time::sleep(SETTLE_DELAY * 2).await;
    assert!(drain(&mut rx).is_empty());
    assert!(attention.has_candidate(&conversation("a")));
    settle_idle(&attention, "a").await;
    assert_eq!(states(&drain(&mut rx)), [("unseen", true)]);
}

#[tokio::test(start_paused = true)]
async fn an_older_settle_does_not_commit_a_newer_end_early() {
    let (sink, mut rx) = test_sink();
    let attention = Attention::new(sink);
    attention.run_ended(&conversation("a"), Some(ended("r1", AttentionReason::Completed)));
    attention.settle(&conversation("a"), || Box::pin(async { Settle::Idle }));
    tokio::time::sleep(SETTLE_DELAY / 2).await;
    attention.run_ended(&conversation("a"), Some(ended("r2", AttentionReason::Completed)));
    attention.settle(&conversation("a"), || Box::pin(async { Settle::Idle }));
    tokio::time::sleep(SETTLE_DELAY / 2 + Duration::from_millis(10)).await;
    assert!(drain(&mut rx).is_empty(), "the first timer's run was replaced");
    tokio::time::sleep(SETTLE_DELAY / 2).await;
    assert_eq!(states(&drain(&mut rx)), [("unseen", true)]);
}

#[tokio::test(start_paused = true)]
async fn a_closed_connection_cannot_focus_again() {
    let (sink, mut rx) = test_sink();
    let attention = Attention::new(sink);
    attention.set_focus(ConnectionId::Attached(1), Some(conversation("a")));
    assert!(!attention.is_focused(&conversation("a")), "only an open connection focuses");
    attention.connection_opened(ConnectionId::Attached(1));
    attention.set_focus(ConnectionId::Attached(1), Some(conversation("a")));
    assert!(attention.is_focused(&conversation("a")));
    attention.connection_closed(ConnectionId::Attached(1));
    assert!(!attention.is_focused(&conversation("a")));
    attention.set_focus(ConnectionId::Attached(1), Some(conversation("a")));
    assert!(!attention.is_focused(&conversation("a")));
    assert!(attention.state().open.is_empty(), "a closed connection is not kept");

    attention.run_ended(&conversation("a"), Some(ended("r1", AttentionReason::Completed)));
    settle_idle(&attention, "a").await;
    assert_eq!(states(&drain(&mut rx)), [("unseen", true)]);
}

#[tokio::test(start_paused = true)]
async fn deleting_or_archiving_ends_attention() {
    let (sink, mut rx) = test_sink();
    let attention = Attention::new(sink);
    attention.run_ended(&conversation("a"), Some(ended("r1", AttentionReason::Completed)));
    attention.run_ended(&conversation("b"), Some(ended("r1", AttentionReason::Completed)));
    settle_idle(&attention, "a").await;
    settle_idle(&attention, "b").await;
    drain(&mut rx);

    attention.forget_conversation(&conversation("a"));
    assert_eq!(states(&drain(&mut rx)), [("none", false)]);
    attention.forget_workspace(&workspace()).await;
    assert_eq!(states(&drain(&mut rx)), [("none", false)]);
    assert!(attention.list().entries.is_empty());
}

#[tokio::test(start_paused = true)]
async fn shutdown_stops_pending_settles() {
    let (sink, mut rx) = test_sink();
    let attention = Attention::new(sink);
    attention.run_ended(&conversation("a"), Some(ended("r1", AttentionReason::Completed)));
    attention.settle(&conversation("a"), || Box::pin(async { Settle::Idle }));
    attention.shutdown();
    tokio::time::sleep(SETTLE_DELAY * 2).await;
    assert!(drain(&mut rx).is_empty());
}

#[test]
fn only_completed_and_failed_runs_can_be_unseen() {
    let make = |status| {
        Ended::from_status(
            workspace(),
            ConversationRunID("r".to_owned()),
            &status,
            &serde_json::json!({"message": " Out of credits "}),
            "Task".to_owned(),
        )
    };
    assert_eq!(make(ConversationRunStatus::Completed).unwrap().detail, None);
    assert_eq!(
        make(ConversationRunStatus::Failed).unwrap().detail.as_deref(),
        Some("Out of credits")
    );
    assert!(make(ConversationRunStatus::Canceled).is_none());
    assert!(make(ConversationRunStatus::Interrupted).is_none());
}

#[test]
fn a_completed_run_carries_the_start_of_its_last_message_as_markdown() {
    let message = "\n## Done\n\nAll **12** tests pass.\n";
    let completed = ended("r", AttentionReason::Completed).with_message(message);
    assert_eq!(
        completed.detail.as_deref(),
        Some("## Done\n\nAll **12** tests pass.")
    );
    let long = ended("r", AttentionReason::Completed).with_message(&"word ".repeat(1000));
    let detail = long.detail.unwrap();
    assert_eq!(detail.chars().count(), DETAIL_CHARS + 1);
    assert!(detail.ends_with('…'));
    assert_eq!(
        ended("r", AttentionReason::Completed).with_message(" \n").detail,
        None
    );
}
