use crate::support::{
    assert_success_response, read_log_text, CoreProcess, FakeAcpRuntime, FakePromptBehavior,
    GitRepoFixture,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// `(projectID, workspaceID, conversationID, runID)` of a Codex run started
/// on `repo`.
fn start_codex_run(
    core: &mut CoreProcess,
    repo: &GitRepoFixture,
) -> (String, String, String, String) {
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"]
        .as_str()
        .expect("projectID")
        .to_string();
    let workspace_id = core.workspace_id(&project_id);
    let created = core.create_conversation(&project_id, Some("Handler chain"), Some("Go"));
    let conversation_id = created["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID")
        .to_string();
    let input_id = created["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("inputID")
        .to_string();
    let started = core.start_conversation_run(&project_id, &conversation_id, "codex", &input_id);
    let run_id = started["run"]["runID"].as_str().expect("runID").to_string();
    (project_id, workspace_id, conversation_id, run_id)
}

/// Every message read while waiting, so the test can see what reached the
/// wire as well as what was stored.
fn wait_for_completed_run(
    core: &mut CoreProcess,
    workspace_id: &str,
    conversation_id: &str,
    run_id: &str,
) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = Vec::new();
    loop {
        let (response, notifications) = core.send_for_id(
            json!({
                "id": 801,
                "method": "conversation.get",
                "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
            }),
            json!(801),
        );
        assert_success_response(&response, &json!(801));
        seen.extend(notifications);
        let status = response["result"]["conversation"]["latestRunPage"]["runs"]
            .as_array()
            .and_then(|runs| runs.iter().find(|run| run["runID"] == run_id))
            .map(|run| run["status"].clone());
        if status == Some(json!("completed")) {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "run {run_id} did not complete; last status {status:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `(providerToolCallID, backgroundTask)` for each tool that has a task.
fn background_tasks(
    core: &mut CoreProcess,
    workspace_id: &str,
    conversation_id: &str,
) -> Vec<(String, Value)> {
    let (response, _) = core.send_for_id(
        json!({
            "id": 802,
            "method": "conversation.listTranscript",
            "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
        }),
        json!(802),
    );
    assert_success_response(&response, &json!(802));
    response["result"]["items"]
        .as_array()
        .expect("transcript items")
        .iter()
        .filter(|item| item["tool"]["backgroundTask"].is_object())
        .map(|item| {
            (
                item["tool"]["providerToolCallID"]
                    .as_str()
                    .expect("providerToolCallID")
                    .to_string(),
                item["tool"]["backgroundTask"].clone(),
            )
        })
        .collect()
}

/// The pinned SDK's typed parse rejects `async_task_*`; the raw handler ahead
/// of it must claim them without disturbing the typed frames around them.
#[test]
fn async_task_lifecycle_crosses_the_sdk_chain_and_survives_a_restart() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "background tasks\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::AsyncTasks);
    // Transcript pages are newest-first: Codex's frames followed Claude's.
    let expected = vec![
        (
            "call_background_dev".to_string(),
            json!({"id": "call_background_dev", "state": "stopped"}),
        ),
        (
            "toolu_01BackgroundDevServer".to_string(),
            json!({"id": "b7x1k2", "state": "completed", "outputFilePath": "/tmp/claude/tasks/b7x1k2.output"}),
        ),
    ];

    let (workspace_id, conversation_id) = {
        let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
        core.initialize();
        let (_, workspace_id, conversation_id, run_id) = start_codex_run(&mut core, &repo);
        let seen = wait_for_completed_run(&mut core, &workspace_id, &conversation_id, &run_id);

        // Each tool's background task, state by state as the transcript
        // revised it: the frames crossed the live chain in order. Which
        // frames decode, progress included, is pinned in `agent/acp/tests.rs`.
        let mut states: Vec<(String, Vec<String>)> = Vec::new();
        for item in seen
            .iter()
            .filter(|message| message["method"] == "conversation.transcriptDelta")
            .filter_map(|message| message["params"]["items"].as_array())
            .flatten()
        {
            let (Some(call), Some(state)) = (
                item["tool"]["providerToolCallID"].as_str(),
                item["tool"]["backgroundTask"]["state"].as_str(),
            ) else {
                continue;
            };
            match states.iter_mut().find(|(id, _)| id == call) {
                Some((_, seen)) if seen.last().map(String::as_str) == Some(state) => {}
                Some((_, seen)) => seen.push(state.to_string()),
                None => states.push((call.to_string(), vec![state.to_string()])),
            }
        }
        assert_eq!(
            states,
            [
                (
                    "toolu_01BackgroundDevServer".to_string(),
                    vec!["running".to_string(), "completed".to_string()]
                ),
                (
                    "call_background_dev".to_string(),
                    vec!["running".to_string(), "stopped".to_string()]
                ),
            ]
        );
        assert!(
            seen.iter().any(|message| {
                message["method"] == "conversation.transcriptDelta"
                    && message["params"]["items"].as_array().is_some_and(|items| {
                        items
                            .iter()
                            .any(|item| item["tool"]["backgroundTask"]["state"] == "completed")
                    })
            }),
            "no transcript delta revised a tool's background task"
        );
        // The overlay of live memory: what a client reads before a restart.
        assert_eq!(
            background_tasks(&mut core, &workspace_id, &conversation_id),
            expected
        );
        core.shutdown();
        (workspace_id, conversation_id)
    };

    let mut restarted = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    restarted.initialize();
    restarted.open_path(repo.path());
    assert_eq!(
        background_tasks(&mut restarted, &workspace_id, &conversation_id),
        expected
    );
    restarted.shutdown();
}

/// Polls `conversation.listTranscript` until the tool's background task is in
/// `state`; returns that page and every message read while waiting.
fn wait_for_task_state(
    core: &mut CoreProcess,
    workspace_id: &str,
    conversation_id: &str,
    tool_call_id: &str,
    state: &str,
) -> (Value, Vec<Value>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = Vec::new();
    loop {
        let (response, notifications) = core.send_for_id(
            json!({
                "id": 804,
                "method": "conversation.listTranscript",
                "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
            }),
            json!(804),
        );
        assert_success_response(&response, &json!(804));
        seen.extend(notifications);
        let current = response["result"]["items"]
            .as_array()
            .expect("transcript items")
            .iter()
            .find(|item| item["tool"]["providerToolCallID"] == tool_call_id)
            .map(|item| item["tool"]["backgroundTask"]["state"].clone());
        if current == Some(json!(state)) {
            return (response["result"].clone(), seen);
        }
        assert!(
            Instant::now() < deadline,
            "{tool_call_id} never reached {state}; last {current:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The run's live background tasks, as a transcript page reports them.
fn live_tasks(page: &Value, run_id: &str) -> Option<u64> {
    page["backgroundTasks"]
        .as_array()?
        .iter()
        .find(|count| count["runID"] == run_id)?["live"]
        .as_u64()
}

/// Polls until the run's live count is `live`; returns every message read
/// while waiting. A page freezes its counts before it reads storage, so a
/// revision written straight to storage can show before its count does.
fn wait_for_live_tasks(
    core: &mut CoreProcess,
    workspace_id: &str,
    conversation_id: &str,
    run_id: &str,
    live: u64,
) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = Vec::new();
    loop {
        let (response, notifications) = core.send_for_id(
            json!({
                "id": 806,
                "method": "conversation.listTranscript",
                "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
            }),
            json!(806),
        );
        assert_success_response(&response, &json!(806));
        seen.extend(notifications);
        let current = live_tasks(&response["result"], run_id);
        if current == Some(live) {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "run {run_id} never had {live} live tasks; last {current:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Codex stamps, spawns, and stops a background terminal after `end_turn`.
/// Those frames revise the completed run's tool, and the run's live count
/// rides the transcript stream (rules 3 and 4).
#[test]
fn async_task_frames_after_end_turn_reach_the_completed_run() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "background after the turn\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::AsyncTasksAfterTurn);
    let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    core.initialize();
    let (_, workspace_id, conversation_id, run_id) = start_codex_run(&mut core, &repo);
    wait_for_completed_run(&mut core, &workspace_id, &conversation_id, &run_id);
    assert!(background_tasks(&mut core, &workspace_id, &conversation_id).is_empty());

    runtime.publish_marker("after-turn-spawn");
    wait_for_task_state(
        &mut core,
        &workspace_id,
        &conversation_id,
        "call_after_turn",
        "running",
    );
    wait_for_live_tasks(&mut core, &workspace_id, &conversation_id, &run_id, 1);

    runtime.publish_marker("after-turn-stop");
    let (_, mut seen) = wait_for_task_state(
        &mut core,
        &workspace_id,
        &conversation_id,
        "call_after_turn",
        "stopped",
    );
    seen.extend(wait_for_live_tasks(
        &mut core,
        &workspace_id,
        &conversation_id,
        &run_id,
        0,
    ));
    // The page can read the write before its delta is sent; one more round
    // trip collects it.
    seen.extend(core.send_for_id(json!({"id": 805, "method": "conversation.get", "params": {"workspaceID": workspace_id, "conversationID": conversation_id}}), json!(805)).1);
    assert!(
        seen.iter().any(|message| {
            message["method"] == "conversation.transcriptDelta"
                && message["params"]["backgroundTasks"] == json!([{"runID": run_id, "live": 0}])
        }),
        "no transcript delta carried the run's count back to zero"
    );
    assert_eq!(
        background_tasks(&mut core, &workspace_id, &conversation_id),
        [(
            "call_after_turn".to_string(),
            json!({"id": "call_after_turn", "state": "stopped"})
        )]
    );
    core.shutdown();
}

/// A task left running by an earlier core can never report its end, so a
/// restarted core reads it as lost (rule 7).
#[test]
fn async_task_running_at_shutdown_reads_lost_after_a_restart() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "background across a restart\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::AsyncTasksAfterTurn);
    let (workspace_id, conversation_id) = {
        let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
        core.initialize();
        let (_, workspace_id, conversation_id, run_id) = start_codex_run(&mut core, &repo);
        wait_for_completed_run(&mut core, &workspace_id, &conversation_id, &run_id);
        runtime.publish_marker("after-turn-spawn");
        wait_for_task_state(
            &mut core,
            &workspace_id,
            &conversation_id,
            "call_after_turn",
            "running",
        );
        core.shutdown();
        (workspace_id, conversation_id)
    };

    let mut restarted = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    restarted.initialize();
    restarted.open_path(repo.path());
    let (page, _) = wait_for_task_state(
        &mut restarted,
        &workspace_id,
        &conversation_id,
        "call_after_turn",
        "lost",
    );
    assert_eq!(page["backgroundTasks"], json!([]));
    restarted.shutdown();
}

/// When the adapter process exits, nothing can end its running tasks, so
/// core marks them lost on the run that started them (rule 7).
#[test]
fn async_task_running_when_the_adapter_exits_is_marked_lost() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "background when the adapter exits\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::AsyncTasksAfterTurn);
    let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    core.initialize();
    let (project_id, workspace_id, conversation_id, run_id) = start_codex_run(&mut core, &repo);
    wait_for_completed_run(&mut core, &workspace_id, &conversation_id, &run_id);
    runtime.publish_marker("after-turn-spawn");
    wait_for_task_state(
        &mut core,
        &workspace_id,
        &conversation_id,
        "call_after_turn",
        "running",
    );

    runtime.set_next_prompt(FakePromptBehavior::ExitAfterUpdate);
    let posted = core.post_conversation_message(&project_id, &conversation_id, "Again");
    let input_id = posted["input"]["runInputID"]
        .as_str()
        .expect("follow-up inputID")
        .to_string();
    core.start_conversation_run(&project_id, &conversation_id, "codex", &input_id);
    wait_for_task_state(
        &mut core,
        &workspace_id,
        &conversation_id,
        "call_after_turn",
        "lost",
    );
    wait_for_live_tasks(&mut core, &workspace_id, &conversation_id, &run_id, 0);
    core.shutdown();
}

/// Traffic core can't use is logged once per label with its shape, never its
/// payload, and doesn't disturb the turn. The SDK parks an unclaimed message
/// that carries a `sessionId` until a dynamic handler is added, and core adds
/// none, so the last handler must answer the request, or the adapter waits on
/// it and the turn never ends.
#[test]
fn unhandled_traffic_is_logged_without_payloads_and_requests_are_answered() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "unhandled traffic\n");
    repo.commit_all("initial");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::UnhandledTraffic);
    // Owned here so the log outlives the process: shutdown flushes it.
    let home = TempDir::new().expect("OTTAM_HOME");
    let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    core.initialize();
    let (_, workspace_id, conversation_id, run_id) = start_codex_run(&mut core, &repo);
    wait_for_completed_run(&mut core, &workspace_id, &conversation_id, &run_id);

    let (transcript, _) = core.send_for_id(
        json!({
            "id": 803,
            "method": "conversation.listTranscript",
            "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
        }),
        json!(803),
    );
    assert_success_response(&transcript, &json!(803));
    let text = transcript["result"]["items"]
        .as_array()
        .expect("transcript items")
        .iter()
        .filter(|item| item["kind"] == "assistantText")
        .filter_map(|item| item["bodyPreview"].as_str())
        .collect::<String>();
    assert!(
        text.contains("Visible despite an extra field."),
        "a chunk with an unmodeled field still lands: {text:?}"
    );
    assert!(
        text.contains("fs/read_text_file with error -32601"),
        "the adapter never quoted a method_not_found answer: {text:?}"
    );

    let log_path = core.log_file_path();
    core.shutdown();
    let logged = read_log_text(&log_path);
    let unhandled = logged
        .lines()
        .filter(|line| line.contains("Unhandled agent traffic"))
        .collect::<Vec<_>>();
    for label in [
        "update:brand_new_kind",
        "field:agent_message_chunk",
        "notification:_ottam_test/unclaimed",
        "request:fs/read_text_file",
    ] {
        assert!(
            unhandled.iter().any(|line| line.contains(label)),
            "no unhandled-traffic line for {label}:\n{logged}"
        );
    }
    assert!(
        unhandled
            .iter()
            .any(|line| line.contains("update:brand_new_kind")
                && line.contains("secretPayload:str")
                && line.contains("sessionUpdate:str")),
        "the unknown kind's shape is missing:\n{logged}"
    );
    assert!(
        !logged.contains("PRIVATE-TEXT"),
        "a payload value reached the log:\n{logged}"
    );
}

/// Payloads reach the log only through the opt-in wire tap, with the filter
/// `AGENTS.md` documents for it.
#[test]
fn wire_log_records_payloads_only_when_opted_in() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "wire log\n");
    repo.commit_all("initial");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::UnhandledTraffic);
    let mut environment = runtime.environment();
    environment.extend([
        ("OTTAM_ACP_WIRE_LOG".to_string(), "1".to_string()),
        (
            "OTTAM_LOG".to_string(),
            "ottam_core=info,acp.agent.wire=trace,warn".to_string(),
        ),
    ]);
    let home = TempDir::new().expect("OTTAM_HOME");
    let mut core = CoreProcess::new_in_home_with_env(home.path(), environment);
    core.initialize();
    let (_, workspace_id, conversation_id, run_id) = start_codex_run(&mut core, &repo);
    wait_for_completed_run(&mut core, &workspace_id, &conversation_id, &run_id);

    let log_path = core.log_file_path();
    core.shutdown();
    let logged = read_log_text(&log_path);
    assert!(
        logged.lines().any(|line| line.contains("acp.agent.wire")
            && line.contains("brand_new_kind")
            && line.contains("PRIVATE-TEXT")),
        "the wire tap logged no frames:\n{logged}"
    );
}

