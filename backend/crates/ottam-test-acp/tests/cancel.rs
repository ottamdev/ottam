//! The fake agent's own cancellation behaviour, driven over its wire.
//!
//! Core cannot observe this: it drops the prompt future the moment a run is
//! cancelled, so whatever the agent answers afterwards is never read. That is
//! exactly why the fixture has to be checked here — a test that later depends
//! on the answer would otherwise be quietly lied to.

#![cfg(unix)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Duration;

struct FakeAgent {
    process: Child,
    input: ChildStdin,
    output: BufReader<std::process::ChildStdout>,
}

impl FakeAgent {
    /// The control directory goes to the child, not through this process's own
    /// environment: these tests run as threads in one process, and a global set
    /// would have them racing for each other's prompt plans.
    fn start(control_directory: &Path) -> Self {
        let binary = std::env::var_os("CARGO_BIN_EXE_ottam-test-acp")
            .map(PathBuf::from)
            .expect("cargo builds the fake ACP binary for its own tests");
        let mut process = Command::new(binary)
            .env("OTTAM_TEST_ACP_CONTROL_DIR", control_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("starts the fake ACP agent");
        let input = process.stdin.take().expect("agent stdin");
        let output = BufReader::new(process.stdout.take().expect("agent stdout"));
        Self {
            process,
            input,
            output,
        }
    }

    fn send(&mut self, value: Value) {
        serde_json::to_writer(&mut self.input, &value).expect("writes request");
        self.input.write_all(b"\n").expect("terminates request");
        self.input.flush().expect("flushes request");
    }

    /// The next message that is a response to `id`, skipping notifications.
    fn response(&mut self, id: i64) -> Value {
        loop {
            let mut line = String::new();
            let read = self
                .output
                .read_line(&mut line)
                .expect("reads agent output");
            assert!(
                read > 0,
                "the agent closed its output before answering {id}"
            );
            let value: Value = serde_json::from_str(line.trim()).expect("agent speaks JSON");
            if value["id"] == json!(id) {
                return value;
            }
        }
    }

    fn open_session(&mut self) -> String {
        self.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}));
        let _ = self.response(1);
        self.send(json!({
            "jsonrpc": "2.0", "id": 2, "method": "session/new",
            "params": {"cwd": "/tmp", "mcpServers": []}
        }));
        self.response(2)["result"]["sessionId"]
            .as_str()
            .expect("sessionId")
            .to_string()
    }
}

impl Drop for FakeAgent {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// One chunk, then a long delay. The cancellation lands inside that delay, with
/// no chunk left to notice it on — the window in which the agent used to answer
/// `end_turn` for a turn that had been cancelled.
#[test]
fn a_cancellation_during_the_last_chunk_delay_still_cancels_the_turn() {
    let control = control_directory("last-chunk-delay");
    std::fs::write(
        control.join("next-prompt.json"),
        json!({
            "behavior": "stream", "reply": "one", "chunkBytes": 16,
            "chunkDelayMillis": 2_000
        })
        .to_string(),
    )
    .expect("publishes the prompt plan");

    let mut agent = FakeAgent::start(&control);
    let session_id = agent.open_session();
    agent.send(json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "go"}]}
    }));
    // Past the only chunk, inside the delay that follows it.
    std::thread::sleep(Duration::from_millis(600));
    agent.send(json!({
        "jsonrpc": "2.0", "method": "session/cancel",
        "params": {"sessionId": session_id}
    }));

    let answer = agent.response(3);
    assert_eq!(answer["result"]["stopReason"], "cancelled", "{answer}");
}

/// The other side of the same window: once the turn has answered, a later
/// cancellation must not retroactively turn it into a cancelled one.
#[test]
fn a_cancellation_after_the_turn_has_answered_changes_nothing() {
    let control = control_directory("after-answer");
    std::fs::write(
        control.join("next-prompt.json"),
        json!({
            "behavior": "stream", "reply": "one", "chunkBytes": 16,
            "chunkDelayMillis": 0
        })
        .to_string(),
    )
    .expect("publishes the prompt plan");

    let mut agent = FakeAgent::start(&control);
    let session_id = agent.open_session();
    agent.send(json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "go"}]}
    }));
    let answer = agent.response(3);
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");

    agent.send(json!({
        "jsonrpc": "2.0", "method": "session/cancel",
        "params": {"sessionId": session_id}
    }));
    agent.send(json!({"jsonrpc": "2.0", "id": 4, "method": "session/load", "params": {}}));
    let alive = agent.response(4);
    assert!(alive["result"].is_object(), "{alive}");
}

fn control_directory(name: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("ottam-fake-acp-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("creates a control directory");
    path
}
