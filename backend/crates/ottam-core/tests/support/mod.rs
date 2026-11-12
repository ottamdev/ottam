#![allow(dead_code)]

use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);
/// A fresh core's first answer waits on `Database::open`, about 30 fsyncs in
/// a new `OTTAM_HOME`. On ext4 each one joins a journal commit that must also
/// flush whatever else the disk owes, such as the binary cargo just linked, so
/// a healthy start can take seconds. Neither core nor the app bounds this wait.
const STARTUP_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(unix)]
#[must_use = "keep the guard alive until the held fixture may proceed"]
pub struct FakeRuntimeReleaseGuard {
    marker_path: PathBuf,
    released: bool,
}

#[cfg(unix)]
impl FakeRuntimeReleaseGuard {
    fn new(marker_path: PathBuf) -> Self {
        Self {
            marker_path,
            released: false,
        }
    }

    pub fn release(mut self) {
        self.publish().unwrap_or_else(|error| {
            panic!(
                "publishes fake runtime marker {}: {error}",
                self.marker_path.display()
            )
        });
        self.released = true;
    }

    fn publish(&self) -> std::io::Result<()> {
        fs::write(&self.marker_path, b"released\n")
    }
}

#[cfg(unix)]
impl Drop for FakeRuntimeReleaseGuard {
    fn drop(&mut self) {
        if !self.released {
            // Never panic while unwinding a failed test. Publishing the marker
            // best-effort lets a held descendant exit before Core is reaped.
            let _ = self.publish();
        }
    }
}

/// The fake ACP agent every adapter fixture runs.
#[cfg(unix)]
pub fn fake_acp_binary() -> PathBuf {
    let binary = std::env::var_os("OTTAM_TEST_ACP_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ottam-test-acp")
        });
    assert!(
        binary.is_file(),
        "ottam-test-acp is missing at {}; build it with `cargo build -p ottam-test-acp` or set OTTAM_TEST_ACP_BINARY",
        binary.display()
    );
    binary
}

#[cfg(unix)]
pub struct FakeAcpRuntime {
    directory: TempDir,
    request_log: PathBuf,
    control_directory: PathBuf,
}

#[cfg(unix)]
pub enum FakePromptBehavior {
    Complete,
    HoldUntilCancel,
    NeverAcknowledge,
    ExitAfterUpdate,
    /// An expired login: rejected before the turn begins, so the input fails
    /// rather than becoming ambiguous.
    AuthRequired,
    /// Background-task frames from both adapters inside one completed turn.
    AsyncTasks,
    /// A Codex tool call in the turn whose stamp and spawn follow `end_turn`
    /// on [`FakeAcpRuntime::publish_marker`]`("after-turn-spawn")`, and its
    /// `stopped` state on `"after-turn-stop"`.
    AsyncTasksAfterTurn,
    /// A background command spawned in the turn; on
    /// [`FakeAcpRuntime::publish_marker`]`("follow-up-complete")` it completes
    /// and the agent answers on its own, as Claude does.
    AsyncTasksFollowUp,
    /// Traffic core can't use, ending with an unadvertised session-scoped
    /// request; the turn ends once it is answered.
    UnhandledTraffic,
    /// Claude's recorded subagent turn, held with the outer thread running
    /// until [`FakeAcpRuntime::publish_marker`]`("subagents-complete")`.
    SubagentThreads,
    /// Claude's `AskUserQuestion` form on a tool call; the turn ends once the
    /// client answers, quoting the answer.
    AskQuestion,
    /// [`Self::AskQuestion`], withdrawn by the adapter on
    /// [`FakeAcpRuntime::publish_marker`]`("withdraw-question")`.
    AskQuestionWithdrawn,
}

#[cfg(unix)]
impl FakePromptBehavior {
    fn wire_value(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::HoldUntilCancel => "holdUntilCancel",
            Self::NeverAcknowledge => "neverAcknowledge",
            Self::ExitAfterUpdate => "exitAfterUpdate",
            Self::AuthRequired => "authRequired",
            Self::AsyncTasks => "asyncTasks",
            Self::AsyncTasksAfterTurn => "asyncTasksAfterTurn",
            Self::AsyncTasksFollowUp => "asyncTasksFollowUp",
            Self::UnhandledTraffic => "unhandledTraffic",
            Self::SubagentThreads => "subagentThreads",
            Self::AskQuestion => "askQuestion",
            Self::AskQuestionWithdrawn => "askQuestionWithdrawn",
        }
    }
}

#[cfg(unix)]
impl FakeAcpRuntime {
    pub fn new() -> Self {
        let directory = TempDir::new().expect("creates fake ACP runtime");
        let request_log = directory.path().join("requests.jsonl");
        let control_directory = directory.path().join("control");
        fs::create_dir(&control_directory).expect("creates fake ACP control directory");
        let binary = fake_acp_binary();
        symlink(&binary, directory.path().join("node")).expect("links fake node runtime");
        symlink(&binary, directory.path().join("npx")).expect("links fake npx runtime");
        write_fake_npm(&directory.path().join("npm"));
        Self {
            directory,
            request_log,
            control_directory,
        }
    }

