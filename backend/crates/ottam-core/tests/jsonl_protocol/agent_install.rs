//! Managed adapter installs, end to end over the wire.
//!
//! Hermetic: the fake toolchain's `npm` is a shell script that writes the same
//! shape npm would (package under `node_modules/`, `bin` entry, lockfile), and
//! `node` is the fake ACP binary — so launching the installed entrypoint in
//! the `verifying` phase performs a real ACP handshake with no network.

use crate::support::{CoreProcess, FakeAcpRuntime, GitRepoFixture};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

/// The codex pin from `registry.rs`. Spelled out rather than imported: these
/// tests assert what lands on disk, so a pin bump should fail loudly here.
const CODEX_PACKAGE: &str = "@agentclientprotocol/codex-acp";
const CODEX_VERSION: &str = "2.5.0";

/// The harness disables installs globally for hermeticity; these tests are
/// the ones that turn the real flow back on.
fn installs_on(fake: &FakeAcpRuntime) -> Vec<(String, String)> {
    let mut environment = fake.environment();
    environment.push(("OTTAM_ADAPTER_INSTALLS".to_string(), "on".to_string()));
    environment
}

fn adapter_dir(home: &Path) -> std::path::PathBuf {
    home.join("agents/codex/adapter")
}

fn start_install(core: &mut CoreProcess, id: u64) -> String {
    let accepted = core.send(json!({
        "id": id,
        "method": "agent.install",
        "params": {"agentID": "codex"},
    }));
    assert!(accepted["error"].is_null(), "install refused: {accepted}");
    assert_eq!(accepted["result"]["name"], CODEX_PACKAGE);
    assert_eq!(accepted["result"]["version"], CODEX_VERSION);
    accepted["result"]["installID"]
        .as_str()
        .expect("installID")
        .to_string()
}

/// Collect `agent.installEvent` params until a terminal phase arrives.
fn drain_install_events(core: &mut CoreProcess, install_id: &str) -> Vec<Value> {
    let mut events = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        let Some(message) = core.read_json_timeout(Duration::from_secs(15)) else {
            break;
        };
        if message["method"] != "agent.installEvent" {
            continue;
        }
        let params = message["params"].clone();
        assert_eq!(
            params["installID"].as_str(),
            Some(install_id),
            "an event arrived for an install this test did not start"
        );
        let phase = params["phase"].as_str().unwrap_or_default().to_string();
        events.push(params);
        if matches!(phase.as_str(), "ready" | "failed" | "cancelled") {
            return events;
        }
    }
    panic!("no terminal install event arrived; saw {events:?}");
}

fn phases(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| event["phase"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn agent<'a>(agents: &'a Value, agent_id: &str) -> &'a Value {
    agents["result"]["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .find(|agent| agent["agentID"] == agent_id)
        .unwrap_or_else(|| panic!("no {agent_id} agent in {agents}"))
}

fn engine<'a>(engines: &'a Value, engine_id: &str) -> &'a Value {
    engines["engines"]
        .as_array()
        .expect("engines")
        .iter()
        .find(|engine| engine["engineID"] == engine_id)
        .unwrap_or_else(|| panic!("no {engine_id} engine in {engines}"))
}

