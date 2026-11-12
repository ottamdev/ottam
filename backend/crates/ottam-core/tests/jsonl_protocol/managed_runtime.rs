//! The managed Node runtime, end to end over the wire.
//!
//! Hermetic: a throwaway HTTP server on loopback serves a tarball the test
//! builds itself, so nothing here reaches nodejs.org. The real download, the
//! real checksum check and the real `tar` all run — only the host the bytes
//! come from is faked.

use crate::support::{node_discovery_environment, CoreProcess, GitRepoFixture, TarballServer};
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// The version `managed_node.rs` pins. Spelled out rather than imported: the
/// tarball this test builds has to be named what core will ask for, so a pin
/// bump should fail loudly here rather than quietly serve the wrong file.
const PINNED_VERSION: &str = "v24.22.1";

/// Below Claude's floor of 22 — the situation that makes a managed runtime
/// worth installing rather than redundant.
const SYSTEM_NODE_VERSION: &str = "v20.11.0";

fn architecture() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "darwin-arm64",
        "x86_64" => "darwin-x64",
        other => panic!("unsupported test architecture {other}"),
    }
}

fn write_executable(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("directories");
    std::fs::write(path, body).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// Build a tarball shaped like nodejs.org's: one top-level
/// `node-<version>-<arch>/` directory that core strips on extraction.
fn build_fake_node_tarball(into: &Path) -> (PathBuf, String) {
    let staging = into.join("staging");
    let root_name = format!("node-{PINNED_VERSION}-{}", architecture());
    let root = staging.join(&root_name);
    write_executable(
        &root.join("bin/node"),
        &format!("#!/bin/sh\necho {PINNED_VERSION}\n"),
    );
    write_executable(&root.join("bin/npx"), "#!/bin/sh\necho 11.0.0\n");

    let tarball = into.join(format!("{root_name}.tar.gz"));
    let status = std::process::Command::new("/usr/bin/tar")
        .args([
            "-czf",
            &tarball.to_string_lossy(),
            "-C",
            &staging.to_string_lossy(),
            &root_name,
        ])
        .status()
        .expect("tar runs");
    assert!(status.success(), "could not build the fake tarball");

    let bytes = std::fs::read(&tarball).expect("read tarball");
    let digest = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes));
    (tarball, digest)
}

/// A stand-in for the user's login shell that reports one fixed `node`.
///
/// This is how the test gets a *system* Node the resolver finds through its
/// normal chain. `OTTAM_NODE` would not do: it short-circuits everything,
/// managed runtime included, so it could never be superseded.
fn fake_login_shell(directory: &Path, node: &Path) -> PathBuf {
    let shell = directory.join("login-shell");
    write_executable(
        &shell,
        &format!(
            "#!/bin/sh\nprintf '%s%s\\n' '__ottam_node__:' '{}'\n",
            node.display()
        ),
    );
    shell
}

/// A machine whose own Node is a fake shell script reporting version 20.
fn system_node(directory: &Path) -> PathBuf {
    let node = directory.join("bin/node");
    write_executable(&node, &format!("#!/bin/sh\necho {SYSTEM_NODE_VERSION}\n"));
    write_executable(&directory.join("bin/npx"), "#!/bin/sh\necho 10.9.0\n");
    node
}

/// Collect `runtime.installEvent` params until a terminal phase arrives.
fn drain_install_events(core: &mut CoreProcess, install_id: &str) -> Vec<Value> {
    let mut events = Vec::new();
    // Generous: this downloads over loopback and shells out to tar. The point of
    // a deadline is that a wedged install fails the test rather than hanging the
    // suite.
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        let Some(message) = core.read_json_timeout(Duration::from_secs(15)) else {
            break;
        };
        if message["method"] != "runtime.installEvent" {
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

fn engine<'a>(engines: &'a Value, engine_id: &str) -> &'a Value {
    engines["engines"]
        .as_array()
        .expect("engines")
        .iter()
        .find(|engine| engine["engineID"] == engine_id)
        .unwrap_or_else(|| panic!("no {engine_id} engine in {engines}"))
}

