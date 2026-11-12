//! ottam-inspect: query a ottam-core headlessly.
//!
//! Attach mode (default) connects to the core the running app owns via
//! `$OTTAM_HOME/core.sock` (or its short `/tmp` stand-in for deep homes,
//! see [`socket_path`]) — real data, live state. `--spawn` starts a
//! fresh core against a scratch home instead. `scripts/inspect-core.sh` is
//! the blessed entry point; see `.claude/skills/core-inspect/`.

mod client;
mod spawn;

use client::{Connection, Incoming};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io::BufRead;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

// sysexits(3) subset.
const EX_USAGE: u8 = 64;
const EX_UNAVAILABLE: u8 = 69;
const EX_SOFTWARE: u8 = 70;

const SOCKET_FILE_NAME: &str = "core.sock";
/// macOS caps `sun_path` at 104 bytes including the terminator (Linux: 108).
const MAX_SOCKET_PATH_BYTES: usize = 103;
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// The core's attach socket for `home`. Mirrors `socket_path` in core's
/// `server/attach.rs`; the shared test vector keeps the two in step.
fn socket_path(home: &Path) -> PathBuf {
    let direct = home.join(SOCKET_FILE_NAME);
    if direct.as_os_str().len() <= MAX_SOCKET_PATH_BYTES {
        return direct;
    }
    let canonical = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    // SAFETY: getuid has no preconditions and cannot fail.
    short_socket_path(&canonical, unsafe { libc::getuid() })
}

fn short_socket_path(canonical_home: &Path, uid: u32) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let hash = canonical_home
        .as_os_str()
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    PathBuf::from(format!("/tmp/ottam-{uid}/{hash:016x}.sock"))
}

const USAGE: &str = "usage: ottam-inspect <command> [options]

commands:
  status                       is a core running for this home, and which
  call <method> [params-json]  one request, pretty-printed result
  conversation <id> [method] [params-json]
                               call a conversation.* method (default: get) given
                               only a conversation ID; resolves its workspace
  listen                       stream notifications as JSONL until ^C
  repl                         raw pipe: JSONL on stdin, everything on stdout

options:
  --home <path>      OTTAM_HOME to inspect (default: $OTTAM_HOME, then ~/.ottam)
  --spawn            spawn a fresh core against a scratch home instead of attaching
  --core-bin <path>  ottam-core binary for --spawn (default: $OTTAM_CORE_PATH, then sibling)
  --keep-home        keep the scratch home after --spawn exits
  --copy-agents      clone <home>/agents (installed adapters) into the scratch home
  --timeout <secs>   per-request timeout (default: 30)
  --methods <a.,b.>  listen: only stream notifications whose method starts with a prefix
  --raw              call: print the whole response envelope, compact";

struct Options {
    command: Command,
    home: Option<PathBuf>,
    spawn: bool,
    core_bin: Option<PathBuf>,
    keep_home: bool,
    copy_agents: bool,
    timeout: Duration,
    method_prefixes: Vec<String>,
    raw: bool,
}

enum Command {
    Status,
    Call {
        method: String,
        params: Value,
    },
    Conversation {
        id: String,
        method: String,
        params: Value,
    },
    Listen,
    Repl,
}

fn main() -> ExitCode {
    let options = match parse_args(std::env::args().skip(1).collect()) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}\n\n{USAGE}");
            return ExitCode::from(EX_USAGE);
        }
    };
    match run(options) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("ottam-inspect: {error}");
            ExitCode::from(EX_SOFTWARE)
        }
    }
}

fn run(options: Options) -> Result<ExitCode, String> {
    match &options.command {
        Command::Status => status(&options),
        Command::Call { method, params } => {
            let (method, params) = (method.clone(), params.clone());
            call(&options, &method, params)
        }
        Command::Conversation { id, method, params } => {
            let (id, method, params) = (id.clone(), method.clone(), params.clone());
            conversation(&options, &id, &method, params)
        }
        Command::Listen => listen(&options),
        Command::Repl => repl(&options),
    }
}