/// The whole flow: a fresh machine reports the adapter uninstalled and the
/// engine unusable; Get runs npm and a real handshake; the engine flips to
/// ready with no refresh in between.
#[test]
fn installing_an_adapter_verifies_the_handshake_and_frees_the_engine() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");

    let fake = FakeAcpRuntime::new();
    let mut core = CoreProcess::new_with_env(installs_on(&fake));
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"]
        .as_str()
        .expect("projectID")
        .to_string();

    // Fresh machine: mandatory install means unusable until Get.
    let agents = core.send(json!({"id": 800, "method": "agent.list", "params": {}}));
    assert_eq!(
        agent(&agents, "codex")["adapterPackage"]["installState"],
        "notInstalled"
    );
    let engines = core.engine_list(&project_id);
    assert_eq!(engine(&engines, "codex")["installState"], "missing");

    let install_id = start_install(&mut core, 801);
    let events = drain_install_events(&mut core, &install_id);
    let seen = phases(&events);
    assert_eq!(
        seen.last().map(String::as_str),
        Some("ready"),
        "install did not finish: {events:?}"
    );
    for expected in ["starting", "installing", "verifying"] {
        assert!(
            seen.iter().any(|phase| phase == expected),
            "missing the {expected} phase in {seen:?}"
        );
    }

    // The install is on disk with its provenance recorded.
    let manifest_path = adapter_dir(core.ottam_home_path())
        .join(CODEX_VERSION)
        .join("manifest.json");
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("the manifest was published"))
            .expect("the manifest parses");
    assert_eq!(manifest["name"], CODEX_PACKAGE);
    assert_eq!(manifest["version"], CODEX_VERSION);
    assert!(!manifest["resolved"].as_str().unwrap_or_default().is_empty());

    // After — and deliberately without a refresh flag: an install that only
    // takes effect on Check Again would be a bug the user works around.
    let agents = core.send(json!({"id": 802, "method": "agent.list", "params": {}}));
    assert_eq!(
        agent(&agents, "codex")["adapterPackage"]["installState"],
        "installed"
    );
    let engines = core.engine_list(&project_id);
    assert_eq!(engine(&engines, "codex")["installState"], "ready");

    core.shutdown();
}

/// A second `agent.install` for the same agent joins the running one rather
/// than starting a duplicate npm.
#[test]
fn a_second_install_for_the_same_agent_joins_the_first() {
    let fake = FakeAcpRuntime::new();
    let mut environment = installs_on(&fake);
    // Long enough that the second request definitely lands mid-install.
    environment.push(("OTTAM_FAKE_NPM_SLEEP".to_string(), "2".to_string()));
    let mut core = CoreProcess::new_with_env(environment);
    core.initialize();

    let first = start_install(&mut core, 810);
    let second = start_install(&mut core, 811);
    assert_eq!(first, second, "the second install did not join the first");

    let events = drain_install_events(&mut core, &first);
    assert_eq!(phases(&events).last().map(String::as_str), Some("ready"));

    core.shutdown();
}

/// npm failing is a `failed` event and nothing published — the engine stays
/// uninstalled rather than half-installed.
#[test]
fn a_failed_npm_install_publishes_nothing() {
    let fake = FakeAcpRuntime::new();
    let mut environment = installs_on(&fake);
    environment.push(("OTTAM_FAKE_NPM_FAIL".to_string(), "1".to_string()));
    let mut core = CoreProcess::new_with_env(environment);
    core.initialize();

    let install_id = start_install(&mut core, 820);
    let events = drain_install_events(&mut core, &install_id);
    let last = events.last().expect("a terminal event");
    assert_eq!(last["phase"], "failed");
    assert!(
        last["error"].as_str().unwrap_or_default().contains("npm"),
        "the failure should name npm: {last}"
    );

    assert!(
        !adapter_dir(core.ottam_home_path())
            .join(CODEX_VERSION)
            .exists(),
        "a failed install was published"
    );

    core.shutdown();
}

/// The verifying phase is real: an adapter that installs cleanly but cannot
/// complete a handshake is rolled back, so `installed` keeps its meaning.
#[test]
fn a_handshake_failure_rolls_the_install_back() {
    let fake = FakeAcpRuntime::new();
    let mut environment = installs_on(&fake);
    environment.push((
        "OTTAM_TEST_ACP_DIE_BEFORE_INITIALIZE".to_string(),
        "1".to_string(),
    ));
    let mut core = CoreProcess::new_with_env(environment);
    core.initialize();

    let install_id = start_install(&mut core, 830);
    let events = drain_install_events(&mut core, &install_id);
    let seen = phases(&events);
    assert!(
        seen.iter().any(|phase| phase == "verifying"),
        "the handshake never ran: {seen:?}"
    );
    let last = events.last().expect("a terminal event");
    assert_eq!(last["phase"], "failed");
    assert!(
        last["error"]
            .as_str()
            .unwrap_or_default()
            .contains("handshake"),
        "the failure should name the handshake: {last}"
    );

    assert!(
        !adapter_dir(core.ottam_home_path())
            .join(CODEX_VERSION)
            .exists(),
        "a handshake failure left the version published"
    );

    core.shutdown();
}

