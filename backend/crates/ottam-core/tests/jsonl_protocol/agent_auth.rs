//! `agent.signIn` end to end, over a real adapter process.
//!
//! The interesting behavior is all about the terminal the user never asked for:
//! it must stay out of `terminal.list`, it must be revealed and readable when
//! the login fails, and it must not outlive the flow that owns it.

use crate::support::{assert_success_response, CoreProcess, FakeAcpRuntime, GitRepoFixture};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// The fake ACP runtime advertises terminal auth for whichever agent it backs,
/// so any of them exercises the client-executed path. Claude is the
/// hidden-terminal agent (`TerminalLoginReveal::WhenStuck`), which is the
/// behavior most of this file is about; the immediate-reveal test below picks
/// an `Immediate` agent instead.
const AGENT_ID: &str = "claude";

struct SignInFixture {
    core: CoreProcess,
    /// None until a project is opened. Settings › Agents is reachable on a
    /// first launch, so a sign-in can genuinely have no workspace to name.
    workspace_id: Option<String>,
    agent_id: &'static str,
    /// Notifications read while correlating a response. `agent.signInEvent`
    /// arrives from a background task, so it lands in the middle of unrelated
    /// request/response pairs and would otherwise be thrown away.
    pending: Vec<Value>,
    repo: GitRepoFixture,
    _runtime: FakeAcpRuntime,
    _home: tempfile::TempDir,
}

fn sign_in_fixture(login: &[&str]) -> SignInFixture {
    sign_in_fixture_for(AGENT_ID, login)
}

fn sign_in_fixture_for(agent_id: &'static str, login: &[&str]) -> SignInFixture {
    let mut fixture = fresh_install_fixture(agent_id, login);
    fixture.workspace_id = Some(fixture.open_a_workspace());
    fixture
}

/// A core that has opened nothing: no project, and so no workspace anywhere in
/// the database. Everything a sign-in needs is already here.
fn fresh_install_fixture(agent_id: &'static str, login: &[&str]) -> SignInFixture {
    let repo = GitRepoFixture::init();
    repo.write("readme.md", "hello\n");
    repo.commit_all("initial");
    let runtime = FakeAcpRuntime::new();
    let home = tempfile::TempDir::new().expect("OTTAM_HOME");
    let mut core = CoreProcess::new_in_home_with_env(
        home.path(),
        runtime.environment_with_a_terminal_login(login[0], &login[1..]),
    );
    core.initialize();
    SignInFixture {
        core,
        workspace_id: None,
        agent_id,
        pending: Vec::new(),
        repo,
        _runtime: runtime,
        _home: home,
    }
}

