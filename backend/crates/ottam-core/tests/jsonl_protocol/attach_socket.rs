//! The attach transport: secondary connections over `$OTTAM_HOME/core.sock`
//! share the primary's dispatcher and handlers, get their responses routed
//! back by connection, see broadcast notifications (minus terminal output
//! deltas), and never receive core→client requests. Primary stdin EOF ends
//! the process and closes every attached connection.

use crate::support::{assert_success_response, CoreProcess, GitRepoFixture};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

const SOCKET_FILE_NAME: &str = "core.sock";

struct AttachClient {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl AttachClient {
    /// Connects with a retry window: the socket is bound during server
    /// startup, which races the harness's first request.
    fn connect(home: &Path) -> Self {
        let path = home.join(SOCKET_FILE_NAME);
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match UnixStream::connect(&path) {
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => panic!(
                    "failed to connect to attach socket at {}: {error}",
                    path.display()
                ),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("sets attach read timeout");
        let reader = BufReader::new(stream.try_clone().expect("clones attach stream"));
        Self {
            writer: stream,
            reader,
        }
    }

    fn send(&mut self, value: Value) {
        let mut line = value.to_string();
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .expect("writes to attach socket");
    }

    /// One line, or `None` on EOF (the core shut the connection down).
    fn read(&mut self) -> Option<Value> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(serde_json::from_str(&line).expect("attach line is JSON")),
            Err(error) => panic!("attach socket read failed: {error}"),
        }
    }

    /// Reads until the response with `id` arrives, collecting everything else
    /// (broadcast notifications) seen on the way.
    fn read_response_id(&mut self, id: &Value) -> (Value, Vec<Value>) {
        let mut skipped = Vec::new();
        loop {
            let value = self.read().expect("attach response before EOF");
            if value.get("id") == Some(id) {
                return (value, skipped);
            }
            skipped.push(value);
        }
    }
}

#[test]
fn attached_request_round_trips_with_its_original_id() {
    let mut core = CoreProcess::new();
    core.initialize();
    let mut attach = AttachClient::connect(core.ottam_home_path());

    attach.send(json!({
        "id": "att/1",
        "method": "workspace.list",
        "params": {"includeArchived": false}
    }));
    let (response, _) = attach.read_response_id(&json!("att/1"));
    assert_success_response(&response, &json!("att/1"));
    assert!(response["result"]["workspaces"].is_array());
    core.shutdown();
}

#[test]
fn core_to_client_requests_stay_on_the_primary() {
    use crate::client_roundtrip::{
        asking_conversation, asking_core, skip, wait_for_question,
    };
    let runtime = crate::support::FakeAcpRuntime::new();
    let mut core = asking_core(&runtime);
    let run = asking_conversation(&mut core);
    let mut attach = AttachClient::connect(core.ottam_home_path());

    // The attached client starts the run; the question it leads to must
    // surface on the primary's stdio, and the answer given there must reach
    // the agent.
    attach.send(json!({
        "id": 9,
        "method": "conversation.startRun",
        "params": {
            "workspaceID": run.workspace_id,
            "conversationID": run.conversation_id,
            "engineID": "codex",
            "runInputID": run.input_id
        }
    }));
    let (started, mut seen) = attach.read_response_id(&json!(9));
    assert_success_response(&started, &json!(9));
    let request = wait_for_question(&mut core);
    skip(&mut core, &request);
    let run_id = started["result"]["run"]["runID"]
        .as_str()
        .expect("runID")
        .to_string();
    core.wait_for_run_status(
        &run.workspace_id,
        &run.conversation_id,
        &run_id,
        &["completed"],
    );

    attach.send(json!({"id": 10, "method": "workspace.list", "params": {}}));
    let (_, skipped) = attach.read_response_id(&json!(10));
    seen.extend(skipped);
    assert!(
        !seen
            .iter()
            .any(|value| value["method"] == json!("agent.askQuestion")),
        "attached connection must never see core->client requests: {seen:?}"
    );
    core.shutdown();
}

#[test]
fn deferred_agent_response_returns_on_the_socket() {
    let mut core = CoreProcess::new();
    core.initialize();
    let mut attach = AttachClient::connect(core.ottam_home_path());

    // agent.list answers through the bridge's deferred spawn_response path.
    attach.send(json!({"id": 11, "method": "agent.list", "params": {}}));
    let (response, _) = attach.read_response_id(&json!(11));
    assert_success_response(&response, &json!(11));
    assert!(response["result"]["agents"].is_array());
    core.shutdown();
}