/// The whole point of the feature: a Node too old for one engine, an install,
/// and that engine going ready — with no explicit refresh in between, which is
/// what proves the cache invalidation.
#[test]
fn installing_the_managed_runtime_makes_an_unsupported_engine_ready() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");

    let dist = TempDir::new().expect("dist directory");
    let (tarball, digest) = build_fake_node_tarball(dist.path());
    let server = TarballServer::serving(&tarball);

    let system = TempDir::new().expect("system runtime directory");
    let shell = fake_login_shell(system.path(), &system_node(system.path()));

    let mut core = CoreProcess::new_with_env(vec![
        node_discovery_environment(),
        ("SHELL".to_string(), shell.display().to_string()),
        ("OTTAM_NODE_DIST_BASE".to_string(), server.base_url),
        ("OTTAM_NODE_DIST_SHA256".to_string(), digest),
    ]);
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"]
        .as_str()
        .expect("projectID")
        .to_string();

    // Before: Claude's adapter wants Node 22 and the machine has 20. Codex (18)
    // is already satisfied, so only Claude has anything to gain.
    let engines = core.engine_list(&project_id);
    let claude = engine(&engines, "claude");
    // Named explicitly because the failure mode is quiet: if the login-shell
    // probe times out, resolution falls through to the developer's real
    // Homebrew Node, and the premise of this test evaporates rather than
    // erroring.
    assert_eq!(
        claude["runtime"]["availability"], "unsupported",
        "expected the fake Node 20 to be resolved, got {}",
        claude["runtime"]
    );
    assert_eq!(claude["runtime"]["foundMajor"], 20);
    assert_eq!(claude["runtime"]["minimumMajor"], 22);
    assert_eq!(claude["runtime"]["source"], "system");
    assert_eq!(
        engine(&engines, "codex")["runtime"]["availability"],
        "ready"
    );

    let accepted = core.send(json!({"id": 900, "method": "runtime.install", "params": {}}));
    assert!(accepted["error"].is_null(), "install refused: {accepted}");
    let install_id = accepted["result"]["installID"]
        .as_str()
        .expect("installID")
        .to_string();
    assert_eq!(accepted["result"]["version"], PINNED_VERSION);
    assert_eq!(accepted["result"]["phase"], "starting");

    let events = drain_install_events(&mut core, &install_id);
    let seen = phases(&events);
    assert_eq!(
        seen.last().map(String::as_str),
        Some("ready"),
        "install did not finish: {events:?}"
    );
    for expected in ["downloading", "verifying", "extracting"] {
        assert!(
            seen.iter().any(|phase| phase == expected),
            "missing the {expected} phase in {seen:?}"
        );
    }
    // Byte counts have to reach the client, or a progress bar cannot exist.
    assert!(
        events
            .iter()
            .any(|event| event["bytesTotal"].as_u64().unwrap_or(0) > 0),
        "no event carried a total size: {events:?}"
    );

    let installed = core
        .ottam_home_path()
        .join("runtime/node")
        .join(PINNED_VERSION)
        .join("bin/node");
    assert!(installed.exists(), "the runtime was not published on disk");
    assert!(
        installed.metadata().expect("metadata").permissions().mode() & 0o111 != 0,
        "the published node is not executable"
    );

    // After — and deliberately *not* through `engine.list` with a refresh: an
    // install that only takes effect when the user hits Check Again would be a
    // bug the user has to work around.
    let engines = core.engine_list(&project_id);
    let claude = engine(&engines, "claude");
    assert_eq!(
        claude["runtime"]["availability"], "ready",
        "claude did not pick up the managed runtime: {claude}"
    );
    assert_eq!(claude["runtime"]["source"], "managed");
    assert_eq!(claude["runtime"]["nodeVersion"], PINNED_VERSION);

    core.shutdown();
}

