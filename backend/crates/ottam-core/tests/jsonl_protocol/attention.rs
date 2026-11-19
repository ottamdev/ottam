//! Attention over the wire: core decides which conversations want the user
//! and when a client alerts. A run that ends while nobody focuses its
//! conversation is unseen until focused; an ask waiting on the client needs
//! input until answered; stopped runs and quitting leave nothing.

use crate::support::{
    assert_success_response, CoreProcess, FakeAcpRuntime, FakePromptBehavior, GitRepoFixture,
};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

/// Core's settle delay plus slack.
const SETTLED: Duration = Duration::from_secs(4);

struct Session {
    core: CoreProcess,
    _runtime: FakeAcpRuntime,
    _repo: GitRepoFixture,
    project_id: String,
    workspace_id: String,
    /// Every `attention.changed` read so far, in order.
    changes: Vec<Value>,
    next_id: u64,
}

impl Session {
    fn start(runtime: FakeAcpRuntime) -> Self {
        Self::start_in(None, runtime)
    }

    fn start_in(home: Option<&Path>, runtime: FakeAcpRuntime) -> Self {
        let repo = GitRepoFixture::init();
        repo.write("README.md", "attention\n");
        repo.commit_all("initial");
        let mut core = match home {
            Some(home) => CoreProcess::new_in_home_with_env(home, runtime.environment()),
            None => CoreProcess::new_with_env(runtime.environment()),
        };
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
        let opened = core.open_path(repo.path());
        let project_id = opened["project"]["projectID"]
            .as_str()
            .expect("projectID")
            .to_string();
        let workspace_id = core.workspace_id(&project_id);
        Self {
            core,
            _runtime: runtime,
            _repo: repo,
            project_id,
            workspace_id,
            changes: Vec::new(),
            next_id: 1000,
        }
    }

    fn keep(&mut self, messages: impl IntoIterator<Item = Value>) -> Vec<Value> {
        let mut others = Vec::new();
        for message in messages {
            if message["method"] == "attention.changed" {
                self.changes.push(message["params"].clone());
            } else {
                others.push(message);
            }
        }
        others
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = json!(self.next_id);
        let (response, others) = self
            .core
            .send_for_id(json!({"id": id, "method": method, "params": params}), id.clone());
        self.keep(others);
        assert_success_response(&response, &id);
        response["result"].clone()
    }

    /// A conversation with its first input, not started.
    fn conversation(&mut self, title: &str) -> (String, String) {
        let created = self
            .core
            .create_conversation(&self.project_id, Some(title), Some("Go"));
        (
            created["conversation"]["conversationID"]
                .as_str()
                .expect("conversationID")
                .to_string(),
            created["latestRunInputPage"]["runInputs"][0]["runInputID"]
                .as_str()
                .expect("inputID")
                .to_string(),
        )
    }

    fn start_run(&mut self, conversation: &str, input: &str) -> String {
        let started = self.request(
            "conversation.startRun",
            json!({
                "workspaceID": self.workspace_id,
                "conversationID": conversation,
                "engineID": "codex",
                "runInputID": input
            }),
        );
        started["run"]["runID"].as_str().expect("runID").to_string()
    }

    fn focus(&mut self, conversation: Option<&str>) {
        let params = match conversation {
            Some(conversation) => json!({"conversationID": conversation}),
            None => json!({}),
        };
        self.request("attention.setFocus", params);
    }

    fn list(&mut self) -> Value {
        self.request("attention.list", json!({}))
    }

    fn run_status(&mut self, conversation: &str, run: &str) -> String {
        let detail = self.request(
            "conversation.get",
            json!({"workspaceID": self.workspace_id, "conversationID": conversation}),
        );
        detail["conversation"]["latestRunPage"]["runs"]
            .as_array()
            .and_then(|runs| runs.iter().find(|entry| entry["runID"] == run))
            .and_then(|entry| entry["status"].as_str())
            .unwrap_or_default()
            .to_string()
    }