/// Runs of the conversation started by the agent, from `conversation.listRuns`.
fn autonomous_runs(
    core: &mut CoreProcess,
    workspace_id: &str,
    conversation_id: &str,
) -> Vec<Value> {
    let (response, _) = core.send_for_id(
        json!({
            "id": 807,
            "method": "conversation.listRuns",
            "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
        }),
        json!(807),
    );
    assert_success_response(&response, &json!(807));
    response["result"]["page"]["runs"]
        .as_array()
        .expect("runs")
        .iter()
        .filter(|run| run["origin"] == "autonomous")
        .cloned()
        .collect()
}

/// Claude's background command ends after the turn, and what it then says
/// on its own lands in an autonomous run that its usage report closes
/// (bg:3). The task's end still revises the run that started it.
#[test]
fn async_task_completion_opens_a_follow_up_run_for_what_the_agent_says_next() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "background follow-up\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::AsyncTasksFollowUp);
    let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    core.initialize();
    let (_, workspace_id, conversation_id, run_id) = start_codex_run(&mut core, &repo);
    wait_for_completed_run(&mut core, &workspace_id, &conversation_id, &run_id);
    wait_for_live_tasks(&mut core, &workspace_id, &conversation_id, &run_id, 1);
    assert!(autonomous_runs(&mut core, &workspace_id, &conversation_id).is_empty());

    runtime.publish_marker("follow-up-complete");
    // The settle window after the usage report is 5 s.
    let deadline = Instant::now() + Duration::from_secs(15);
    let follow_up = loop {
        let runs = autonomous_runs(&mut core, &workspace_id, &conversation_id);
        if let Some(run) = runs.iter().find(|run| run["status"] == "completed") {
            assert_eq!(runs.len(), 1, "one follow-up: {runs:?}");
            break run.clone();
        }
        assert!(
            Instant::now() < deadline,
            "no follow-up completed: {runs:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let follow_up_id = follow_up["runID"].as_str().expect("follow-up runID");
    let (page, _) = wait_for_task_state(
        &mut core,
        &workspace_id,
        &conversation_id,
        "toolu_follow_up",
        "completed",
    );
    let items = page["items"].as_array().expect("transcript items");
    let owner = |predicate: &dyn Fn(&Value) -> bool| {
        items
            .iter()
            .find(|item| predicate(item))
            .map(|item| item["runID"].clone())
    };
    assert_eq!(
        owner(&|item| item["bodyPreview"] == "Retained ACP reply."),
        Some(json!(follow_up_id))
    );
    assert_eq!(
        owner(&|item| item["tool"]["providerToolCallID"] == "toolu_follow_up"),
        Some(json!(run_id))
    );
    assert_eq!(live_tasks(&page, &run_id).unwrap_or(0), 0);
    core.shutdown();
}

fn stop_background_task(
    core: &mut CoreProcess,
    workspace_id: &str,
    conversation_id: &str,
    run_id: &str,
    async_task_id: &str,
) -> bool {
    let (response, _) = core.send_for_id(
        json!({
            "id": 808,
            "method": "conversation.stopBackgroundTask",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id,
                "runID": run_id,
                "asyncTaskID": async_task_id,
            }
        }),
        json!(808),
    );
    assert_success_response(&response, &json!(808));
    response["result"]["stopped"]
        .as_bool()
        .expect("stopped flag")
}