    pub fn set_next_prompt(&self, behavior: FakePromptBehavior) {
        self.publish_prompt_plan(json!({"behavior": behavior.wire_value()}));
    }

    /// A recorded native subagent turn of `recording` (`claude` or `codex`),
    /// held with every subagent running until
    /// [`Self::publish_marker`]`("subagents-complete")`; or, `cancelled`, held
    /// until `session/cancel` and then reporting its subagent cancelled.
    pub fn set_next_native_subagents(&self, recording: &str, cancelled: bool) {
        self.publish_prompt_plan(json!({
            "behavior": if cancelled { "nativeSubagentsCancelled" } else { "nativeSubagents" },
            "recording": recording,
        }));
    }

    /// The recorded native subagent turn `scenario` of `recording`, such as
    /// `NativeResumed`, played straight through.
    pub fn set_next_native_subagent_turn(&self, recording: &str, scenario: &str) {
        self.publish_prompt_plan(json!({
            "behavior": "nativeSubagentTurn",
            "recording": recording,
            "scenario": scenario,
        }));
    }

    /// The next `session/load` replays `recording`'s native subagent history.
    pub fn set_next_load_replay(&self, recording: &str) {
        let destination = self.control_directory.join("next-load.json");
        let temporary = self.control_directory.join("next-load.json.tmp");
        fs::write(&temporary, json!({"recording": recording}).to_string())
            .expect("writes fake ACP load plan");
        fs::rename(temporary, destination).expect("publishes fake ACP load plan");
    }

    /// A reply delivered in chunks over time, so a test can act on the turn
    /// while it is still producing output.
    pub fn set_next_streamed_prompt(&self, reply: &str, chunk_bytes: usize, chunk_delay: Duration) {
        self.publish_prompt_plan(json!({
            "behavior": "stream",
            "reply": reply,
            "chunkBytes": chunk_bytes,
            "chunkDelayMillis": chunk_delay.as_millis() as u64,
        }));
    }

    /// A turn that calls `tool` on the session's `server` MCP entry through
    /// the URL and token core gave it, replying `MCP result: …` or
    /// `MCP error: …`.
    pub fn set_next_mcp_call(&self, server: &str, tool: &str, arguments: Value) {
        self.publish_prompt_plan(json!({
            "behavior": "callMcpTool",
            "server": server,
            "tool": tool,
            "arguments": arguments,
        }));
    }

    fn publish_prompt_plan(&self, plan: Value) {
        let destination = self.control_directory.join("next-prompt.json");
        let temporary = self.control_directory.join("next-prompt.json.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec(&plan).expect("encodes fake ACP prompt plan"),
        )
        .expect("writes fake ACP prompt plan");
        fs::rename(temporary, destination).expect("publishes fake ACP prompt plan");
    }

    pub fn environment(&self) -> Vec<(String, String)> {
        let inherited_path = std::env::var("PATH").unwrap_or_default();
        vec![
            // Runtime discovery consults the login shell before process PATH;
            // a developer's shell can replace our fake toolchain with real npm.
            // Select this fixture explicitly, including its sibling npm/npx.
            (
                "OTTAM_NODE".to_string(),
                self.directory
                    .path()
                    .join("node")
                    .to_string_lossy()
                    .to_string(),
            ),
            (
                "PATH".to_string(),
                format!("{}:{inherited_path}", self.directory.path().display()),
            ),
            (
                "OTTAM_FAKE_ACP_LOG".to_string(),
                self.request_log.to_string_lossy().to_string(),
            ),
            (
                "OTTAM_TEST_ACP_CONTROL_DIR".to_string(),
                self.control_directory.to_string_lossy().to_string(),
            ),
        ]
    }

    /// Make the agent take `millis` to resolve a turn after `session/cancel`.
    /// That window is when its session is still busy with the cancelled prompt,
    /// so a follow-up prompt launched inside it collides with the old one.
    pub fn environment_with_a_slow_cancel(&self, millis: u64) -> Vec<(String, String)> {
        let mut environment = self.environment();
        environment.push((
            "OTTAM_TEST_ACP_CANCEL_DELAY_MS".to_string(),
            millis.to_string(),
        ));
        environment
    }

    /// Advertise and implement `_session/steering`, so the adapter reports
    /// `native` steering and a mid-run message joins the running turn.
    pub fn environment_with_native_steering(&self) -> Vec<(String, String)> {
        let mut environment = self.environment();
        environment.push(("OTTAM_TEST_ACP_STEERING".to_string(), "1".to_string()));
        environment
    }

    /// Advertise `_session/steering` and answer every steer with `outcome`,
    /// whatever is actually running. Covers the outcomes only one of the two
    /// real adapters produces, plus `methodNotFound` for an agent that
    /// advertised the extension without registering it.
    pub fn environment_with_a_forced_steer_outcome(&self, outcome: &str) -> Vec<(String, String)> {
        let mut environment = self.environment_with_native_steering();
        environment.push((
            "OTTAM_TEST_ACP_STEER_OUTCOME".to_string(),
            outcome.to_string(),
        ));
        environment
    }