    fn wait_for_status(&mut self, conversation: &str, run: &str, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = self.run_status(conversation, run);
            if status == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "run {run} never became {expected}; last {status}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Reads for `duration`, returning everything but attention changes.
    fn read_for(&mut self, duration: Duration) -> Vec<Value> {
        let deadline = Instant::now() + duration;
        let mut others = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return others;
            }
            if let Some(message) = self.core.read_json_timeout(remaining) {
                others.extend(self.keep([message]));
            }
        }
    }

    /// The first change for `conversation` in `state` after `skip` changes,
    /// read for up to `timeout`.
    fn wait_for_change(&mut self, conversation: &str, state: &str, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(change) = self.changes.iter().find(|change| {
                change["entry"]["conversationID"] == conversation
                    && change["entry"]["state"] == state
            }) {
                return change.clone();
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "no {state} change for {conversation}; saw {:?}",
                self.changes
            );
            if let Some(message) = self.core.read_json_timeout(remaining) {
                self.keep([message]);
            }
        }
    }

    /// The method's first core→client request, read for up to 10 s.
    fn wait_for_request(&mut self, method: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "no {method} reached the client");
            if let Some(message) = self.core.read_json_timeout(remaining) {
                if message["method"] == method {
                    return message;
                }
                self.keep([message]);
            }
        }
    }

    fn states_for(&self, conversation: &str) -> Vec<(String, bool)> {
        self.changes
            .iter()
            .filter(|change| change["entry"]["conversationID"] == conversation)
            .map(|change| {
                (
                    change["entry"]["state"].as_str().unwrap_or_default().to_string(),
                    change["notify"].as_bool().unwrap_or_default(),
                )
            })
            .collect()
    }
}

fn runtime(behavior: FakePromptBehavior) -> FakeAcpRuntime {
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(behavior);
    runtime
}

#[test]
fn an_unfocused_completion_is_unseen_until_focused() {
    let mut session = Session::start(runtime(FakePromptBehavior::Complete));
    let (conversation, input) = session.conversation("Unfocused");
    let run = session.start_run(&conversation, &input);

    let unseen = session.wait_for_change(&conversation, "unseen", Duration::from_secs(10));
    assert_eq!(unseen["notify"], true);
    assert_eq!(unseen["entry"]["reason"], "completed");
    assert_eq!(unseen["entry"]["runID"], run);
    assert_eq!(unseen["entry"]["title"], "Unfocused");
    assert_eq!(unseen["entry"]["workspaceID"], session.workspace_id);
    let listed = session.list();
    assert_eq!(listed["entries"].as_array().unwrap().len(), 1);
    assert!(listed["revision"].as_u64().unwrap() >= unseen["revision"].as_u64().unwrap());

    session.focus(Some(&conversation));
    let cleared = session.wait_for_change(&conversation, "none", Duration::from_secs(2));
    assert_eq!(cleared["notify"], false);
    assert!(cleared["revision"].as_u64() > unseen["revision"].as_u64());
    assert!(session.list()["entries"].as_array().unwrap().is_empty());
    session.core.shutdown();
}

#[test]
fn a_focused_completion_stays_seen() {
    let mut session = Session::start(runtime(FakePromptBehavior::Complete));
    let (conversation, input) = session.conversation("Focused");
    session.focus(Some(&conversation));
    let run = session.start_run(&conversation, &input);
    session.wait_for_status(&conversation, &run, "completed");
    session.read_for(SETTLED);
    assert!(session.states_for(&conversation).is_empty());
    assert!(session.list()["entries"].as_array().unwrap().is_empty());
    session.core.shutdown();
}

#[test]
fn a_permission_needs_input_until_answered_and_a_stop_leaves_nothing() {
    let mut session = Session::start(runtime(FakePromptBehavior::RequestPermission));
    let (conversation, input) = session.conversation("Permission");
    let run = session.start_run(&conversation, &input);
    let request = session.wait_for_request("agent.requestPermission");
    let asked = session.wait_for_change(&conversation, "needsInput", Duration::from_secs(2));
    assert_eq!(asked["entry"]["reason"], "permission");
    assert_eq!(asked["entry"]["runID"], run);
    assert!(asked["entry"]["detail"]
        .as_str()
        .is_some_and(|detail| detail.starts_with("rm -rf")));
    // Nobody focuses it, so it alerts once the ask has waited a moment.
    session.read_for(Duration::from_millis(800));
    assert_eq!(
        session.states_for(&conversation),
        [("needsInput".to_string(), false), ("needsInput".to_string(), true)]
    );

    let option = request["params"]["options"]
        .as_array()
        .and_then(|options| options.iter().find(|option| option["kind"] == "allowOnce"))
        .and_then(|option| option["optionID"].as_str())
        .expect("allow once")
        .to_string();
    session.core.write_message(json!({
        "id": request["id"],
        "result": {"outcome": "selected", "optionID": option}
    }));
    session.wait_for_change(&conversation, "none", Duration::from_secs(5));

    // The turn holds until stopped; a stopped run is never unseen.
    session.request(
        "conversation.cancelRun",
        json!({"workspaceID": session.workspace_id, "conversationID": conversation, "runID": run}),
    );
    session.wait_for_status(&conversation, &run, "canceled");
    session.read_for(SETTLED);
    assert_eq!(session.states_for(&conversation).len(), 3);
    assert!(session.list()["entries"].as_array().unwrap().is_empty());
    session.core.shutdown();
}