/// Stop reaches the agent in the session of the run that started the task
/// (bg:4); the task's end arrives as its state, and a task core does not
/// hold live is not sent.
#[test]
fn stop_background_task_stops_a_live_task_in_its_runs_session() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "background stop\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::AsyncTasksFollowUp);
    let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    core.initialize();
    let (_, workspace_id, conversation_id, run_id) = start_codex_run(&mut core, &repo);
    wait_for_completed_run(&mut core, &workspace_id, &conversation_id, &run_id);
    wait_for_live_tasks(&mut core, &workspace_id, &conversation_id, &run_id, 1);

    // Task ids are scoped to the run that recorded them.
    assert!(!stop_background_task(
        &mut core,
        &workspace_id,
        &conversation_id,
        "conversation_run_elsewhere",
        "task_follow_up",
    ));
    assert!(stop_background_task(
        &mut core,
        &workspace_id,
        &conversation_id,
        &run_id,
        "task_follow_up",
    ));
    wait_for_task_state(
        &mut core,
        &workspace_id,
        &conversation_id,
        "toolu_follow_up",
        "stopped",
    );
    wait_for_live_tasks(&mut core, &workspace_id, &conversation_id, &run_id, 0);
    assert!(!stop_background_task(
        &mut core,
        &workspace_id,
        &conversation_id,
        &run_id,
        "task_follow_up",
    ));
    assert_eq!(
        runtime
            .request_log()
            .matches("_session/async_task/stop")
            .count(),
        1
    );
    core.shutdown();
}

