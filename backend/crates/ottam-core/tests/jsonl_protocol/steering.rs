use crate::support::{
    assert_error_data_kind, assert_success_response, CoreProcess, FakeAcpRuntime,
    FakePromptBehavior, GitRepoFixture,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

fn wait_for_run_status(
    core: &mut CoreProcess,
    workspace_id: &str,
    conversation_id: &str,
    run_id: &str,
    expected: &[&str],
    timeout: Duration,
) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let (response, _) = core.send_for_id(
            json!({
                "id": 711,
                "method": "conversation.get",
                "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
            }),
            json!(711),
        );
        assert_success_response(&response, &json!(711));
        let detail = response["result"]["conversation"].clone();
        if let Some(run) = detail["latestRunPage"]["runs"]
            .as_array()
            .and_then(|runs| runs.iter().find(|run| run["runID"] == run_id))
        {
            if expected.contains(&run["status"].as_str().unwrap_or_default()) {
                return detail;
            }
        }
        assert!(
            Instant::now() < deadline,
            "run {run_id} did not reach {expected:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn codex_steering_capability(core: &mut CoreProcess, project_id: &str) -> String {
    core.engine_list(project_id)["engines"]
        .as_array()
        .expect("engines")
        .iter()
        .find(|engine| engine["engineID"] == "codex")
        .expect("codex engine")["steeringCapability"]
        .as_str()
        .expect("steeringCapability")
        .to_string()
}

#[cfg(unix)]
#[test]
fn native_steer_delivers_on_the_running_test_engine_and_stop_has_no_followup() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "native steering\n");
    repo.commit_all("initial");
    let mut core = CoreProcess::new_with_env(vec![(
        "OTTAM_TEST_NATIVE_STEER_ENGINE".to_string(),
        "1".to_string(),
    )]);
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let workspace_id = core.workspace_id(project_id);
    let engines = core.engine_list(project_id);
    assert_eq!(
        engines["engines"]
            .as_array()
            .expect("engines")
            .iter()
            .find(|engine| engine["engineID"] == "test.native")
            .expect("test.native engine")["steeringCapability"],
        "native"
    );
    let conversation = core.create_conversation(
        project_id,
        Some("Native steer"),
        Some("Hold the native test turn"),
    );
    let conversation_id = conversation["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID");
    let input_id = conversation["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("initial input");
    let started = core.start_conversation_run(project_id, conversation_id, "test.native", input_id);
    let run_id = started["run"]["runID"].as_str().expect("runID");
    wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        run_id,
        &["running"],
        Duration::from_secs(5),
    );

    let (steered, _) = core.send_for_id(
        json!({
            "id": 712,
            "method": "conversation.steerRun",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id,
                "runID": run_id,
                "message": "Prefer the concise explanation",
                "idempotencyKey": "native-process-steer"
            }
        }),
        json!(712),
    );
    assert_success_response(&steered, &json!(712));
    assert_eq!(steered["result"]["run"]["runID"], run_id);
    assert_eq!(steered["result"]["run"]["status"], "running");
    assert_eq!(steered["result"]["input"]["status"], "delivered");

    let (stopped, _) = core.send_for_id(json!({
        "id": 713,
        "method": "conversation.cancelRun",
        "params": {"workspaceID": workspace_id, "conversationID": conversation_id, "runID": run_id}
    }), json!(713));
    assert_success_response(&stopped, &json!(713));
    let detail = wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        run_id,
        &["canceled"],
        Duration::from_secs(5),
    );
    assert_eq!(
        detail["latestRunPage"]["runs"]
            .as_array()
            .expect("runs")
            .len(),
        1,
        "Stop must not create a replacement run"
    );
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn acp_steer_interrupts_partial_output_and_continues_the_same_session() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "interrupt steering\n");
    repo.commit_all("initial");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::HoldUntilCancel);
    let mut core = CoreProcess::new_with_env(runtime.environment());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let workspace_id = core.workspace_id(project_id);
    let conversation = core.create_conversation(
        project_id,
        Some("Interrupt steer"),
        Some("Begin the initial ACP turn"),
    );
    let conversation_id = conversation["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID");
    let input_id = conversation["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("initial input");
    let started = core.start_conversation_run(project_id, conversation_id, "codex", input_id);
    let old_run_id = started["run"]["runID"].as_str().expect("old runID");
    let running = wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        old_run_id,
        &["running"],
        Duration::from_secs(5),
    );
    let old_session_id = running["latestRunPage"]["runs"]
        .as_array()
        .expect("runs")
        .iter()
        .find(|run| run["runID"] == old_run_id)
        .and_then(|run| run["agentSessionID"].as_str())
        .expect("old sessionID")
        .to_string();

    let (steered, _) = core.send_for_id(
        json!({
            "id": 714,
            "method": "conversation.steerRun",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id,
                "runID": old_run_id,
                "message": "Continue with the revised direction",
                "idempotencyKey": "interrupt-process-steer"
            }
        }),
        json!(714),
    );
    assert_success_response(&steered, &json!(714));
    let followup_id = steered["result"]["run"]["runID"]
        .as_str()
        .expect("follow-up runID");
    assert_ne!(followup_id, old_run_id);
    let detail = wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        followup_id,
        &["completed"],
        Duration::from_secs(5),
    );
    let runs = detail["latestRunPage"]["runs"].as_array().expect("runs");
    let old = runs
        .iter()
        .find(|run| run["runID"] == old_run_id)
        .expect("old run");
    let followup = runs
        .iter()
        .find(|run| run["runID"] == followup_id)
        .expect("follow-up run");
    assert_eq!(old["status"], "interrupted");
    assert_eq!(followup["agentSessionID"], old_session_id);
    let transcript = core.send(
        json!({"id":715,"method":"conversation.listTranscript","params":{
            "workspaceID":workspace_id,"conversationID":conversation_id
        }}),
    );
    assert!(transcript["result"]["items"]
        .as_array()
        .expect("items")
        .iter()
        .any(|item| item["runID"] == old_run_id
            && item["kind"] == "assistantText"
            && item["completion"] == "partial"));
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn steer_after_completion_loses_the_race_without_creating_a_target_run() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "completion race\n");
    repo.commit_all("initial");
    let runtime = FakeAcpRuntime::new();
    let mut core = CoreProcess::new_with_env(runtime.environment());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let workspace_id = core.workspace_id(project_id);
    let conversation = core.create_conversation(
        project_id,
        Some("Completion race"),
        Some("Complete before the steer arrives"),
    );
    let conversation_id = conversation["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID");
    let input_id = conversation["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("initial input");
    let started = core.start_conversation_run(project_id, conversation_id, "codex", input_id);
    let run_id = started["run"]["runID"].as_str().expect("runID");
    wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        run_id,
        &["completed"],
        Duration::from_secs(5),
    );
    let (steer, _) = core.send_for_id(
        json!({
            "id": 715,
            "method": "conversation.steerRun",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id,
                "runID": run_id,
                "message": "Too late",
                "idempotencyKey": "completed-race-steer"
            }
        }),
        json!(715),
    );
    assert_error_data_kind(&steer, "runNoLongerActive");
    let detail = core.get_conversation(project_id, conversation_id);
    assert_eq!(
        detail["latestRunPage"]["runs"]
            .as_array()
            .expect("runs")
            .len(),
        1
    );
    core.shutdown();
}

