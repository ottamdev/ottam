//! Core's own MCP client connection to one configured server: a local
//! process speaking over its stdio, or a remote server over Streamable HTTP.
//!
//! A local process runs in its own process group with the workspace's first
//! root as its working directory, and its roots are the workspace's roots.
//! Its stderr is kept as one last line for the server's failed state, in
//! memory only: it can carry anything the server prints, so it is never
//! logged.
//!
//! A remote server signed in to carries its OAuth token, refreshed as it
//! nears expiry. A 401 that one refresh does not cure, with or without a
//! token, means the server needs a sign-in: [`UpstreamError::SignedOut`].

use super::calls::{self, InFlightCalls, UpstreamEvents};
use super::config::{ResolvedServer, ResolvedTransport};
use super::oauth::{self, StoredOAuth};
use axum::http::{HeaderName, HeaderValue};
use ottam_protocol::{McpServerID, WorkspaceID};
use rmcp::model::{
    ClientCapabilities, ClientConfig, ElicitRequestParams, ElicitResult, ElicitationCapability,
    FormElicitationCapability, Implementation,
};
// Roots are deprecated from protocol 2026-07-28 (SEP-2577), but servers on
// earlier versions still read them to find the project.
use rmcp::model::ClientJsonRpcMessage;
#[allow(deprecated)]
use rmcp::model::{ListRootsResult, Root};
use rmcp::service::{NotificationContext, RequestContext, RunningService};
use rmcp::transport::auth::AuthClient;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpClientTransportConfig, StreamableHttpError,
    StreamableHttpPostResponse,
};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::{ClientHandler, ErrorData, RoleClient, ServiceExt};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStderr, Command};

/// How long `initialize` may take before the server counts as failed.
pub(super) const START_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a stopped process has to exit after SIGTERM before SIGKILL.
const TERMINATE_GRACE: Duration = Duration::from_secs(3);
/// The longest stderr line a failed state carries.
const STDERR_TAIL_CHARS: usize = 300;
/// How much of one stderr line is held while it is read; enough for any
/// line's first `STDERR_TAIL_CHARS`, whatever its encoding.
const STDERR_LINE_BYTES: usize = 4 * STDERR_TAIL_CHARS;
/// How long an exited process's stderr may take to close.
const STDERR_SETTLE: Duration = Duration::from_millis(500);
/// Same-origin 307 and 308 redirects a remote server's client follows, as
/// `rmcp`'s own default client does.
const MAX_REDIRECTS: usize = 10;

/// Why a connection could not start, or why it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UpstreamError {
    /// The message a failed state shows; it never carries a value.
    Failed(String),
    /// The server needs a sign-in.
    SignedOut,
}

impl UpstreamError {
    pub(crate) fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }
}

/// The workspace a local server process runs for.
#[derive(Clone, Debug)]
pub(crate) struct McpWorkspace {
    pub(crate) id: WorkspaceID,
    /// The workspace's project directories, declared as the server's roots.
    /// The first is the process's working directory.
    pub(crate) roots: Vec<PathBuf>,
}

/// What core sends upstream on behalf of every session using the server.
pub(crate) type UpstreamPeer = rmcp::Peer<RoleClient>;

/// A live upstream connection as its callers use it. Cheap to clone.
#[derive(Clone)]
pub(crate) struct Upstream {
    pub(crate) peer: UpstreamPeer,
    /// Calls in flight, for attributing the server's own requests.
    pub(crate) calls: InFlightCalls,
    /// The roots the connection declared; empty for a remote server.
    pub(crate) roots: Arc<Vec<PathBuf>>,
    /// Whether the server's answer to the latest request was a 401 that no
    /// refresh cured.
    pub(crate) signed_out: Arc<AtomicBool>,
    /// The runtime's sign-in epoch when this connection started.
    pub(crate) sign_in_epoch: u64,
}

impl std::fmt::Debug for Upstream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Upstream")
            .field("roots", &self.roots)
            .finish_non_exhaustive()
    }
}