    /// Advertise `method_id` as an authentication method whose `authenticate`
    /// succeeds at once — an adapter-executed login the user finishes.
    pub fn environment_with_a_login(&self, method_id: &str) -> Vec<(String, String)> {
        let mut environment = self.environment();
        environment.push((
            "OTTAM_TEST_ACP_AUTH_METHOD".to_string(),
            method_id.to_string(),
        ));
        environment
    }

    /// Advertise `method_id` as an authentication method and accept the
    /// resulting `authenticate` without ever answering it — an adapter-executed
    /// browser login the user never finishes.
    pub fn environment_with_a_login_that_never_returns(
        &self,
        method_id: &str,
    ) -> Vec<(String, String)> {
        let mut environment = self.environment();
        environment.push((
            "OTTAM_TEST_ACP_AUTH_METHOD".to_string(),
            method_id.to_string(),
        ));
        environment.push((
            "OTTAM_TEST_ACP_AUTH_BEHAVIOR".to_string(),
            "neverReply".to_string(),
        ));
        environment
    }

    /// Advertise a client-executed login whose command is `program args...`.
    /// The test owns the command, so it can decide how the login ends.
    pub fn environment_with_a_terminal_login(
        &self,
        program: &str,
        args: &[&str],
    ) -> Vec<(String, String)> {
        let mut environment = self.environment();
        let mut command = vec![program.to_string()];
        command.extend(args.iter().map(|argument| argument.to_string()));
        environment.push((
            "OTTAM_TEST_ACP_AUTH_TERMINAL".to_string(),
            command.join("\t"),
        ));
        environment
    }

    pub fn request_log(&self) -> String {
        fs::read_to_string(&self.request_log).unwrap_or_default()
    }

    pub fn wait_for_initialize_seen(&self, timeout: Duration) {
        self.wait_for_control_marker("initialize-seen", timeout);
    }

    pub fn initialize_release_guard(&self) -> FakeRuntimeReleaseGuard {
        self.release_guard("initialize-release")
    }

    pub fn wait_for_npm_started(&self, timeout: Duration) {
        self.wait_for_control_marker("npm-started", timeout);
    }

    pub fn npm_release_guard(&self) -> FakeRuntimeReleaseGuard {
        self.release_guard("npm-release")
    }

    /// Publishes a control marker the fake adapter waits on.
    pub fn publish_marker(&self, marker: &str) {
        fs::write(self.control_directory.join(marker), b"published\n")
            .unwrap_or_else(|error| panic!("publishes fake runtime marker {marker}: {error}"));
    }

