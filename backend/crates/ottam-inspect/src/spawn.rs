//! Spawn mode: a fresh ottam-core against a scratch home.
//!
//! For probing without the running app's state (or when no app is running).
//! The scratch home means no flock conflict and no risk to real data; the
//! env defaults keep the core from spawning adapter/npx subprocesses unless
//! the caller explicitly re-enables them.

use crate::client::Connection;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Env vars defaulted off for a scratch core; each is only set when the
/// caller has not already exported a value, so they stay overridable.
const QUIET_ENV_DEFAULTS: &[(&str, &str)] = &[
    ("OTTAM_MODEL_CATALOG_PROBE", "off"),
    ("OTTAM_ADAPTER_INSTALLS", "off"),
    ("OTTAM_MANAGED_NODE", "off"),
];

pub struct SpawnedCore {
    pub connection: Connection,
    pub home: PathBuf,
    child: Child,
    /// Present while the home should be deleted on drop (no `--keep-home`).
    _temp_home: Option<tempfile::TempDir>,
}

pub struct SpawnOptions {
    pub core_bin: Option<PathBuf>,
    pub keep_home: bool,
    /// Clone `<source-home>/agents` (installed adapters) into the scratch
    /// home so install-state questions answer like the real home would.
    pub copy_agents_from: Option<PathBuf>,
}

impl SpawnedCore {
    pub fn shutdown(mut self) {
        // Dropping the connection closes the child's stdin; stdin EOF is the
        // core's normal shutdown signal.
        drop(self.connection);
        let _ = self.child.wait();
        if self._temp_home.is_none() {
            // --keep-home: the caller wants the scratch home for inspection.
            eprintln!("scratch home kept at {}", self.home.display());
        }
    }
}

pub fn spawn(options: &SpawnOptions, timeout: Duration) -> Result<SpawnedCore, String> {
    let core_bin = resolve_core_bin(options.core_bin.as_deref())?;
    let temp_home = tempfile::TempDir::with_prefix("ottam-inspect-")
        .map_err(|error| format!("failed to create a scratch home: {error}"))?;
    let (home, temp_home) = if options.keep_home {
        (temp_home.keep(), None)
    } else {
        (temp_home.path().to_path_buf(), Some(temp_home))
    };
    if let Some(source_home) = &options.copy_agents_from {
        let source = source_home.join("agents");
        if source.is_dir() {
            copy_tree(&source, &home.join("agents")).map_err(|error| {
                format!("failed to copy agents from {}: {error}", source.display())
            })?;
        }
    }

    let mut command = Command::new(&core_bin);
    command
        .env("OTTAM_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in QUIET_ENV_DEFAULTS {
        // An explicit export from the caller always wins.
        if std::env::var_os(key).is_some() {
            continue;
        }
        // `--copy-agents` exists to make install-state questions answer like
        // the real home. Defaulting the store off would leave the copied tree
        // unread and every agent reporting the opted-out `installed` fallback —
        // the one answer the flag is meant to avoid.
        if *key == "OTTAM_ADAPTER_INSTALLS" && options.copy_agents_from.is_some() {
            continue;
        }
        command.env(key, value);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to spawn {}: {error}", core_bin.display()))?;
    let stdin = child.stdin.take().expect("stdin is piped");
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut connection = Connection::new(stdout, stdin);

    // Spawn mode is this core's primary client, so it initializes normally.
    connection.round_trip(&initialize_request(), timeout)?;
    Ok(SpawnedCore {
        connection,
        home,
        child,
        _temp_home: temp_home,
    })
}

pub fn initialize_request() -> serde_json::Value {
    json!({
        "id": "inspect/initialize",
        "method": "initialize",
        "params": {
            "clientInfo": {
                "name": "ottam-inspect",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {"experimentalMethods": []}
        }
    })
}

/// `--core-bin` > `$OTTAM_CORE_PATH` > a `ottam-core` sibling of this
/// binary (the layout `cargo build` produces in `target/<profile>/`).
fn resolve_core_bin(flag: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = flag {
        return Ok(path.to_path_buf());
    }
    if let Some(configured) = std::env::var_os("OTTAM_CORE_PATH") {
        return Ok(PathBuf::from(configured));
    }
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("ottam-core")));
    match sibling {
        Some(path) if path.is_file() => Ok(path),
        _ => Err(
            "cannot find ottam-core: pass --core-bin, set OTTAM_CORE_PATH, \
             or build it next to ottam-inspect"
                .to_string(),
        ),
    }
}

fn copy_tree(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let to = target.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &to)?;
        }
        // Symlinks are skipped: adapter installs are plain trees.
    }
    Ok(())
}