/// Where a connection's events go.
#[derive(Clone)]
pub(super) struct ConnectionHooks {
    pub(super) server_id: McpServerID,
    pub(super) workspace_id: Option<WorkspaceID>,
    pub(super) events: Option<Arc<dyn UpstreamEvents>>,
}

/// Core's side of the MCP session with an upstream server.
struct UpstreamClient {
    roots: Arc<Vec<PathBuf>>,
    calls: InFlightCalls,
    hooks: ConnectionHooks,
}

impl ClientHandler for UpstreamClient {
    #[allow(deprecated)]
    fn get_info(&self) -> ClientConfig {
        // A remote server is shared by every workspace, so it gets no roots.
        let mut capabilities = if self.roots.is_empty() {
            ClientCapabilities::default()
        } else {
            ClientCapabilities::builder().enable_roots().build()
        };
        // Forms go to the question panel; URL mode has nowhere to go yet.
        let mut elicitation = ElicitationCapability::default();
        elicitation.form = Some(FormElicitationCapability::default());
        capabilities.elicitation = Some(elicitation);
        ClientConfig::new(
            capabilities,
            Implementation::new("ottam", env!("CARGO_PKG_VERSION")),
        )
    }

    #[allow(deprecated)]
    async fn list_roots(
        &self,
        _context: RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, ErrorData> {
        Ok(list_roots(&self.roots))
    }

    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, ErrorData> {
        let (Some(origin), Some(events)) = (self.calls.sole_origin(), &self.hooks.events) else {
            tracing::info!(
                server = %self.hooks.server_id.0,
                "declined an MCP server's question that no single chat's call explains"
            );
            return Ok(calls::declined());
        };
        // The server may withdraw the question itself, too.
        let withdrawn = origin.cancelled.child_token();
        let _asked = withdrawn.clone().drop_guard();
        tokio::spawn({
            let (withdrawn, server_cancelled) = (withdrawn.clone(), context.ct);
            async move {
                tokio::select! {
                    () = server_cancelled.cancelled() => withdrawn.cancel(),
                    () = withdrawn.cancelled() => {}
                }
            }
        });
        let origin = calls::CallOrigin {
            cancelled: withdrawn,
            ..origin
        };
        Ok(events.elicit(origin, request).await)
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        if let Some(events) = &self.hooks.events {
            events.tools_changed(&self.hooks.server_id, self.hooks.workspace_id.as_ref());
        }
    }
}

#[allow(deprecated)]
fn list_roots(paths: &[PathBuf]) -> ListRootsResult {
    let roots = paths
        .iter()
        .filter_map(|path| {
            let uri = reqwest::Url::from_directory_path(path).ok()?;
            let root = Root::new(uri.to_string());
            Some(match path.file_name().and_then(|name| name.to_str()) {
                Some(name) => root.with_name(name),
                None => root,
            })
        })
        .collect();
    ListRootsResult::new(roots)
}

/// A live connection. Dropping it without [`Self::stop`] still kills a
/// local process's group, without waiting for it.
pub(super) struct Connection {
    pub(super) upstream: Upstream,
    service: RunningService<RoleClient, UpstreamClient>,
    process: Option<LocalProcess>,
    stderr_tail: StderrTail,
}

impl Connection {
    /// Wait until the connection ends on its own, or until `stop` fires.
    /// Either way the process is gone when this returns; the result says
    /// why the connection ended, `None` when it was stopped.
    pub(super) async fn run_until(
        self,
        stop: tokio_util::sync::CancellationToken,
    ) -> Option<UpstreamError> {
        let Self {
            upstream,
            service,
            process,
            stderr_tail,
        } = self;
        let cancel = service.cancellation_token();
        let mut waiting = std::pin::pin!(service.waiting());
        let stopped = tokio::select! {
            _ = stop.cancelled() => {
                cancel.cancel();
                let _ = waiting.as_mut().await;
                true
            }
            _ = waiting.as_mut() => false,
        };
        let status = match process {
            Some(process) => Some(process.terminate().await),
            None => None,
        };
        if stopped {
            return None;
        }
        if upstream.signed_out.load(Ordering::Acquire) {
            return Some(UpstreamError::SignedOut);
        }
        let reason = match status.flatten() {
            Some(status) => format!("The server exited ({status})"),
            None => "The server closed its connection".to_owned(),
        };
        Some(UpstreamError::Failed(with_tail(reason, &stderr_tail).await))
    }
}

/// Open a connection to `server`, for `workspace` when it is local.
/// `path_prefix` goes first on a local server's inherited PATH; a PATH the
/// server's own config sets still wins. A remote server uses the OAuth
/// credentials in `oauth` when it has any.
pub(super) async fn connect(
    server: &ResolvedServer,
    workspace: Option<&McpWorkspace>,
    path_prefix: Option<&Path>,
    hooks: ConnectionHooks,
    oauth: StoredOAuth,
) -> Result<Connection, UpstreamError> {
    let roots = workspace
        .map(|workspace| workspace.roots.clone())
        .unwrap_or_default();
    let client = UpstreamClient {
        roots: Arc::new(roots),
        calls: InFlightCalls::default(),
        hooks,
    };
    match &server.transport {
        ResolvedTransport::Stdio { command, args, env } => {
            connect_local(command, args, env, path_prefix, client)
                .await
                .map_err(UpstreamError::Failed)
        }
        ResolvedTransport::Http { url, headers } => {
            connect_remote(url, headers, client, oauth).await
        }
    }
}

async fn connect_local(
    command: &str,
    args: &[String],
    env: &[(String, String)],
    path_prefix: Option<&Path>,
    client: UpstreamClient,
) -> Result<Connection, String> {
    let mut spawn = Command::new(command);
    if let Some(prefix) = path_prefix {
        spawn.env("PATH", crate::agent::toolchain_path(prefix));
    }
    spawn
        .args(args)
        .envs(env.iter().map(|(name, value)| (name, value)))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0);
    if let Some(root) = client.roots.first() {
        spawn.current_dir(root);
    }
    let mut child = spawn
        .spawn()
        .map_err(|error| format!("Could not start {command}: {error}"))?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        unreachable!("every stdio stream is piped")
    };
    let process = LocalProcess::new(child);
    let stderr_tail = StderrTail::follow(stderr);
    let (roots, calls) = (Arc::clone(&client.roots), client.calls.clone());
    let started = tokio::time::timeout(START_TIMEOUT, client.serve((stdout, stdin))).await;
    let service = match started {
        Ok(Ok(service)) => service,
        failed => {
            let reason = match failed {
                Err(_) => "The server did not finish starting in time".to_owned(),
                Ok(Err(error)) => format!("The server did not finish starting: {error}"),
                Ok(Ok(_)) => unreachable!("handled above"),
            };
            let status = process.terminate().await;
            let reason = match status {
                Some(status) => format!("{reason} ({status})"),
                None => reason,
            };
            return Err(with_tail(reason, &stderr_tail).await);
        }
    };
    Ok(Connection {
        upstream: Upstream {
            peer: service.peer().clone(),
            calls,
            roots,
            signed_out: Arc::default(),
            sign_in_epoch: 0,
        },
        service,
        process: Some(process),
        stderr_tail,
    })
}