#[test]
fn deferred_git_diff_returns_on_the_socket() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let mut core = CoreProcess::new();
    core.initialize();
    let opened = core.open_path(repo.path());
    let source_id = opened["source"]["projectSourceID"]
        .as_str()
        .expect("projectSourceID");
    let mut attach = AttachClient::connect(core.ottam_home_path());

    // git.diffSnapshot answers through DeferredResponses off the dispatcher.
    attach.send(json!({
        "id": 12,
        "method": "git.diffSnapshot",
        "params": {"projectSourceID": source_id}
    }));
    let (response, _) = attach.read_response_id(&json!(12));
    assert_success_response(&response, &json!(12));
    assert!(response["result"]["files"].is_array());
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn notifications_broadcast_without_terminal_output_deltas() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let mut core = CoreProcess::new();
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let mut attach = AttachClient::connect(core.ottam_home_path());

    let terminal = core.open_terminal(project_id, 24, 80);
    let session_id = terminal["sessionID"].as_str().expect("sessionID");
    core.send_for_id(
        json!({
            "id": 40,
            "method": "terminal.write",
            "params": {
                "sessionID": session_id,
                "dataBase64": base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    "exit\n"
                )
            }
        }),
        json!(40),
    );

    // The exit notification broadcasts to the attached connection; the output
    // deltas ride the primary writer's terminal queue and must never appear.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "terminal.exited never broadcast");
        let value = attach.read().expect("broadcast before EOF");
        assert_ne!(
            value["method"],
            json!("terminal.outputDelta"),
            "terminal output deltas must stay on the primary"
        );
        if value["method"] == json!("terminal.exited")
            && value["params"]["sessionID"] == json!(session_id)
        {
            break;
        }
    }
    core.shutdown();
}

#[test]
fn primary_stdin_close_ends_attached_connections_and_unlinks_the_socket() {
    let mut core = CoreProcess::new();
    core.initialize();
    let home = core.ottam_home_path().to_path_buf();
    let mut attach = AttachClient::connect(&home);

    attach.send(json!({
        "id": 1,
        "method": "workspace.list",
        "params": {"includeArchived": false}
    }));
    let (response, _) = attach.read_response_id(&json!(1));
    assert_success_response(&response, &json!(1));

    core.shutdown();
    assert_eq!(attach.read(), None, "attach socket should EOF on core exit");
    assert!(
        !home.join(SOCKET_FILE_NAME).exists(),
        "core should unlink its attach socket on shutdown"
    );
}

/// SIGTERM with stdin still open takes the same graceful path as stdin EOF:
/// exit 0, attached connections closed, socket unlinked.
#[test]
fn sigterm_runs_graceful_shutdown() {
    let mut core = CoreProcess::new();
    core.initialize();
    let home = core.ottam_home_path().to_path_buf();
    let mut attach = AttachClient::connect(&home);
    attach.send(json!({
        "id": 1,
        "method": "workspace.list",
        "params": {"includeArchived": false}
    }));
    let (response, _) = attach.read_response_id(&json!(1));
    assert_success_response(&response, &json!(1));

    // The client sends SIGKILL 2s after SIGTERM.
    core.send_sigterm();
    let status = core
        .wait_for_exit(Duration::from_secs(2))
        .expect("core exits within 2s of SIGTERM");
    assert!(status.success(), "core exited with {status}");
    assert_eq!(attach.read(), None, "attach socket should EOF on core exit");
    assert!(
        !home.join(SOCKET_FILE_NAME).exists(),
        "core should unlink its attach socket on SIGTERM"
    );
}

/// The client's real quit sequence delivers both: stdin EOF, then SIGTERM
/// while shutdown may already be running. Either order must exit 0.
#[test]
fn sigterm_during_stdin_eof_shutdown_still_exits_cleanly() {
    let mut core = CoreProcess::new();
    core.initialize();
    let home = core.ottam_home_path().to_path_buf();

    core.close_stdin();
    core.send_sigterm();
    let status = core
        .wait_for_exit(Duration::from_secs(2))
        .expect("core exits within 2s of stdin EOF and SIGTERM");
    assert!(status.success(), "core exited with {status}");
    assert!(
        !home.join(SOCKET_FILE_NAME).exists(),
        "core should unlink its attach socket"
    );
}

#[test]
fn attach_gate_disables_the_socket() {
    let mut core =
        CoreProcess::new_with_env(vec![("OTTAM_ATTACH".to_string(), "off".to_string())]);
    core.initialize();
    // Startup is complete (initialize answered); the socket must not exist.
    assert!(!core.ottam_home_path().join(SOCKET_FILE_NAME).exists());
    core.shutdown();
}
