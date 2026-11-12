//! Native subagent sessions (`sub:3`) through the SDK chain, replaying the
//! recorded Claude and Codex sessions: each child session's updates land in
//! its own thread, a subagent's permission request names it, Stop cancels the
//! live threads, and reopening the session replays nothing into storage.
use crate::support::{
    assert_success_response, read_log_text, CoreProcess, FakeAcpRuntime, GitRepoFixture,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct Conversation {
    project_id: String,
    workspace_id: String,
    conversation_id: String,
}

fn initialize_with_permissions(core: &mut CoreProcess) {
    let response = core.send(json!({
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": {"name": "test", "version": "0.0.0"},
            "capabilities": {"experimentalMethods": ["agent.requestPermission"]}
        }
    }));
    assert_success_response(&response, &json!(1));
}

fn start_run(
    core: &mut CoreProcess,
    repo: &GitRepoFixture,
    engine: &str,
) -> (Conversation, String) {
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"]
        .as_str()
        .expect("projectID")
        .to_string();
    let workspace_id = core.workspace_id(&project_id);
    let created = core.create_conversation(&project_id, Some("Native subagents"), Some("Go"));
    let conversation_id = created["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID")
        .to_string();
    let input_id = created["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("inputID")
        .to_string();
    let started = core.start_conversation_run(&project_id, &conversation_id, engine, &input_id);
    let run_id = started["run"]["runID"].as_str().expect("runID").to_string();
    (
        Conversation {
            project_id,
            workspace_id,
            conversation_id,
        },
        run_id,
    )
}

fn start_next_run(
    core: &mut CoreProcess,
    conversation: &Conversation,
    engine: &str,
    message: &str,
) -> String {
    let (posted, _) = core.send_for_id(
        json!({
            "id": 900,
            "method": "conversation.postMessage",
            "params": {
                "workspaceID": conversation.workspace_id,
                "conversationID": conversation.conversation_id,
                "message": message,
                "idempotencyKey": format!("native-subagents-{message}")
            }
        }),
        json!(900),
    );
    assert_success_response(&posted, &json!(900));
    let input_id = posted["result"]["input"]["runInputID"]
        .as_str()
        .expect("follow-up input")
        .to_string();
    let started = core.start_conversation_run(
        &conversation.project_id,
        &conversation.conversation_id,
        engine,
        &input_id,
    );
    started["run"]["runID"].as_str().expect("runID").to_string()
}

fn run_status(core: &mut CoreProcess, conversation: &Conversation, run_id: &str) -> String {
    let (response, _) = core.send_for_id(
        json!({
            "id": 901,
            "method": "conversation.get",
            "params": {
                "workspaceID": conversation.workspace_id,
                "conversationID": conversation.conversation_id
            }
        }),
        json!(901),
    );
    assert_success_response(&response, &json!(901));
    response["result"]["conversation"]["latestRunPage"]["runs"]
        .as_array()
        .and_then(|runs| runs.iter().find(|run| run["runID"] == run_id))
        .and_then(|run| run["status"].as_str())
        .unwrap_or_default()
        .to_string()
}

fn wait_for_status(
    core: &mut CoreProcess,
    conversation: &Conversation,
    run_id: &str,
    status: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while run_status(core, conversation, run_id) != status {
        assert!(
            Instant::now() < deadline,
            "run {run_id} never reached {status}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn list_transcript(core: &mut CoreProcess, conversation: &Conversation) -> Value {
    let (response, _) = core.send_for_id(
        json!({
            "id": 902,
            "method": "conversation.listTranscript",
            "params": {
                "workspaceID": conversation.workspace_id,
                "conversationID": conversation.conversation_id,
                "limit": 200
            }
        }),
        json!(902),
    );
    assert_success_response(&response, &json!(902));
    // Counts compare whole transcripts: one page must hold it all.
    assert!(
        response["result"]["nextCursor"].is_null(),
        "the transcript outgrew one page"
    );
    response["result"].clone()
}

fn list_thread(core: &mut CoreProcess, conversation: &Conversation, thread: &str) -> Value {
    let (response, _) = core.send_for_id(
        json!({
            "id": 904,
            "method": "conversation.listTranscript",
            "params": {
                "workspaceID": conversation.workspace_id,
                "conversationID": conversation.conversation_id,
                "subagentThreadID": thread,
                "limit": 200
            }
        }),
        json!(904),
    );
    assert_success_response(&response, &json!(904));
    response["result"].clone()
}

/// `thread id -> (state, parent thread, name)` for every thread's anchor.
fn threads(page: &Value) -> Vec<(String, String, Option<String>, Option<String>)> {
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|item| {
            let thread = &item["tool"]["subagentThread"];
            Some((
                thread["id"].as_str()?.to_string(),
                thread["state"].as_str()?.to_string(),
                thread["parentThreadID"].as_str().map(str::to_owned),
                thread["name"].as_str().map(str::to_owned),
            ))
        })
        .collect()
}

/// `(kind, thread)` of every item that belongs to a thread.
fn members(page: &Value) -> Vec<(String, String)> {
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|item| {
            Some((
                item["kind"].as_str()?.to_string(),
                item["subagentThreadID"].as_str()?.to_string(),
            ))
        })
        .collect()
}

/// Answers the first permission request core forwards, returning its params.
fn answer_permission(core: &mut CoreProcess) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "no permission request reached the client"
        );
        let Some(message) = core.read_json_timeout(remaining) else {
            continue;
        };
        if message["method"] != "agent.requestPermission" {
            continue;
        }
        return allow_once(core, &message);
    }
}

