//! Core→client requests over the client link, end to end. `agent.askQuestion`
//! is the vehicle: the fake adapter's turn holds until this client answers, so
//! the request stays pending for as long as a test needs. How the reader
//! routes answers past a full dispatch queue and releases waiters on EOF is
//! pinned by the unit tests in `server/transport.rs`.

use crate::support::{
    assert_success_response, CoreProcess, FakeAcpRuntime, FakePromptBehavior, GitRepoFixture,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub(crate) struct AskingRun {
    pub(crate) project_id: String,
    pub(crate) workspace_id: String,
    pub(crate) conversation_id: String,
    pub(crate) input_id: String,
    _repo: GitRepoFixture,
}

/// A core on a fake adapter whose next turn asks a question, initialized as a
/// client that answers `agent.askQuestion`.
pub(crate) fn asking_core(runtime: &FakeAcpRuntime) -> CoreProcess {
    runtime.set_next_prompt(FakePromptBehavior::AskQuestion);
    let mut core = CoreProcess::new_with_env(runtime.environment());
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
    core
}

/// A conversation with one message, ready for a Codex run.
pub(crate) fn asking_conversation(core: &mut CoreProcess) -> AskingRun {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "client link\n");
    repo.commit_all("initial");
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"]
        .as_str()
        .expect("projectID")
        .to_string();
    let workspace_id = core.workspace_id(&project_id);
    let created = core.create_conversation(&project_id, Some("Client link"), Some("Ask me"));
    let conversation_id = created["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID")
        .to_string();
    let input_id = created["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("inputID")
        .to_string();
    AskingRun {
        project_id,
        workspace_id,
        conversation_id,
        input_id,
        _repo: repo,
    }
}

pub(crate) fn wait_for_question(core: &mut CoreProcess) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "no agent.askQuestion reached the client"
        );
        if let Some(message) = core.read_json_timeout(remaining) {
            if message["method"] == "agent.askQuestion" {
                return message;
            }
        }
    }
}

pub(crate) fn skip(core: &mut CoreProcess, request: &Value) {
    core.write_message(json!({"id": request["id"], "result": {"outcome": "skipped"}}));
}

#[test]
fn unmatched_client_response_is_ignored_without_corrupting_dispatch() {
    let mut core = CoreProcess::new();
    core.write_message(json!({
        "id": "core/req/999",
        "result": {"unexpected": true}
    }));
    core.expect_no_response(Duration::from_millis(100));

    let initialize = core.initialize();
    assert_success_response(&initialize, &json!(1));
    core.shutdown();
}

/// End to end only: core exits with a question open. The reader closing its
/// pending requests on EOF is pinned by the transport unit tests, since here
/// shutdown also cancels the run that is waiting.
#[test]
fn stdin_close_with_a_question_open_exits_cleanly() {
    let runtime = FakeAcpRuntime::new();
    let mut core = asking_core(&runtime);
    let run = asking_conversation(&mut core);
    core.start_conversation_run(
        &run.project_id,
        &run.conversation_id,
        "codex",
        &run.input_id,
    );
    wait_for_question(&mut core);

    // `shutdown` closes stdin without answering.
    core.shutdown();
}

#[test]
fn stdin_close_while_the_adapter_starts_exits_cleanly() {
    let runtime = FakeAcpRuntime::new();
    let mut core = asking_core(&runtime);
    let run = asking_conversation(&mut core);
    core.start_conversation_run(
        &run.project_id,
        &run.conversation_id,
        "codex",
        &run.input_id,
    );

    // EOF lands while the run is still starting its adapter, before any
    // question exists.
    core.shutdown();
}