async fn connect_remote(
    url: &str,
    headers: &[(String, String)],
    client: UpstreamClient,
    oauth: StoredOAuth,
) -> Result<Connection, UpstreamError> {
    let mut custom_headers = HashMap::new();
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| UpstreamError::failed(format!("{name} is not a valid header name")))?;
        let mut value = HeaderValue::from_str(value).map_err(|_| {
            UpstreamError::failed(format!("The {name} header's value is not valid"))
        })?;
        value.set_sensitive(true);
        custom_headers.insert(name, value);
    }
    let config = StreamableHttpClientTransportConfig::with_uri(url).custom_headers(custom_headers);
    let http = reqwest013::Client::builder()
        .pool_max_idle_per_host(0)
        .redirect(same_origin_redirects())
        .build()
        .map_err(|_| UpstreamError::failed("Ottam could not set up its HTTP client"))?;
    let signed_out = Arc::new(AtomicBool::new(false));
    // Behind a sign-in, the token goes on every request; without one, a
    // server that needs it answers 401 and reads as signed out.
    let started = match oauth::stored_manager(url, oauth)
        .await
        .map_err(UpstreamError::Failed)?
    {
        Some(manager) => {
            let http = Watched::new(AuthClient::new(http, manager), &signed_out);
            serve_remote(
                client,
                StreamableHttpClientTransport::with_client(http, config),
            )
            .await
        }
        None => {
            let http = Watched::new(http, &signed_out);
            serve_remote(
                client,
                StreamableHttpClientTransport::with_client(http, config),
            )
            .await
        }
    };
    let (service, calls, roots) = started.map_err(|reason| {
        if signed_out.load(Ordering::Acquire) {
            UpstreamError::SignedOut
        } else {
            UpstreamError::Failed(reason)
        }
    })?;
    Ok(Connection {
        upstream: Upstream {
            peer: service.peer().clone(),
            calls,
            roots,
            signed_out,
            sign_in_epoch: 0,
        },
        service,
        process: None,
        stderr_tail: StderrTail::default(),
    })
}