fn allow_once(core: &mut CoreProcess, request: &Value) -> Value {
    let params = request["params"].clone();
    let option = params["options"]
        .as_array()
        .and_then(|options| options.iter().find(|option| option["kind"] == "allowOnce"))
        .and_then(|option| option["optionID"].as_str())
        .expect("allow once")
        .to_string();
    core.write_message(json!({
        "id": request["id"],
        "result": {"outcome": "selected", "optionID": option}
    }));
    params
}

/// Waits for `run` to complete, allowing every permission request on the way.
fn complete_allowing(core: &mut CoreProcess, conversation: &Conversation, run_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (response, others) = core.send_for_id(
            json!({
                "id": 905,
                "method": "conversation.get",
                "params": {
                    "workspaceID": conversation.workspace_id,
                    "conversationID": conversation.conversation_id
                }
            }),
            json!(905),
        );
        for request in others
            .iter()
            .filter(|message| message["method"] == "agent.requestPermission")
        {
            allow_once(core, request);
        }
        let completed = response["result"]["conversation"]["latestRunPage"]["runs"]
            .as_array()
            .and_then(|runs| runs.iter().find(|run| run["runID"] == run_id))
            .is_some_and(|run| run["status"] == "completed");
        if completed {
            return;
        }
        assert!(Instant::now() < deadline, "run {run_id} never completed");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn rows_with_ids(rows: &[&Value]) -> usize {
    rows.iter()
        .filter(|item| item["providerMessageID"].is_string())
        .count()
}

fn native_subagents_across_the_sdk_chain(recording: &str) {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "native subagents\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_native_subagents(recording, false);

    let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    initialize_with_permissions(&mut core);
    let (conversation, run_id) = start_run(&mut core, &repo, recording);

    // The subagent asks on its own session: core names it.
    let permission = answer_permission(&mut core);
    assert_eq!(
        permission["conversationID"],
        json!(conversation.conversation_id)
    );
    assert_eq!(permission["runID"], json!(run_id));
    let asking = permission["subagentThreadID"]
        .as_str()
        .expect("a subagent asked")
        .to_string();
    assert!(permission["subagentName"].is_string(), "{permission}");

    // Held before any subagent ends: three threads, one nested, all running.
    let deadline = Instant::now() + Duration::from_secs(10);
    let running = loop {
        let page = list_transcript(&mut core, &conversation);
        if threads(&page).len() == 3 && members(&page).iter().any(|(_, thread)| *thread != asking) {
            break page;
        }
        assert!(Instant::now() < deadline, "the threads never opened");
        std::thread::sleep(Duration::from_millis(10));
    };
    let opened = threads(&running);
    assert!(
        opened.iter().all(|(_, state, _, _)| state == "running"),
        "{opened:?}"
    );
    let nested: Vec<_> = opened
        .iter()
        .filter(|(_, _, parent, _)| parent.is_some())
        .collect();
    assert_eq!(nested.len(), 1, "{opened:?}");
    assert!(opened.iter().any(|(id, ..)| *id == asking));
    assert_eq!(
        running["subagentThreads"],
        json!([{"runID": run_id, "live": 3}])
    );

    runtime.publish_marker("subagents-complete");
    wait_for_status(&mut core, &conversation, &run_id, "completed");
    let finished = list_transcript(&mut core, &conversation);
    let ended = threads(&finished);
    assert!(
        ended.iter().all(|(_, state, _, _)| state == "completed"),
        "{ended:?}"
    );
    // Every thread got the subagent's own text and calls.
    let joined = members(&finished);
    for (thread, ..) in &ended {
        assert!(
            joined
                .iter()
                .any(|(kind, member)| member == thread && kind == "assistantText"),
            "{thread} has no text: {joined:?}"
        );
        assert!(
            joined
                .iter()
                .any(|(kind, member)| member == thread && kind == "tool"),
            "{thread} has no calls: {joined:?}"
        );
    }
    // One thread's page: its rows and its nested thread's, both anchors, and
    // nothing of the parent's or the sibling's.
    let (nested_id, outer_id) = ended
        .iter()
        .find_map(|(id, _, parent, _)| Some((id.clone(), parent.clone()?)))
        .expect("a nested thread");
    let page = list_thread(&mut core, &conversation, &outer_id);
    let family = [outer_id.as_str(), nested_id.as_str()];
    let shown = page["items"].as_array().expect("items");
    assert!(!shown.is_empty());
    for item in shown {
        let member = item["subagentThreadID"]
            .as_str()
            .is_some_and(|thread| family.contains(&thread));
        let anchor = item["tool"]["subagentThread"]["id"]
            .as_str()
            .is_some_and(|thread| family.contains(&thread));
        assert!(member || anchor, "{item}");
    }
    for thread in family {
        assert!(shown
            .iter()
            .any(|item| item["tool"]["subagentThread"]["id"] == thread));
        assert!(shown
            .iter()
            .any(|item| item["kind"] == "assistantText" && item["subagentThreadID"] == thread));
    }
    let everything = members(&finished)
        .iter()
        .filter(|(_, thread)| family.contains(&thread.as_str()))
        .count();
    assert_eq!(members(&page).len(), everything);

    // The parent's text is one row per message, never split by a child's.
    let parent_text: Vec<&Value> = finished["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["kind"] == "assistantText" && item["subagentThreadID"].is_null())
        .collect();
    let mut messages: Vec<&str> = parent_text
        .iter()
        .filter_map(|item| item["providerMessageID"].as_str())
        .collect();
    assert!(!parent_text.is_empty(), "the parent wrote no text");
    assert_eq!(rows_with_ids(&parent_text), parent_text.len());
    let rows = messages.len();
    messages.dedup();
    assert_eq!(rows, messages.len(), "a parent message was split");
    // Nothing claude-agent-acp reports for its hidden launch call is a row.
    assert!(
        finished["items"]
            .as_array()
            .expect("items")
            .iter()
            .all(|item| item["kind"] != "tool"
                || item["tool"]["title"].is_string()
                || item["tool"]["subagentThread"].is_object()),
        "an untitled tool row was recorded"
    );

    // Stop mid-subagent: its thread reads cancelled.
    runtime.set_next_native_subagents(recording, true);
    let cancelled_run = start_next_run(&mut core, &conversation, recording, "Run one subagent");
    let deadline = Instant::now() + Duration::from_secs(10);
    while threads(&list_transcript(&mut core, &conversation)).len() < 4 {
        assert!(
            Instant::now() < deadline,
            "the cancelled turn's thread never opened"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let (stopped, _) = core.send_for_id(
        json!({
            "id": 903,
            "method": "conversation.cancelRun",
            "params": {
                "workspaceID": conversation.workspace_id,
                "conversationID": conversation.conversation_id,
                "runID": cancelled_run
            }
        }),
        json!(903),
    );
    assert_success_response(&stopped, &json!(903));
    wait_for_status(&mut core, &conversation, &cancelled_run, "canceled");
    let after_cancel = list_transcript(&mut core, &conversation);
    let last = threads(&after_cancel);
    assert_eq!(last.len(), 4, "{last:?}");
    let stopped: Vec<_> = last
        .iter()
        .filter(|(id, ..)| !ended.iter().any(|(earlier, ..)| earlier == id))
        .collect();
    assert_eq!(stopped.len(), 1, "{last:?}");
    assert_eq!(stopped[0].1, "cancelled", "{last:?}");
    assert!(
        last.iter()
            .filter(|(id, ..)| *id != stopped[0].0)
            .all(|(_, state, ..)| state == "completed"),
        "Stop touched an earlier turn's thread: {last:?}"
    );
    let expected = (threads(&after_cancel), members(&after_cancel));
    let log_path = core.log_file_path();
    core.shutdown();

    // Reopen: a restarted core loads the session, whose replay announces the
    // children again under other ids; none of it is recorded.
    let mut restarted = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    initialize_with_permissions(&mut restarted);
    restarted.open_path(repo.path());
    let reopened = list_transcript(&mut restarted, &conversation);
    assert_eq!((threads(&reopened), members(&reopened)), expected);
    // The thread's page reads the same from storage as it did live.
    let stored = list_thread(&mut restarted, &conversation, &outer_id);
    assert_eq!(
        (threads(&stored), members(&stored)),
        (threads(&page), members(&page))
    );
    runtime.set_next_load_replay(recording);
    let replayed_run = start_next_run(&mut restarted, &conversation, recording, "And now?");
    wait_for_status(&mut restarted, &conversation, &replayed_run, "completed");
    assert!(
        runtime.request_log().contains("session/load"),
        "the follow-up never loaded the session"
    );
    let after_load = list_transcript(&mut restarted, &conversation);
    assert_eq!((threads(&after_load), members(&after_load)), expected);
    let restarted_log = restarted.log_file_path();
    restarted.shutdown();

    for log in [read_log_text(&log_path), read_log_text(&restarted_log)] {
        assert!(!log.is_empty(), "core wrote no log to check");
        assert!(
            !log.lines()
                .any(|line| line.contains("reason=\"unattributed\"")
                    || line.contains("reason=unattributed")),
            "a subagent update went unattributed:\n{log}"
        );
    }
}

#[test]
fn native_subagent_threads_record_claude_children_by_session() {
    native_subagents_across_the_sdk_chain("claude");
}

#[test]
fn native_subagent_threads_record_codex_children_by_session() {
    native_subagents_across_the_sdk_chain("codex");
}

/// `(thread, anchor call, state, prompt)` of every run's anchor, oldest first.
fn anchors(page: &Value) -> Vec<(String, String, String, Option<String>)> {
    let mut anchors: Vec<_> = page["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|item| {
            let thread = &item["tool"]["subagentThread"];
            Some((
                item["position"].as_i64()?,
                (
                    thread["id"].as_str()?.to_string(),
                    item["tool"]["providerToolCallID"].as_str()?.to_string(),
                    thread["state"].as_str()?.to_string(),
                    thread["prompt"].as_str().map(str::to_owned),
                ),
            ))
        })
        .collect();
    anchors.sort_by_key(|(position, _)| *position);
    anchors.into_iter().map(|(_, anchor)| anchor).collect()
}

/// `(thread, message)` of every parent call that messaged a running subagent.
fn follow_ups(page: &Value) -> Vec<(String, String)> {
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|item| {
            let follow_up = &item["tool"]["subagentFollowUp"];
            Some((
                follow_up["threadID"].as_str()?.to_string(),
                follow_up["message"].as_str()?.to_string(),
            ))
        })
        .collect()
}

fn run_turn(
    core: &mut CoreProcess,
    runtime: &FakeAcpRuntime,
    conversation: &Conversation,
    recording: &str,
    scenario: &str,
) {
    runtime.set_next_native_subagent_turn(recording, scenario);
    let run = start_next_run(core, conversation, recording, scenario);
    complete_allowing(core, conversation, &run);
}

/// The parent messages a subagent after it finished and while it runs: each
/// subagent keeps one thread, a resumed run adds an anchor carrying the
/// message, and a message to a running subagent marks the parent's call.
fn follow_ups_across_the_sdk_chain(recording: &str) {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "native subagents\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_native_subagent_turn(recording, "NativeFirstRun");
    let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    initialize_with_permissions(&mut core);
    let (conversation, first_run) = start_run(&mut core, &repo, recording);
    complete_allowing(&mut core, &conversation, &first_run);
    let first = anchors(&list_transcript(&mut core, &conversation));
    assert_eq!(first.len(), 1, "{first:?}");
    let (thread, ..) = first[0].clone();
    assert_eq!(first[0].1, thread, "a first run's anchor is its thread's");

    run_turn(
        &mut core,
        &runtime,
        &conversation,
        recording,
        "NativeResumed",
    );
    let resumed = anchors(&list_transcript(&mut core, &conversation));
    if recording == "claude" {
        // Claude resumes the finished subagent as `<id>:generation:2`.
        assert_eq!(resumed.len(), 2, "{resumed:?}");
        assert_eq!(resumed[1].0, thread, "the resumed run left its thread");
        assert_eq!(resumed[1].1, format!("{thread}:generation:2"));
        assert_eq!(resumed[1].2, "completed");
        assert!(
            resumed[1]
                .3
                .as_deref()
                .is_some_and(|prompt| prompt.contains("count the words")),
            "{resumed:?}"
        );
        // The thread's page holds both runs.
        let page = list_thread(&mut core, &conversation, &thread);
        assert_eq!(anchors(&page).len(), 2);
    } else {
        // codex-acp forwards nothing of a finished subagent's new work.
        assert_eq!(resumed, first);
    }

    run_turn(
        &mut core,
        &runtime,
        &conversation,
        recording,
        "NativeRunningFollowUp",
    );
    let finished = list_transcript(&mut core, &conversation);
    let running = anchors(&finished);
    let (running_thread, ..) = running.last().expect("a third thread").clone();
    assert_ne!(running_thread, thread);
    let page = list_thread(&mut core, &conversation, &running_thread);
    // The subagent's own calls after the message are in its thread.
    assert!(
        page["items"].as_array().expect("items").iter().any(|item| {
            item["subagentThreadID"] == running_thread.as_str()
                && item["tool"]["title"]
                    .as_str()
                    .is_some_and(|title| title.contains("hostname"))
        }),
        "{page}"
    );
    let messages = follow_ups(&finished);
    if recording == "claude" {
        // Only the call to the running subagent is marked; the resumed one
        // carries its message on its anchor.
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert_eq!(messages[0].0, running_thread);
        assert!(messages[0].1.contains("hostname"), "{messages:?}");
        assert_eq!(follow_ups(&page), messages);
    } else {
        // codex-acp hides its collaboration calls in native mode.
        assert!(messages.is_empty(), "{messages:?}");
    }
    let expected = (
        anchors(&finished),
        members(&finished),
        follow_ups(&finished),
    );
    core.shutdown();

    let mut restarted = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    initialize_with_permissions(&mut restarted);
    restarted.open_path(repo.path());
    let reopened = list_transcript(&mut restarted, &conversation);
    assert_eq!(
        (
            anchors(&reopened),
            members(&reopened),
            follow_ups(&reopened)
        ),
        expected
    );
    restarted.shutdown();
}

#[test]
fn native_subagent_follow_ups_join_claude_threads() {
    follow_ups_across_the_sdk_chain("claude");
}

#[test]
fn native_subagent_follow_ups_join_codex_threads() {
    follow_ups_across_the_sdk_chain("codex");
}

/// A Claude subagent that reports through `SubagentHandback`: the call stays
/// a row, and the report is the thread's text after it.
#[test]
fn native_subagent_handback_is_the_threads_text() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "native subagents\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_native_subagent_turn("claude", "NativeHandback");
    let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    initialize_with_permissions(&mut core);
    let (conversation, run) = start_run(&mut core, &repo, "claude");
    complete_allowing(&mut core, &conversation, &run);
    let thread = anchors(&list_transcript(&mut core, &conversation))[0]
        .0
        .clone();
    let page = list_thread(&mut core, &conversation, &thread);
    let mut items: Vec<&Value> = page["items"].as_array().expect("items").iter().collect();
    items.sort_by_key(|item| item["position"].as_i64());
    let call = items
        .iter()
        .position(|item| item["tool"]["name"] == "SubagentHandback")
        .expect("the handback call is a row");
    let report = items
        .iter()
        .position(|item| {
            item["kind"] == "assistantText"
                && item["providerMessageID"] == "handback:toolu_01HandbackRebuiltFixture0"
        })
        .expect("the report is text");
    assert!(report > call, "the report precedes its call");
    assert_eq!(items[report]["subagentThreadID"], thread.as_str());
    assert_eq!(
        items[report]["bodyPreview"],
        "The first line of README.md is `# m3-walk`."
    );
    // The subagent's own last message stays its own row.
    assert!(
        items[..call]
            .iter()
            .any(|item| item["kind"] == "assistantText"
                && item["subagentThreadID"] == thread.as_str())
    );
    core.shutdown();
}

/// ACP's merged Subagent Sessions shape, as oh-my-pi's open PR sends it
/// (hand-built fixture): each `subagent_update` child becomes one thread
/// named by its title, its assignment `session_message` becomes the thread's
/// prompt, its own updates land in it, and `idle` ends it with its reason.
#[test]
fn acp_subagent_sessions_become_threads() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "native subagents\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_native_subagent_turn("omp", "NativeTurn");
    let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    initialize_with_permissions(&mut core);
    // Any adapter replays the frames; the shape is what is under test.
    let (conversation, run_id) = start_run(&mut core, &repo, "claude");
    complete_allowing(&mut core, &conversation, &run_id);

    let page = list_transcript(&mut core, &conversation);
    let mut found = threads(&page);
    found.sort_by(|left, right| left.3.cmp(&right.3));
    let summary: Vec<_> = found
        .iter()
        .map(|(_, state, parent, name)| (name.clone(), state.clone(), parent.clone()))
        .collect();
    assert_eq!(
        summary,
        vec![
            (Some("RustBackend".into()), "completed".into(), None),
            (Some("SwiftApp".into()), "failed".into(), None),
        ]
    );
    let mut prompts: Vec<_> = anchors(&page).into_iter().map(|anchor| anchor.3).collect();
    prompts.sort();
    assert_eq!(
        prompts,
        vec![
            Some("Map the Rust backend".to_string()),
            Some("Map the Swift app".to_string())
        ]
    );

    let (rust, ..) = &found[0];
    let mut kinds: Vec<_> = members(&list_thread(&mut core, &conversation, rust))
        .into_iter()
        .filter(|(_, thread)| thread == rust)
        .map(|(kind, _)| kind)
        .collect();
    kinds.sort();
    assert_eq!(kinds, vec!["assistantText", "tool"], "{kinds:?}");
}