/// A real agent keeps its session busy until it resolves the cancelled turn, so
/// the follow-up has to wait for that before it prompts. Launching into the gap
/// failed it with `sessionBusy`, which the user saw as a mid-run message marked
/// Failed while the turn it interrupted was still winding down.
///
/// The agent here takes four seconds to stop, long enough that a wait bounded
/// to the cancel's own budget would give up. The steer request must not carry
/// that delay either: it cancels and returns, and the follow-up run does the
/// waiting on its own time.
#[cfg(unix)]
#[test]
fn steer_waits_for_a_slow_cancel_before_prompting_the_followup() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "slow cancel\n");
    repo.commit_all("initial");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::HoldUntilCancel);
    let mut core = CoreProcess::new_with_env(runtime.environment_with_a_slow_cancel(4_000));
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let workspace_id = core.workspace_id(project_id);
    let conversation = core.create_conversation(
        project_id,
        Some("Slow cancel steer"),
        Some("Begin a turn that winds down slowly"),
    );
    let conversation_id = conversation["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID");
    let input_id = conversation["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("initial input");
    let started = core.start_conversation_run(project_id, conversation_id, "codex", input_id);
    let old_run_id = started["run"]["runID"].as_str().expect("old runID");
    wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        old_run_id,
        &["running"],
        Duration::from_secs(5),
    );

    let requested = Instant::now();
    let (steered, _) = core.send_for_id(
        json!({
            "id": 716,
            "method": "conversation.steerRun",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id,
                "runID": old_run_id,
                "message": "Sent while the first turn is still running",
                "idempotencyKey": "slow-cancel-steer"
            }
        }),
        json!(716),
    );
    let steer_latency = requested.elapsed();
    assert_success_response(&steered, &json!(716));
    assert!(
        steer_latency < Duration::from_secs(3),
        "steering must not wait out the agent's wind-down: took {steer_latency:?}"
    );
    let followup_id = steered["result"]["run"]["runID"]
        .as_str()
        .expect("follow-up runID");
    assert_ne!(followup_id, old_run_id);

    let detail = wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        followup_id,
        &["completed", "failed"],
        Duration::from_secs(20),
    );
    let followup = detail["latestRunPage"]["runs"]
        .as_array()
        .expect("runs")
        .iter()
        .find(|run| run["runID"] == followup_id)
        .expect("follow-up run")
        .clone();
    assert_eq!(
        followup["status"], "completed",
        "follow-up must not collide with the cancelled prompt: {followup}"
    );
    let delivered = detail["latestRunInputPage"]["runInputs"]
        .as_array()
        .expect("run inputs")
        .iter()
        .find(|input| input["kind"] == "steer")
        .expect("steer input")
        .clone();
    assert_eq!(delivered["status"], "delivered", "{delivered}");
    core.shutdown();
}