/// Cancel while npm is running: the fake npm publishes a readiness marker and
/// blocks, so the only way the install settles before release is the
/// process-group kill landing.
#[test]
fn cancelling_mid_npm_leaves_nothing_behind() {
    let fake = FakeAcpRuntime::new();
    let mut environment = installs_on(&fake);
    environment.push((
        "OTTAM_FAKE_NPM_HOLD_UNTIL_RELEASE".to_string(),
        "1".to_string(),
    ));
    let mut core = CoreProcess::new_with_env(environment);
    core.initialize();
    let npm_release = fake.npm_release_guard();

    let install_id = start_install(&mut core, 840);
    // The protocol's `installing` event precedes process spawn. This marker is
    // written by the fake npm itself, proving cancellation hits a live child.
    fake.wait_for_npm_started(Duration::from_secs(10));

    let cancelled = core.send(json!({
        "id": 841,
        "method": "agent.installCancel",
        "params": {"installID": install_id},
    }));
    assert!(cancelled["error"].is_null(), "cancel failed: {cancelled}");

    let events = drain_install_events(&mut core, &install_id);
    assert_eq!(
        events.last().expect("a terminal event")["phase"],
        "cancelled"
    );

    // Neither a published version nor a staging directory survives.
    let leftovers: Vec<String> = std::fs::read_dir(adapter_dir(core.ottam_home_path()))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        leftovers.is_empty(),
        "a cancelled install left files behind: {leftovers:?}"
    );

    // Let the fixture proceed if cancellation failed to reap it; successful
    // cancellation has already killed the process group before this point.
    npm_release.release();
    core.shutdown();
}

/// The client closes stdin to quit. An install in flight must not hold the
/// process open: shutdown cancels it, killing npm's process group.
#[test]
fn stdin_eof_during_npm_install_exits_promptly() {
    let fake = FakeAcpRuntime::new();
    let mut environment = installs_on(&fake);
    environment.push((
        "OTTAM_FAKE_NPM_HOLD_UNTIL_RELEASE".to_string(),
        "1".to_string(),
    ));
    let mut core = CoreProcess::new_with_env(environment);
    core.initialize();
    let npm_release = fake.npm_release_guard();

    start_install(&mut core, 843);
    fake.wait_for_npm_started(Duration::from_secs(10));

    let started = Instant::now();
    core.close_stdin();
    let status = core.wait_for_exit(Duration::from_secs(10));
    npm_release.release();
    let status = status.expect("core still running 10s after stdin closed mid-install");
    assert!(status.success(), "core exited with {status}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "shutdown took {:?}",
        started.elapsed()
    );
}

#[test]
fn dropping_the_fixture_guard_releases_held_npm() {
    let fake = FakeAcpRuntime::new();
    let mut environment = installs_on(&fake);
    environment.push((
        "OTTAM_FAKE_NPM_HOLD_UNTIL_RELEASE".to_string(),
        "1".to_string(),
    ));
    let mut core = CoreProcess::new_with_env(environment);
    core.initialize();
    let npm_release = fake.npm_release_guard();

    let install_id = start_install(&mut core, 842);
    fake.wait_for_npm_started(Duration::from_secs(10));
    drop(npm_release);

    let events = drain_install_events(&mut core, &install_id);
    assert_eq!(events.last().expect("a terminal event")["phase"], "ready");
    core.shutdown();
}

