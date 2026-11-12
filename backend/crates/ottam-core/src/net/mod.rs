//! Fetching remote artifacts.
//!
//! One download path for everything Ottam installs on the user's behalf —
//! the managed Node runtime today, adapter packages and vendor binaries next.
//! The shape is the same every time: fetch, hash while the bytes stream past,
//! verify against a pin, land the file atomically, report progress along the
//! way. Keeping it here rather than inside any one installer is what lets a new
//! payload be a new caller instead of a second pipeline.
//!
//! The client is deliberately in-process rather than a `curl` subprocess:
//! credentials belong in a header rather than on `argv` where `ps` can read
//! them, a status code is a better error than an exit code, and hashing during
//! the stream avoids a second pass over a file that may be very large.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

/// Long enough to survive a slow DNS answer or a captive-portal stall, short
/// enough that an unreachable host fails while the user is still watching.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-read, not per-request: a multi-gigabyte download is allowed to take as
/// long as it takes, but a connection that has gone silent must not hang the
/// install forever.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Progress is throttled to bound the notification rate no matter how fast the
/// transfer runs. A fast link delivers chunks far more often than any UI can
/// use, and every event crosses the JSONL channel to the app.
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(100);

/// What to fetch, where to put it, and how to know it arrived intact.
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub url: String,
    pub dest: PathBuf,
    /// Lowercase hex. `None` skips verification, which is only right for
    /// payloads that carry their own integrity story.
    pub expected_sha256: Option<String>,
}

impl DownloadRequest {
    pub fn new(url: impl Into<String>, dest: impl Into<PathBuf>) -> Self {
        Self {
            url: url.into(),
            dest: dest.into(),
            expected_sha256: None,
        }
    }

    pub fn expecting_sha256(mut self, digest: impl Into<String>) -> Self {
        self.expected_sha256 = Some(digest.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadProgress {
    /// `total` is absent when the server sends no content length.
    Started {
        total: Option<u64>,
    },
    Received {
        received: u64,
        total: Option<u64>,
    },
    Verifying,
}

/// Distinguishable failures, because they call for different answers: a bad
/// checksum is worth retrying, a 404 means the pin is wrong and retrying will
/// never help, and a dropped connection is the user's network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    Http { status: u16 },
    Network(String),
    ChecksumMismatch { expected: String, actual: String },
    Io(String),
    Cancelled,
}

impl fmt::Display for DownloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { status } => write!(formatter, "server returned HTTP {status}"),
            Self::Network(detail) => write!(formatter, "network error: {detail}"),
            Self::ChecksumMismatch { expected, actual } => write!(
                formatter,
                "checksum mismatch: expected {expected}, got {actual}"
            ),
            Self::Io(detail) => write!(formatter, "file error: {detail}"),
            Self::Cancelled => write!(formatter, "cancelled"),
        }
    }
}

impl std::error::Error for DownloadError {}

/// A shared HTTP client. Build one per process and clone the handle: reqwest
/// pools connections internally, so a per-request client would throw away the
/// TLS handshake every time.
#[derive(Debug, Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
}

impl HttpClient {
    pub fn new() -> Result<Self, DownloadError> {
        let inner = reqwest::Client::builder()
            .user_agent(concat!("Ottam/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .map_err(network_error)?;
        Ok(Self { inner })
    }

    /// Stream `request.url` to `request.dest`.
    ///
    /// The bytes land in a sibling `.part` file and are renamed into place only
    /// after the checksum passes, so `dest` never exists in a half-written or
    /// corrupt state — callers can treat its presence as proof of a good
    /// payload. Every failure path, cancellation included, removes the partial.
    pub async fn download(
        &self,
        request: DownloadRequest,
        cancel: &CancellationToken,
        on_progress: impl Fn(DownloadProgress),
    ) -> Result<PathBuf, DownloadError> {
        if let Some(parent) = request.dest.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(io_error)?;
        }
        let partial = partial_path(&request.dest);
        // A `.part` left by an earlier crash is not resumable: we have no way
        // to know how much of it is sound, and its bytes were never hashed.
        let _ = tokio::fs::remove_file(&partial).await;

        match self
            .stream_to_file(&request, &partial, cancel, &on_progress)
            .await
        {
            Ok(()) => {
                tokio::fs::rename(&partial, &request.dest)
                    .await
                    .map_err(io_error)?;
                Ok(request.dest)
            }
            Err(error) => {
                let _ = tokio::fs::remove_file(&partial).await;
                Err(error)
            }
        }
    }

    async fn stream_to_file(
        &self,
        request: &DownloadRequest,
        partial: &Path,
        cancel: &CancellationToken,
        on_progress: &impl Fn(DownloadProgress),
    ) -> Result<(), DownloadError> {
        let mut response = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(DownloadError::Cancelled),
            result = self.inner.get(&request.url).send() => result.map_err(network_error)?,
        };
        if !response.status().is_success() {
            return Err(DownloadError::Http {
                status: response.status().as_u16(),
            });
        }

        let total = response.content_length();
        on_progress(DownloadProgress::Started { total });

        let mut file = tokio::fs::File::create(partial).await.map_err(io_error)?;
        let mut hasher = Sha256::new();
        let mut received = 0u64;
        let mut throttle = ProgressThrottle::new();

        loop {
            let chunk = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(DownloadError::Cancelled),
                result = response.chunk() => result.map_err(network_error)?,
            };
            let Some(chunk) = chunk else { break };
            hasher.update(&chunk);
            file.write_all(&chunk).await.map_err(io_error)?;
            received += chunk.len() as u64;
            if throttle.ready() {
                on_progress(DownloadProgress::Received { received, total });
            }
        }
        file.flush().await.map_err(io_error)?;
        drop(file);
        // The throttle can swallow the last chunk, and a progress bar that
        // stops at 99% reads as a hang.
        on_progress(DownloadProgress::Received { received, total });