/// Attach mode's connection; distinguishes "no core running" for exit codes.
fn attach(options: &Options) -> Result<Connection, ExitCode> {
    let home = resolve_home(options.home.as_deref());
    let socket = socket_path(&home);
    match UnixStream::connect(&socket) {
        Ok(stream) => {
            let reader = stream.try_clone().map_err(|error| {
                eprintln!("ottam-inspect: failed to clone socket: {error}");
                ExitCode::from(EX_SOFTWARE)
            })?;
            Ok(Connection::new(reader, stream))
        }
        Err(error) => {
            eprintln!(
                "no core is running for {} ({}: {error})\n\
                 hint: start the app, or use --spawn for a scratch core",
                home.display(),
                socket.display()
            );
            Err(ExitCode::from(EX_UNAVAILABLE))
        }
    }
}

enum Session {
    Attached(Connection),
    Spawned(spawn::SpawnedCore),
}

impl Session {
    fn open(options: &Options) -> Result<Result<Session, ExitCode>, String> {
        if options.spawn {
            let spawn_options = spawn::SpawnOptions {
                core_bin: options.core_bin.clone(),
                keep_home: options.keep_home,
                copy_agents_from: options
                    .copy_agents
                    .then(|| resolve_home(options.home.as_deref())),
            };
            let core = spawn::spawn(&spawn_options, options.timeout)?;
            Ok(Ok(Session::Spawned(core)))
        } else {
            match attach(options) {
                Ok(connection) => Ok(Ok(Session::Attached(connection))),
                Err(code) => Ok(Err(code)),
            }
        }
    }

    fn connection(&mut self) -> &mut Connection {
        match self {
            Session::Attached(connection) => connection,
            Session::Spawned(core) => &mut core.connection,
        }
    }

    fn close(self) {
        if let Session::Spawned(core) = self {
            core.shutdown();
        }
    }
}

fn status(options: &Options) -> Result<ExitCode, String> {
    let home = resolve_home(options.home.as_deref());
    let mut connection = match attach(options) {
        Ok(connection) => connection,
        Err(code) => return Ok(code),
    };
    // Initialize is side-effect-free for attached connections and is the
    // canonical "who are you" request.
    let response = connection.round_trip(&spawn::initialize_request(), options.timeout)?;
    let result = &response["result"];
    println!("core running for {}", home.display());
    println!("  socket:           {}", socket_path(&home).display());
    println!(
        "  core version:     {}",
        result["coreVersion"].as_str().unwrap_or("?")
    );
    println!(
        "  instance id:      {}",
        result["coreInstanceID"].as_str().unwrap_or("?")
    );
    Ok(ExitCode::SUCCESS)
}

fn call(options: &Options, method: &str, params: Value) -> Result<ExitCode, String> {
    let mut session = match Session::open(options)? {
        Ok(session) => session,
        Err(code) => return Ok(code),
    };
    let request = json!({"id": "inspect/1", "method": method, "params": params});
    let response = session.connection().round_trip(&request, options.timeout)?;
    session.close();
    Ok(print_response(options, &response))
}

/// Conversation IDs are globally unique, but every `conversation.*` method is
/// scoped to a workspace. Find the owning workspace, then make the scoped call.
fn conversation(
    options: &Options,
    conversation_id: &str,
    method: &str,
    params: Value,
) -> Result<ExitCode, String> {
    let mut session = match Session::open(options)? {
        Ok(session) => session,
        Err(code) => return Ok(code),
    };
    let result = resolve_and_call(
        session.connection(),
        options,
        conversation_id,
        method,
        params,
    );
    session.close();
    result
}