/// Cancel during `verifying` — after npm is done and the version is published,
/// while the handshake runs. The probe can take up to a minute, so without
/// racing it against the token the user waits that out and is then told the
/// install is `ready`.
#[test]
fn cancelling_during_verification_rolls_the_install_back() {
    let fake = FakeAcpRuntime::new();
    let mut environment = installs_on(&fake);
    environment.push((
        "OTTAM_TEST_ACP_HOLD_INITIALIZE".to_string(),
        "1".to_string(),
    ));
    let mut core = CoreProcess::new_with_env(environment);
    core.initialize();
    let initialize_release = fake.initialize_release_guard();

    let install_id = start_install(&mut core, 860);
    // `verifying` is emitted before the ACP process has necessarily read its
    // first request. Wait for the fake adapter itself to observe initialize.
    fake.wait_for_initialize_seen(Duration::from_secs(10));

    let cancelled = core.send(json!({
        "id": 861,
        "method": "agent.installCancel",
        "params": {"installID": install_id},
    }));
    assert!(cancelled["error"].is_null(), "cancel failed: {cancelled}");

    let events = drain_install_events(&mut core, &install_id);
    assert!(
        phases(&events).iter().any(|phase| phase == "verifying"),
        "the handshake never entered verification: {events:?}"
    );
    assert_eq!(
        events.last().expect("a terminal event")["phase"],
        "cancelled",
        "cancel during verifying must not settle as ready"
    );

    // The published version is rolled back: a cancelled install never leaves
    // something `installState` would report as installed.
    let leftovers: Vec<String> = std::fs::read_dir(adapter_dir(core.ottam_home_path()))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        leftovers.is_empty(),
        "a cancelled install left files behind: {leftovers:?}"
    );

    // The model-catalog probe is deliberately detached from the install. Let
    // its fake initialize finish so graceful Core shutdown does not wait for
    // the production 60-second probe timeout.
    initialize_release.release();
    core.shutdown();
}

/// Remove undoes Get: the adapter tree is gone, the agent reports
/// `notInstalled`, and with `removeHome` the isolated home goes too.
#[test]
fn uninstalling_removes_the_adapter_and_optionally_the_home() {
    let fake = FakeAcpRuntime::new();
    let mut core = CoreProcess::new_with_env(installs_on(&fake));
    core.initialize();

    let install_id = start_install(&mut core, 870);
    let events = drain_install_events(&mut core, &install_id);
    assert_eq!(
        phases(&events).last().map(String::as_str),
        Some("ready"),
        "install did not finish: {events:?}"
    );
    let home = core.ottam_home_path().to_path_buf();
    let home = home.as_path();
    assert!(adapter_dir(home).join(CODEX_VERSION).is_dir());

    // Something in the home, so the remove-home assertion sees a real delete.
    let agent_home = home.join("agents/codex/home");
    std::fs::create_dir_all(&agent_home).expect("agent home");
    std::fs::write(agent_home.join("auth.json"), "{}").expect("auth file");

    // Plain remove: the adapter goes, the home stays.
    let (removed, notifications) = core.send_for_id(
        json!({
            "id": 871,
            "method": "agent.uninstall",
            "params": {"agentID": "codex"},
        }),
        json!(871),
    );
    assert!(removed["error"].is_null(), "uninstall refused: {removed}");
    assert!(
        notifications.iter().any(|event| {
            event["method"] == "engine.changed" && event["params"]["engineID"] == "codex"
        }),
        "removal must invalidate open Chat clients: {notifications:?}"
    );
    assert!(!adapter_dir(home).exists(), "adapter tree survived removal");
    assert!(
        agent_home.join("auth.json").is_file(),
        "plain remove touched the home"
    );

    let agents = core.send(json!({"id": 872, "method": "agent.list", "params": {}}));
    assert_eq!(
        agent(&agents, "codex")["adapterPackage"]["installState"],
        "notInstalled"
    );

    // Remove with the home: nothing of the agent is left under `agents/codex`
    // but the (now empty) parent.
    let (removed, notifications) = core.send_for_id(
        json!({
            "id": 873,
            "method": "agent.uninstall",
            "params": {"agentID": "codex", "removeHome": true},
        }),
        json!(873),
    );
    assert!(removed["error"].is_null(), "uninstall refused: {removed}");
    assert!(
        notifications.iter().any(|event| {
            event["method"] == "engine.changed" && event["params"]["engineID"] == "codex"
        }),
        "removal without a cached catalog must still invalidate clients: {notifications:?}"
    );
    assert!(!agent_home.exists(), "removeHome left the home behind");

    core.shutdown();
}

