//! Binary-lane adapter installs, end to end over the wire.
//!
//! Hermetic: the opt-in `test.binary` agent downloads from a loopback HTTP
//! server, and the "vendor binary" it gets is a shell script that execs the
//! fake ACP agent. The real download, digest check, `tar`, publish, and
//! handshake all run; only the host the bytes come from is faked.

use crate::support::{fake_acp_binary, CoreProcess, TarballServer};
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const AGENT: &str = "test.binary";
const VERSION: &str = "1.0.0";
const COMMAND: &str = "binary-test-acp";

/// The executable a vendor would ship: here, a script that becomes the fake
/// ACP agent.
fn write_fake_binary(path: &Path) -> Vec<u8> {
    let body = format!("#!/bin/sh\nexec '{}' \"$@\"\n", fake_acp_binary().display());
    std::fs::create_dir_all(path.parent().expect("parent")).expect("directories");
    std::fs::write(path, &body).expect("write binary");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    body.into_bytes()
}

fn sha256(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read download");
    hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes))
}

/// A raw executable download, as oh-my-pi ships.
fn raw_download(into: &Path) -> PathBuf {
    let path = into.join("download");
    write_fake_binary(&path);
    path
}

/// An npm-style platform tarball with the executable at `package/bin/<cmd>`,
/// as OpenCode ships.
fn tarball_download(into: &Path) -> PathBuf {
    let staging = into.join("staging");
    write_fake_binary(&staging.join("package/bin").join(COMMAND));
    std::fs::write(staging.join("package/package.json"), "{}").expect("package.json");
    let tarball = into.join("download.tgz");
    let status = std::process::Command::new("/usr/bin/tar")
        .args([
            "-czf",
            &tarball.to_string_lossy(),
            "-C",
            &staging.to_string_lossy(),
            "package",
        ])
        .status()
        .expect("tar runs");
    assert!(status.success(), "could not build the fake tarball");
    tarball
}

fn environment(url: &str, sha256: &str, member: Option<&str>) -> Vec<(String, String)> {
    let mut environment = vec![
        ("OTTAM_ADAPTER_INSTALLS".to_string(), "on".to_string()),
        (
            "OTTAM_TEST_BINARY_AGENT_URL".to_string(),
            url.to_string(),
        ),
        (
            "OTTAM_TEST_BINARY_AGENT_SHA256".to_string(),
            sha256.to_string(),
        ),
    ];
    if let Some(member) = member {
        environment.push((
            "OTTAM_TEST_BINARY_AGENT_MEMBER".to_string(),
            member.to_string(),
        ));
    }
    environment
}

fn adapter_dir(home: &Path) -> PathBuf {
    home.join("agents").join(AGENT).join("adapter")
}

fn start_install(core: &mut CoreProcess, id: u64) -> String {
    let accepted = core.send(json!({
        "id": id,
        "method": "agent.install",
        "params": {"agentID": AGENT},
    }));
    assert!(accepted["error"].is_null(), "install refused: {accepted}");
    assert_eq!(accepted["result"]["version"], VERSION);
    accepted["result"]["installID"]
        .as_str()
        .expect("installID")
        .to_string()
}

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
        assert_eq!(params["installID"].as_str(), Some(install_id));
        let phase = params["phase"].as_str().unwrap_or_default().to_string();
        events.push(params);
        if matches!(phase.as_str(), "ready" | "failed" | "cancelled") {
            return events;
        }
    }
    panic!("no terminal install event arrived; saw {events:?}");
}