/// Steering a turn that is still producing output.
///
/// The fake used to stream on its reading loop, so it could not see
/// `session/cancel` until the whole reply had gone out — the one shape a
/// mid-run message actually takes was the one the fixture could not express.
///
/// Two things have to hold. The interrupted turn keeps the output it had
/// already produced, and genuinely less than the whole reply. And the follow-up
/// starts answering promptly: it waits for the session to stop prompting, so an
/// agent that ignores the cancellation and streams to the end makes the reader
/// wait out a reply that was already abandoned.
#[cfg(unix)]
#[test]
fn steer_interrupts_a_streaming_turn_and_keeps_the_partial_reply() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "streamed steering\n");
    repo.commit_all("initial");
    let reply = "the streamed sentence continues. ".repeat(80);
    let runtime = FakeAcpRuntime::new();
    let chunk_delay = Duration::from_millis(30);
    runtime.set_next_streamed_prompt(&reply, 16, chunk_delay);
    // Long enough that ignoring the cancellation is unmistakable in the timing
    // rather than a close call.
    let full_stream = chunk_delay * (reply.len() / 16) as u32;
    assert!(full_stream > Duration::from_secs(4), "{full_stream:?}");
    let mut core = CoreProcess::new_with_env(runtime.environment());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let workspace_id = core.workspace_id(project_id);
    let conversation = core.create_conversation(
        project_id,
        Some("Streamed steer"),
        Some("Begin a turn that streams for a while"),
    );
    let conversation_id = conversation["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID");
    let input_id = conversation["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("initial input");
    let started = core.start_conversation_run(project_id, conversation_id, "codex", input_id);
    let old_run_id = started["run"]["runID"].as_str().expect("old runID");
    wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        old_run_id,
        &["running"],
        Duration::from_secs(5),
    );
    // Interrupt part-way through, not before the first chunk: the assertion
    // below is that output survives the interrupt, which says nothing unless
    // some was produced first.
    std::thread::sleep(Duration::from_millis(400));

    let (steered, _) = core.send_for_id(
        json!({
            "id": 717,
            "method": "conversation.steerRun",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id,
                "runID": old_run_id,
                "message": "Sent while the reply is still streaming",
                "idempotencyKey": "streamed-steer"
            }
        }),
        json!(717),
    );
    let steered_at = Instant::now();
    assert_success_response(&steered, &json!(717));
    let followup_id = steered["result"]["run"]["runID"]
        .as_str()
        .expect("follow-up runID");
    assert_ne!(followup_id, old_run_id);

    let detail = wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        followup_id,
        &["completed", "failed"],
        Duration::from_secs(20),
    );
    let settled_after = steered_at.elapsed();
    assert!(
        settled_after < full_stream / 2,
        "the follow-up waited out the abandoned reply: {settled_after:?} of a \
         {full_stream:?} stream"
    );
    let runs = detail["latestRunPage"]["runs"].as_array().expect("runs");
    let old = runs
        .iter()
        .find(|run| run["runID"] == old_run_id)
        .expect("old run");
    assert_eq!(old["status"], "interrupted", "{old}");
    let followup = runs
        .iter()
        .find(|run| run["runID"] == followup_id)
        .expect("follow-up run");
    assert_eq!(followup["status"], "completed", "{followup}");

    let transcript = core.send(
        json!({"id":799,"method":"conversation.listTranscript","params":{
            "workspaceID":workspace_id,"conversationID":conversation_id
        }}),
    );
    assert_success_response(&transcript, &json!(799));
    let partial = transcript["result"]["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["runID"] == old_run_id && item["kind"] == "assistantText")
        .expect("interrupted normalized segment");
    assert_eq!(partial["completion"], "partial", "{partial}");
    let body = partial["bodyPreview"].as_str().expect("partial body");
    assert!(!body.is_empty(), "the interrupted turn kept no output");
    assert!(
        body.len() < reply.len(),
        "the turn was not interrupted: kept all {} bytes",
        reply.len()
    );
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn acp_native_steer_joins_the_running_turn_without_a_followup_run() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "acp native steering\n");
    repo.commit_all("initial");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::HoldUntilCancel);
    let mut core = CoreProcess::new_with_env(runtime.environment_with_native_steering());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let workspace_id = core.workspace_id(project_id);
    let conversation = core.create_conversation(
        project_id,
        Some("ACP native steer"),
        Some("Hold the ACP turn"),
    );
    let conversation_id = conversation["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID");
    let input_id = conversation["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("initial input");
    let started = core.start_conversation_run(project_id, conversation_id, "codex", input_id);
    let run_id = started["run"]["runID"].as_str().expect("runID");
    wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        run_id,
        &["running"],
        Duration::from_secs(5),
    );
    // The capability is only known once the adapter has answered `initialize`,
    // which the run above has now forced.
    assert_eq!(
        core.engine_list(project_id)["engines"]
            .as_array()
            .expect("engines")
            .iter()
            .find(|engine| engine["engineID"] == "codex")
            .expect("codex engine")["steeringCapability"],
        "native"
    );

    let (steered, _) = core.send_for_id(
        json!({
            "id": 731,
            "method": "conversation.steerRun",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id,
                "runID": run_id,
                "message": "Prefer the concise explanation",
                "idempotencyKey": "acp-native-steer"
            }
        }),
        json!(731),
    );
    assert_success_response(&steered, &json!(731));
    assert_eq!(steered["result"]["run"]["runID"], run_id);
    assert_eq!(steered["result"]["input"]["status"], "delivered");
    let request_log = runtime.request_log();
    assert!(
        request_log.contains("_session/steering"),
        "the steer must go out as the extension request: {request_log}"
    );
    assert!(
        request_log.contains("\"idleBehavior\":\"promptRequired\""),
        "the steer must ask for the message back on an idle session: {request_log}"
    );

    let (stopped, _) = core.send_for_id(json!({
        "id": 732,
        "method": "conversation.cancelRun",
        "params": {"workspaceID": workspace_id, "conversationID": conversation_id, "runID": run_id}
    }), json!(732));
    assert_success_response(&stopped, &json!(732));
    let detail = wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        run_id,
        &["canceled"],
        Duration::from_secs(5),
    );
    assert_eq!(
        detail["latestRunPage"]["runs"]
            .as_array()
            .expect("runs")
            .len(),
        1,
        "a delivered native steer must not create a second run"
    );
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn a_steer_the_agent_did_not_deliver_falls_back_to_interrupt_and_continue() {
    // claude-agent-acp's answer when the turn settled before the steer landed:
    // the message is untouched and still the host's to send. codex's
    // `startedNewTurn` looks similar but is not — it keeps the message — and is
    // covered with the other ambiguous outcomes.
    let outcome = "promptRequired";
    let idempotency_key = "acp-steer-prompt-required";
    let repo = GitRepoFixture::init();
    repo.write("README.md", "late steering\n");
    repo.commit_all("initial");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::HoldUntilCancel);
    let mut core =
        CoreProcess::new_with_env(runtime.environment_with_a_forced_steer_outcome(outcome));
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let workspace_id = core.workspace_id(project_id);
    let conversation =
        core.create_conversation(project_id, Some("Late steer"), Some("Hold the ACP turn"));
    let conversation_id = conversation["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID");
    let input_id = conversation["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("initial input");
    let started = core.start_conversation_run(project_id, conversation_id, "codex", input_id);
    let old_run_id = started["run"]["runID"].as_str().expect("runID").to_string();
    let running = wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        &old_run_id,
        &["running"],
        Duration::from_secs(5),
    );
    let old_session_id = running["latestRunPage"]["runs"]
        .as_array()
        .expect("runs")
        .iter()
        .find(|run| run["runID"] == old_run_id.as_str())
        .and_then(|run| run["agentSessionID"].as_str())
        .expect("old sessionID")
        .to_string();

    let (steered, _) = core.send_for_id(
        json!({
            "id": 733,
            "method": "conversation.steerRun",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id,
                "runID": old_run_id,
                "message": "Continue with the revised direction",
                "idempotencyKey": idempotency_key
            }
        }),
        json!(733),
    );
    assert_success_response(&steered, &json!(733));
    let followup_id = steered["result"]["run"]["runID"]
        .as_str()
        .expect("follow-up runID")
        .to_string();
    assert_ne!(followup_id, old_run_id, "{outcome} must fall through");
    let detail = wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        &followup_id,
        &["completed"],
        Duration::from_secs(5),
    );
    let runs = detail["latestRunPage"]["runs"].as_array().expect("runs");
    assert_eq!(
        runs.iter()
            .find(|run| run["runID"] == old_run_id.as_str())
            .expect("old run")["status"],
        "interrupted"
    );
    assert_eq!(
        runs.iter()
            .find(|run| run["runID"] == followup_id.as_str())
            .expect("follow-up run")["agentSessionID"],
        old_session_id
    );
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn an_agent_that_advertises_steering_without_implementing_it_is_demoted() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "lying adapter\n");
    repo.commit_all("initial");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::HoldUntilCancel);
    let mut core = CoreProcess::new_with_env(
        runtime.environment_with_a_forced_steer_outcome("methodNotFound"),
    );
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let workspace_id = core.workspace_id(project_id);
    let conversation =
        core.create_conversation(project_id, Some("Lying adapter"), Some("Hold the ACP turn"));
    let conversation_id = conversation["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID");
    let input_id = conversation["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("initial input");
    let started = core.start_conversation_run(project_id, conversation_id, "codex", input_id);
    let old_run_id = started["run"]["runID"].as_str().expect("runID").to_string();
    wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        &old_run_id,
        &["running"],
        Duration::from_secs(5),
    );
    assert_eq!(
        codex_steering_capability(&mut core, project_id),
        "native",
        "the adapter's advertisement is believed until it is tested"
    );

    // An adapter bug is not the user's message to lose: nothing was delivered,
    // so the same request continues down interrupt-and-continue.
    let (steered, _) = core.send_for_id(
        json!({
            "id": 734,
            "method": "conversation.steerRun",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id,
                "runID": old_run_id,
                "message": "Continue with the revised direction",
                "idempotencyKey": "acp-steer-method-not-found"
            }
        }),
        json!(734),
    );
    assert_success_response(&steered, &json!(734));
    let followup_id = steered["result"]["run"]["runID"]
        .as_str()
        .expect("follow-up runID")
        .to_string();
    assert_ne!(followup_id, old_run_id);
    // The claim has now been tested and found false, so it stops being made.
    assert_eq!(
        codex_steering_capability(&mut core, project_id),
        "interruptThenPrompt"
    );

    let detail = wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        &followup_id,
        &["completed"],
        Duration::from_secs(5),
    );
    assert_eq!(
        detail["latestRunPage"]["runs"]
            .as_array()
            .expect("runs")
            .iter()
            .find(|run| run["runID"] == old_run_id.as_str())
            .expect("old run")["status"],
        "interrupted"
    );
    // One wasted round trip, not one per steer.
    assert_eq!(
        runtime.request_log().matches("_session/steering").count(),
        1
    );
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn a_steer_the_agent_could_not_account_for_is_left_unresolved() {
    // Two answers Core must not resolve on the agent's behalf. `failed` is
    // codex's catch-all around a throw part-way through delivering.
    // `startedNewTurn` is worse: the message is in a turn nobody is waiting on,
    // and the cancel aimed at that turn is an unacknowledged notification
    // racing whatever it has already run. Re-sending either could run the same
    // instruction twice.
    for (outcome, idempotency_key) in [
        ("failed", "acp-steer-failed"),
        ("startedNewTurn", "acp-steer-started-new-turn"),
    ] {
        let repo = GitRepoFixture::init();
        repo.write("README.md", "ambiguous steering\n");
        repo.commit_all("initial");
        let runtime = FakeAcpRuntime::new();
        runtime.set_next_prompt(FakePromptBehavior::HoldUntilCancel);
        let mut core =
            CoreProcess::new_with_env(runtime.environment_with_a_forced_steer_outcome(outcome));
        core.initialize();
        let opened = core.open_path(repo.path());
        let project_id = opened["project"]["projectID"].as_str().expect("projectID");
        let workspace_id = core.workspace_id(project_id);
        let conversation = core.create_conversation(
            project_id,
            Some("Ambiguous steer"),
            Some("Hold the ACP turn"),
        );
        let conversation_id = conversation["conversation"]["conversationID"]
            .as_str()
            .expect("conversationID");
        let input_id = conversation["latestRunInputPage"]["runInputs"][0]["runInputID"]
            .as_str()
            .expect("initial input");
        let started = core.start_conversation_run(project_id, conversation_id, "codex", input_id);
        let run_id = started["run"]["runID"].as_str().expect("runID").to_string();
        wait_for_run_status(
            &mut core,
            &workspace_id,
            conversation_id,
            &run_id,
            &["running"],
            Duration::from_secs(5),
        );

        let (steered, _) = core.send_for_id(
            json!({
                "id": 735,
                "method": "conversation.steerRun",
                "params": {
                    "workspaceID": workspace_id,
                    "conversationID": conversation_id,
                    "runID": run_id,
                    "message": "Continue with the revised direction",
                    "idempotencyKey": idempotency_key
                }
            }),
            json!(735),
        );
        assert_error_data_kind(&steered, "promptExecution");
        // `startedNewTurn` cancels the detached turn on its way out, which under
        // the fake ends the held one too, so the run's own status is not the
        // invariant here. What must hold for both is that nothing replaced it.
        let detail = wait_for_run_status(
            &mut core,
            &workspace_id,
            conversation_id,
            &run_id,
            &["running", "canceled", "completed", "interrupted"],
            Duration::from_secs(5),
        );
        assert_eq!(
            detail["latestRunPage"]["runs"]
                .as_array()
                .expect("runs")
                .len(),
            1,
            "an unresolved steer must not create a replacement run ({outcome})"
        );
        assert_eq!(
            detail["latestRunInputPage"]["runInputs"]
                .as_array()
                .expect("run inputs")
                .iter()
                .find(|input| input["idempotencyKey"] == idempotency_key)
                .expect("steer input")["status"],
            "deliveryUnknown",
            "never failed or delivered by assumption ({outcome})"
        );
        core.shutdown();
    }
}