#[test]
fn uninstall_reports_a_home_removal_failure() {
    let fake = FakeAcpRuntime::new();
    let mut core = CoreProcess::new_with_env(installs_on(&fake));
    core.initialize();

    let install_id = start_install(&mut core, 874);
    let events = drain_install_events(&mut core, &install_id);
    assert_eq!(phases(&events).last().map(String::as_str), Some("ready"));

    let home = core.ottam_home_path();
    let agent_home = home.join("agents/codex/home");
    std::fs::remove_dir_all(&agent_home).expect("remove probe-created home");
    std::fs::write(&agent_home, "not a directory").expect("blocking home file");
    let removed = core.send(json!({
        "id": 875,
        "method": "agent.uninstall",
        "params": {"agentID": "codex", "removeHome": true},
    }));
    assert!(
        removed["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("failed to remove codex's home")),
        "filesystem failure was hidden: {removed}"
    );

    core.shutdown();
}

/// Cancelling an id core never issued is deliberately not an error.
#[test]
fn cancelling_an_unknown_install_is_a_no_op() {
    let fake = FakeAcpRuntime::new();
    let mut core = CoreProcess::new_with_env(installs_on(&fake));
    core.initialize();

    let response = core.send(json!({
        "id": 850,
        "method": "agent.installCancel",
        "params": {"installID": "agent_install_never_issued"},
    }));
    assert!(response["error"].is_null(), "unexpected error: {response}");
    assert!(response["result"].is_object());

    core.shutdown();
}

/// A pin bump reinstalls without asking again: the old install is the standing
/// consent. Fresh machines (no install at all) still wait for Get — covered by
/// the happy-path test, whose install only starts when requested.
#[test]
fn a_bumped_pin_reinstalls_in_the_background_on_launch() {
    let home = tempfile::TempDir::new().expect("home");
    // Pre-seed an older install of the same package. Only the manifest's name
    // matters for consent; the entrypoint being gone mirrors reality (the old
    // version dir is whatever the previous app left).
    let old_dir = home.path().join("agents/codex/adapter/0.0.9");
    std::fs::create_dir_all(&old_dir).expect("old version dir");
    std::fs::write(
        old_dir.join("manifest.json"),
        serde_json::to_vec(&json!({
            "name": CODEX_PACKAGE,
            "version": "0.0.9",
            "entrypoint": "dist/index.js",
            "installedAtUnix": 0,
        }))
        .expect("encodes manifest"),
    )
    .expect("writes manifest");

    let fake = FakeAcpRuntime::new();
    let mut core = CoreProcess::new_in_home_with_env(home.path(), installs_on(&fake));
    core.initialize();

    // No `agent.install` request: the events arrive unprompted.
    let deadline = Instant::now() + Duration::from_secs(60);
    let install_id = loop {
        assert!(Instant::now() < deadline, "no auto-reinstall event arrived");
        let Some(message) = core.read_json_timeout(Duration::from_secs(15)) else {
            panic!("no auto-reinstall event arrived");
        };
        if message["method"] == "agent.installEvent" && message["params"]["agentID"] == "codex" {
            break message["params"]["installID"]
                .as_str()
                .expect("installID")
                .to_string();
        }
    };
    let events = drain_install_events(&mut core, &install_id);
    assert_eq!(phases(&events).last().map(String::as_str), Some("ready"));

    let root = adapter_dir(core.ottam_home_path());
    assert!(
        root.join(CODEX_VERSION).join("manifest.json").exists(),
        "the new pin was not installed"
    );
    assert!(
        !root.join("0.0.9").exists(),
        "the superseded version was not pruned"
    );

    core.shutdown();
}

/// Core dies while a pin bump is verifying. On restart the unverified version
/// must not count as installed: the bump starts again, and the older version
/// it would replace is not pruned out from under it.
#[test]
fn a_pin_bump_killed_during_verification_restarts_and_keeps_the_old_version() {
    let home = tempfile::TempDir::new().expect("home");
    let old_dir = home.path().join("agents/codex/adapter/0.0.9");
    std::fs::create_dir_all(&old_dir).expect("old version dir");
    std::fs::write(
        old_dir.join("manifest.json"),
        serde_json::to_vec(&json!({
            "name": CODEX_PACKAGE,
            "version": "0.0.9",
            "entrypoint": "dist/index.js",
            "installedAtUnix": 0,
        }))
        .expect("encodes manifest"),
    )
    .expect("writes manifest");

    let fake = FakeAcpRuntime::new();
    let mut environment = installs_on(&fake);
    environment.push((
        "OTTAM_TEST_ACP_HOLD_INITIALIZE".to_string(),
        "1".to_string(),
    ));
    let initialize_release = fake.initialize_release_guard();
    let mut core = CoreProcess::new_in_home_with_env(home.path(), environment.clone());
    core.initialize();
    fake.wait_for_initialize_seen(Duration::from_secs(30));
    let root = adapter_dir(home.path());
    assert!(root.join(CODEX_VERSION).exists(), "the bump was not published");
    core.kill_now();

    let mut core = CoreProcess::new_in_home_with_env(home.path(), environment);
    core.initialize();
    let deadline = Instant::now() + Duration::from_secs(10);
    let install_id = loop {
        let message = core.read_json_timeout(Duration::from_millis(500));
        if let Some(message) = message {
            if message["method"] == "agent.installEvent" && message["params"]["agentID"] == "codex"
            {
                break message["params"]["installID"]
                    .as_str()
                    .expect("installID")
                    .to_string();
            }
        }
        assert!(
            Instant::now() < deadline,
            "no reinstall after restart: the unverified bump counted as installed \
             (0.0.9 still on disk: {})",
            root.join("0.0.9").exists()
        );
    };
    assert!(
        root.join("0.0.9").join("manifest.json").exists(),
        "the older version was pruned before the bump verified"
    );

    initialize_release.release();
    let events = drain_install_events(&mut core, &install_id);
    assert_eq!(phases(&events).last().map(String::as_str), Some("ready"));
    core.shutdown();
}

fn codex_package(core: &mut CoreProcess, id: u64) -> Value {
    let listed = core.send(json!({"id": id, "method": "agent.list", "params": {}}));
    listed["result"]["agents"]
        .as_array()
        .and_then(|agents| agents.iter().find(|agent| agent["agentID"] == "codex"))
        .map(|agent| agent["adapterPackage"].clone())
        .unwrap_or_else(|| panic!("codex missing from agent.list: {listed}"))
}

/// An intact older install keeps the agent usable through a pin bump: the
/// list reports it installed, says which version runs, and how the update is
/// going — never `notInstalled`, which sends the user to first-run setup.
#[test]
fn a_bumped_pin_keeps_reporting_the_older_install_until_it_lands() {
    let home = tempfile::TempDir::new().expect("home");
    let old_dir = home.path().join("agents/codex/adapter/0.0.9");
    let entrypoint = "node_modules/@agentclientprotocol/codex-acp/dist/index.js";
    std::fs::create_dir_all(old_dir.join(entrypoint).parent().expect("parent"))
        .expect("old version dir");
    std::fs::write(old_dir.join(entrypoint), "// adapter").expect("old entrypoint");
    std::fs::write(
        old_dir.join("manifest.json"),
        serde_json::to_vec(&json!({
            "name": CODEX_PACKAGE,
            "version": "0.0.9",
            "entrypoint": entrypoint,
            "installedAtUnix": 0,
        }))
        .expect("encodes manifest"),
    )
    .expect("writes manifest");

    let fake = FakeAcpRuntime::new();
    let mut core = CoreProcess::new_in_home_with_env(home.path(), installs_on(&fake));
    core.initialize();

    let during = codex_package(&mut core, 880);
    assert_eq!(during["installState"], "installed", "{during}");
    assert_eq!(during["installedVersion"], "0.0.9", "{during}");
    assert_eq!(during["update"]["version"], CODEX_VERSION, "{during}");
    assert!(
        ["updating", "pending"].contains(&during["update"]["state"].as_str().unwrap_or("")),
        "{during}"
    );

    let deadline = Instant::now() + Duration::from_secs(60);
    let install_id = loop {
        assert!(Instant::now() < deadline, "no auto-reinstall event arrived");
        let Some(message) = core.read_json_timeout(Duration::from_secs(15)) else {
            panic!("no auto-reinstall event arrived");
        };
        if message["method"] == "agent.installEvent" && message["params"]["agentID"] == "codex" {
            break message["params"]["installID"]
                .as_str()
                .expect("installID")
                .to_string();
        }
    };
    let events = drain_install_events(&mut core, &install_id);
    assert_eq!(phases(&events).last().map(String::as_str), Some("ready"));

    let after = codex_package(&mut core, 881);
    assert_eq!(after["installedVersion"], CODEX_VERSION, "{after}");
    assert!(after["update"].is_null(), "{after}");
    assert!(
        !adapter_dir(core.ottam_home_path())
            .join("0.0.9")
            .exists(),
        "nothing ran from the superseded version, so it should be pruned"
    );

    core.shutdown();
}

/// A pin bump whose handshake fails must leave the old version alone. It is
/// both the working adapter and the standing consent that retries the bump on
/// the next launch — pruning it at publish time and then rolling the new one
/// back would strand the user on a manual *Get* for a transient failure.
#[test]
fn a_failed_pin_bump_keeps_the_version_it_would_have_replaced() {
    let home = tempfile::TempDir::new().expect("home");
    let old_dir = home.path().join("agents/codex/adapter/0.0.9");
    std::fs::create_dir_all(&old_dir).expect("old version dir");
    std::fs::write(
        old_dir.join("manifest.json"),
        serde_json::to_vec(&json!({
            "name": CODEX_PACKAGE,
            "version": "0.0.9",
            "entrypoint": "dist/index.js",
            "installedAtUnix": 0,
        }))
        .expect("encodes manifest"),
    )
    .expect("writes manifest");

    let fake = FakeAcpRuntime::new();
    let mut environment = installs_on(&fake);
    environment.push((
        "OTTAM_TEST_ACP_DIE_BEFORE_INITIALIZE".to_string(),
        "1".to_string(),
    ));
    let mut core = CoreProcess::new_in_home_with_env(home.path(), environment);
    core.initialize();

    let deadline = Instant::now() + Duration::from_secs(60);
    let install_id = loop {
        assert!(Instant::now() < deadline, "no auto-reinstall event arrived");
        let Some(message) = core.read_json_timeout(Duration::from_secs(15)) else {
            panic!("no auto-reinstall event arrived");
        };
        if message["method"] == "agent.installEvent" && message["params"]["agentID"] == "codex" {
            break message["params"]["installID"]
                .as_str()
                .expect("installID")
                .to_string();
        }
    };
    let events = drain_install_events(&mut core, &install_id);
    assert_eq!(phases(&events).last().map(String::as_str), Some("failed"));

    let root = adapter_dir(core.ottam_home_path());
    assert!(
        !root.join(CODEX_VERSION).exists(),
        "the unverified version stayed published"
    );
    assert!(
        root.join("0.0.9").join("manifest.json").exists(),
        "the failed bump destroyed the standing consent"
    );

    core.shutdown();
}