type Served = (
    RunningService<RoleClient, UpstreamClient>,
    InFlightCalls,
    Arc<Vec<PathBuf>>,
);

async fn serve_remote<C: StreamableHttpClient>(
    client: UpstreamClient,
    transport: StreamableHttpClientTransport<C>,
) -> Result<Served, String> {
    let (roots, calls) = (Arc::clone(&client.roots), client.calls.clone());
    let service = tokio::time::timeout(START_TIMEOUT, client.serve(transport))
        .await
        .map_err(|_| "The server did not answer in time".to_owned())?
        .map_err(|error| format!("The server did not finish starting: {error}"))?;
    Ok((service, calls, roots))
}

/// Same-origin 307 and 308 redirects only: they keep the method and body,
/// and the session id and headers never reach another host.
fn same_origin_redirects() -> reqwest013::redirect::Policy {
    reqwest013::redirect::Policy::custom(|attempt| {
        let preserves_body = matches!(
            attempt.status(),
            reqwest013::StatusCode::TEMPORARY_REDIRECT | reqwest013::StatusCode::PERMANENT_REDIRECT
        );
        let same_origin = attempt
            .previous()
            .first()
            .is_some_and(|original| original.origin() == attempt.url().origin());
        if !preserves_body || !same_origin {
            attempt.stop()
        } else if attempt.previous().len() > MAX_REDIRECTS {
            attempt.error(format!("exceeded {MAX_REDIRECTS} redirects"))
        } else {
            attempt.follow()
        }
    })
}

/// An HTTP client that notes whether the server's latest answer to a
/// request was that it wants a sign-in. Only requests count: a refused
/// optional event stream says nothing about the calls, and a later success
/// clears an earlier refusal.
#[derive(Clone)]
struct Watched<C> {
    inner: C,
    signed_out: Arc<AtomicBool>,
}

impl<C> Watched<C> {
    fn new(inner: C, signed_out: &Arc<AtomicBool>) -> Self {
        Self {
            inner,
            signed_out: Arc::clone(signed_out),
        }
    }

    fn watch<T, E>(
        &self,
        result: Result<T, StreamableHttpError<E>>,
    ) -> Result<T, StreamableHttpError<E>>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        match &result {
            Ok(_) => self.signed_out.store(false, Ordering::Release),
            Err(StreamableHttpError::AuthRequired(_)) => {
                self.signed_out.store(true, Ordering::Release)
            }
            Err(_) => {}
        }
        result
    }
}

type SseStream = futures::stream::BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>;

