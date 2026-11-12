mod mcp_client;

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

fn permission_config(current: &str) -> Value {
    json!([{"id":"mode", "name":"Mode", "category":"mode", "type":"select",
    "currentValue":current, "options":[
        {"value":"default","name":"Manual"},
        {"value":"auto","name":"Auto"},
        {"value":"acceptEdits","name":"Accept edits"}
    ]}])
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
enum PromptBehavior {
    Complete,
    HoldUntilCancel,
    /// A silent pending tool for idle-animation CPU attribution.
    HoldToolUntilCancel,
    ChangingTools,
    NeverAcknowledge,
    ExitAfterUpdate,
    /// A definitive rejection before the turn begins, in claude-agent-acp's
    /// shape: `auth_required` carrying `data.errorKind`. Nothing was accepted,
    /// so the input is failed rather than left ambiguous.
    AuthRequired,
    /// A turn whose reply arrives the way a real agent's does: many small
    /// `agent_message_chunk` deltas, paced by `chunkDelayMillis`. Everything
    /// else emits its reply in one update, which exercises none of the
    /// per-chunk render path.
    Stream,
    /// A turn that stops on a tool call and asks the client to approve it,
    /// then waits exactly like `HoldUntilCancel`. The permission stays pending
    /// until the client answers, which is what a real approval prompt looks
    /// like on the wire.
    RequestPermission,
    NormalizedTranscript,
    TranscriptPayloads,
    TranscriptHistory,
    CaptureLoss,
    SessionRetry,
    SessionLimit,
    SessionFailure,
    /// Output is retained before OpenCode's real quota error ends the turn.
    UsageLimitFailure,
    /// Both adapters' background-task frames from the source-derived
    /// fixture, interleaved with the tool calls they stamp, inside one turn.
    AsyncTasks,
    /// Codex's order: a tool call inside the turn, then, after `end_turn`,
    /// its background stamp and spawn once the `after-turn-spawn` control
    /// marker appears, and its `stopped` state once `after-turn-stop` does.
    AsyncTasksAfterTurn,
    /// Claude's background command: inside the turn, a tool call, its
    /// background stamp, and the task's spawn. Once the `follow-up-complete`
    /// control marker appears, and unless `_session/async_task/stop` stopped
    /// it first, the task completes and the agent answers on its own: the
    /// reply in `chunkBytes` chunks, then a usage report stamped with a
    /// task-notification origin, as Claude ends a cycle it ran itself.
    AsyncTasksFollowUp,
    /// Claude 0.85.1's recorded subagent turn: an asynchronous subagent that
    /// launches a nested one, beside a launch that fails. With a control
    /// directory the turn holds after the first child call, the thread still
    /// running, until the `subagents-complete` marker appears.
    SubagentThreads,
    /// A recorded native subagent turn (`recording`: `claude` or `codex`):
    /// two parallel subagents, one launching a nested one, and a subagent's
    /// permission request, which waits for the client's answer. With a
    /// control directory the turn holds before the first subagent ends,
    /// until the `subagents-complete` marker appears.
    NativeSubagents,
    /// A recorded native subagent turn cancelled mid-subagent: it holds
    /// until `session/cancel`, then reports the subagent cancelled.
    NativeSubagentsCancelled,
    /// A recorded native subagent turn played straight through: `scenario`
    /// of `recording`, such as `NativeResumed`.
    NativeSubagentTurn,
    /// Claude's recorded `NativeFirstRun`: one subagent, after a permission
    /// request. For harnesses that arm a behavior alone.
    NativeSubagentFirstRun,
    /// Claude's recorded `NativeResumed`: the parent messages the finished
    /// subagent of `NativeFirstRun`, which resumes.
    NativeSubagentResumed,
    /// A turn of traffic the client can't use: an update kind the SDK doesn't
    /// know, a chunk carrying a field it doesn't model, and a session-scoped
    /// notification and request (`fs/read_text_file`) it never advertised.
    /// Payloads carry `PRIVATE-TEXT`. The turn ends only once the client
    /// answers the request, quoting the answer's error code.
    UnhandledTraffic,
    /// Claude's `AskUserQuestion`: a tool call, then an `elicitation/create`
    /// form for it with one single-select question and its custom answer.
    /// The turn ends once the client answers, quoting the answer; a `cancel`
    /// answer ends it cancelled.
    AskQuestion,
    /// [`Self::AskQuestion`], withdrawn with `$/cancel_request` once the
    /// `withdraw-question` marker appears.
    AskQuestionWithdrawn,
    /// The reply, a `usage_update` of `contextUsed` of `contextSize` tokens,
    /// and a prompt response carrying the turn's token usage.
    Usage,
    /// [`Self::Usage`]'s reply and `usage_update`, held until
    /// `session/cancel`; the cancelled response carries usage and arrives
    /// after the client has stopped waiting, as Claude's does.
    UsageCancelled,
    /// A call to `tool` on the session's `server` entry with `arguments`,
    /// through the URL and headers that entry carries: a tool call, then
    /// its outcome as the reply (`MCP result: …` or `MCP error: …`).
    CallMcpTool,
}

impl PromptBehavior {
    fn wire_value(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::HoldUntilCancel => "holdUntilCancel",
            Self::HoldToolUntilCancel => "holdToolUntilCancel",
            Self::ChangingTools => "changingTools",
            Self::NeverAcknowledge => "neverAcknowledge",
            Self::ExitAfterUpdate => "exitAfterUpdate",
            Self::AuthRequired => "authRequired",
            Self::Stream => "stream",
            Self::RequestPermission => "requestPermission",
            Self::NormalizedTranscript => "normalizedTranscript",
            Self::TranscriptPayloads => "transcriptPayloads",
            Self::TranscriptHistory => "transcriptHistory",
            Self::CaptureLoss => "captureLoss",
            Self::SessionRetry => "sessionRetry",
            Self::SessionLimit => "sessionLimit",
            Self::SessionFailure => "sessionFailure",
            Self::UsageLimitFailure => "usageLimitFailure",
            Self::AsyncTasks => "asyncTasks",
            Self::AsyncTasksAfterTurn => "asyncTasksAfterTurn",
            Self::AsyncTasksFollowUp => "asyncTasksFollowUp",
            Self::SubagentThreads => "subagentThreads",
            Self::NativeSubagents => "nativeSubagents",
            Self::NativeSubagentsCancelled => "nativeSubagentsCancelled",
            Self::NativeSubagentTurn => "nativeSubagentTurn",
            Self::NativeSubagentFirstRun => "nativeSubagentFirstRun",
            Self::NativeSubagentResumed => "nativeSubagentResumed",
            Self::UnhandledTraffic => "unhandledTraffic",
            Self::AskQuestion => "askQuestion",
            Self::AskQuestionWithdrawn => "askQuestionWithdrawn",
            Self::Usage => "usage",
            Self::UsageCancelled => "usageCancelled",
            Self::CallMcpTool => "callMcpTool",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptPlan {
    behavior: PromptBehavior,
    #[serde(default = "default_reply")]
    reply: String,
    /// Streaming shape, ignored by every other behavior. The defaults are a
    /// token-sized delta at roughly the rate a real adapter delivers them.
    #[serde(default = "default_chunk_bytes")]
    chunk_bytes: usize,
    #[serde(default = "default_chunk_delay_millis")]
    chunk_delay_millis: u64,
    /// Which native subagent recording to replay: `claude` or `codex`.
    #[serde(default)]
    recording: Option<String>,
    /// The recorded turn `nativeSubagentTurn` plays.
    #[serde(default)]
    scenario: Option<String>,
    /// The context `usage` and `usageCancelled` report.
    #[serde(default = "default_context_used")]
    context_used: u64,
    #[serde(default = "default_context_size")]
    context_size: u64,
    /// The entry, tool, and arguments `callMcpTool` uses.
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    arguments: Option<Value>,
}

/// One of the recorded native subagent sessions: its frames under `scenario`
/// (`NativeTurn`, `NativeCancelled`, `NativeReplay`) for `recording`, with the
/// recorded root session renamed to `session_id`. Child sessions keep their
/// recorded ids.
fn native_subagent_frames(recording: &str, scenario: &str, session_id: &str) -> Vec<Value> {
    let fixture = match recording {
        "codex" => include_str!(
            "../../ottam-core/src/agent/fixtures/codex-native-subagents-2.1.1.json"
        ),
        "claude" => include_str!(
            "../../ottam-core/src/agent/fixtures/claude-native-subagents-0.85.1.json"
        ),
        // ACP Subagent Sessions as oh-my-pi's open PR emits them; hand-built.
        "omp" => include_str!(
            "../../ottam-core/src/agent/fixtures/omp-subagent-sessions-pr15043.json"
        ),
        other => panic!("no native subagent recording named {other:?}"),
    };
    let fixture: Value = serde_json::from_str(fixture).expect("recorded native subagents");
    let name = recording;
    let frames = fixture[format!("{name}{scenario}")]
        .as_array()
        .expect("recorded frames")
        .clone();
    let root = frames[0]["params"]["sessionId"]
        .as_str()
        .expect("root session")
        .to_owned();
    frames
        .into_iter()
        .map(|mut frame| {
            if frame["params"]["sessionId"] == root.as_str() {
                frame["params"]["sessionId"] = json!(session_id);
            }
            frame
        })
        .collect()
}

/// Emits recorded frames. A permission request goes out under `id` and waits
/// for the client's answer, as the agent would.
fn emit_recorded(
    output: &Arc<Mutex<io::Stdout>>,
    answers: &Arc<Mutex<HashMap<String, Sender<()>>>>,
    frames: impl IntoIterator<Item = Value>,
) {
    for (index, mut frame) in frames.into_iter().enumerate() {
        if frame["method"] != "session/request_permission" {
            emit(output, frame);
            continue;
        }
        let id = format!("ottam-test-native-permission-{index}");
        frame["id"] = json!(id);
        let (sender, receiver) = mpsc::channel();
        answers.lock().expect("answers lock").insert(id, sender);
        emit(output, frame);
        if receiver.recv_timeout(Duration::from_secs(30)).is_err() {
            return;
        }
    }
}

fn default_reply() -> String {
    "Retained ACP reply.".to_string()
}

fn default_context_used() -> u64 {
    24_100
}

fn default_context_size() -> u64 {
    200_000
}

/// The token usage `usage` and `usageCancelled` answer with.
fn turn_usage() -> Value {
    json!({"totalTokens": 12_345, "inputTokens": 9_800, "outputTokens": 545, "cachedReadTokens": 2_000})
}

fn default_chunk_bytes() -> usize {
    24
}

fn default_chunk_delay_millis() -> u64 {
    20
}

/// Split on character boundaries at or after `chunk_bytes`, so a multi-byte
/// character is never cut in half on the wire.
fn split_into_chunks(reply: &str, chunk_bytes: usize) -> Vec<String> {
    let limit = chunk_bytes.max(1);
    let mut chunks = Vec::new();
    let mut current = String::new();
    for character in reply.chars() {
        current.push(character);
        if current.len() >= limit {
            chunks.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn emit(output: &Arc<Mutex<io::Stdout>>, value: Value) {
    let mut output = output.lock().expect("stdout lock");
    serde_json::to_writer(&mut *output, &value).expect("writes ACP response");
    output.write_all(b"\n").expect("terminates ACP response");
    output.flush().expect("flushes ACP response");
}

fn log_request(path: Option<&Path>, value: &Value) {
    let Some(path) = path else { return };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("opens fake ACP request log");
    serde_json::to_writer(&mut file, value).expect("writes fake ACP request log");
    file.write_all(b"\n").expect("terminates fake ACP log line");
}

/// Waits up to 30 seconds for a test to publish `marker`, and consumes it so
/// a later prompt waits for its own.
fn wait_for_marker(control_dir: &Path, marker: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    let path = control_dir.join(marker);
    while !path.is_file() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = fs::remove_file(path);
    true
}

/// The recording `next-load.json` names for the next `session/load` to
/// replay, consumed so a later load replays nothing.
fn take_load_recording(control_dir: Option<&Path>) -> Option<String> {
    let path = control_dir?.join("next-load.json");
    let contents = fs::read(&path).ok()?;
    fs::remove_file(&path).expect("consumes fake ACP load plan");
    let plan: Value = serde_json::from_slice(&contents).expect("decodes fake ACP load plan");
    plan["recording"].as_str().map(str::to_owned)
}

fn take_prompt_plan(control_dir: Option<&Path>) -> PromptPlan {
    let default = PromptPlan {
        behavior: PromptBehavior::Complete,
        reply: default_reply(),
        chunk_bytes: default_chunk_bytes(),
        chunk_delay_millis: default_chunk_delay_millis(),
        recording: None,
        scenario: None,
        context_used: default_context_used(),
        context_size: default_context_size(),
        server: None,
        tool: None,
        arguments: None,
    };
    let Some(path) = control_dir.map(|directory| directory.join("next-prompt.json")) else {
        return default;
    };
    let Ok(contents) = fs::read(&path) else {
        return default;
    };
    fs::remove_file(&path).expect("consumes fake ACP prompt plan");
    serde_json::from_slice(&contents).expect("decodes fake ACP prompt plan")
}

/// The fake advertises image support unless a test asks it not to, so the
/// per-model refusal path can be exercised without a second binary.
fn image_input_supported() -> bool {
    std::env::var("OTTAM_FAKE_ACP_REJECT_IMAGE").as_deref() != Ok("1")
}

/// The advertised login, if a test asked for one. Named rather than boolean so
/// a test can assert the client sent back the id it was offered.
fn advertised_auth_method() -> Option<String> {
    env::var("OTTAM_TEST_ACP_AUTH_METHOD").ok()
}

/// A client-executed login, as `OTTAM_TEST_ACP_AUTH_TERMINAL=<program>\t<arg>...`.
///
/// This is claude-agent-acp's shape: a `terminal` method carrying an explicit
/// absolute command under `_meta["terminal-auth"]`, which the client is expected
/// to run itself. Tab-separated so a test can hand over a whole `sh -c` script.
fn advertised_terminal_auth() -> Option<(String, Vec<String>)> {
    let raw = env::var("OTTAM_TEST_ACP_AUTH_TERMINAL").ok()?;
    let mut parts = raw.split('\t').map(str::to_string);
    let program = parts.next()?;
    Some((program, parts.collect()))
}

/// Stands in for an adapter-executed login that opens a browser and waits: the
/// request is accepted and simply never answered, which is what a user who
/// walks away from the OAuth tab looks like on the wire.
fn authenticate_never_replies() -> bool {
    env::var("OTTAM_TEST_ACP_AUTH_BEHAVIOR").as_deref() == Ok("neverReply")
}

/// How long the agent takes to wind a cancelled turn down, as
/// `OTTAM_TEST_ACP_CANCEL_DELAY_MS=<millis>`.
///
/// `session/cancel` is a notification: a real agent finishes the tool call it
/// is in and only then resolves the prompt, and its session stays busy for the
/// whole of that window. Defaults to zero so every other test keeps the
/// instantaneous timing it was written against.
fn cancel_settle_delay() -> Duration {
    env::var("OTTAM_TEST_ACP_CANCEL_DELAY_MS")
        .ok()
        .and_then(|millis| millis.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or_default()
}

/// Whether the fake advertises `_session/steering`, as
/// `OTTAM_TEST_ACP_STEERING=1`. Off by default so every test written
/// against interrupt-and-continue keeps that path.
fn steering_supported() -> bool {
    env::var("OTTAM_TEST_ACP_STEERING").as_deref() == Ok("1")
}

/// An answer the fake gives to every steer regardless of what is running, as
/// `OTTAM_TEST_ACP_STEER_OUTCOME=<outcome>`. Covers the outcomes only one
/// of the two real adapters produces, plus `methodNotFound` for an agent that
/// advertised the extension and did not register it.
fn forced_steer_outcome() -> Option<String> {
    env::var("OTTAM_TEST_ACP_STEER_OUTCOME").ok()
}

fn hold_initialize_until_released(control_dir: Option<&Path>) {
    let control_dir = control_dir.expect("initialize hold requires the fake ACP control directory");
    fs::write(control_dir.join("initialize-seen"), b"seen\n")
        .expect("publishes initialize-seen marker");
    let release = control_dir.join("initialize-release");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !release.is_file() {
        if Instant::now() >= deadline {
            eprintln!("timed out waiting for initialize-release");
            process::exit(124);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn prompt_carries_an_image(request: &Value) -> bool {
    request["params"]["prompt"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block["type"].as_str() == Some("image"))
        })
        .unwrap_or(false)
}

fn executable_name() -> String {
    env::args()
        .next()
        .and_then(|path| PathBuf::from(path).file_name().map(|name| name.to_owned()))
        .and_then(|name| name.to_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The `mcpServers` entries a `session/new` or `session/load` carried.
fn entries_of(request: &Value) -> HashMap<String, mcp_client::Entry> {
    request["params"]["mcpServers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(mcp_client::Entry::from_acp)
        .collect()
}

/// [`PromptBehavior::CallMcpTool`]'s turn: the tool call, its outcome, and
/// the outcome as the reply.
fn call_mcp_tool(
    output: &Arc<Mutex<io::Stdout>>,
    request_id: Option<Value>,
    session_id: &str,
    server: &str,
    tool: &str,
    arguments: &Value,
    entry: Option<mcp_client::Entry>,
) {
    let update = |update: Value| {
        emit(
            output,
            json!({
                "jsonrpc": "2.0", "method": "session/update",
                "params": {"sessionId": session_id, "update": update}
            }),
        );
    };
    let tool_call_id = format!("mcp-{server}-{tool}");
    update(json!({
        "sessionUpdate": "tool_call", "toolCallId": tool_call_id,
        "title": format!("mcp__{server}__{tool}"), "kind": "other",
        "status": "in_progress", "rawInput": arguments
    }));
    let outcome = match entry {
        Some(entry) => mcp_client::call_tool(&entry, tool, arguments),
        None => Err(format!("this session has no {server} MCP server")),
    };
    let (status, text) = match &outcome {
        Ok(text) => ("completed", format!("MCP result: {text}")),
        Err(error) => ("failed", format!("MCP error: {error}")),
    };
    update(json!({
        "sessionUpdate": "tool_call_update", "toolCallId": tool_call_id, "status": status,
        "content": [{"type": "content", "content": {"type": "text", "text": text}}]
    }));
    update(json!({
        "sessionUpdate": "agent_message_chunk",
        "messageId": format!("message-{}", request_id.as_ref().unwrap_or(&Value::Null)),
        "content": {"type": "text", "text": text}
    }));
    emit(
        output,
        json!({"jsonrpc": "2.0", "id": request_id, "result": {"stopReason": "end_turn"}}),
    );
}

fn main() {
    let executable = executable_name();
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if arguments.first().map(String::as_str) == Some("--version") {
        println!(
            "{}",
            if executable == "node" {
                "v22.0.0"
            } else {
                "10.0.0"
            }
        );
        return;
    }

    // A broken adapter: answers `--version` (so runtime probes still pass) but
    // dies before speaking ACP. What a bad pin looks like at handshake time.
    if env::var("OTTAM_TEST_ACP_DIE_BEFORE_INITIALIZE").as_deref() == Ok("1") {
        eprintln!("fake adapter refusing to start");
        std::process::exit(1);
    }

    let request_log = env::var_os("OTTAM_FAKE_ACP_LOG").map(PathBuf::from);
    let control_dir = env::var_os("OTTAM_TEST_ACP_CONTROL_DIR").map(PathBuf::from);
    let output = Arc::new(Mutex::new(io::stdout()));
    let held = Arc::new(Mutex::new(HashMap::<String, Sender<()>>::new()));
    // Background tasks `_session/async_task/stop` can still stop:
    // `(session, task)`.
    let live_tasks = Arc::new(Mutex::new(HashSet::<(String, String)>::new()));
    let mut session_counter = 0_u64;
    let mut follow_up_counter = 0_u64;
    let mut session_modes: HashMap<String, String> = HashMap::new();
    let mut permission_counter = 0_u64;
    // Each session's `mcpServers` entries, by name.
    let mut mcp_entries: HashMap<String, HashMap<String, mcp_client::Entry>> = HashMap::new();
    // Outbound request id -> the prompt it holds open: `(prompt id, session)`.
    let mut unclaimed_requests: HashMap<String, (Option<Value>, String)> = HashMap::new();
    // Outbound `elicitation/create` id -> the prompt it holds open.
    let mut questions: HashMap<String, (Option<Value>, String)> = HashMap::new();
    // Recorded permission requests waiting for the client's answer.
    let answers = Arc::new(Mutex::new(HashMap::<String, Sender<()>>::new()));

    for line in io::stdin().lock().lines() {
        let line = line.expect("reads ACP request");
        let request: Value = serde_json::from_str(&line).expect("decodes ACP request");
        log_request(request_log.as_deref(), &request);
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let request_id = request.get("id").cloned();

        // The client answers the requests we send it (`session/request_permission`).
        // Those lines carry no method and are not ours to reply to.
        if request.get("method").is_none() {
            if let Some(answer) = request["id"]
                .as_str()
                .and_then(|id| answers.lock().expect("answers lock").remove(id))
            {
                let _ = answer.send(());
                continue;
            }
            if let Some((prompt_id, session_id)) =
                request["id"].as_str().and_then(|id| questions.remove(id))
            {
                let answer = request.get("result").unwrap_or(&request["error"]);
                emit(
                    &output,
                    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":{"sessionUpdate":"tool_call_update","toolCallId":"toolu_question","status":"completed"}}}),
                );
                emit(
                    &output,
                    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":format!("The client answered the question: {answer}")}}}}),
                );
                let stop_reason = if answer["action"] == "cancel" {
                    "cancelled"
                } else {
                    "end_turn"
                };
                emit(
                    &output,
                    json!({"jsonrpc":"2.0","id":prompt_id,"result":{"stopReason":stop_reason}}),
                );
                continue;
            }
            let answered = request["id"]
                .as_str()
                .and_then(|id| unclaimed_requests.remove(id));
            if let Some((prompt_id, session_id)) = answered {
                let code = request["error"]["code"].clone();
                emit(
                    &output,
                    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":format!("\nThe client answered fs/read_text_file with error {code}.")}}}}),
                );
                emit(
                    &output,
                    json!({"jsonrpc":"2.0","id":prompt_id,"result":{"stopReason":"end_turn"}}),
                );
            }
            continue;
        }

        // Hold `initialize` at a deterministic point until the test releases
        // it. This keeps an install in `verifying` long enough to cancel while
        // still allowing the detached model-catalog probe to settle cleanly.
        if method == "initialize"
            && (env::var("OTTAM_TEST_ACP_HOLD_INITIALIZE").as_deref() == Ok("1")
                || control_dir
                    .as_ref()
                    .is_some_and(|directory| directory.join("initialize-hold").exists()))
        {
            hold_initialize_until_released(control_dir.as_deref());
        }

        match method {
            "initialize" => {
                let auth_methods = match (advertised_auth_method(), advertised_terminal_auth()) {
                    (_, Some((program, args))) => json!([{
                        "id": "test-terminal-login",
                        "name": "Test terminal login",
                        "type": "terminal",
                        "args": args,
                        "_meta": {"terminal-auth": {
                            "command": program,
                            "args": args,
                            "label": "Test terminal login"
                        }}
                    }]),
                    (Some(id), None) => json!([{"id": id, "name": "Test login"}]),
                    (None, None) => json!([]),
                };
                emit(
                    &output,
                    json!({
                        "jsonrpc": "2.0", "id": request_id,
                        "result": {
                            "protocolVersion": 1,
                            "agentCapabilities": {
                                "loadSession": true,
                                "sessionCapabilities": {"delete": {}},
                                "promptCapabilities": {"image": image_input_supported()},
                                // Like every pinned adapter, so core sends
                                // its proxy's servers as http entries.
                                "mcpCapabilities": {"http": true}
                            },
                            "authMethods": auth_methods,
                            "agentInfo": {"name": "ottam-test-acp", "version": env!("CARGO_PKG_VERSION")},
                            // Where both real adapters advertise the steering
                            // extension: top-level `_meta`, beside the
                            // capabilities rather than inside them.
                            "_meta": {"steering": {"supported": steering_supported()}}
                        }
                    }),
                );
            }
            "authenticate" => {
                if authenticate_never_replies() {
                    continue;
                }
                emit(
                    &output,
                    json!({"jsonrpc": "2.0", "id": request_id, "result": {}}),
                );
            }
            "session/new" => {
                session_counter += 1;
                let session_id = format!("ottam-test-session-{session_counter}");
                session_modes.insert(session_id.clone(), "default".into());
                mcp_entries.insert(session_id.clone(), entries_of(&request));
                emit(
                    &output,
                    json!({
                        "jsonrpc": "2.0", "id": request_id,
                        "result": {"sessionId": session_id, "configOptions": permission_config("default")}
                    }),
                );
            }
            "session/load" => {
                let session_id = request
                    .pointer("/params/sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                mcp_entries.insert(session_id.to_owned(), entries_of(&request));
                // A test may ask for a recorded native subagent history,
                // replayed ahead of the answer as the adapters do.
                if let Some(recording) = take_load_recording(control_dir.as_deref()) {
                    for frame in native_subagent_frames(&recording, "NativeReplay", session_id) {
                        emit(&output, frame);
                    }
                }
                let mode = session_modes
                    .entry(session_id.into())
                    .or_insert_with(|| "default".into());
                emit(
                    &output,
                    json!({"jsonrpc":"2.0", "id":request_id,
                    "result":{"configOptions":permission_config(mode)}}),
                );
            }
            "session/set_config_option" => {
                let session_id = request
                    .pointer("/params/sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let config_id = request.pointer("/params/configId").and_then(Value::as_str);
                let mode = request
                    .pointer("/params/value")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if config_id != Some("mode") || !["default", "auto", "acceptEdits"].contains(&mode)
                {
                    emit(
                        &output,
                        json!({"jsonrpc":"2.0", "id":request_id,
                        "error":{"code":-32602,"message":"Unsupported permission mode"}}),
                    );
                } else {
                    session_modes.insert(session_id.into(), mode.into());
                    emit(
                        &output,
                        json!({"jsonrpc":"2.0", "id":request_id,
                        "result":{"configOptions":permission_config(mode)}}),
                    );
                }
            }
            "session/delete" => emit(
                &output,
                json!({"jsonrpc": "2.0", "id": request_id, "result": {}}),
            ),
            "session/prompt" => {
                // Stands in for an agent whose selected model has no vision:
                // ACP has no way to say so ahead of time, so the refusal can
                // only arrive here, at prompt time.
                if !image_input_supported() && prompt_carries_an_image(&request) {
                    emit(
                        &output,
                        json!({
                            "jsonrpc": "2.0", "id": request_id,
                            "error": {
                                "code": -32602,
                                "message": "The current model does not support image input"
                            }
                        }),
                    );
                    continue;
                }
                let session_id = request["params"]["sessionId"]
                    .as_str()
                    .expect("prompt sessionID")
                    .to_string();
                let plan = take_prompt_plan(control_dir.as_deref());
                log_request(
                    request_log.as_deref(),
                    &json!({"testPromptBehavior": plan.behavior.wire_value()}),
                );
                if matches!(plan.behavior, PromptBehavior::NeverAcknowledge) {
                    continue;
                }
                if matches!(plan.behavior, PromptBehavior::AuthRequired) {
                    // Verbatim claude-agent-acp: the failure is narrated as an
                    // agent message chunk first, then rejected as a plain
                    // internal error whose category lives in `data.errorKind`.
                    // Nothing was accepted; the chunk is the error, not output.
                    emit(
                        &output,
                        json!({
                            "jsonrpc": "2.0", "method": "session/update",
                            "params": {
                                "sessionId": session_id,
                                "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "content": {
                                        "type": "text",
                                        "text": "Failed to authenticate: OAuth session expired and could not be refreshed"
                                    }
                                }
                            }
                        }),
                    );
                    emit(
                        &output,
                        json!({
                            "jsonrpc": "2.0", "id": request_id,
                            "error": {
                                "code": -32603,
                                "message": "Internal error: Failed to authenticate: OAuth session expired and could not be refreshed",
                                "data": {"errorKind": "authentication_failed"}
                            }
                        }),
                    );
                    continue;
                }
                if matches!(plan.behavior, PromptBehavior::CallMcpTool) {
                    let server = plan.server.clone().expect("callMcpTool names a server");
                    let tool = plan.tool.clone().expect("callMcpTool names a tool");
                    let arguments = plan.arguments.clone().unwrap_or_else(|| json!({}));
                    let entry = mcp_entries
                        .get(&session_id)
                        .and_then(|entries| entries.get(&server))
                        .cloned();
                    let output = Arc::clone(&output);
                    // Off the reading loop: the proxy may ask the client
                    // something mid-call, which this loop must still read.
                    thread::spawn(move || {
                        call_mcp_tool(
                            &output,
                            request_id,
                            &session_id,
                            &server,
                            &tool,
                            &arguments,
                            entry,
                        );
                    });
                    continue;
                }
                let message_id = format!("message-{}", request_id.as_ref().unwrap_or(&Value::Null));
                if matches!(plan.behavior, PromptBehavior::Stream) {
                    // Streamed off the reading loop, so the fake keeps answering
                    // requests while a turn is in flight. Doing it inline meant
                    // `session/cancel` was not merely ignored — it was never
                    // read until the whole reply had gone out, which no real
                    // agent does and which hid the cost of prompting a session
                    // that is still busy.
                    let (sender, receiver) = mpsc::channel();
                    held.lock()
                        .expect("held prompts lock")
                        .insert(session_id.clone(), sender);
                    let output = Arc::clone(&output);
                    let held = Arc::clone(&held);
                    thread::spawn(move || {
                        let mut cancelled = false;
                        for chunk in split_into_chunks(&plan.reply, plan.chunk_bytes) {
                            if receiver.try_recv().is_ok() {
                                cancelled = true;
                                break;
                            }
                            emit(
                                &output,
                                json!({
                                    "jsonrpc": "2.0", "method": "session/update",
                                    "params": {
                                        "sessionId": session_id,
                                        "update": {
                                            "sessionUpdate": "agent_message_chunk",
                                            "messageId": message_id,
                                            "content": {"type": "text", "text": chunk}
                                        }
                                    }
                                }),
                            );
                            if plan.chunk_delay_millis > 0 {
                                thread::sleep(Duration::from_millis(plan.chunk_delay_millis));
                            }
                        }
                        // Commit to an answer under the lock the cancellation
                        // takes. Once the sender is gone the turn can no longer
                        // be cancelled, and anything that beat it is already in
                        // the channel — including a cancellation that landed
                        // while the last chunk's delay was being slept off,
                        // which the loop had no iteration left to notice.
                        held.lock().expect("held prompts lock").remove(&session_id);
                        if !cancelled {
                            cancelled = receiver.try_recv().is_ok();
                        }
                        if cancelled {
                            // A cancelled turn is not resolved the instant the
                            // notification lands: the agent stops what it is
                            // doing first, and its session stays busy until it
                            // answers.
                            thread::sleep(cancel_settle_delay());
                        }
                        emit(
                            &output,
                            json!({
                                "jsonrpc": "2.0", "id": request_id,
                                "result": {
                                    "stopReason": if cancelled { "cancelled" } else { "end_turn" }
                                }
                            }),
                        );
                    });
                    continue;
                }
                if matches!(plan.behavior, PromptBehavior::SubagentThreads) {
                    let recording: Value = serde_json::from_str(include_str!(
                        "../../ottam-core/src/agent/fixtures/claude-subagents-0.85.1.json"
                    ))
                    .expect("recorded subagent turn");
                    let frames: Vec<Value> = recording["claudeAsyncParallel"]
                        .as_array()
                        .expect("recorded frames")
                        .iter()
                        .filter(|frame| frame["method"] == "session/update")
                        .map(|frame| {
                            let mut frame = frame.clone();
                            frame["params"]["sessionId"] = json!(session_id);
                            frame
                        })
                        .collect();
                    // Hold where the nested subagent launches: the outer
                    // thread has one child call and is still running.
                    let split = frames
                        .iter()
                        .position(|frame| {
                            frame["params"]["update"]["toolCallId"]
                                == "toolu_01SvuHrKZ1tbgaM9bUsFEoMT"
                        })
                        .expect("nested launch");
                    let (held_frames, rest) = frames.split_at(split);
                    for frame in held_frames {
                        emit(&output, frame.clone());
                    }
                    let rest = rest.to_vec();
                    let output = Arc::clone(&output);
                    let control_dir = control_dir.clone();
                    thread::spawn(move || {
                        if let Some(control_dir) = control_dir {
                            if !wait_for_marker(&control_dir, "subagents-complete") {
                                return;
                            }
                        }
                        for frame in rest {
                            emit(&output, frame);
                        }
                        emit(
                            &output,
                            json!({"jsonrpc": "2.0", "id": request_id, "result": {"stopReason": "end_turn"}}),
                        );
                    });
                    continue;
                }
                let scenario = match plan.behavior {
                    PromptBehavior::NativeSubagentTurn => plan.scenario.clone(),
                    PromptBehavior::NativeSubagentFirstRun => Some("NativeFirstRun".into()),
                    PromptBehavior::NativeSubagentResumed => Some("NativeResumed".into()),
                    _ => None,
                };
                if let Some(scenario) = scenario {
                    let recording = plan.recording.clone().unwrap_or_else(|| "claude".into());
                    let frames = native_subagent_frames(&recording, &scenario, &session_id);
                    let output = Arc::clone(&output);
                    let answers = Arc::clone(&answers);
                    thread::spawn(move || {
                        emit_recorded(&output, &answers, frames);
                        emit(
                            &output,
                            json!({"jsonrpc": "2.0", "id": request_id, "result": {"stopReason": "end_turn"}}),
                        );
                    });
                    continue;
                }
                if matches!(
                    plan.behavior,
                    PromptBehavior::NativeSubagents | PromptBehavior::NativeSubagentsCancelled
                ) {
                    let recording = plan.recording.clone().unwrap_or_else(|| "claude".into());
                    let cancelled =
                        matches!(plan.behavior, PromptBehavior::NativeSubagentsCancelled);
                    let frames = native_subagent_frames(
                        &recording,
                        if cancelled {
                            "NativeCancelled"
                        } else {
                            "NativeTurn"
                        },
                        &session_id,
                    );
                    // Hold before the first subagent ends: every thread is
                    // running and has reported.
                    let split = frames
                        .iter()
                        .position(|frame| {
                            frame["params"]["update"]["sessionUpdate"] == "subagent_state_update"
                        })
                        .expect("a subagent ends");
                    let (held_frames, rest) = frames.split_at(split);
                    let (held_frames, rest) = (held_frames.to_vec(), rest.to_vec());
                    let (cancel_sender, cancel) = mpsc::channel();
                    if cancelled {
                        held.lock()
                            .expect("held prompts lock")
                            .insert(session_id.clone(), cancel_sender);
                    }
                    let output = Arc::clone(&output);
                    let answers = Arc::clone(&answers);
                    let control_dir = control_dir.clone();
                    thread::spawn(move || {
                        emit_recorded(&output, &answers, held_frames);
                        if cancelled {
                            if cancel.recv_timeout(Duration::from_secs(30)).is_err() {
                                return;
                            }
                        } else if let Some(control_dir) = control_dir {
                            if !wait_for_marker(&control_dir, "subagents-complete") {
                                return;
                            }
                        }
                        emit_recorded(&output, &answers, rest);
                        emit(
                            &output,
                            json!({"jsonrpc": "2.0", "id": request_id, "result": {
                                "stopReason": if cancelled { "cancelled" } else { "end_turn" }
                            }}),
                        );
                    });
                    continue;
                }
                // A follow-up's reply is what the agent says once its task
                // ends, not part of the turn that started it.
                if !matches!(plan.behavior, PromptBehavior::AsyncTasksFollowUp) {
                    emit(
                        &output,
                        json!({
                            "jsonrpc": "2.0", "method": "session/update",
                            "params": {
                                "sessionId": session_id,
                                "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "messageId": message_id,
                                    "content": {"type": "text", "text": plan.reply}
                                }
                            }
                        }),
                    );
                }
                if matches!(plan.behavior, PromptBehavior::CaptureLoss) {
                    // Exceed the metadata budget deterministically, then replace
                    // the title so the screenshot shows the loss marker itself.
                    for update in [
                        json!({"sessionUpdate":"tool_call","toolCallId":"loss-1","title":"x".repeat(5000),"kind":"read","status":"in_progress","rawInput":{}}),
                        json!({"sessionUpdate":"tool_call_update","toolCallId":"loss-1","title":"Inspect retained output","status":"completed"}),
                        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"This output follows the capture gap."}}),
                    ] {
                        emit(
                            &output,
                            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":update}}),
                        );
                    }
                }
                if matches!(plan.behavior, PromptBehavior::AsyncTasks) {
                    let fixtures: Value = serde_json::from_str(include_str!(
                        "../../ottam-core/src/agent/fixtures/async-tasks.json"
                    ))
                    .expect("async task frames");
                    for adapter in ["claude", "codex"] {
                        for frame in fixtures[adapter].as_array().expect("adapter frames") {
                            let mut frame = frame.clone();
                            frame["params"]["sessionId"] = json!(session_id);
                            emit(&output, frame);
                        }
                    }
                }
                if matches!(plan.behavior, PromptBehavior::AsyncTasksAfterTurn) {
                    let update = {
                        let session_id = session_id.clone();
                        move |update: Value| json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":update}})
                    };
                    emit(
                        &output,
                        update(
                            json!({"sessionUpdate":"tool_call","toolCallId":"call_after_turn","kind":"execute","title":"npm run dev","status":"in_progress"}),
                        ),
                    );
                    let output = Arc::clone(&output);
                    let control_dir = control_dir
                        .clone()
                        .expect("after-turn tasks need the fake ACP control directory");
                    thread::spawn(move || {
                        if !wait_for_marker(&control_dir, "after-turn-spawn") {
                            return;
                        }
                        emit(
                            &output,
                            update(
                                json!({"sessionUpdate":"tool_call_update","toolCallId":"call_after_turn","_meta":{"jetbrains":{"air":{"asyncTasks":{"backgrounded":true}}}}}),
                            ),
                        );
                        emit(
                            &output,
                            update(
                                json!({"sessionUpdate":"async_task_spawned","asyncTaskId":"call_after_turn","name":"npm run dev","taskType":"shell","canStop":true,"toolCallId":"call_after_turn"}),
                            ),
                        );
                        if !wait_for_marker(&control_dir, "after-turn-stop") {
                            return;
                        }
                        emit(
                            &output,
                            update(
                                json!({"sessionUpdate":"async_task_state_update","asyncTaskId":"call_after_turn","state":"stopped","toolCallId":"call_after_turn"}),
                            ),
                        );
                    });
                }
                if matches!(plan.behavior, PromptBehavior::AsyncTasksFollowUp) {
                    let update = {
                        let session_id = session_id.clone();
                        move |update: Value| json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":update}})
                    };
                    // Ids are unique per session in a real adapter; the
                    // first prompt keeps the plain ones tests name.
                    follow_up_counter += 1;
                    let suffix = match follow_up_counter {
                        1 => String::new(),
                        n => format!("_{n}"),
                    };
                    let (call, task) = (
                        format!("toolu_follow_up{suffix}"),
                        format!("task_follow_up{suffix}"),
                    );
                    for frame in [
                        json!({"sessionUpdate":"tool_call","toolCallId":call,"kind":"execute","title":"sleep 30; echo done","status":"in_progress","rawInput":{"command":"sleep 30; echo done","run_in_background":true}}),
                        json!({"sessionUpdate":"async_task_spawned","asyncTaskId":task,"name":"sleep 30; echo done","taskType":"shell","canStop":true,"toolCallId":call}),
                        json!({"sessionUpdate":"tool_call_update","toolCallId":call,"status":"completed","_meta":{"jetbrains":{"air":{"asyncTasks":{"backgrounded":true}}}}}),
                        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Started it in the background."}}),
                    ] {
                        emit(&output, update(frame));
                    }
                    live_tasks
                        .lock()
                        .expect("live tasks lock")
                        .insert((session_id.clone(), task.clone()));
                    let output = Arc::clone(&output);
                    let live_tasks = Arc::clone(&live_tasks);
                    let control_dir = control_dir
                        .clone()
                        .expect("follow-ups need the fake ACP control directory");
                    let key = (session_id.clone(), task.clone());
                    let chunks = split_into_chunks(&plan.reply, plan.chunk_bytes);
                    let delay = Duration::from_millis(plan.chunk_delay_millis);
                    thread::spawn(move || {
                        if !wait_for_marker(&control_dir, "follow-up-complete") {
                            return;
                        }
                        if !live_tasks.lock().expect("live tasks lock").remove(&key) {
                            return;
                        }
                        emit(
                            &output,
                            update(
                                json!({"sessionUpdate":"async_task_state_update","asyncTaskId":task,"state":"completed","summary":"Exited with code 0"}),
                            ),
                        );
                        for chunk in chunks {
                            thread::sleep(delay);
                            emit(
                                &output,
                                update(
                                    json!({"sessionUpdate":"agent_message_chunk","messageId":format!("follow-up-reply{suffix}"),"content":{"type":"text","text":chunk}}),
                                ),
                            );
                        }
                        emit(
                            &output,
                            update(
                                json!({"sessionUpdate":"usage_update","used":1200,"size":200000,"_meta":{"_claude/origin":{"kind":"task-notification"}}}),
                            ),
                        );
                    });
                }
                if matches!(
                    plan.behavior,
                    PromptBehavior::AskQuestion | PromptBehavior::AskQuestionWithdrawn
                ) {
                    let outbound = format!("ottam-test-question-{session_id}");
                    let custom = json!({"questionId": "question_0", "isCustomAnswer": true});
                    emit(
                        &output,
                        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":{"sessionUpdate":"tool_call","toolCallId":"toolu_question","title":"AskUserQuestion","kind":"other","status":"in_progress"}}}),
                    );
                    emit(
                        &output,
                        json!({"jsonrpc":"2.0","id":outbound,"method":"elicitation/create","params":{
                            "mode": "form",
                            "sessionId": session_id,
                            "toolCallId": "toolu_question",
                            "message": "Please answer the following questions.",
                            "requestedSchema": {"type": "object", "properties": {
                                "question_0": {"type": "string", "title": "Approach", "description": "Which approach should I take?",
                                    "oneOf": [
                                        {"const": "Refactor", "title": "Refactor", "description": "Smaller diff."},
                                        {"const": "Rewrite", "title": "Rewrite"}
                                    ]},
                                "question_0_custom": {"type": "string", "title": "Other",
                                    "_meta": {"jetbrains": {"air": {"version": 1, "customAnswer": custom}}}}
                            }}
                        }}),
                    );
                    if matches!(plan.behavior, PromptBehavior::AskQuestionWithdrawn) {
                        let output = Arc::clone(&output);
                        let control_dir =
                            control_dir.clone().expect("withdrawal control directory");
                        let withdrawn = outbound.clone();
                        thread::spawn(move || {
                            if wait_for_marker(&control_dir, "withdraw-question") {
                                emit(
                                    &output,
                                    json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":withdrawn}}),
                                );
                            }
                        });
                    }
                    questions.insert(outbound, (request_id, session_id));
                    continue;
                }
                if matches!(plan.behavior, PromptBehavior::UnhandledTraffic) {
                    let outbound = format!("ottam-test-unclaimed-{}", session_id);
                    emit(
                        &output,
                        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":{"sessionUpdate":"brand_new_kind","secretPayload":"PRIVATE-TEXT"}}}),
                    );
                    emit(
                        &output,
                        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Visible despite an extra field."},"brandNewField":"PRIVATE-TEXT"}}}),
                    );
                    emit(
                        &output,
                        json!({"jsonrpc":"2.0","method":"_ottam_test/unclaimed","params":{"sessionId":session_id,"note":"PRIVATE-TEXT"}}),
                    );
                    emit(
                        &output,
                        json!({"jsonrpc":"2.0","id":outbound,"method":"fs/read_text_file","params":{"sessionId":session_id,"path":"/tmp/ottam-test-unclaimed"}}),
                    );
                    unclaimed_requests.insert(outbound, (request_id, session_id));
                    continue;
                }
                if matches!(plan.behavior, PromptBehavior::NormalizedTranscript) {
                    let read_path = std::env::current_dir()
                        .expect("fixture working directory")
                        .join("docs/architecture/backend/core.md");
                    for update in [
                        json!({"sessionUpdate":"agent_thought_chunk","messageId":"shared-reasoning","content":{"type":"text","text":"I will check the requested change before continuing."}}),
                        json!({"sessionUpdate":"tool_call","toolCallId":"read-1","title":format!("Read file '{}'", read_path.display()),"kind":"read","status":"completed","rawInput":{"path":read_path}}),
                        json!({"sessionUpdate":"tool_call","toolCallId":"write-1","title":"Requested file change","kind":"edit","status":"in_progress","rawInput":{"path":"example.txt","content":"NOT CAPTURED INPUT"}}),
                        json!({"sessionUpdate":"tool_call_update","toolCallId":"write-1","status":"failed","rawOutput":"NOT CAPTURED OUTPUT"}),
                    ] {
                        emit(
                            &output,
                            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":update}}),
                        );
                    }
                }
                if matches!(plan.behavior, PromptBehavior::TranscriptHistory) {
                    let notify = |update: Value| {
                        emit(
                            &output,
                            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":update}}),
                        );
                    };
                    notify(
                        json!({"sessionUpdate":"plan","entries":[{"content":"Oldest retained plan: history reached the beginning.","priority":"medium","status":"completed"}]}),
                    );
                    notify(
                        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"The oldest retained response is present."}}),
                    );
                    // Unpaced, like an agent replaying a long history.
                    for index in 0..1200 {
                        notify(
                            json!({"sessionUpdate":"tool_call","toolCallId":format!("history-{index}"),"title":format!("Inspect retained activity {index}"),"kind":"execute","status":"completed"}),
                        );
                    }
                    notify(
                        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"The saved assistant response is intact. Returning to this conversation should use the whole reading column, including beside the tools pane."}}),
                    );
                    notify(
                        json!({"sessionUpdate":"tool_call","toolCallId":"history-reference","title":"Inspect unsupported image reference","kind":"read","status":"completed","content":[{"type":"content","content":{"type":"resource_link","name":"Image reference","uri":"https://example.invalid/image.png","mimeType":"image/png"}}]}),
                    );
                }
                if matches!(plan.behavior, PromptBehavior::TranscriptPayloads) {
                    for update in [
                        json!({"sessionUpdate":"plan","entries":[{"content":"Inspect saved payloads","priority":"medium","status":"in_progress"}]}),
                        json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"Check the saved details\nThe requested edit and its result are separate records. This longer reasoning paragraph wraps across several lines so the expanded card grows to fit the discussion before reaching its scrolling limit. Keeping the explanation in one paragraph also exercises wrapped-text measurement rather than newline counting."}}),
                        json!({"sessionUpdate":"tool_call","toolCallId":"payload-edit","title":"Update greeting","kind":"edit","status":"failed","rawInput":{"file_path":"greeting.swift","old_string":"let greeting = \"Hello\"","new_string":"let greeting = \"Welcome\""},"rawOutput":"Permission denied; the requested edit was not applied."}),
                        json!({"sessionUpdate":"tool_call","toolCallId":"payload-missing","title":"Inspect image reference","kind":"read","status":"completed","content":[{"type":"content","content":{"type":"image","mimeType":"image/png","data":"","uri":"https://example.invalid/unavailable.png"}}]}),
                        json!({"sessionUpdate":"tool_call","toolCallId":"payload-image","title":"Inspect captured image","kind":"read","status":"completed","content":[{"type":"content","content":{"type":"image","mimeType":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAL0lEQVR4nO3OIQEAAAgDMDIRlpw0gBg3E/Ornr2kEhAQEBAQEBAQEBAQEBAQSAceb2Pcl6j00g8AAAAASUVORK5CYII="}}]}),
                        json!({"sessionUpdate":"tool_call","toolCallId":"payload-limited","title":"Inspect partial output","kind":"read","status":"completed","rawOutput":"Captured output line\n".repeat(850_000)}),
                        json!({"sessionUpdate":"tool_call","toolCallId":"payload-shell","title":"Run payload fixture","kind":"execute","status":"in_progress","rawInput":{"command":"printf 'first line'\nprintf 'second line'"},"_meta":{"terminal_info":{"terminal_id":"payload-shell"},"terminal_output":{"terminal_id":"payload-shell","data":"First line of locally captured output.\n"}}}),
                    ] {
                        emit(
                            &output,
                            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":update}}),
                        );
                    }
                    let output = Arc::clone(&output);
                    thread::spawn(move || {
                        thread::sleep(Duration::from_secs(45));
                        emit(
                            &output,
                            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":{"sessionUpdate":"tool_call_update","toolCallId":"payload-shell","status":"completed","_meta":{"terminal_output":{"terminal_id":"payload-shell","data":"Final snapshot saved successfully."},"terminal_exit":{"terminal_id":"payload-shell","exit_code":0,"signal":null}}}}}),
                        );
                        for update in [
                            json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"One final file check."}}),
                            json!({"sessionUpdate":"tool_call","toolCallId":"payload-single","title":"Read summary.txt","kind":"read","status":"completed","rawInput":{"path":"summary.txt"},"rawOutput":"Single tool details."}),
                        ] {
                            emit(
                                &output,
                                json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":update}}),
                            );
                        }
                        emit(
                            &output,
                            json!({"jsonrpc":"2.0","id":request_id,"result":{"stopReason":"end_turn"}}),
                        );
                    });
                    continue;
                }
                if matches!(
                    plan.behavior,
                    PromptBehavior::SessionRetry
                        | PromptBehavior::SessionLimit
                        | PromptBehavior::SessionFailure
                ) {
                    let fixtures: Vec<Value> = serde_json::from_str(include_str!(
                        "../../ottam-core/src/agent/fixtures/session-signals.json"
                    ))
                    .expect("synthetic signals");
                    let index = match plan.behavior {
                        PromptBehavior::SessionRetry => 3,
                        PromptBehavior::SessionLimit => 4,
                        _ => 5,
                    };
                    emit(
                        &output,
                        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":fixtures[index]}}),
                    );
                }
                if matches!(plan.behavior, PromptBehavior::ChangingTools) {
                    let (sender, receiver) = mpsc::channel();
                    held.lock()
                        .expect("held prompts lock")
                        .insert(session_id.clone(), sender);
                    let output = Arc::clone(&output);
                    let held = Arc::clone(&held);
                    thread::spawn(move || {
                        let stages = [
                            ("read", "Read configuration", "read"),
                            ("read", "Read configuration and workspace preferences", "read"),
                            ("search", "Search source files for animation lifecycle", "search"),
                            ("run", "Run step 12", "execute"),
                            ("run", "Run step 21", "execute"),
                            ("build", "Inspect very long command: swift build --package-path app --scratch-path .build/diagnostic-animation-validation --product OttamApp --configuration release --verbose --enable-code-coverage --sanitize thread --experimental-explicit-module-build --disable-automatic-resolution --force-resolved-versions --manifest-cache none --triple arm64-apple-macosx26.0 --jobs 2", "execute"),
                            ("build", "Build", "execute"),
                        ];
                        let mut previous = "";
                        for (id, title, kind) in stages {
                            if !previous.is_empty() && previous != id {
                                emit(
                                    &output,
                                    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":{"sessionUpdate":"tool_call_update","toolCallId":previous,"status":"completed"}}}),
                                );
                            }
                            let event = if previous == id {
                                "tool_call_update"
                            } else {
                                "tool_call"
                            };
                            emit(
                                &output,
                                json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":{"sessionUpdate":event,"toolCallId":id,"title":title,"kind":kind,"status":"in_progress"}}}),
                            );
                            previous = id;
                            if receiver.recv_timeout(Duration::from_secs(6)).is_ok() {
                                emit(
                                    &output,
                                    json!({"jsonrpc":"2.0","id":request_id,"result":{"stopReason":"cancelled"}}),
                                );
                                return;
                            }
                        }
                        emit(
                            &output,
                            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":{"sessionUpdate":"tool_call_update","toolCallId":previous,"status":"completed"}}}),
                        );
                        emit(
                            &output,
                            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"\nTool sequence completed."}}}}),
                        );
                        held.lock().expect("held prompts lock").remove(&session_id);
                        emit(
                            &output,
                            json!({"jsonrpc":"2.0","id":request_id,"result":{"stopReason":"end_turn"}}),
                        );
                    });
                    continue;
                }
                if matches!(plan.behavior, PromptBehavior::HoldToolUntilCancel) {
                    emit(
                        &output,
                        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":{"sessionUpdate":"tool_call","toolCallId":"idle-tool","title":"Waiting for diagnostic tool","kind":"execute","status":"in_progress","rawInput":{"command":"diagnostic fixture: wait until canceled"}}}}),
                    );
                }
                if matches!(plan.behavior, PromptBehavior::RequestPermission) {
                    // A long single-line command on purpose: the approval card
                    // wraps it, which is what makes the pinned composer bar
                    // tall enough to matter for layout.
                    permission_counter += 1;
                    emit(
                        &output,
                        json!({
                            "jsonrpc": "2.0",
                            "id": format!("ottam-test-permission-{permission_counter}"),
                            "method": "session/request_permission",
                            "params": {
                                "sessionId": session_id,
                                "toolCall": {
                                    "toolCallId": format!("call-{permission_counter}"),
                                    "title": "rm -rf .build/run-app/canonical/tmp/ottam-inspect-JrgZhZ; ls scripts/ | head -20",
                                    "kind": "execute"
                                },
                                "options": [
                                    {"optionId": "reject-once", "name": "Deny", "kind": "reject_once"},
                                    {"optionId": "allow-once", "name": "Allow Once", "kind": "allow_once"},
                                    {"optionId": "allow-always", "name": "Always Allow", "kind": "allow_always"}
                                ]
                            }
                        }),
                    );
                }

                if matches!(
                    plan.behavior,
                    PromptBehavior::Usage | PromptBehavior::UsageCancelled
                ) {
                    emit(
                        &output,
                        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":{"sessionUpdate":"usage_update","used":plan.context_used,"size":plan.context_size}}}),
                    );
                }

                match plan.behavior {
                    PromptBehavior::Usage => emit(
                        &output,
                        json!({"jsonrpc":"2.0","id":request_id,"result":{"stopReason":"end_turn","usage":turn_usage()}}),
                    ),
                    PromptBehavior::UsageCancelled => {
                        let (sender, receiver) = mpsc::channel();
                        held.lock()
                            .expect("held prompts lock")
                            .insert(session_id, sender);
                        let output = Arc::clone(&output);
                        thread::spawn(move || {
                            let _ = receiver.recv();
                            // Late enough that the stopped run is already final.
                            thread::sleep(cancel_settle_delay().max(Duration::from_millis(300)));
                            emit(
                                &output,
                                json!({"jsonrpc":"2.0","id":request_id,"result":{"stopReason":"cancelled","usage":turn_usage()}}),
                            );
                        });
                    }
                    PromptBehavior::SessionFailure => {
                        let fixtures: Vec<Value> = serde_json::from_str(include_str!(
                            "../../ottam-core/src/agent/fixtures/session-signals.json"
                        ))
                        .expect("synthetic signals");
                        emit(
                            &output,
                            json!({"jsonrpc":"2.0","id":request_id,"result":{"stopReason":"end_turn","_meta":fixtures[5]["_meta"]}}),
                        );
                    }
                    PromptBehavior::UsageLimitFailure => emit(
                        &output,
                        json!({"jsonrpc":"2.0","id":request_id,"error":{
                            "code":-32603,"message":"Internal error: Go usage limit exceeded"
                        }}),
                    ),
                    PromptBehavior::Complete
                    | PromptBehavior::TranscriptHistory
                    | PromptBehavior::CaptureLoss
                    | PromptBehavior::AsyncTasks
                    | PromptBehavior::AsyncTasksAfterTurn
                    | PromptBehavior::AsyncTasksFollowUp => emit(
                        &output,
                        json!({"jsonrpc": "2.0", "id": request_id, "result": {"stopReason": "end_turn"}}),
                    ),
                    PromptBehavior::NeverAcknowledge
                    | PromptBehavior::AuthRequired
                    | PromptBehavior::Stream
                    | PromptBehavior::SubagentThreads
                    | PromptBehavior::NativeSubagents
                    | PromptBehavior::NativeSubagentsCancelled
                    | PromptBehavior::NativeSubagentTurn
                    | PromptBehavior::NativeSubagentFirstRun
                    | PromptBehavior::NativeSubagentResumed
                    | PromptBehavior::UnhandledTraffic
                    | PromptBehavior::AskQuestion
                    | PromptBehavior::AskQuestionWithdrawn
                    | PromptBehavior::CallMcpTool => unreachable!(),
                    PromptBehavior::TranscriptPayloads | PromptBehavior::ChangingTools => {
                        unreachable!()
                    }
                    PromptBehavior::ExitAfterUpdate => process::exit(17),
                    PromptBehavior::HoldUntilCancel
                    | PromptBehavior::HoldToolUntilCancel
                    | PromptBehavior::RequestPermission
                    | PromptBehavior::SessionRetry
                    | PromptBehavior::SessionLimit
                    | PromptBehavior::NormalizedTranscript => {
                        let (sender, receiver) = mpsc::channel();
                        held.lock()
                            .expect("held prompts lock")
                            .insert(session_id, sender);
                        let output = Arc::clone(&output);
                        thread::spawn(move || {
                            let _ = receiver.recv();
                            thread::sleep(cancel_settle_delay());
                            emit(
                                &output,
                                json!({
                                    "jsonrpc": "2.0", "id": request_id,
                                    "result": {"stopReason": "cancelled"}
                                }),
                            );
                        });
                    }
                }
            }
            "_session/steering" if steering_supported() => {
                let session_id = request["params"]["sessionId"]
                    .as_str()
                    .expect("steer sessionID")
                    .to_string();
                let text = request["params"]["prompt"]
                    .as_array()
                    .and_then(|blocks| blocks.iter().find(|block| block["type"] == "text"))
                    .and_then(|block| block["text"].as_str())
                    .unwrap_or_default()
                    .to_string();
                let asked_for_the_message_back =
                    request["params"]["_meta"]["steering"]["idleBehavior"].as_str()
                        == Some("promptRequired");
                let turn_is_running = held
                    .lock()
                    .expect("held prompts lock")
                    .contains_key(&session_id);
                let outcome = match forced_steer_outcome() {
                    Some(forced) => forced,
                    None if turn_is_running => "injected".to_string(),
                    // claude-agent-acp's split: the host that opted in gets its
                    // message back, and one that did not gets a turn it never
                    // asked for, which is what codex-acp always does.
                    None if asked_for_the_message_back => "promptRequired".to_string(),
                    None => "startedNewTurn".to_string(),
                };
                if outcome == "methodNotFound" {
                    emit(
                        &output,
                        json!({
                            "jsonrpc": "2.0", "id": request_id,
                            "error": {
                                "code": -32601,
                                "message": "unsupported fake ACP method: _session/steering"
                            }
                        }),
                    );
                    continue;
                }
                if outcome == "injected" {
                    // The steered message's own output streams as an ordinary
                    // update rather than through this response, so the running
                    // turn's body grows to cover it.
                    emit(
                        &output,
                        json!({
                            "jsonrpc": "2.0", "method": "session/update",
                            "params": {
                                "sessionId": session_id,
                                "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "content": {"type": "text", "text": format!(" Steered: {text}")}
                                }
                            }
                        }),
                    );
                }
                let mut result = json!({"outcome": outcome});
                if outcome == "promptRequired" {
                    result["reason"] = json!("noRunningTurn");
                }
                emit(
                    &output,
                    json!({"jsonrpc": "2.0", "id": request_id, "result": result}),
                );
            }
            // Both adapters' stop request: Claude's `stopTask`, Codex's
            // background-terminal terminate. A task that already ended
            // answers `stopped: false`.
            "_session/async_task/stop" => {
                let session_id = request["params"]["sessionId"].as_str().unwrap_or_default();
                let task = request["params"]["asyncTaskId"]
                    .as_str()
                    .unwrap_or_default();
                let stopped = live_tasks
                    .lock()
                    .expect("live tasks lock")
                    .remove(&(session_id.to_string(), task.to_string()));
                if stopped {
                    emit(
                        &output,
                        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session_id,"update":{"sessionUpdate":"async_task_state_update","asyncTaskId":task,"state":"stopped"}}}),
                    );
                }
                emit(
                    &output,
                    json!({"jsonrpc": "2.0", "id": request_id, "result": {"stopped": stopped}}),
                );
            }
            "session/cancel" => {
                if let Some(session_id) = request["params"]["sessionId"].as_str() {
                    if let Some(sender) = held.lock().expect("held prompts lock").remove(session_id)
                    {
                        let _ = sender.send(());
                    }
                }
            }
            _ => emit(
                &output,
                json!({
                    "jsonrpc": "2.0", "id": request_id,
                    "error": {"code": -32601, "message": format!("unsupported fake ACP method: {method}")}
                }),
            ),
        }
    }
}