    fn wait_for_control_marker(&self, marker: &str, timeout: Duration) {
        let path = self.control_directory.join(marker);
        let deadline = Instant::now() + timeout;
        while !path.is_file() {
            assert!(
                Instant::now() < deadline,
                "fake runtime never published {marker}; requests={}",
                self.request_log()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn release_guard(&self, marker: &str) -> FakeRuntimeReleaseGuard {
        FakeRuntimeReleaseGuard::new(self.control_directory.join(marker))
    }
}

/// A stand-in for `npm install --prefix <staging> ... <name>@<version>` that
/// writes the same shape npm would: the package under `node_modules/` with a
/// `bin` entry, the entrypoint file, and a lockfile carrying provenance. Knobs
/// via environment: `OTTAM_FAKE_NPM_FAIL=1` exits nonzero,
/// `OTTAM_FAKE_NPM_SLEEP=<secs>` stalls first, and
/// `OTTAM_FAKE_NPM_HOLD_UNTIL_RELEASE=1` publishes an `npm-started`
/// control marker and waits for `npm-release`.
#[cfg(unix)]
fn write_fake_npm(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let script = r#"#!/bin/sh
if [ "$OTTAM_FAKE_NPM_HOLD_UNTIL_RELEASE" = "1" ]; then
  control_dir="${OTTAM_TEST_ACP_CONTROL_DIR:?missing fake runtime control directory}"
  : > "$control_dir/npm-started"
  hold_iterations=0
  while [ ! -f "$control_dir/npm-release" ]; do
    hold_iterations=$((hold_iterations + 1))
    if [ "$hold_iterations" -ge 3000 ]; then
      echo "timed out waiting for npm-release" >&2
      exit 124
    fi
    sleep 0.01
  done
fi
if [ -n "$OTTAM_FAKE_NPM_SLEEP" ]; then sleep "$OTTAM_FAKE_NPM_SLEEP"; fi
if [ -n "$OTTAM_FAKE_NPM_FAIL" ]; then
  echo "npm ERR! E404 no such package" >&2
  exit 1
fi
prefix=""
prev=""
spec=""
for arg in "$@"; do
  if [ "$prev" = "--prefix" ]; then prefix="$arg"; fi
  prev="$arg"
  spec="$arg"
done
name="${spec%@*}"
version="${spec##*@}"
pkgdir="$prefix/node_modules/$name"
mkdir -p "$pkgdir/dist"
printf '{"name":"%s","version":"%s","bin":{"fake-acp":"dist/index.js"}}\n' "$name" "$version" > "$pkgdir/package.json"
printf '// fake adapter entrypoint\n' > "$pkgdir/dist/index.js"
printf '{"lockfileVersion":3,"packages":{"node_modules/%s":{"version":"%s","resolved":"https://registry.invalid/%s/-/%s.tgz","integrity":"sha512-fake"}}}\n' "$name" "$version" "$name" "$version" > "$prefix/package-lock.json"
exit 0
"#;
    fs::write(path, script).expect("writes fake npm");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmods fake npm");
}

pub struct CoreProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout_rx: Receiver<String>,
    stderr: Arc<Mutex<String>>,
    _owned_ottam_home: Option<TempDir>,
    ottam_home_path: PathBuf,
    env: Vec<(String, String)>,
    project_workspaces: HashMap<String, String>,
    project_sources: HashMap<String, String>,
    exited: bool,
    /// Messages read while correlating a response, replayed to the next reader.
    ///
    /// Core answers a request from its dispatch loop but emits notifications
    /// from background tasks, so a notification may reach stdout between a
    /// request and its response. That is ordinary JSON-RPC behavior — the Swift
    /// client correlates on `id` — so the harness correlates too instead of
    /// assuming the next line is the response.
    buffered: VecDeque<Value>,
    /// Set by the first line read from stdout; until then core may still be
    /// opening its database.
    has_answered: bool,
}

impl CoreProcess {
    pub fn new() -> Self {
        let ottam_home = TempDir::new().expect("creates OTTAM_HOME");
        let ottam_home_path = ottam_home.path().to_path_buf();
        Self::spawn(&ottam_home_path, None, Some(ottam_home), Vec::new())
    }

    pub fn new_in_home(ottam_home: &Path) -> Self {
        Self::spawn(ottam_home, None, None, Vec::new())
    }

    pub fn new_in_home_with_env(ottam_home: &Path, env: Vec<(String, String)>) -> Self {
        Self::spawn(ottam_home, None, None, env)
    }

    pub fn new_in_home_with_current_dir(ottam_home: &Path, current_dir: &Path) -> Self {
        Self::spawn(ottam_home, Some(current_dir), None, Vec::new())
    }

    pub fn new_with_env(env: Vec<(String, String)>) -> Self {
        let ottam_home = TempDir::new().expect("creates OTTAM_HOME");
        let ottam_home_path = ottam_home.path().to_path_buf();
        Self::spawn(&ottam_home_path, None, Some(ottam_home), env)
    }

    fn spawn(
        ottam_home: &Path,
        current_dir: Option<&Path>,
        owned_home: Option<TempDir>,
        env: Vec<(String, String)>,
    ) -> Self {
        let mut command = core_command(ottam_home);
        for (key, value) in &env {
            command.env(key, value);
        }
        if let Some(current_dir) = current_dir {
            command.current_dir(current_dir);
        }

        let mut child = command.spawn().expect("spawns ottam-core");
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        let (stdout_tx, stdout_rx) = mpsc::channel();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else {
                    break;
                };
                if stdout_tx.send(line).is_err() {
                    break;
                }
            }
        });

        let stderr_text = Arc::new(Mutex::new(String::new()));
        let stderr_sink = Arc::clone(&stderr_text);
        thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut buffer = [0; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        let mut text = stderr_sink.lock().expect("stderr lock");
                        text.push_str(&String::from_utf8_lossy(&buffer[..count]));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            child,
            stdin: Some(stdin),
            stdout_rx,
            stderr: stderr_text,
            _owned_ottam_home: owned_home,
            ottam_home_path: ottam_home.to_path_buf(),
            env,
            project_workspaces: HashMap::new(),
            project_sources: HashMap::new(),
            exited: false,
            buffered: VecDeque::new(),
            has_answered: false,
        }
    }
}

/// For a test of Node discovery itself: core treats a blank `OTTAM_NODE` as
/// unset, so the resolver walks its normal chain. Pair it with a fake `SHELL`;
/// the real login shell is what [`core_command`] exists to keep out.
pub fn node_discovery_environment() -> (String, String) {
    ("OTTAM_NODE".to_string(), String::new())
}