fn resolve_and_call(
    connection: &mut Connection,
    options: &Options,
    conversation_id: &str,
    method: &str,
    params: Value,
) -> Result<ExitCode, String> {
    let workspaces = connection.round_trip(
        // Archived workspaces keep their conversations, and a copied ID may
        // outlive the workspace's place in the sidebar.
        &json!({
            "id": "inspect/workspaces",
            "method": "workspace.list",
            "params": {"includeArchived": true}
        }),
        options.timeout,
    )?;
    if workspaces.get("error").is_some() {
        return Ok(print_response(options, &workspaces));
    }
    let workspace_ids = workspaces["result"]["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        // An older core lists flat summaries; attach mode may talk to one.
        .filter_map(|item| {
            item["workspace"]["workspaceID"]
                .as_str()
                .or(item["workspaceID"].as_str())
        });

    // `conversation.get` doubles as the ownership probe: the core answers
    // `unknownConversationID` for a workspace that does not own the ID.
    let mut owner = None;
    for workspace_id in workspace_ids {
        let probe = connection.round_trip(
            &json!({
                "id": "inspect/probe",
                "method": "conversation.get",
                "params": {"workspaceID": workspace_id, "conversationID": conversation_id},
            }),
            options.timeout,
        )?;
        match probe.get("error") {
            None => {
                owner = Some(workspace_id.to_string());
                break;
            }
            Some(error) if error["data"]["kind"] == "unknownConversationID" => continue,
            Some(_) => return Ok(print_response(options, &probe)),
        }
    }
    let Some(workspace_id) = owner else {
        eprintln!("no workspace owns conversation {conversation_id}");
        return Ok(ExitCode::FAILURE);
    };

    let mut params = match params {
        Value::Object(map) => map,
        _ => return Err("params must be a JSON object".to_string()),
    };
    params.insert("workspaceID".to_string(), json!(workspace_id));
    params.insert("conversationID".to_string(), json!(conversation_id));
    let request = json!({"id": "inspect/1", "method": method, "params": params});
    let response = connection.round_trip(&request, options.timeout)?;
    Ok(print_response(options, &response))
}

fn print_response(options: &Options, response: &Value) -> ExitCode {
    let failed = response.get("error").is_some();
    if options.raw {
        println!("{response}");
    } else if failed {
        println!("{}", pretty(&response["error"]));
    } else {
        println!("{}", pretty(&response["result"]));
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn listen(options: &Options) -> Result<ExitCode, String> {
    let mut connection = match attach(options) {
        Ok(connection) => connection,
        Err(code) => return Ok(code),
    };
    // No timeout between events: listening is open-ended until ^C or the
    // core goes away.
    loop {
        match connection.read(Duration::from_secs(u64::MAX / 4))? {
            Incoming::Eof => {
                eprintln!("core closed the connection");
                return Ok(ExitCode::SUCCESS);
            }
            Incoming::Message(value) => {
                let is_notification = value.get("id").is_none();
                if !is_notification {
                    continue;
                }
                if !options.method_prefixes.is_empty() {
                    let method = value["method"].as_str().unwrap_or("");
                    if !options
                        .method_prefixes
                        .iter()
                        .any(|prefix| method.starts_with(prefix.as_str()))
                    {
                        continue;
                    }
                }
                println!("{value}");
            }
        }
    }
}

/// How long the drain after stdin EOF waits for responses still outstanding.
/// Bounds `echo '<request>' | ottam-inspect repl`, whose whole output
/// arrives after the last input line.
const REPL_DRAIN_GRACE: Duration = Duration::from_secs(10);

fn repl(options: &Options) -> Result<ExitCode, String> {
    let mut session = match Session::open(options)? {
        Ok(session) => session,
        Err(code) => return Ok(code),
    };
    // Two genuinely independent pumps: a thread turns stdin into a channel so
    // this loop never blocks on input, and core messages print the moment they
    // arrive. Reading stdin inline would hide every late response and
    // notification behind the next line the user happens to type.
    let (input_tx, pending_input) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if input_tx.send(line).is_err() {
                break;
            }
        }
    });

    // Ids sent but not yet answered, so EOF can drain them instead of cutting
    // the session off mid-response.
    let mut outstanding: BTreeSet<String> = BTreeSet::new();
    loop {
        match session.connection().read(Duration::from_millis(50)) {
            Ok(Incoming::Message(value)) => {
                settle(&mut outstanding, &value);
                println!("{value}");
                continue;
            }
            Ok(Incoming::Eof) => {
                eprintln!("core closed the connection");
                return Ok(ExitCode::SUCCESS);
            }
            Err(_) => {} // timeout: nothing queued right now
        }
        match pending_input.try_recv() {
            Ok(line) => {
                if line.trim().is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(&line)
                    .map_err(|error| format!("stdin line is not JSON: {error}"))?;
                if let Some(id) = value.get("id") {
                    outstanding.insert(id.to_string());
                }
                session.connection().send(&value)?;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => break,
        }
    }

    // stdin is done. Anything still outstanding was asked for, so wait for it
    // rather than closing the session out from under the answer.
    let deadline = Instant::now() + REPL_DRAIN_GRACE;
    while !outstanding.is_empty() && Instant::now() < deadline {
        match session.connection().read(Duration::from_millis(50)) {
            Ok(Incoming::Message(value)) => {
                settle(&mut outstanding, &value);
                println!("{value}");
            }
            Ok(Incoming::Eof) => break,
            Err(_) => {}
        }
    }
    if !outstanding.is_empty() {
        eprintln!(
            "gave up waiting for {} outstanding response(s) after {}s",
            outstanding.len(),
            REPL_DRAIN_GRACE.as_secs()
        );
    }
    session.close();
    Ok(ExitCode::SUCCESS)
}

/// Clear a request id once its response arrives. Notifications carry no `id`
/// and leave the set alone.
fn settle(outstanding: &mut BTreeSet<String>, message: &Value) {
    if let Some(id) = message.get("id") {
        outstanding.remove(&id.to_string());
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// `--home` > `$OTTAM_HOME` > `~/.ottam` — mirrors the core's own
/// resolution (`storage::ottam_home`).
fn resolve_home(flag: Option<&Path>) -> PathBuf {
    if let Some(path) = flag {
        return path.to_path_buf();
    }
    if let Ok(configured) = std::env::var("OTTAM_HOME") {
        let trimmed = configured.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".ottam")
}

fn parse_args(args: Vec<String>) -> Result<Options, String> {
    let mut positional = Vec::new();
    let mut home = None;
    let mut spawn = false;
    let mut core_bin = None;
    let mut keep_home = false;
    let mut copy_agents = false;
    let mut timeout = Duration::from_secs(DEFAULT_TIMEOUT_SECS);
    let mut method_prefixes = Vec::new();
    let mut raw = false;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--home" => home = Some(PathBuf::from(required_value(&arg, &mut iter)?)),
            "--spawn" => spawn = true,
            "--core-bin" => core_bin = Some(PathBuf::from(required_value(&arg, &mut iter)?)),
            "--keep-home" => keep_home = true,
            "--copy-agents" => copy_agents = true,
            "--timeout" => {
                let value = required_value(&arg, &mut iter)?;
                let secs: u64 = value
                    .parse()
                    .map_err(|_| format!("--timeout expects seconds, got {value:?}"))?;
                timeout = Duration::from_secs(secs);
            }
            "--methods" => {
                method_prefixes = required_value(&arg, &mut iter)?
                    .split(',')
                    .map(|prefix| prefix.trim().to_string())
                    .filter(|prefix| !prefix.is_empty())
                    .collect();
            }
            "--raw" => raw = true,
            "--help" | "-h" => return Err("ottam-inspect".to_string()),
            other if other.starts_with("--") => return Err(format!("unknown option {other}")),
            _ => positional.push(arg),
        }
    }

    let mut positional = positional.into_iter();
    let command = match positional.next().as_deref() {
        Some("status") => Command::Status,
        Some("call") => {
            let method = positional
                .next()
                .ok_or("call expects a method name".to_string())?;
            let params = match positional.next() {
                Some(text) => serde_json::from_str(&text)
                    .map_err(|error| format!("params are not valid JSON: {error}"))?,
                None => json!({}),
            };
            Command::Call { method, params }
        }
        Some("conversation") => {
            let id = positional
                .next()
                .ok_or("conversation expects a conversation ID".to_string())?;
            // Bare names are shorthand: `listEvents` means `conversation.listEvents`.
            let method = match positional.next() {
                Some(name) if name.contains('.') => name,
                Some(name) => format!("conversation.{name}"),
                None => "conversation.get".to_string(),
            };
            let params = match positional.next() {
                Some(text) => serde_json::from_str(&text)
                    .map_err(|error| format!("params are not valid JSON: {error}"))?,
                None => json!({}),
            };
            Command::Conversation { id, method, params }
        }
        Some("listen") => Command::Listen,
        Some("repl") => Command::Repl,
        Some(other) => return Err(format!("unknown command {other:?}")),
        None => return Err("missing command".to_string()),
    };
    if let Some(extra) = positional.next() {
        return Err(format!("unexpected argument {extra:?}"));
    }
    if spawn && matches!(command, Command::Listen) {
        return Err("listen only makes sense against a running core (drop --spawn)".to_string());
    }

    Ok(Options {
        command,
        home,
        spawn,
        core_bin,
        keep_home,
        copy_agents,
        timeout,
        method_prefixes,
        raw,
    })
}

fn required_value(flag: &str, iter: &mut impl Iterator<Item = String>) -> Result<String, String> {
    iter.next().ok_or(format!("{flag} expects a value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, String> {
        parse_args(args.iter().map(|arg| arg.to_string()).collect())
    }

    #[test]
    fn finds_the_core_socket_for_short_and_long_homes() {
        let home = Path::new("/Users/someone/.ottam");
        assert_eq!(socket_path(home), home.join(SOCKET_FILE_NAME));
        // Shared with core's server::attach tests; change both together.
        assert_eq!(
            short_socket_path(Path::new("/a/deep/ottam/home"), 501),
            PathBuf::from("/tmp/ottam-501/792a758c0955099a.sock")
        );
    }

    #[test]
    fn parses_call_with_params_and_flags() {
        let options = parse(&[
            "call",
            "agent.list",
            r#"{"refreshRuntime": true}"#,
            "--home",
            "/tmp/x",
            "--timeout",
            "5",
            "--raw",
        ])
        .expect("parses");
        let Command::Call { method, params } = &options.command else {
            panic!("expected call");
        };
        assert_eq!(method, "agent.list");
        assert_eq!(params["refreshRuntime"], serde_json::json!(true));
        assert_eq!(options.home.as_deref(), Some(Path::new("/tmp/x")));
        assert_eq!(options.timeout, Duration::from_secs(5));
        assert!(options.raw);
    }

    #[test]
    fn call_defaults_params_to_an_empty_object() {
        let options = parse(&["call", "workspace.list"]).expect("parses");
        let Command::Call { params, .. } = &options.command else {
            panic!("expected call");
        };
        assert_eq!(params, &serde_json::json!({}));
    }

    #[test]
    fn conversation_defaults_to_get_and_expands_bare_method_names() {
        let options = parse(&["conversation", "c1"]).expect("parses");
        let Command::Conversation { id, method, params } = &options.command else {
            panic!("expected conversation");
        };
        assert_eq!(id, "c1");
        assert_eq!(method, "conversation.get");
        assert_eq!(params, &serde_json::json!({}));

        let options =
            parse(&["conversation", "c1", "listEvents", r#"{"limit": 5}"#]).expect("parses");
        let Command::Conversation { method, params, .. } = &options.command else {
            panic!("expected conversation");
        };
        assert_eq!(method, "conversation.listEvents");
        assert_eq!(params["limit"], serde_json::json!(5));

        let options = parse(&["conversation", "c1", "run.get"]).expect("parses");
        let Command::Conversation { method, .. } = &options.command else {
            panic!("expected conversation");
        };
        assert_eq!(method, "run.get");
        assert!(parse(&["conversation"]).is_err());
    }

    #[test]
    fn listen_splits_method_prefixes() {
        let options = parse(&["listen", "--methods", "conversation.,agent."]).expect("parses");
        assert_eq!(options.method_prefixes, vec!["conversation.", "agent."]);
    }

    #[test]
    fn rejects_unknown_commands_options_and_spawned_listen() {
        assert!(parse(&["frobnicate"]).is_err());
        assert!(parse(&["call", "x", "--wat"]).is_err());
        assert!(parse(&["call"]).is_err());
        assert!(parse(&["call", "x", "not-json"]).is_err());
        assert!(parse(&["listen", "--spawn"]).is_err());
        assert!(parse(&[]).is_err());
    }
}