impl SignInFixture {
    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        let (response, skipped) = self.core.send_for_id(
            json!({"id": id, "method": method, "params": params}),
            json!(id),
        );
        self.pending.extend(skipped);
        response
    }

    fn open_a_workspace(&mut self) -> String {
        let path = self.repo.path().to_path_buf();
        let opened = self.core.open_path(&path);
        let project_id = opened["project"]["projectID"]
            .as_str()
            .expect("projectID")
            .to_string();
        self.core.workspace_id(&project_id)
    }

    fn sign_in(&mut self, id: i64) -> Value {
        let agent_id = self.agent_id;
        let mut params = json!({"agentID": agent_id});
        // Sent only when there is one: an omitted `workspaceID` is the
        // Settings case, and the shape the core has to accept.
        if let Some(workspace_id) = self.workspace_id.clone() {
            params["workspaceID"] = json!(workspace_id);
        }
        let response = self.request(id, "agent.signIn", params);
        assert_success_response(&response, &json!(id));
        response["result"].clone()
    }

    /// Read `agent.signInEvent` notifications until one reaches `phase`.
    ///
    /// The supervisor runs on its own task and the login is a real process, so
    /// the wait is on the clock rather than on a fixed number of lines.
    fn wait_for_phase(&mut self, phase: &str) -> Value {
        if let Some(index) = self
            .pending
            .iter()
            .position(|message| is_phase(message, phase))
        {
            return self.pending.remove(index)["params"].clone();
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "no {phase:?} sign-in event arrived\nseen: {:?}",
                self.pending
            );
            let message = self
                .core
                .read_response_timeout(remaining)
                .unwrap_or_else(|| panic!("core went quiet before the sign-in reached {phase:?}"));
            if is_phase(&message, phase) {
                return message["params"].clone();
            }
            self.pending.push(message);
        }
    }

    fn listed_terminals(&mut self, id: i64) -> Vec<Value> {
        let workspace_id = self.workspace_id.clone().expect("a workspace to list");
        self.listed_terminals_in(id, &workspace_id)
    }

    fn listed_terminals_in(&mut self, id: i64, workspace_id: &str) -> Vec<Value> {
        let response = self.request(id, "terminal.list", json!({"workspaceID": workspace_id}));
        assert_success_response(&response, &json!(id));
        response["result"]["terminals"]
            .as_array()
            .expect("terminals")
            .clone()
    }

    fn replay(&mut self, id: i64, terminal_id: &str) -> Value {
        let mut params = json!({"terminalID": terminal_id});
        // Scoping only, and only when the caller has a workspace: a login
        // that belongs to none is addressed by terminal id alone.
        if let Some(workspace_id) = self.workspace_id.clone() {
            params["workspaceID"] = json!(workspace_id);
        }
        self.request(id, "terminal.replay", params)
    }

    /// Replay until the scrollback carries `needle`. `awaitingUser` fires when
    /// the login is spawned, not when it has written anything, so the first
    /// read can legitimately come back empty.
    fn replay_until(&mut self, id: i64, terminal_id: &str, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        for attempt in 0.. {
            let replay = self.replay(id + attempt, terminal_id);
            assert_success_response(&replay, &json!(id + attempt));
            let text = scrollback(&replay);
            if text.contains(needle) {
                return text;
            }
            assert!(
                Instant::now() < deadline,
                "no {needle:?} in the login scrollback\nlast read: {text:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        unreachable!("the loop returns or panics")
    }
}

fn is_phase(message: &Value, phase: &str) -> bool {
    message["method"] == "agent.signInEvent" && message["params"]["phase"] == phase
}

fn scrollback(replay: &Value) -> String {
    replay["result"]["chunks"]
        .as_array()
        .expect("chunks")
        .iter()
        .map(|chunk| {
            let bytes = STANDARD
                .decode(chunk["dataBase64"].as_str().expect("replay data"))
                .expect("replay data is base64");
            String::from_utf8_lossy(&bytes).into_owned()
        })
        .collect()
}

fn terminal_id(event: &Value) -> String {
    event["terminal"]["terminalID"]
        .as_str()
        .expect("the terminal is addressable from the event")
        .to_string()
}

#[cfg(unix)]
#[test]
fn a_login_that_succeeds_is_never_shown_to_the_user() {
    // Long enough that the list below runs while the login is still alive —
    // a terminal that has already exited would prove nothing.
    let mut fixture = sign_in_fixture(&["/bin/sh", "-c", "sleep 2; exit 0"]);
    let started = fixture.sign_in(800);
    assert_eq!(started["delivery"], "clientTerminal");
    assert_eq!(started["phase"], "starting");

    let awaiting = fixture.wait_for_phase("awaitingUser");
    assert_eq!(awaiting["revealTerminal"], false);
    let terminal = terminal_id(&awaiting);

    // Withheld from the list: the user never asked for this terminal, so it has
    // no business in the sidebar.
    assert!(
        !fixture
            .listed_terminals(801)
            .iter()
            .any(|listed| listed["terminalID"] == terminal.as_str()),
        "a hidden login terminal must not be listed"
    );

    let succeeded = fixture.wait_for_phase("succeeded");
    assert_eq!(succeeded["revealTerminal"], false);
    assert_eq!(succeeded["authSessionID"], started["authSessionID"]);
    fixture.core.shutdown();
}

#[cfg(unix)]
#[test]
fn a_login_that_fails_is_revealed_with_a_terminal_the_client_can_replay() {
    let mut fixture = sign_in_fixture(&["/bin/sh", "-c", "printf 'no credentials\\n'; exit 1"]);
    fixture.sign_in(810);
    fixture.wait_for_phase("awaitingUser");

    let failed = fixture.wait_for_phase("failed");
    assert_eq!(failed["revealTerminal"], true);
    let terminal = terminal_id(&failed);

    // The scrollback is the only account of the failure, so the terminal has to
    // still be there when the client goes looking for it.
    let replay = fixture.replay(811, &terminal);
    assert_success_response(&replay, &json!(811));
    fixture.core.shutdown();
}

#[cfg(unix)]
#[test]
fn cancelling_a_login_kills_the_command_it_started() {
    let mut fixture = sign_in_fixture(&["/bin/sh", "-c", "sleep 600"]);
    let started = fixture.sign_in(820);
    let awaiting = fixture.wait_for_phase("awaitingUser");
    let terminal = terminal_id(&awaiting);

    let cancelled = fixture.request(
        821,
        "agent.signInCancel",
        json!({"agentID": AGENT_ID, "authSessionID": started["authSessionID"]}),
    );
    assert_success_response(&cancelled, &json!(821));
    let event = fixture.wait_for_phase("cancelled");
    assert_eq!(event["authSessionID"], started["authSessionID"]);

    // Killed, not merely forgotten: nothing addresses the session any more.
    let replay = fixture.replay(822, &terminal);
    assert_eq!(replay["error"]["data"]["kind"], "terminalNotFound");
    fixture.core.shutdown();
}

#[cfg(unix)]
#[test]
fn an_immediate_login_is_revealed_up_front_and_finished_by_the_user() {
    // Codex is an `Immediate` agent in the builtin table; its login stands in
    // for a full-screen picker here: a program that never exits on its own,
    // so the user is the one who declares it complete.
    let mut fixture = sign_in_fixture_for("codex", &["/bin/sh", "-c", "sleep 600"]);
    let started = fixture.sign_in(840);

    let awaiting = fixture.wait_for_phase("awaitingUser");
    assert_eq!(
        awaiting["revealTerminal"], true,
        "the terminal is the sign-in, so it shows from the first frame"
    );
    let terminal = terminal_id(&awaiting);

    let finished = fixture.request(
        841,
        "agent.signInFinish",
        json!({"agentID": "codex", "authSessionID": started["authSessionID"]}),
    );
    assert_success_response(&finished, &json!(841));
    let succeeded = fixture.wait_for_phase("succeeded");
    assert_eq!(succeeded["authSessionID"], started["authSessionID"]);

    // Done closed the login terminal along the way — nothing addresses the
    // session any more.
    let replay = fixture.replay(842, &terminal);
    assert_eq!(replay["error"]["data"]["kind"], "terminalNotFound");
    fixture.core.shutdown();
}

#[cfg(unix)]
#[test]
fn a_login_with_no_workspace_runs_in_the_agents_home_and_belongs_to_nobody() {
    // The first launch someone signs in from has no project open and no
    // workspace in the database, so there is nothing to borrow. The login
    // prints where it landed and then stays alive, so the two reads below run
    // against a terminal that still exists.
    let mut fixture = fresh_install_fixture(
        AGENT_ID,
        &[
            "/bin/sh",
            "-c",
            "pwd; printf '__ottam_home_login__\\n'; sleep 600",
        ],
    );
    let started = fixture.sign_in(850);
    assert_eq!(started["delivery"], "clientTerminal");

    let awaiting = fixture.wait_for_phase("awaitingUser");
    let terminal = terminal_id(&awaiting);

    // Addressed by terminal id alone — the caller has no workspace to scope
    // the replay with, and the scrollback is what a revealed sheet renders.
    let scrollback = fixture.replay_until(851, &terminal, "__ottam_home_login__");
    assert!(
        scrollback.contains(&format!("/{AGENT_ID}/home")),
        "the login runs in the agent's isolated home, got {scrollback:?}"
    );

    // Opening a project afterwards must not adopt it. A terminal that belongs
    // to no workspace belongs to no sidebar either.
    let workspace_id = fixture.open_a_workspace();
    assert!(
        !fixture
            .listed_terminals_in(880, &workspace_id)
            .iter()
            .any(|listed| listed["terminalID"] == terminal.as_str()),
        "a workspace-less login terminal must not appear in any workspace"
    );
    fixture.core.shutdown();
}

#[cfg(unix)]
#[test]
fn pressing_sign_in_twice_runs_one_login() {
    // The failed-run card and the engine picker can both fire, and neither
    // knows about the other.
    let mut fixture = sign_in_fixture(&["/bin/sh", "-c", "sleep 600"]);
    let first = fixture.sign_in(830);
    fixture.wait_for_phase("awaitingUser");

    let second = fixture.sign_in(831);
    assert_eq!(second["authSessionID"], first["authSessionID"]);
    assert_eq!(second["phase"], "awaitingUser");

    fixture.request(
        832,
        "agent.signInCancel",
        json!({"agentID": AGENT_ID, "authSessionID": first["authSessionID"]}),
    );
    fixture.wait_for_phase("cancelled");
    fixture.core.shutdown();
}