/// The hermetic core command every harness process starts from, with piped
/// stdio. Tests that need raw control of the pipes spawn it directly.
pub fn core_command(ottam_home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ottam-core"));
    command
        .env("OTTAM_HOME", ottam_home)
        // An override pointing at nothing, which core takes as authoritative.
        // Without it the first `agent.list` / `engine.list` runs real
        // discovery, whose cold login-shell probe can outlast
        // `DEFAULT_RESPONSE_TIMEOUT` on a loaded runner. A test that wants a
        // toolchain sets its own: see `FakeAcpRuntime::environment` and
        // `node_discovery_environment`.
        .env("OTTAM_NODE", "/nonexistent/ottam-test-node")
        // Never let tests spawn real npx adapter processes for the
        // background model-catalog probe.
        .env("OTTAM_MODEL_CATALOG_PROBE", "off")
        // Pre-installed-adapters behaviour: engines launch via the fake
        // toolchain without a Get step. The install tests override this
        // to exercise the real flow.
        .env("OTTAM_ADAPTER_INSTALLS", "off")
        // Never write MCP secrets to the login Keychain.
        .env("OTTAM_SECRET_STORE", "memory")
        // Any free port, so parallel cores never share a sign-in's.
        .env("OTTAM_MCP_REDIRECT_PORT", "0")
        .env_remove("OTTAM_LOG")
        .env_remove("RUST_LOG")
        .env_remove("OTTAM_LOG_DIR")
        .env_remove("OTTAM_CORE_LOG_FILE")
        .env_remove("OTTAM_LOG_TO_STDERR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

impl CoreProcess {
    /// `duration`, stretched to the startup budget until core first answers.
    fn budget(&self, duration: Duration) -> Duration {
        if self.has_answered {
            duration
        } else {
            duration.max(STARTUP_RESPONSE_TIMEOUT)
        }
    }

    pub fn send(&mut self, value: Value) -> Value {
        let expected_id = value.get("id").cloned();
        self.write_json(value);
        let Some(expected_id) = expected_id.filter(|id| !id.is_null()) else {
            // A notification has no response to correlate; read the next line.
            return self
                .read_response_timeout(DEFAULT_RESPONSE_TIMEOUT)
                .unwrap_or_else(|| {
                    panic!(
                        "timed out waiting for response\nstderr:\n{}",
                        self.stderr_text()
                    )
                });
        };
        let (response, skipped) =
            self.read_until_response_id(&expected_id, DEFAULT_RESPONSE_TIMEOUT);
        // Anything read past is still owed to whoever reads next, in order.
        for (index, message) in skipped.into_iter().enumerate() {
            self.buffered.insert(index, message);
        }
        response
    }

    pub fn send_for_id(&mut self, value: Value, expected_id: Value) -> (Value, Vec<Value>) {
        self.write_json(value);
        self.read_until_response_id(&expected_id, DEFAULT_RESPONSE_TIMEOUT)
    }

    pub fn write_raw_line(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(stdin, "{line}").expect("writes raw JSONL line");
        stdin.flush().expect("flushes stdin");
    }

    pub fn write_notification(&mut self, value: Value) {
        self.write_json(value);
    }

    pub fn write_message(&mut self, value: Value) {
        self.write_json(value);
    }

    pub fn read_response_timeout(&mut self, duration: Duration) -> Option<Value> {
        self.read_within(self.budget(duration))
    }

    fn read_within(&mut self, duration: Duration) -> Option<Value> {
        if let Some(buffered) = self.buffered.pop_front() {
            return Some(buffered);
        }
        match self.stdout_rx.recv_timeout(duration) {
            Ok(line) => {
                self.has_answered = true;
                Some(serde_json::from_str(&line).unwrap_or_else(|error| {
                    panic!(
                        "response was not JSON: {line}\nerror: {error}\nstderr:\n{}",
                        self.stderr_text()
                    )
                }))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "stdout closed before response\nstderr:\n{}",
                    self.stderr_text()
                )
            }
        }
    }

    pub fn read_json_timeout(&mut self, duration: Duration) -> Option<Value> {
        self.read_response_timeout(duration)
    }

    pub fn read_until_response_id(
        &mut self,
        expected_id: &Value,
        duration: Duration,
    ) -> (Value, Vec<Value>) {
        let duration = self.budget(duration);
        let started = std::time::Instant::now();
        let mut notifications = Vec::new();
        loop {
            let elapsed = started.elapsed();
            assert!(
                elapsed < duration,
                "timed out waiting for response id {expected_id}\nnotifications: {notifications:?}\nstderr:\n{}",
                self.stderr_text()
            );
            let remaining = duration.saturating_sub(elapsed);
            let value = self
                .read_response_timeout(remaining)
                .unwrap_or_else(|| {
                    panic!(
                        "timed out waiting for response id {expected_id}\nnotifications: {notifications:?}\nstderr:\n{}\ncore log:\n{}",
                        self.stderr_text(),
                        fs::read_to_string(
                            self.ottam_home_path
                                .join("logs")
                                .join(format!("core-{}.log", self.child.id()))
                        ).unwrap_or_default()
                    )
                });
            if value.get("id") == Some(expected_id) {
                return (value, notifications);
            }
            notifications.push(value);
        }
    }

    pub fn expect_no_response(&mut self, duration: Duration) {
        if let Some(response) = self.read_within(duration) {
            panic!("expected no response, received {response}");
        }
    }

    pub fn initialize(&mut self) -> Value {
        let response = self.send(json!({
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": {"name": "test", "version": "0.0.0"},
                "capabilities": {"experimentalMethods": []}
            }
        }));
        assert_success_response(&response, &json!(1));
        response
    }

    pub fn send_initialized(&mut self) {
        self.write_notification(json!({
            "method": "initialized",
            "params": {}
        }));
    }

    pub fn open_path(&mut self, path: &Path) -> Value {
        let response = self.send(json!({
            "id": 2,
            "method": "project.openPath",
            "params": {
                "path": path.to_string_lossy()
            }
        }));
        assert_success_response(&response, &json!(2));
        let result = response["result"].clone();
        self.remember_workspace(&result);
        result
    }

    pub fn open_path_string(&mut self, path: &str) -> Value {
        let response = self.send(json!({
            "id": 2,
            "method": "project.openPath",
            "params": {
                "path": path
            }
        }));
        assert_success_response(&response, &json!(2));
        let result = response["result"].clone();
        self.remember_workspace(&result);
        result
    }

    pub fn diff_by_project_source_id(&mut self, project_source_id: &str) -> Value {
        let response = self.send(json!({
            "id": 3,
            "method": "git.diffSnapshot",
            "params": {
                "projectSourceID": project_source_id
            }
        }));
        assert_success_response(&response, &json!(3));
        response["result"].clone()
    }

    pub fn engine_list(&mut self, project_id: &str) -> Value {
        let workspace_id = self.workspace_id(project_id);
        let response = self.send(json!({
            "id": 40,
            "method": "engine.list",
            "params": {
                "workspaceID": workspace_id
            }
        }));
        assert_success_response(&response, &json!(40));
        response["result"].clone()
    }

    pub fn engine_list_with_runtime_refresh(&mut self, project_id: &str) -> Value {
        let workspace_id = self.workspace_id(project_id);
        let response = self.send(json!({
            "id": 41,
            "method": "engine.list",
            "params": {
                "workspaceID": workspace_id,
                "refreshRuntime": true
            }
        }));
        assert_success_response(&response, &json!(41));
        response["result"].clone()
    }

    pub fn create_conversation(
        &mut self,
        project_id: &str,
        title: Option<&str>,
        message: Option<&str>,
    ) -> Value {
        let mut params = json!({
            "workspaceID": self.workspace_id(project_id),
            "workingRoot": {
                "kind": "project",
                "projectID": project_id,
                "sourceID": self.project_source_id(project_id)
            }
        });
        if let Some(title) = title {
            params["title"] = json!(title);
        }
        if let Some(message) = message {
            params["message"] = json!({
                "body": message,
                "idempotencyKey": "jsonl-test-create-message"
            });
        }
        let response = self.send(json!({
            "id": 41,
            "method": "conversation.create",
            "params": params
        }));
        assert_success_response(&response, &json!(41));
        response["result"]["conversation"].clone()
    }

    pub fn list_conversations(&mut self, project_id: &str) -> Value {
        let workspace_id = self.workspace_id(project_id);
        let response = self.send(json!({
            "id": 42,
            "method": "conversation.list",
            "params": {
                "workspaceID": workspace_id
            }
        }));
        assert_success_response(&response, &json!(42));
        response["result"]["conversations"].clone()
    }

    pub fn get_conversation(&mut self, project_id: &str, conversation_id: &str) -> Value {
        let workspace_id = self.workspace_id(project_id);
        let response = self.send(json!({
            "id": 43,
            "method": "conversation.get",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id
            }
        }));
        assert_success_response(&response, &json!(43));
        response["result"]["conversation"].clone()
    }

    pub fn post_conversation_message(
        &mut self,
        project_id: &str,
        conversation_id: &str,
        message: &str,
    ) -> Value {
        let params = json!({
            "workspaceID": self.workspace_id(project_id),
            "conversationID": conversation_id,
            "message": message,
            "idempotencyKey": "jsonl-test-post-message"
        });
        let response = self.send(json!({
            "id": 44,
            "method": "conversation.postMessage",
            "params": params
        }));
        assert_success_response(&response, &json!(44));
        response["result"].clone()
    }

    pub fn start_conversation_run(
        &mut self,
        project_id: &str,
        conversation_id: &str,
        engine_id: &str,
        run_input_id: &str,
    ) -> Value {
        let workspace_id = self.workspace_id(project_id);
        let response = self.send(json!({
            "id": 45,
            "method": "conversation.startRun",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id,
                "engineID": engine_id,
                "runInputID": run_input_id
            }
        }));
        assert_success_response(&response, &json!(45));
        response["result"].clone()
    }

    /// Polls `conversation.get` until `run_id` reaches one of `expected`, and
    /// returns that run's summary.
    pub fn wait_for_run_status(
        &mut self,
        workspace_id: &str,
        conversation_id: &str,
        run_id: &str,
        expected: &[&str],
    ) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last = Value::Null;
        loop {
            let (response, _) = self.send_for_id(
                json!({
                    "id": 46,
                    "method": "conversation.get",
                    "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
                }),
                json!(46),
            );
            assert_success_response(&response, &json!(46));
            if let Some(run) = response["result"]["conversation"]["latestRunPage"]["runs"]
                .as_array()
                .and_then(|runs| runs.iter().find(|run| run["runID"] == run_id))
            {
                last = run["status"].clone();
                if expected.iter().any(|status| run["status"] == *status) {
                    return run.clone();
                }
            }
            assert!(
                Instant::now() < deadline,
                "run {run_id} never reached {expected:?}; last status was {last}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn open_terminal(&mut self, project_id: &str, rows: u16, cols: u16) -> Value {
        let workspace_id = self.workspace_id(project_id);
        let (response, _) = self.send_for_id(
            json!({
                "id": 30,
                "method": "terminal.open",
                "params": {
                    "workspaceID": workspace_id,
                    "workingRoot": {
                        "kind": "project",
                        "projectID": project_id,
                        "sourceID": self.project_source_id(project_id)
                    },
                    "rows": rows,
                    "cols": cols
                }
            }),
            json!(30),
        );
        assert_success_response(&response, &json!(30));
        response["result"].clone()
    }

    pub fn workspace_id(&self, project_id: &str) -> String {
        self.project_workspaces
            .get(project_id)
            .cloned()
            .unwrap_or_else(|| panic!("project {project_id} has no remembered Workspace"))
    }

    pub fn project_source_id(&self, project_id: &str) -> String {
        self.project_sources
            .get(project_id)
            .cloned()
            .unwrap_or_else(|| panic!("project {project_id} has no remembered ProjectSource"))
    }

    fn remember_workspace(&mut self, opened: &Value) {
        let Some(project_id) = opened["project"]["projectID"].as_str() else {
            return;
        };
        let workspace_id = opened["createdWorkspace"]["workspaceID"]
            .as_str()
            .or_else(|| {
                opened["workspaces"]
                    .as_array()
                    .and_then(|workspaces| workspaces.first())
                    .and_then(|workspace| workspace["workspaceID"].as_str())
            });
        if let Some(workspace_id) = workspace_id {
            self.project_workspaces
                .insert(project_id.to_string(), workspace_id.to_string());
        }
        if let Some(source_id) = opened["source"]["projectSourceID"].as_str() {
            self.project_sources
                .insert(project_id.to_string(), source_id.to_string());
        }
    }

    pub fn open_path_and_diff(&mut self, repo_path: &Path) -> Value {
        let opened = self.open_path(repo_path);
        let source_id = opened["source"]["projectSourceID"]
            .as_str()
            .expect("projectSourceID");
        self.diff_by_project_source_id(source_id)
    }

    pub fn ottam_home_path(&self) -> &Path {
        &self.ottam_home_path
    }

    pub fn log_file_path(&self) -> PathBuf {
        if let Some((_, value)) = self
            .env
            .iter()
            .find(|(key, _)| key == "OTTAM_CORE_LOG_FILE")
        {
            return PathBuf::from(value);
        }
        self.ottam_home_path
            .join("logs")
            .join(format!("core-{}.log", self.child.id()))
    }

    pub fn log_text(&self) -> String {
        read_log_text(&self.log_file_path())
    }

    pub fn stderr_text(&self) -> String {
        self.stderr.lock().expect("stderr lock").clone()
    }

    /// Wait for `needle` to appear in the child's stderr.
    ///
    /// A child's stderr is drained by a reader thread, so the process can exit
    /// before its final write lands in the shared buffer. Reading once right
    /// after `wait_for_exit` therefore races the reader — rarely when the
    /// machine is idle, often when the whole suite runs in parallel.
    pub fn wait_for_stderr_containing(&self, needle: &str, duration: Duration) -> bool {
        let started = std::time::Instant::now();
        loop {
            if self.stderr_text().contains(needle) {
                return true;
            }
            if started.elapsed() >= duration {
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn wait_for_exit(&mut self, duration: Duration) -> Option<std::process::ExitStatus> {
        let started = std::time::Instant::now();
        while started.elapsed() < duration {
            if let Some(status) = self.child.try_wait().expect("polls ottam-core") {
                self.exited = true;
                return Some(status);
            }
            thread::sleep(Duration::from_millis(10));
        }
        None
    }

    /// Close core's stdin without waiting, the client's first shutdown step.
    pub fn close_stdin(&mut self) {
        drop(self.stdin.take());
    }

    #[cfg(unix)]
    pub fn send_sigterm(&self) {
        // SAFETY: the pid is this harness's live child; SIGTERM is a constant.
        let result = unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        assert_eq!(result, 0, "sends SIGTERM to ottam-core");
    }

    #[cfg(unix)]
    pub fn kill_now(mut self) {
        // Process-test crash simulation: the child is the isolated core owned by
        // this harness, and waiting here guarantees SQLite locks are released.
        self.child.kill().expect("kills ottam-core");
        let _ = self.child.wait().expect("waits for killed ottam-core");
        self.exited = true;
    }

    pub fn shutdown(mut self) {
        drop(self.stdin.take());
        let status = self.child.wait().expect("waits for ottam-core");
        self.exited = true;
        assert!(
            status.success(),
            "ottam-core exited with {status}\nstderr:\n{}",
            self.stderr_text()
        );
    }

    fn write_json(&mut self, value: Value) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(
            stdin,
            "{}",
            serde_json::to_string(&value).expect("encodes JSON")
        )
        .expect("writes JSONL request");
        stdin.flush().expect("flushes stdin");
    }
}

pub fn read_log_text(path: &Path) -> String {
    let mut last = String::new();
    for _ in 0..20 {
        last = fs::read_to_string(path).unwrap_or_default();
        if !last.is_empty() {
            return last;
        }
        thread::sleep(Duration::from_millis(50));
    }
    last
}

impl Drop for CoreProcess {
    fn drop(&mut self) {
        if self.exited {
            return;
        }
        drop(self.stdin.take());
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
            Err(_) => {
                let _ = self.child.kill();
            }
        }
    }
}

pub struct GitRepoFixture {
    _temp_dir: Option<TempDir>,
    root: PathBuf,
}

impl GitRepoFixture {
    pub fn init() -> Self {
        let temp_dir = TempDir::new().expect("creates repo tempdir");
        let root = temp_dir.path().to_path_buf();
        let fixture = Self {
            _temp_dir: Some(temp_dir),
            root,
        };
        fixture.init_git();
        fixture
    }

    pub fn init_at(root: PathBuf) -> Self {
        fs::create_dir_all(&root).expect("creates repo root");
        let fixture = Self {
            _temp_dir: None,
            root,
        };
        fixture.init_git();
        fixture
    }

    pub fn commit_all(&self, message: &str) {
        self.git(["add", "-A"]);
        self.git(["commit", "-m", message]);
    }

    pub fn commit_empty(&self, message: &str) {
        self.git(["commit", "--allow-empty", "-m", message]);
    }

    pub fn write(&self, path: &str, contents: &str) {
        let full_path = self.root.join(path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).expect("creates parent directory");
        }
        fs::write(full_path, contents).expect("writes fixture file");
    }

    pub fn write_bytes(&self, path: &str, contents: &[u8]) {
        let full_path = self.root.join(path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).expect("creates parent directory");
        }
        fs::write(full_path, contents).expect("writes fixture bytes");
    }

    pub fn mkdir(&self, path: &str) {
        fs::create_dir_all(self.root.join(path)).expect("creates fixture directory");
    }

    #[cfg(unix)]
    pub fn symlink(&self, source: &Path, link: &Path) {
        std::os::unix::fs::symlink(source, link).expect("creates symlink");
    }

    #[cfg(unix)]
    pub fn fifo(&self, path: &str) {
        let full_path = self.root.join(path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).expect("creates parent directory");
        }
        let output = Command::new("mkfifo")
            .arg(&full_path)
            .output()
            .expect("runs mkfifo");
        assert!(
            output.status.success(),
            "mkfifo failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    pub fn git<const N: usize>(&self, args: [&str; N]) -> String {
        run_git(&self.root, args)
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn nested_path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn init_git(&self) {
        self.git(["init"]);
        self.git(["config", "user.name", "Ottam Test"]);
        self.git(["config", "user.email", "test@example.com"]);
    }
}

