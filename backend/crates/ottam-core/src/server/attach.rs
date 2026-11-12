//! The attach transport: `$OTTAM_HOME/core.sock`, or a short path under
//! `/tmp` when that one is too long to bind (see [`socket_path`]).
//!
//! While the core runs it accepts secondary ("attached") connections over a
//! unix socket beside its home. Attached clients speak the same JSONL JSON-RPC
//! as the primary — same dispatcher, same handlers — with three structural
//! differences: their responses route back by [`ConnectionId`], they receive
//! broadcast notifications but never core→client requests, and their
//! disconnect is just a registry removal while primary EOF ends the process.
//!
//! The socket exists for inspection tooling (`ottam-inspect`, agents
//! debugging the live app) and any future CLI. It is 0600 and unauthenticated:
//! same trust domain as the sqlite file beside it. `OTTAM_ATTACH=off`
//! disables it; a bind failure is never fatal.

use crate::server::client_link::PendingClientRequests;
use crate::server::outbound::{AttachedRegistry, ConnectionId, OutboundSink};
use crate::server::transport::{reader_task, writer_task, InboundMessage};
use crate::terminal::TerminalOutputQueue;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::Sender;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub(super) const SOCKET_FILE_NAME: &str = "core.sock";

/// macOS caps `sun_path` at 104 bytes including the terminator (Linux: 108).
const MAX_SOCKET_PATH_BYTES: usize = 103;

/// Where the attach socket for `home` lives: `home/core.sock` when it fits,
/// otherwise `/tmp/ottam-<uid>/<hash of the canonical home>.sock`, so a
/// deep home (a linked worktree's isolated instance) still gets one.
/// `ottam-inspect` derives the same path; the shared test vector in
/// `long_homes_use_a_short_path` keeps the two in step.
pub(crate) fn socket_path(home: &Path) -> PathBuf {
    let direct = home.join(SOCKET_FILE_NAME);
    if direct.as_os_str().len() <= MAX_SOCKET_PATH_BYTES {
        return direct;
    }
    let canonical = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    // SAFETY: getuid has no preconditions and cannot fail.
    short_socket_path(&canonical, unsafe { libc::getuid() })
}

fn short_socket_path(canonical_home: &Path, uid: u32) -> PathBuf {
    // FNV-1a, spelled out so the inspector can match it without sharing a
    // crate; std's hasher is not stable across releases.
    let hash = canonical_home
        .as_os_str()
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    PathBuf::from(format!("/tmp/ottam-{uid}/{hash:016x}.sock"))
}

/// Creates the short path's directory, or refuses one this user does not
/// own exclusively: `/tmp` is shared, and the socket is unauthenticated.
fn prepare_short_socket_dir(dir: &Path) -> Result<(), String> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    let metadata = std::fs::symlink_metadata(dir).map_err(|error| error.to_string())?;
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    if !metadata.is_dir() || metadata.uid() != uid || metadata.permissions().mode() & 0o077 != 0 {
        return Err("not a private directory owned by this user".into());
    }
    Ok(())
}

/// Attached writers never carry terminal-output deltas — the queue only exists
/// to satisfy the writer task's signature and stays empty.
const ATTACHED_TERMINAL_QUEUE_BYTES: usize = 1024;

pub(super) struct AttachRuntime {
    handle: JoinHandle<()>,
}

impl AttachRuntime {
    /// Waits for the accept loop to finish tearing down (it unlinks the
    /// socket on the way out). Only meaningful after `primary_done` fired.
    pub(super) async fn finish(self) {
        if let Err(join_error) = self.handle.await {
            tracing::warn!(error = %join_error, "attach accept task panicked");
        }
    }
}

/// Binds the socket and spawns the accept loop, or returns `None` (gate off /
/// bind failure) — the server runs identically without the attach transport.
pub(super) fn spawn(
    home: &Path,
    registry: AttachedRegistry,
    dispatch_tx: Sender<InboundMessage>,
    outbound: OutboundSink,
    primary_done: CancellationToken,
) -> Option<AttachRuntime> {
    let (listener, socket_path) = bind(home)?;
    let handle = tokio::spawn(accept_loop(
        listener,
        socket_path,
        registry,
        dispatch_tx,
        outbound,
        primary_done,
    ));
    Some(AttachRuntime { handle })
}