fn binary_agent(core: &mut CoreProcess, id: u64) -> Value {
    let agents = core.send(json!({"id": id, "method": "agent.list", "params": {}}));
    agents["result"]["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .find(|agent| agent["agentID"] == AGENT)
        .unwrap_or_else(|| panic!("no {AGENT} agent in {agents}"))
        .clone()
}

fn leftovers(home: &Path) -> Vec<String> {
    std::fs::read_dir(adapter_dir(home))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

fn assert_installs(download: &Path, member: Option<&str>) {
    let server = TarballServer::serving(download);
    let mut core =
        CoreProcess::new_with_env(environment(&server.base_url, &sha256(download), member));
    core.initialize();

    // Before: no Node involved, and the pin is not on disk.
    let agent = binary_agent(&mut core, 900);
    assert_eq!(agent["adapterPackage"]["source"], "binary");
    assert_eq!(agent["adapterPackage"]["installState"], "notInstalled");
    assert_eq!(agent["runtime"]["availability"], "ready");
    assert!(agent["runtime"]["nodeVersion"].is_null(), "{agent}");

    let install_id = start_install(&mut core, 901);
    let events = drain_install_events(&mut core, &install_id);
    let phases: Vec<_> = events
        .iter()
        .map(|event| event["phase"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(phases.last(), Some(&"ready"), "install failed: {events:?}");
    assert!(phases.contains(&"verifying"), "{phases:?}");

    let version_dir = adapter_dir(core.ottam_home_path()).join(VERSION);
    let executable = version_dir.join("bin").join(COMMAND);
    let mode = std::fs::metadata(&executable)
        .expect("the executable was published")
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111, "the executable is not executable");
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(version_dir.join("manifest.json")).expect("the manifest was published"),
    )
    .expect("the manifest parses");
    assert_eq!(
        manifest["integrity"],
        format!("sha256:{}", sha256(download))
    );
    assert_eq!(manifest["entrypoint"], format!("bin/{COMMAND}"));
    assert!(manifest["nodeVersion"].is_null());
    // Only the published version remains: no staging, no download.
    assert_eq!(leftovers(core.ottam_home_path()), vec![VERSION]);
    // The handshake ran in a managed home with its declared directory.
    assert!(core
        .ottam_home_path()
        .join("agents")
        .join(AGENT)
        .join("home/config")
        .is_dir());

    let agent = binary_agent(&mut core, 902);
    assert_eq!(agent["adapterPackage"]["installState"], "installed");
    core.shutdown();
}

#[test]
fn binary_lane_installs_a_raw_executable_without_node() {
    let fixtures = TempDir::new().expect("fixtures");
    assert_installs(&raw_download(fixtures.path()), None);
}

#[test]
fn binary_lane_installs_an_executable_from_a_tarball() {
    let fixtures = TempDir::new().expect("fixtures");
    let member = format!("package/bin/{COMMAND}");
    assert_installs(&tarball_download(fixtures.path()), Some(&member));
}

#[test]
fn binary_lane_rejects_a_digest_mismatch() {
    let fixtures = TempDir::new().expect("fixtures");
    let download = raw_download(fixtures.path());
    let server = TarballServer::serving(&download);
    let mut core = CoreProcess::new_with_env(environment(&server.base_url, &"0".repeat(64), None));
    core.initialize();

    let install_id = start_install(&mut core, 910);
    let events = drain_install_events(&mut core, &install_id);
    let last = events.last().expect("a terminal event");
    assert_eq!(last["phase"], "failed");
    assert!(
        last["error"]
            .as_str()
            .unwrap_or_default()
            .contains("download"),
        "the failure should name the download: {last}"
    );
    assert!(leftovers(core.ottam_home_path()).is_empty());
    core.shutdown();
}

#[test]
fn binary_lane_cancels_mid_download() {
    let server = TarballServer::stalling();
    let mut core = CoreProcess::new_with_env(environment(&server.base_url, &"0".repeat(64), None));
    core.initialize();

    let install_id = start_install(&mut core, 920);
    // Let the request reach the stalling server before cancelling.
    std::thread::sleep(Duration::from_millis(500));
    let cancelled = core.send(json!({
        "id": 921,
        "method": "agent.installCancel",
        "params": {"installID": install_id},
    }));
    assert!(cancelled["error"].is_null(), "cancel failed: {cancelled}");
    let events = drain_install_events(&mut core, &install_id);
    assert_eq!(
        events.last().expect("a terminal event")["phase"],
        "cancelled"
    );
    assert!(leftovers(core.ottam_home_path()).is_empty());
    core.shutdown();
}

#[test]
fn binary_lane_rolls_back_a_failed_handshake() {
    let fixtures = TempDir::new().expect("fixtures");
    let download = raw_download(fixtures.path());
    let server = TarballServer::serving(&download);
    let mut environment = environment(&server.base_url, &sha256(&download), None);
    environment.push((
        "OTTAM_TEST_ACP_DIE_BEFORE_INITIALIZE".to_string(),
        "1".to_string(),
    ));
    let mut core = CoreProcess::new_with_env(environment);
    core.initialize();

    let install_id = start_install(&mut core, 930);
    let events = drain_install_events(&mut core, &install_id);
    let last = events.last().expect("a terminal event");
    assert_eq!(last["phase"], "failed");
    assert!(
        last["error"]
            .as_str()
            .unwrap_or_default()
            .contains("handshake"),
        "{last}"
    );
    assert!(!adapter_dir(core.ottam_home_path())
        .join(VERSION)
        .exists());
    core.shutdown();
}