        if let Some(expected) = &request.expected_sha256 {
            on_progress(DownloadProgress::Verifying);
            let actual = hex::encode(hasher.finalize());
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(DownloadError::ChecksumMismatch {
                    expected: expected.clone(),
                    actual,
                });
            }
        }
        Ok(())
    }
}

fn partial_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

fn network_error(error: reqwest::Error) -> DownloadError {
    DownloadError::Network(error.to_string())
}

fn io_error(error: std::io::Error) -> DownloadError {
    DownloadError::Io(error.to_string())
}

struct ProgressThrottle {
    last_emitted_at: Option<Instant>,
}

impl ProgressThrottle {
    fn new() -> Self {
        Self {
            last_emitted_at: None,
        }
    }

    fn ready(&mut self) -> bool {
        let ready = self
            .last_emitted_at
            .is_none_or(|at| at.elapsed() >= PROGRESS_MIN_INTERVAL);
        if ready {
            self.last_emitted_at = Some(Instant::now());
        }
        ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    /// A single-request HTTP server. Enough to exercise the real client
    /// against a real socket without reaching the network.
    struct TestServer {
        url: String,
        _task: tokio::task::JoinHandle<()>,
    }

    impl TestServer {
        /// Serve `body` with a content length, then close.
        async fn serving(body: Vec<u8>) -> Self {
            Self::spawn(move |mut socket| async move {
                let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                let _ = socket.write_all(header.as_bytes()).await;
                let _ = socket.write_all(&body).await;
                let _ = socket.flush().await;
            })
            .await
        }

        /// Announce a body, send a first chunk, then stall forever — the shape
        /// a cancellation has to interrupt.
        async fn stalling() -> Self {
            Self::spawn(|mut socket| async move {
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\nhead")
                    .await;
                let _ = socket.flush().await;
                std::future::pending::<()>().await;
            })
            .await
        }

        async fn failing(status: u16) -> Self {
            Self::spawn(move |mut socket| async move {
                let header = format!("HTTP/1.1 {status} Nope\r\nContent-Length: 0\r\n\r\n");
                let _ = socket.write_all(header.as_bytes()).await;
                let _ = socket.flush().await;
            })
            .await
        }

        async fn spawn<F, Fut>(respond: F) -> Self
        where
            F: FnOnce(tokio::net::TcpStream) -> Fut + Send + 'static,
            Fut: std::future::Future<Output = ()> + Send,
        {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let address = listener.local_addr().expect("address");
            let task = tokio::spawn(async move {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                // Drain the request line and headers; reqwest will not read a
                // response written before its request is consumed.
                let mut buffer = [0u8; 1024];
                let _ = socket.read(&mut buffer).await;
                respond(socket).await;
            });
            Self {
                url: format!("http://{address}/artifact"),
                _task: task,
            }
        }
    }

    fn sha256_of(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    #[tokio::test]
    async fn a_verified_download_lands_at_its_destination() {
        let body = b"managed node tarball".to_vec();
        let server = TestServer::serving(body.clone()).await;
        let directory = tempfile::tempdir().expect("tempdir");
        let dest = directory.path().join("nested").join("artifact.tar.gz");
        let client = HttpClient::new().expect("client");

        let path = client
            .download(
                DownloadRequest::new(&server.url, &dest).expecting_sha256(sha256_of(&body)),
                &CancellationToken::new(),
                |_| {},
            )
            .await
            .expect("download");

        assert_eq!(path, dest);
        assert_eq!(std::fs::read(&dest).expect("read"), body);
        assert!(
            !partial_path(&dest).exists(),
            "partial file was left behind"
        );
    }

    #[tokio::test]
    async fn a_checksum_mismatch_fails_and_leaves_nothing_behind() {
        let server = TestServer::serving(b"corrupted payload".to_vec()).await;
        let directory = tempfile::tempdir().expect("tempdir");
        let dest = directory.path().join("artifact.tar.gz");
        let client = HttpClient::new().expect("client");

        let error = client
            .download(
                DownloadRequest::new(&server.url, &dest).expecting_sha256(sha256_of(b"expected")),
                &CancellationToken::new(),
                |_| {},
            )
            .await
            .expect_err("should reject a bad payload");

        assert!(
            matches!(error, DownloadError::ChecksumMismatch { .. }),
            "unexpected error: {error}"
        );
        // The whole point of the `.part` staging: a corrupt payload must never
        // be visible at the destination a caller trusts.
        assert!(!dest.exists(), "corrupt payload was published");
        assert!(
            !partial_path(&dest).exists(),
            "partial file was left behind"
        );
    }

    #[tokio::test]
    async fn an_http_failure_reports_its_status() {
        let server = TestServer::failing(404).await;
        let directory = tempfile::tempdir().expect("tempdir");
        let dest = directory.path().join("artifact.tar.gz");
        let client = HttpClient::new().expect("client");

        let error = client
            .download(
                DownloadRequest::new(&server.url, &dest),
                &CancellationToken::new(),
                |_| {},
            )
            .await
            .expect_err("should surface the status");

        assert_eq!(error, DownloadError::Http { status: 404 });
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn cancelling_mid_stream_stops_and_cleans_up() {
        let server = TestServer::stalling().await;
        let directory = tempfile::tempdir().expect("tempdir");
        let dest = directory.path().join("artifact.tar.gz");
        let client = HttpClient::new().expect("client");
        let cancel = CancellationToken::new();

        let watcher = cancel.clone();
        let observed = std::sync::Arc::new(Mutex::new(Vec::new()));
        let recorder = std::sync::Arc::clone(&observed);
        // Cancel once the transfer is demonstrably underway, so the test proves
        // an interrupted stream rather than a request that never started.
        let download = client.download(
            DownloadRequest::new(&server.url, &dest),
            &cancel,
            move |progress| {
                recorder.lock().expect("lock").push(progress);
                if matches!(progress, DownloadProgress::Received { .. }) {
                    watcher.cancel();
                }
            },
        );

        let error = download.await.expect_err("should cancel");

        assert_eq!(error, DownloadError::Cancelled);
        assert!(!dest.exists());
        assert!(
            !partial_path(&dest).exists(),
            "partial file was left behind"
        );
        assert!(
            observed
                .lock()
                .expect("lock")
                .iter()
                .any(|progress| matches!(progress, DownloadProgress::Received { .. })),
            "expected the stream to start before cancelling"
        );
    }

    #[tokio::test]
    async fn progress_reports_a_total_and_a_final_exact_count() {
        let body = vec![7u8; 5000];
        let server = TestServer::serving(body.clone()).await;
        let directory = tempfile::tempdir().expect("tempdir");
        let dest = directory.path().join("artifact.bin");
        let client = HttpClient::new().expect("client");
        let observed = std::sync::Arc::new(Mutex::new(Vec::new()));
        let recorder = std::sync::Arc::clone(&observed);

        client
            .download(
                DownloadRequest::new(&server.url, &dest),
                &CancellationToken::new(),
                move |progress| recorder.lock().expect("lock").push(progress),
            )
            .await
            .expect("download");

        let events = observed.lock().expect("lock").clone();
        assert_eq!(
            events.first(),
            Some(&DownloadProgress::Started {
                total: Some(body.len() as u64)
            })
        );
        assert_eq!(
            events.last(),
            Some(&DownloadProgress::Received {
                received: body.len() as u64,
                total: Some(body.len() as u64),
            }),
            "the final event must carry the exact byte count"
        );
    }

    #[test]
    fn the_throttle_bounds_the_event_rate() {
        let mut throttle = ProgressThrottle::new();
        assert!(throttle.ready(), "the first chunk should always report");
        assert!(!throttle.ready(), "a chunk on its heels should be dropped");
        std::thread::sleep(PROGRESS_MIN_INTERVAL);
        assert!(throttle.ready(), "the interval should reopen the gate");
    }

    #[test]
    fn the_partial_path_sits_beside_its_destination() {
        assert_eq!(
            partial_path(Path::new("/tmp/runtime/node.tar.gz")),
            PathBuf::from("/tmp/runtime/node.tar.gz.part")
        );
    }
}