impl<C: StreamableHttpClient + Sync> StreamableHttpClient for Watched<C> {
    type Error = C::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let result = self
            .inner
            .post_message(uri, message, session_id, auth_header, custom_headers)
            .await;
        self.watch(result)
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let result = self
            .inner
            .post_message_with_max_sse_event_size(
                uri,
                message,
                session_id,
                auth_header,
                custom_headers,
                max_sse_event_size,
            )
            .await;
        self.watch(result)
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        self.inner
            .delete_session(uri, session_id, auth_header, custom_headers)
            .await
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<SseStream, StreamableHttpError<Self::Error>> {
        self.inner
            .get_stream(uri, session_id, last_event_id, auth_header, custom_headers)
            .await
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<SseStream, StreamableHttpError<Self::Error>> {
        self.inner
            .get_stream_with_max_sse_event_size(
                uri,
                session_id,
                last_event_id,
                auth_header,
                custom_headers,
                max_sse_event_size,
            )
            .await
    }
}

async fn with_tail(reason: String, tail: &StderrTail) -> String {
    match tail.last().await {
        Some(line) => format!("{reason}: {line}"),
        None => reason,
    }
}

/// The last non-empty line a local server wrote to stderr.
#[derive(Clone, Default)]
struct StderrTail {
    line: Arc<Mutex<Option<String>>>,
    ended: Option<tokio::sync::watch::Receiver<bool>>,
}

impl StderrTail {
    fn follow(stderr: ChildStderr) -> Self {
        let (ended_tx, ended) = tokio::sync::watch::channel(false);
        let tail = Self {
            line: Arc::default(),
            ended: Some(ended),
        };
        let line = Arc::clone(&tail.line);
        tokio::spawn(async move {
            // Bytes, not lines: a server's stderr need not be UTF-8, and it
            // must be drained to the end, or the server's next write to it
            // fails once the pipe closes.
            let mut reader = BufReader::new(stderr);
            let mut pending = Vec::new();
            loop {
                let chunk = match reader.fill_buf().await {
                    Ok([]) | Err(_) => break,
                    Ok(chunk) => chunk,
                };
                let read = chunk.len();
                for piece in chunk.split_inclusive(|byte| *byte == b'\n') {
                    let room = STDERR_LINE_BYTES.saturating_sub(pending.len());
                    pending.extend_from_slice(&piece[..piece.len().min(room)]);
                    if piece.ends_with(b"\n") {
                        keep_line(&line, &pending);
                        pending.clear();
                    }
                }
                reader.consume(read);
            }
            keep_line(&line, &pending);
            let _ = ended_tx.send(true);
        });
        tail
    }

    /// The last line, once stderr has closed or briefly after the process
    /// exited, so a crash's final words make it in.
    async fn last(&self) -> Option<String> {
        if let Some(mut ended) = self.ended.clone() {
            let _ = tokio::time::timeout(STDERR_SETTLE, ended.wait_for(|ended| *ended)).await;
        }
        self.line
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// Keep `bytes` as the last line, unless it is blank.
fn keep_line(line: &Mutex<Option<String>>, bytes: &[u8]) {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    if !text.is_empty() {
        let text: String = text.chars().take(STDERR_TAIL_CHARS).collect();
        *line.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(text);
    }
}

/// A local server process, leader of its own process group.
struct LocalProcess {
    child: Child,
    group: libc::pid_t,
    reaped: bool,
}

impl LocalProcess {
    fn new(child: Child) -> Self {
        let group = child.id().map_or(0, |pid| pid as libc::pid_t);
        Self {
            child,
            group,
            reaped: false,
        }
    }

    /// SIGTERM the group, SIGKILL it if the leader outlives the grace
    /// period, and reap the leader. Returns how the leader exited, if known.
    async fn terminate(mut self) -> Option<std::process::ExitStatus> {
        self.signal(libc::SIGTERM);
        let status = match tokio::time::timeout(TERMINATE_GRACE, self.child.wait()).await {
            Ok(status) => status.ok(),
            Err(_) => {
                self.signal(libc::SIGKILL);
                self.child.wait().await.ok()
            }
        };
        // Anything the leader left behind in its group goes with it.
        self.signal(libc::SIGKILL);
        self.reaped = true;
        status
    }

    fn signal(&self, signal: libc::c_int) {
        if self.group > 0 {
            // SAFETY: signals the group this process leads; ESRCH once it
            // is empty is expected and ignored.
            unsafe {
                libc::killpg(self.group, signal);
            }
        }
    }
}

impl Drop for LocalProcess {
    fn drop(&mut self) {
        if !self.reaped {
            // Tokio reaps a dropped child in the background.
            self.signal(libc::SIGKILL);
        }
    }
}