/// An app update that moves the Node pin: the older managed runtime keeps the
/// agents running — reported with the pending upgrade, never as "Node not
/// found" — while core downloads the new pin unprompted, then prunes the old
/// runtime nothing runs on.
#[test]
fn a_moved_node_pin_upgrades_in_the_background_on_launch() {
    let dist = TempDir::new().expect("dist directory");
    let (tarball, digest) = build_fake_node_tarball(dist.path());
    let server = TarballServer::serving(&tarball);

    let system = TempDir::new().expect("system runtime directory");
    let shell = fake_login_shell(system.path(), &system_node(system.path()));

    let home = TempDir::new().expect("home");
    let older = home.path().join("runtime/node/v24.0.0");
    write_executable(&older.join("bin/node"), "#!/bin/sh\necho v24.0.0\n");
    write_executable(&older.join("bin/npx"), "#!/bin/sh\necho 11.0.0\n");
    write_executable(&older.join("bin/npm"), "#!/bin/sh\necho 11.0.0\n");

    let mut core = CoreProcess::new_in_home_with_env(
        home.path(),
        vec![
            node_discovery_environment(),
            ("SHELL".to_string(), shell.display().to_string()),
            ("OTTAM_NODE_DIST_BASE".to_string(), server.base_url),
            ("OTTAM_NODE_DIST_SHA256".to_string(), digest),
        ],
    );
    core.initialize();

    // No `runtime.install` request: the upgrade starts on connect.
    let deadline = Instant::now() + Duration::from_secs(60);
    let install_id = loop {
        assert!(Instant::now() < deadline, "no background upgrade started");
        let Some(message) = core.read_json_timeout(Duration::from_secs(15)) else {
            panic!("no background upgrade started");
        };
        if message["method"] == "runtime.installEvent" {
            assert_eq!(message["params"]["version"], PINNED_VERSION);
            break message["params"]["installID"]
                .as_str()
                .expect("installID")
                .to_string();
        }
    };
    let events = drain_install_events(&mut core, &install_id);
    assert_eq!(
        phases(&events).last().map(String::as_str),
        Some("ready"),
        "upgrade did not finish: {events:?}"
    );

    let node_root = core.ottam_home_path().join("runtime/node");
    assert!(node_root.join(PINNED_VERSION).join("bin/node").exists());
    assert!(
        !node_root.join("v24.0.0").exists(),
        "nothing ran on the superseded runtime, so it should be pruned"
    );

    let listed = core.send(json!({"id": 920, "method": "agent.list", "params": {}}));
    let claude = listed["result"]["agents"]
        .as_array()
        .and_then(|agents| agents.iter().find(|agent| agent["agentID"] == "claude"))
        .unwrap_or_else(|| panic!("claude missing: {listed}"));
    assert_eq!(claude["runtime"]["availability"], "ready", "{claude}");
    assert_eq!(claude["runtime"]["source"], "managed");
    assert_eq!(claude["runtime"]["nodeVersion"], PINNED_VERSION);
    assert!(claude["runtime"]["managedUpdate"].is_null(), "{claude}");

    core.shutdown();
}

/// Before the new pin lands, the older managed runtime is what resolves, and
/// the list says an upgrade is pending — even with no network to get it.
#[test]
fn an_older_managed_runtime_stands_in_when_the_upgrade_cannot_download() {
    let system = TempDir::new().expect("system runtime directory");
    let shell = fake_login_shell(system.path(), &system_node(system.path()));

    let home = TempDir::new().expect("home");
    let older = home.path().join("runtime/node/v24.0.0");
    write_executable(&older.join("bin/node"), "#!/bin/sh\necho v24.0.0\n");
    write_executable(&older.join("bin/npx"), "#!/bin/sh\necho 11.0.0\n");
    write_executable(&older.join("bin/npm"), "#!/bin/sh\necho 11.0.0\n");

    let mut core = CoreProcess::new_in_home_with_env(
        home.path(),
        vec![
            node_discovery_environment(),
            ("SHELL".to_string(), shell.display().to_string()),
            // Nothing listens here: the download fails like an offline Mac.
            (
                "OTTAM_NODE_DIST_BASE".to_string(),
                "http://127.0.0.1:9/dist".to_string(),
            ),
            ("OTTAM_NODE_DIST_SHA256".to_string(), "0".repeat(64)),
        ],
    );
    core.initialize();

    let deadline = Instant::now() + Duration::from_secs(60);
    let install_id = loop {
        assert!(Instant::now() < deadline, "no background upgrade started");
        let Some(message) = core.read_json_timeout(Duration::from_secs(15)) else {
            panic!("no background upgrade started");
        };
        if message["method"] == "runtime.installEvent" {
            break message["params"]["installID"]
                .as_str()
                .expect("installID")
                .to_string();
        }
    };
    let events = drain_install_events(&mut core, &install_id);
    assert_eq!(phases(&events).last().map(String::as_str), Some("failed"));

    let listed = core.send(json!({"id": 921, "method": "agent.list", "params": {}}));
    let claude = listed["result"]["agents"]
        .as_array()
        .and_then(|agents| agents.iter().find(|agent| agent["agentID"] == "claude"))
        .unwrap_or_else(|| panic!("claude missing: {listed}"));
    assert_eq!(claude["runtime"]["availability"], "ready", "{claude}");
    assert_eq!(claude["runtime"]["source"], "managed");
    assert_eq!(claude["runtime"]["nodeVersion"], "v24.0.0");
    assert_eq!(
        claude["runtime"]["managedUpdate"]["version"],
        PINNED_VERSION
    );
    assert_eq!(claude["runtime"]["managedUpdate"]["state"], "failed");
    assert!(
        claude["runtime"]["managedUpdate"]["error"].is_string(),
        "{claude}"
    );
    assert!(
        core.ottam_home_path()
            .join("runtime/node/v24.0.0/bin/node")
            .exists(),
        "a failed upgrade removed the runtime the agents run on"
    );

    core.shutdown();
}