fn bind(home: &Path) -> Option<(UnixListener, PathBuf)> {
    if std::env::var("OTTAM_ATTACH").is_ok_and(|value| value == "off") {
        tracing::info!("attach socket disabled by OTTAM_ATTACH=off");
        return None;
    }
    let path = socket_path(home);
    if let Some(dir) = path.parent().filter(|dir| *dir != home) {
        if let Err(error) = prepare_short_socket_dir(dir) {
            tracing::warn!(
                dir = %dir.display(),
                error = %error,
                "attach socket directory unusable; running without the attach transport"
            );
            return None;
        }
    }
    // The core.lock flock is already held by this process, so an existing
    // socket file is stale — a previous core that exited without unlinking.
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            // Inspection is a convenience; never fatal.
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "attach socket bind failed; running without the attach transport"
            );
            return None;
        }
    };
    if let Err(error) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!(path = %path.display(), error = %error, "failed to chmod attach socket");
    }
    tracing::info!(path = %path.display(), "attach socket listening");
    Some((listener, path))
}

/// Accepts attached connections until the primary reader finishes (stdin EOF),
/// then aborts them — their dispatch senders drop, letting the dispatch queue
/// drain and close — and unlinks the socket.
async fn accept_loop(
    listener: UnixListener,
    socket_path: PathBuf,
    registry: AttachedRegistry,
    dispatch_tx: Sender<InboundMessage>,
    outbound: OutboundSink,
    primary_done: CancellationToken,
) {
    let mut connections = tokio::task::JoinSet::new();
    let mut next_id: u64 = 1;
    loop {
        tokio::select! {
            biased;
            _ = primary_done.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let id = next_id;
                    next_id += 1;
                    tracing::info!(connection = id, "attach connection accepted");
                    connections.spawn(serve_connection(
                        stream,
                        id,
                        registry.clone(),
                        dispatch_tx.clone(),
                        outbound.clone(),
                    ));
                }
                Err(error) => {
                    tracing::warn!(error = %error, "attach accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.shutdown().await;
    drop(listener);
    let _ = std::fs::remove_file(&socket_path);
    tracing::info!("attach socket closed");
}

/// One attached connection: a reader feeding the shared dispatcher and a
/// writer draining this connection's registry channel. Mirrors the primary's
/// actor split with two deliberate differences — a fresh (never-used)
/// `PendingClientRequests`, so response-shaped lines from an attached client
/// can never complete the app's pending core→client requests, and a fresh
/// empty terminal-output queue, so terminal deltas stay primary-only.
async fn serve_connection(
    stream: UnixStream,
    id: u64,
    registry: AttachedRegistry,
    dispatch_tx: Sender<InboundMessage>,
    outbound: OutboundSink,
) {
    let (read_half, write_half) = stream.into_split();
    let (writer_tx, writer_rx) = tokio::sync::mpsc::unbounded_channel();
    registry.register(id, writer_tx);
    // Removes the registry entry on every exit path, including this task
    // being aborted at shutdown; the writer then drains out and exits.
    let registry_guard = RegistryGuard { registry, id };
    let writer = tokio::spawn(writer_task(
        writer_rx,
        TerminalOutputQueue::new(ATTACHED_TERMINAL_QUEUE_BYTES),
        write_half,
    ));
    let result = reader_task(
        tokio::io::BufReader::new(read_half),
        ConnectionId::Attached(id),
        dispatch_tx,
        outbound,
        PendingClientRequests::default(),
    )
    .await;
    if let Err(error) = result {
        tracing::warn!(connection = id, error = %error, "attach connection reader failed");
    }
    drop(registry_guard);
    let _ = writer.await;
    tracing::info!(connection = id, "attach connection closed");
}

struct RegistryGuard {
    registry: AttachedRegistry,
    id: u64,
}

impl Drop for RegistryGuard {
    fn drop(&mut self) {
        self.registry.remove(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_homes_keep_the_socket_beside_them() {
        let home = Path::new("/Users/someone/.ottam");
        assert_eq!(socket_path(home), home.join(SOCKET_FILE_NAME));
    }

    #[test]
    fn long_homes_use_a_short_path() {
        // Shared with ottam-inspect's copy; change both together.
        assert_eq!(
            short_socket_path(Path::new("/a/deep/ottam/home"), 501),
            PathBuf::from("/tmp/ottam-501/792a758c0955099a.sock")
        );
        let home = PathBuf::from(format!("/nonexistent/{}", "d".repeat(120)));
        let path = socket_path(&home);
        assert!(path.as_os_str().len() <= MAX_SOCKET_PATH_BYTES);
        assert_eq!(path, socket_path(&home));
        assert_ne!(path, socket_path(&home.join("other")));
    }

    #[test]
    fn the_short_directory_must_be_private_and_ours() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sockets");
        prepare_short_socket_dir(&dir).unwrap();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare_short_socket_dir(&dir).is_err());
    }
}