#[test]
fn a_question_needs_input_then_the_answered_turn_is_unseen() {
    let mut session = Session::start(runtime(FakePromptBehavior::AskQuestion));
    let (conversation, input) = session.conversation("Question");
    session.start_run(&conversation, &input);
    let request = session.wait_for_request("agent.askQuestion");
    let asked = session.wait_for_change(&conversation, "needsInput", Duration::from_secs(2));
    assert_eq!(asked["entry"]["reason"], "question");
    session
        .core
        .write_message(json!({"id": request["id"], "result": {"outcome": "skipped"}}));
    session.wait_for_change(&conversation, "unseen", Duration::from_secs(10));
    let states: Vec<String> = session
        .states_for(&conversation)
        .into_iter()
        .map(|(state, _)| state)
        .collect();
    let first_none = states.iter().position(|state| state == "none").unwrap();
    let first_unseen = states.iter().position(|state| state == "unseen").unwrap();
    assert!(first_none < first_unseen, "{states:?}");
    session.core.shutdown();
}

#[test]
fn a_failed_run_is_unseen_as_failed() {
    let mut session = Session::start(runtime(FakePromptBehavior::SessionFailure));
    let (conversation, input) = session.conversation("Failing");
    let run = session.start_run(&conversation, &input);
    let unseen = session.wait_for_change(&conversation, "unseen", Duration::from_secs(10));
    assert_eq!(session.run_status(&conversation, &run), "failed");
    assert_eq!(unseen["entry"]["reason"], "failed");
    assert_eq!(unseen["notify"], true);
    session.core.shutdown();
}

#[test]
fn a_steered_turn_is_unseen_once() {
    let mut session = Session::start(runtime(FakePromptBehavior::HoldUntilCancel));
    let (conversation, input) = session.conversation("Steered");
    let first = session.start_run(&conversation, &input);
    session.wait_for_status(&conversation, &first, "running");
    let steered = session.request(
        "conversation.steerRun",
        json!({
            "workspaceID": session.workspace_id,
            "conversationID": conversation,
            "runID": first,
            "message": "Change course",
            "idempotencyKey": "attention-steer"
        }),
    );
    let follow_up = steered["run"]["runID"].as_str().expect("runID").to_string();
    session.wait_for_status(&conversation, &follow_up, "completed");
    session.read_for(SETTLED);
    let unseen: Vec<&Value> = session
        .changes
        .iter()
        .filter(|change| change["entry"]["state"] == "unseen")
        .collect();
    assert_eq!(unseen.len(), 1, "{:?}", session.changes);
    assert_eq!(unseen[0]["entry"]["runID"], follow_up);
    assert_eq!(unseen[0]["notify"], true);
    session.core.shutdown();
}

#[test]
fn quitting_mid_run_leaves_nothing_unseen() {
    let home = tempfile::TempDir::new().expect("home");
    let mut session = Session::start_in(
        Some(home.path()),
        runtime(FakePromptBehavior::HoldUntilCancel),
    );
    let (conversation, input) = session.conversation("Quitting");
    let run = session.start_run(&conversation, &input);
    session.wait_for_status(&conversation, &run, "running");
    session.core.shutdown();
    assert!(session
        .changes
        .iter()
        .all(|change| change["entry"]["state"] != "unseen"));

    let mut restarted = Session::start_in(Some(home.path()), FakeAcpRuntime::new());
    assert!(restarted.list()["entries"].as_array().unwrap().is_empty());
    restarted.core.shutdown();
}

#[test]
fn an_unseen_conversation_survives_a_restart_until_focused() {
    let home = tempfile::TempDir::new().expect("home");
    let mut session = Session::start_in(Some(home.path()), runtime(FakePromptBehavior::Complete));
    let (conversation, input) = session.conversation("Restarted");
    let run = session.start_run(&conversation, &input);
    session.wait_for_change(&conversation, "unseen", Duration::from_secs(10));
    session.core.shutdown();

    let mut restarted = Session::start_in(Some(home.path()), FakeAcpRuntime::new());
    let listed = restarted.list();
    let entries = listed["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["conversationID"], conversation);
    assert_eq!(entries[0]["state"], "unseen");
    assert_eq!(entries[0]["reason"], "completed");
    assert_eq!(entries[0]["runID"], run);
    assert_eq!(entries[0]["title"], "Restarted");
    restarted.focus(Some(&conversation));
    let cleared = restarted.wait_for_change(&conversation, "none", Duration::from_secs(2));
    assert_eq!(cleared["notify"], false);
    restarted.core.shutdown();

    let mut again = Session::start_in(Some(home.path()), FakeAcpRuntime::new());
    assert!(again.list()["entries"].as_array().unwrap().is_empty());
    again.core.shutdown();
}