pub fn assert_success_response(response: &Value, expected_id: &Value) {
    assert_eq!(&response["id"], expected_id, "response id should correlate");
    assert!(
        response.get("result").is_some() ^ response.get("error").is_some(),
        "response must contain exactly one of result or error: {response}"
    );
    assert!(
        response.get("result").is_some(),
        "expected success: {response}"
    );
}

pub fn assert_error_response(
    response: &Value,
    expected_id: &Value,
    expected_code: i64,
    expected_message_fragment: &str,
) {
    assert_eq!(&response["id"], expected_id, "response id should correlate");
    assert!(
        response.get("result").is_some() ^ response.get("error").is_some(),
        "response must contain exactly one of result or error: {response}"
    );
    assert_eq!(
        response["error"]["code"].as_i64(),
        Some(expected_code),
        "unexpected error response: {response}"
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(expected_message_fragment),
        "error message did not contain {expected_message_fragment:?}: {response}"
    );
}

pub fn assert_error_data_kind(response: &Value, expected_kind: &str) {
    assert_eq!(
        response["error"]["data"]["kind"].as_str(),
        Some(expected_kind),
        "unexpected structured error data: {response}"
    );
}

pub fn run_git<const N: usize>(repo_path: &Path, args: [&str; N]) -> String {
    let output = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .output()
        .expect("runs git");
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Serves the same bytes at any path, for as many requests as arrive, until the
/// test ends. Deliberately the crudest possible HTTP/1.1 responder.
pub struct TarballServer {
    pub base_url: String,
}

impl TarballServer {
    pub fn serving(tarball: &Path) -> Self {
        let body = std::fs::read(tarball).expect("read tarball");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request);
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/gzip\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        Self {
            base_url: format!("http://{address}"),
        }
    }

    /// Promises far more than it delivers, then goes quiet — a download that
    /// stays in flight until something cancels it.
    pub fn stalling() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                std::thread::spawn(move || {
                    let mut request = [0u8; 4096];
                    let _ = stream.read(&mut request);
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\n",
                    );
                    let _ = stream.write_all(&[0u8; 4096]);
                    let _ = stream.flush();
                    // Hold the connection open. The test's tempdirs and the core
                    // process both outlive this thread by design.
                    std::thread::sleep(Duration::from_secs(120));
                });
            }
        });
        Self {
            base_url: format!("http://{address}"),
        }
    }
}
