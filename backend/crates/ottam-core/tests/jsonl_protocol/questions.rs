use crate::support::{
    assert_success_response, CoreProcess, FakeAcpRuntime, FakePromptBehavior, GitRepoFixture,
};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct Run {
    workspace_id: String,
    conversation_id: String,
    run_id: String,
}

/// A core whose client answers `agent.askQuestion`, with a Codex run started
/// on the fake adapter's `behavior`.
fn start(behavior: FakePromptBehavior) -> (FakeAcpRuntime, GitRepoFixture, CoreProcess, Run) {
    start_in(None, behavior)
}

fn start_in(
    home: Option<&Path>,
    behavior: FakePromptBehavior,
) -> (FakeAcpRuntime, GitRepoFixture, CoreProcess, Run) {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "questions\n");
    repo.commit_all("initial");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(behavior);
    let mut core = match home {
        Some(home) => CoreProcess::new_in_home_with_env(home, runtime.environment()),
        None => CoreProcess::new_with_env(runtime.environment()),
    };
    initialize(&mut core);
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"]
        .as_str()
        .expect("projectID")
        .to_string();
    let workspace_id = core.workspace_id(&project_id);
    let created = core.create_conversation(&project_id, Some("Questions"), Some("Ask me"));
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
    (
        runtime,
        repo,
        core,
        Run {
            workspace_id,
            conversation_id,
            run_id,
        },
    )
}

/// Initializes as a client that answers `agent.askQuestion`.
fn initialize(core: &mut CoreProcess) {
    let response = core.send(json!({
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": {"name": "test", "version": "0.0.0"},
            "capabilities": {
                "experimentalMethods": ["agent.requestPermission", "agent.askQuestion"]
            }
        }
    }));
    assert_success_response(&response, &json!(1));
}

/// The first message read whose `method` is `method` and which `matches`.
fn wait_for(core: &mut CoreProcess, method: &str, matches: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "no {method} reached the client");
        let Some(message) = core.read_json_timeout(remaining) else {
            continue;
        };
        if message["method"] == method && matches(&message) {
            return message;
        }
    }
}

fn wait_for_question(core: &mut CoreProcess) -> Value {
    wait_for(core, "agent.askQuestion", |_| true)
}