/// `(providerToolCallID, subagentThread)` for a control call.
type ThreadRecord = (String, Value);
/// `(providerToolCallID, subagentThreadID)` for a member row.
type ThreadMember = (String, String);

fn subagent_threads(page: &Value) -> (Vec<ThreadRecord>, Vec<ThreadMember>) {
    let items = page["items"].as_array().expect("transcript items");
    let call = |item: &Value| {
        item["tool"]["providerToolCallID"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    let threads = items
        .iter()
        .filter(|item| item["tool"]["subagentThread"].is_object())
        .map(|item| (call(item), item["tool"]["subagentThread"].clone()))
        .collect();
    let members = items
        .iter()
        .filter_map(|item| Some((call(item), item["subagentThreadID"].as_str()?.to_string())))
        .collect();
    (threads, members)
}

fn list_transcript(core: &mut CoreProcess, workspace_id: &str, conversation_id: &str) -> Value {
    let (response, _) = core.send_for_id(
        json!({
            "id": 805,
            "method": "conversation.listTranscript",
            "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
        }),
        json!(805),
    );
    assert_success_response(&response, &json!(805));
    response["result"].clone()
}

/// Claude's recorded subagent turn through the SDK chain: each child joins
/// its control call's thread by the first report's parent stamp, the
/// asynchronous thread runs until the turn ends, and the grouping survives a
/// restart.
#[test]
fn subagent_threads_group_recorded_children_across_the_sdk_chain() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "subagents\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::SubagentThreads);
    let outer = "toolu_01B7bGspZr9HB4ctXRrxemcW";
    let outer_thread = format!("subagent:{outer}");

    let (workspace_id, conversation_id, expected) = {
        let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
        core.initialize();
        let (_, workspace_id, conversation_id, run_id) = start_codex_run(&mut core, &repo);
        // Held: the outer thread has its first child call and is running.
        let deadline = Instant::now() + Duration::from_secs(5);
        let running = loop {
            let page = list_transcript(&mut core, &workspace_id, &conversation_id);
            let (_, members) = subagent_threads(&page);
            if !members.is_empty() {
                break page;
            }
            assert!(Instant::now() < deadline, "no child call joined a thread");
            std::thread::sleep(Duration::from_millis(10));
        };
        let (threads, _) = subagent_threads(&running);
        let state = |threads: &[(String, Value)], call: &str| {
            threads
                .iter()
                .find(|(id, _)| id == call)
                .map(|(_, thread)| thread["state"].clone())
        };
        assert_eq!(state(&threads, outer), Some(json!("running")));
        assert_eq!(
            state(&threads, "toolu_018oDuABh8ft9pADCzi9jv5H"),
            Some(json!("failed"))
        );
        assert_eq!(
            running["subagentThreads"],
            json!([{"runID": run_id, "live": 1}])
        );

        runtime.publish_marker("subagents-complete");
        wait_for_completed_run(&mut core, &workspace_id, &conversation_id, &run_id);
        let finished = list_transcript(&mut core, &workspace_id, &conversation_id);
        let (threads, members) = subagent_threads(&finished);
        assert_eq!(state(&threads, outer), Some(json!("completed")));
        let nested = threads
            .iter()
            .find(|(id, _)| id == "toolu_01SvuHrKZ1tbgaM9bUsFEoMT")
            .map(|(_, thread)| thread.clone())
            .expect("nested thread");
        assert_eq!(nested["parentThreadID"], json!(outer_thread));
        assert_eq!(
            members
                .iter()
                .filter(|(_, thread)| *thread == outer_thread)
                .count(),
            3,
            "{members:?}"
        );
        assert_eq!(
            finished["subagentThreads"],
            json!([{"runID": run_id, "live": 0}])
        );
        core.shutdown();
        (workspace_id, conversation_id, (threads, members))
    };

    let mut restarted = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    restarted.initialize();
    restarted.open_path(repo.path());
    let page = list_transcript(&mut restarted, &workspace_id, &conversation_id);
    assert_eq!(subagent_threads(&page), expected);
    restarted.shutdown();
}