/// A payload that does not match its pin is refused and nothing is published.
/// Serving a real tarball under a digest core will not accept is the same
/// failure as a corrupted download or a tampered mirror.
#[test]
fn a_payload_that_fails_its_checksum_is_never_published() {
    let dist = TempDir::new().expect("dist directory");
    let (tarball, _) = build_fake_node_tarball(dist.path());
    let server = TarballServer::serving(&tarball);

    let mut core = CoreProcess::new_with_env(vec![
        ("OTTAM_NODE_DIST_BASE".to_string(), server.base_url),
        ("OTTAM_NODE_DIST_SHA256".to_string(), "0".repeat(64)),
    ]);
    core.initialize();

    let accepted = core.send(json!({"id": 910, "method": "runtime.install", "params": {}}));
    let install_id = accepted["result"]["installID"]
        .as_str()
        .expect("installID")
        .to_string();

    let events = drain_install_events(&mut core, &install_id);
    let last = events.last().expect("a terminal event");
    assert_eq!(last["phase"], "failed");
    assert!(
        last["error"]
            .as_str()
            .unwrap_or_default()
            .contains("checksum"),
        "the failure should name the checksum: {last}"
    );

    assert!(
        !core
            .ottam_home_path()
            .join("runtime/node")
            .join(PINNED_VERSION)
            .exists(),
        "a corrupt payload was published"
    );

    core.shutdown();
}

/// Cancel while the bytes are still moving. The server here never finishes, so
/// the only way this test terminates is the cancellation actually reaching the
/// in-flight download.
#[test]
fn cancelling_mid_download_leaves_nothing_behind() {
    let server = TarballServer::stalling();

    let mut core = CoreProcess::new_with_env(vec![(
        "OTTAM_NODE_DIST_BASE".to_string(),
        server.base_url,
    )]);
    core.initialize();

    let accepted = core.send(json!({"id": 930, "method": "runtime.install", "params": {}}));
    let install_id = accepted["result"]["installID"]
        .as_str()
        .expect("installID")
        .to_string();

    // Wait until the transfer is genuinely under way; cancelling during
    // `starting` would prove nothing about the download path.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "the download never started");
        let Some(message) = core.read_json_timeout(Duration::from_secs(15)) else {
            panic!("the download never started");
        };
        if message["method"] == "runtime.installEvent"
            && message["params"]["phase"] == "downloading"
        {
            break;
        }
    }

    let cancelled = core.send(json!({
        "id": 931,
        "method": "runtime.installCancel",
        "params": {"installID": install_id},
    }));
    assert!(cancelled["error"].is_null(), "cancel failed: {cancelled}");

    let events = drain_install_events(&mut core, &install_id);
    assert_eq!(
        events.last().expect("a terminal event")["phase"],
        "cancelled"
    );

    let runtime_root = core.ottam_home_path().join("runtime/node");
    assert!(
        !runtime_root.join(PINNED_VERSION).exists(),
        "a cancelled install was published"
    );
    // No `.part` file, no staging directory — a cancelled install must not
    // leave the next one to clean up after it.
    let leftovers: Vec<String> = std::fs::read_dir(runtime_root.join("downloads"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        leftovers.is_empty(),
        "partial downloads survived: {leftovers:?}"
    );

    core.shutdown();
}

/// Cancelling an id core never issued is deliberately not an error: a client
/// racing the terminal event should not be punished for arriving second.
#[test]
fn cancelling_an_unknown_install_is_a_no_op() {
    let mut core = CoreProcess::new();
    core.initialize();

    let response = core.send(json!({
        "id": 920,
        "method": "runtime.installCancel",
        "params": {"installID": "runtime_install_never_issued"},
    }));
    assert!(response["error"].is_null(), "unexpected error: {response}");
    assert!(response["result"].is_object());

    core.shutdown();
}