fn wait_for_status(core: &mut CoreProcess, run: &Run, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (response, _) = core.send_for_id(
            json!({
                "id": 901,
                "method": "conversation.get",
                "params": {"workspaceID": run.workspace_id, "conversationID": run.conversation_id}
            }),
            json!(901),
        );
        assert_success_response(&response, &json!(901));
        let status = response["result"]["conversation"]["latestRunPage"]["runs"]
            .as_array()
            .and_then(|runs| runs.iter().find(|entry| entry["runID"] == run.run_id))
            .map(|entry| entry["status"].clone());
        if status == Some(json!(expected)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "run {} never became {expected}; last status {status:?}",
            run.run_id
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The question record on the asking tool, and the assistant's text.
fn transcript(core: &mut CoreProcess, run: &Run) -> (Value, String) {
    let (response, _) = core.send_for_id(
        json!({
            "id": 902,
            "method": "conversation.listTranscript",
            "params": {"workspaceID": run.workspace_id, "conversationID": run.conversation_id}
        }),
        json!(902),
    );
    assert_success_response(&response, &json!(902));
    let items = response["result"]["items"]
        .as_array()
        .expect("transcript items");
    let question = items
        .iter()
        .find(|item| item["tool"]["providerToolCallID"] == "toolu_question")
        .map(|item| item["tool"]["question"].clone())
        .expect("the asking tool");
    let text = items
        .iter()
        .filter(|item| item["kind"] == "assistantText")
        .filter_map(|item| item["bodyPreview"].as_str())
        .collect::<String>();
    (question, text)
}

#[test]
fn an_answered_question_reaches_the_agent_and_its_tool() {
    let (_runtime, _repo, mut core, run) = start(FakePromptBehavior::AskQuestion);
    let request = wait_for_question(&mut core);
    let params = &request["params"];
    assert_eq!(params["runID"], json!(run.run_id));
    assert_eq!(params["providerToolCallID"], "toolu_question");
    let question = &params["questions"][0];
    assert_eq!(question["prompt"], "Which approach should I take?");
    assert_eq!(question["kind"]["type"], "singleSelect");
    assert_eq!(question["kind"]["allowsOther"], true);
    assert_eq!(
        question["kind"]["options"][0]["description"],
        "Smaller diff."
    );
    let question_id = question["id"].clone();

    core.write_message(json!({
        "id": request["id"],
        "result": {
            "outcome": "submitted",
            "answers": [{"questionID": question_id, "values": ["Refactor"], "other": "keep it small"}]
        }
    }));
    wait_for_status(&mut core, &run, "completed");
    let (record, text) = transcript(&mut core, &run);
    assert!(
        text.contains(r#""question_0":"Refactor""#)
            && text.contains(r#""question_0_custom":"keep it small""#),
        "the agent never saw the answer: {text:?}"
    );
    assert_eq!(record["state"], "answered");
    assert_eq!(
        record["answers"][0],
        json!({"prompt": "Which approach should I take?", "values": ["Refactor", "keep it small"], "secure": false})
    );
    core.shutdown();
}

#[test]
fn a_skipped_question_declines_and_records_skipped() {
    let (_runtime, _repo, mut core, run) = start(FakePromptBehavior::AskQuestion);
    let request = wait_for_question(&mut core);
    core.write_message(json!({"id": request["id"], "result": {"outcome": "skipped"}}));
    wait_for_status(&mut core, &run, "completed");
    let (record, text) = transcript(&mut core, &run);
    assert!(
        text.contains(r#""action":"decline""#),
        "Codex skips by declining: {text:?}"
    );
    assert_eq!(record["state"], "skipped");
    core.shutdown();
}

/// Stop withdraws the question from the client and leaves it not answered.
#[test]
fn stopping_the_run_withdraws_its_question() {
    let (_runtime, _repo, mut core, run) = start(FakePromptBehavior::AskQuestion);
    let request = wait_for_question(&mut core);
    let (stopped, others) = core.send_for_id(
        json!({
            "id": 903,
            "method": "conversation.cancelRun",
            "params": {
                "workspaceID": run.workspace_id,
                "conversationID": run.conversation_id,
                "runID": run.run_id
            }
        }),
        json!(903),
    );
    assert_success_response(&stopped, &json!(903));
    let withdrawn = |message: &Value| {
        message["method"] == "$/cancelRequest" && message["params"]["id"] == request["id"]
    };
    if !others.iter().any(withdrawn) {
        wait_for(&mut core, "$/cancelRequest", withdrawn);
    }
    wait_for_status(&mut core, &run, "canceled");
    let (record, _) = transcript(&mut core, &run);
    assert_eq!(record["state"], "notAnswered");
    core.shutdown();
}

/// The adapter withdrawing its form (`$/cancel_request`) withdraws it from
/// the client too, and the adapter hears the cancellation.
#[test]
fn an_adapter_withdrawal_reaches_the_client() {
    let (runtime, _repo, mut core, run) = start(FakePromptBehavior::AskQuestionWithdrawn);
    let request = wait_for_question(&mut core);
    runtime.publish_marker("withdraw-question");
    wait_for(&mut core, "$/cancelRequest", |message| {
        message["params"]["id"] == request["id"]
    });
    wait_for_status(&mut core, &run, "completed");
    let (record, text) = transcript(&mut core, &run);
    assert!(
        text.contains("-32800"),
        "the adapter never heard its request cancelled: {text:?}"
    );
    assert_eq!(record["state"], "notAnswered");
    core.shutdown();
}

/// Quitting with a question pending leaves it not answered, and a relaunch
/// shows it that way.
#[test]
fn a_question_pending_at_quit_is_not_answered_after_relaunch() {
    let home = TempDir::new().expect("OTTAM_HOME");
    let (runtime, _repo, mut core, run) =
        start_in(Some(home.path()), FakePromptBehavior::AskQuestion);
    let request = wait_for_question(&mut core);
    assert_eq!(request["params"]["runID"], json!(run.run_id));
    core.shutdown();

    let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    initialize(&mut core);
    let (record, _) = transcript(&mut core, &run);
    assert_eq!(record["state"], "notAnswered");
    core.shutdown();
}
