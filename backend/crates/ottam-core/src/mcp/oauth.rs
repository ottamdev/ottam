//! Signing in to remote servers with OAuth, through `rmcp`'s `auth`
//! client: metadata discovery, dynamic registration or a client ID the user
//! registered, PKCE, and refresh.
//!
//! A sign-in listens for the browser's return on one fixed loopback port
//! and path, bound only while it waits, so the redirect URI a hand
//! registration needs never changes. One sign-in waits at a time; starting
//! another replaces it, and a start still finding the server's sign-in stops
//! when cancelled. Each server's credentials are one JSON value in the
//! [`SecretStore`], read once and then kept in memory. Tokens and codes are
//! never logged or put in an error.

use super::secrets::SecretStore;
use crate::core_error::CoreError;
use ottam_protocol::{McpServerID, McpSignInOutcome};
use futures::future::BoxFuture;
use rmcp::transport::auth::{
    AuthError, AuthorizationCallback, AuthorizationManager, AuthorizationRequest,
    AuthorizationSession, CredentialStore, StoredCredentials,
};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// The port every sign-in returns to, unless `OTTAM_MCP_REDIRECT_PORT`
/// names another.
pub(crate) const REDIRECT_PORT: u16 = 47391;
const CALLBACK_PATH: &str = "/oauth/callback";
/// How long a sign-in waits for the browser before giving up.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// How long the reply page has to reach the browser before the port goes.
const RELEASE_GRACE: Duration = Duration::from_secs(2);
const CLIENT_NAME: &str = "Ottam";

/// The redirect port from the environment, else [`REDIRECT_PORT`].
pub(crate) fn redirect_port() -> u16 {
    std::env::var("OTTAM_MCP_REDIRECT_PORT")
        .ok()
        .and_then(|port| port.parse().ok())
        .unwrap_or(REDIRECT_PORT)
}

fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}{CALLBACK_PATH}")
}

/// Every server's stored credentials as last read or written, by account,
/// `None` when there are none. `rmcp` loads them before each request, and
/// listing shows each remote server's, so without this each would be a
/// Keychain read.
#[derive(Clone, Default)]
pub(crate) struct OAuthCache(Arc<Mutex<HashMap<String, Option<String>>>>);

impl OAuthCache {
    fn entries(&self) -> MutexGuard<'_, HashMap<String, Option<String>>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One server's OAuth credentials in the secret store, as `rmcp` loads and
/// saves them.
#[derive(Clone)]
pub(crate) struct StoredOAuth {
    secrets: Arc<dyn SecretStore>,
    cache: OAuthCache,
    account: String,
}

impl StoredOAuth {
    pub(crate) fn new(
        secrets: Arc<dyn SecretStore>,
        cache: OAuthCache,
        server_id: &McpServerID,
    ) -> Self {
        Self {
            secrets,
            cache,
            account: format!("{}/oauth", server_id.0),
        }
    }

    async fn read(&self) -> Result<Option<StoredCredentials>, CoreError> {
        let cached = self.cache.entries().get(&self.account).cloned();
        let stored = match cached {
            Some(stored) => stored,
            None => {
                let (secrets, account) = (Arc::clone(&self.secrets), self.account.clone());
                let stored = blocking(move || secrets.get(&account)).await?;
                // A write or delete that finished meanwhile is newer.
                self.cache
                    .entries()
                    .entry(self.account.clone())
                    .or_insert(stored)
                    .clone()
            }
        };
        // Unreadable credentials are as good as none: sign in again.
        Ok(stored.and_then(|json| serde_json::from_str(&json).ok()))
    }

    async fn write(&self, credentials: &StoredCredentials) -> Result<(), CoreError> {
        let json = serde_json::to_string(credentials)
            .map_err(|_| CoreError::new("secretStoreUnavailable", "credentials did not encode"))?;
        let (secrets, account, value) = (
            Arc::clone(&self.secrets),
            self.account.clone(),
            json.clone(),
        );
        let written = blocking(move || secrets.set(&account, &value)).await;
        self.settle(written.as_ref().ok().map(|()| Some(json)));
        written
    }

    /// Record what the store now holds, or, after a failed change, forget
    /// what it was thought to hold so the next read asks it.
    fn settle(&self, stored: Option<Option<String>>) {
        let mut entries = self.cache.entries();
        match stored {
            Some(stored) => entries.insert(self.account.clone(), stored),
            None => entries.remove(&self.account),
        };
    }

    /// Whether tokens are stored, expired or not.
    pub(crate) async fn signed_in(&self) -> bool {
        matches!(self.read().await, Ok(Some(stored)) if stored.token_response.is_some())
    }

    /// The client ID the last sign-in used, kept after its tokens go.
    async fn client_id(&self) -> Option<String> {
        self.read()
            .await
            .ok()
            .flatten()
            .map(|stored| stored.client_id)
    }

    /// Forget the server's tokens and client ID.
    pub(crate) async fn forget(&self) -> Result<(), CoreError> {
        let (secrets, account) = (Arc::clone(&self.secrets), self.account.clone());
        let deleted = blocking(move || secrets.delete(&account)).await;
        self.settle(deleted.as_ref().ok().map(|()| None));
        deleted
    }

    /// Forget a saved client ID that holds no tokens, so the next sign-in
    /// registers Ottam again.
    async fn forget_client_id(&self) {
        if matches!(self.read().await, Ok(Some(stored)) if stored.token_response.is_none()) {
            if let Err(error) = self.forget().await {
                tracing::warn!(
                    kind = error.kind,
                    "an MCP server's client ID was not removed"
                );
            }
        }
    }
}

#[async_trait::async_trait]
impl CredentialStore for StoredOAuth {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        self.read().await.map_err(store_error)
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        self.write(&credentials).await.map_err(store_error)
    }

    async fn clear(&self) -> Result<(), AuthError> {
        self.forget().await.map_err(store_error)
    }

    /// Drop tokens the server will not refresh, so the server reads as
    /// signed out, but keep the client ID for the next sign-in.
    async fn on_refresh_token_rejected(
        &self,
        credentials: &StoredCredentials,
    ) -> Result<(), AuthError> {
        let current = self.read().await.map_err(store_error)?;
        let token = |stored: &StoredCredentials| serde_json::to_value(&stored.token_response).ok();
        if current.as_ref().and_then(token) != token(credentials) {
            return Ok(());
        }
        let kept = StoredCredentials::new(credentials.client_id.clone(), None, Vec::new(), None)
            .with_issuer(credentials.issuer.clone());
        self.write(&kept).await.map_err(store_error)
    }
}

fn store_error(error: CoreError) -> AuthError {
    AuthError::CredentialStoreError(error.message)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, CoreError> + Send + 'static,
) -> Result<T, CoreError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| CoreError::new("secretStoreUnavailable", "the secret store stopped"))?
}

/// An authorization manager for `url` holding the stored credentials, or
/// `None` when no tokens are stored. Discovers the server's metadata, so
/// its token endpoint is known for refresh.
pub(super) async fn stored_manager(
    url: &str,
    store: StoredOAuth,
) -> Result<Option<AuthorizationManager>, String> {
    if !store.signed_in().await {
        return Ok(None);
    }
    let mut manager = manager_for(url)
        .await
        .map_err(|_| "The server's URL is not valid for sign-in".to_owned())?;
    manager.set_credential_store(store);
    match manager.initialize_from_store().await {
        Ok(true) => Ok(Some(manager)),
        Ok(false) => Ok(None),
        Err(error) => Err(format!(
            "The server's sign-in could not be checked: {}",
            describe(&error)
        )),
    }
}

/// An authorization manager for `url`. The user chose the server, so its
/// authorization server may be on a private network, as a company's often
/// is; loopback still needs a loopback server URL.
async fn manager_for(url: &str) -> Result<AuthorizationManager, AuthError> {
    AuthorizationManager::new_allowing_private_addresses(url).await
}

/// What reports a sign-in's outcome once its port is released.
pub(super) type Finished =
    Box<dyn FnOnce(McpSignInOutcome, Option<String>) -> BoxFuture<'static, ()> + Send>;

/// The sign-in starting or waiting for the browser, if any.
pub(crate) struct SignIns {
    port: u16,
    /// Held only briefly, never across a network call, so cancelling,
    /// signing out, and quitting never wait on a server.
    pending: Mutex<Option<Pending>>,
    /// One start at a time owns the redirect port.
    starts: tokio::sync::Mutex<()>,
    next: AtomicU64,
}

struct Pending {
    id: u64,
    server_id: McpServerID,
    cancel: CancellationToken,
    /// The wait for the browser; `None` while the start is still finding
    /// the server's sign-in.
    task: Option<JoinHandle<()>>,
}

impl Pending {
    /// Stop it; returns once a waiting sign-in has released its port.
    async fn stop(self) {
        self.cancel.cancel();
        if let Some(task) = self.task {
            let _ = task.await;
        }
    }
}

/// A started sign-in: where the browser goes and where it comes back.
pub(super) struct Started {
    pub(super) authorization_url: String,
    pub(super) redirect_uri: String,
}

impl SignIns {
    /// Sign-ins returning to `port`; `0` picks a free port for each, for
    /// tests.
    pub(crate) fn new(port: u16) -> Self {
        Self {
            port,
            pending: Mutex::default(),
            starts: tokio::sync::Mutex::default(),
            next: AtomicU64::new(1),
        }
    }

    pub(crate) fn redirect_uri(&self) -> String {
        redirect_uri(self.port)
    }

    fn pending(&self) -> MutexGuard<'_, Option<Pending>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Start signing in to the server at `url`, replacing any sign-in
    /// starting or waiting. `finished` hears how it ends, once the port is
    /// released. Cancelled before the browser has its page, it finishes as
    /// cancelled and fails with `mcpSignInCancelled`.
    pub(super) async fn start(
        &self,
        server_id: McpServerID,
        url: &str,
        client_id: Option<String>,
        store: StoredOAuth,
        finished: Finished,
    ) -> Result<Started, CoreError> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let cancel = CancellationToken::new();
        let previous = self.pending().replace(Pending {
            id,
            server_id,
            cancel: cancel.clone(),
            task: None,
        });
        if let Some(previous) = previous {
            previous.stop().await;
        }
        let started = tokio::select! {
            started = self.prepare(url, client_id, store) => started,
            () = cancel.cancelled() => Err(cancelled()),
        };
        if cancel.is_cancelled() {
            drop(started);
            finished(McpSignInOutcome::Cancelled, None).await;
            return Err(cancelled());
        }
        let (listener, port, session, reused_client) = match started {
            Ok(started) => started,
            Err(error) => {
                self.forget_pending(id);
                return Err(error);
            }
        };
        let authorization_url = session.get_authorization_url().to_owned();
        let redirect_uri = redirect_uri(port);
        let task = tokio::spawn(wait(
            Waiting {
                listener,
                port,
                session,
                cancel,
                reused_client,
            },
            finished,
        ));
        // Cancelled meanwhile, its token is too, so the wait ends itself.
        if let Some(pending) = self.pending().as_mut().filter(|pending| pending.id == id) {
            pending.task = Some(task);
        }
        Ok(Started {
            authorization_url,
            redirect_uri,
        })
    }

    /// Bind the redirect port and find the server's sign-in. The port stays
    /// this start's alone until it is released.
    async fn prepare(
        &self,
        url: &str,
        client_id: Option<String>,
        store: StoredOAuth,
    ) -> Result<(TcpListener, u16, AuthorizationSession, Option<StoredOAuth>), CoreError> {
        let _start = self.starts.lock().await;
        let listener = TcpListener::bind(("127.0.0.1", self.port))
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::AddrInUse => CoreError::new(
                    "mcpRedirectPortInUse",
                    format!(
                        "Another app is using port {}, where sign-ins return. Quit it and try again.",
                        self.port
                    ),
                )
                .recoverable(true),
                _ => listen_failed(&error),
            })?;
        let port = listener
            .local_addr()
            .map_err(|error| listen_failed(&error))?
            .port();
        let (session, reused_client) =
            authorize(url, client_id, store, &redirect_uri(port)).await?;
        if !is_web_page(session.get_authorization_url()) {
            return Err(CoreError::new(
                "mcpSignInUnavailable",
                "The server's sign-in page is not a web page Ottam can open",
            )
            .recoverable(true));
        }
        Ok((listener, port, session, reused_client))
    }

    fn forget_pending(&self, id: u64) {
        let mut pending = self.pending();
        if pending.as_ref().is_some_and(|pending| pending.id == id) {
            pending.take();
        }
    }

    /// Stop `server_id`'s sign-in, if it is the one starting or waiting;
    /// returns once a waiting one's port is released.
    pub(crate) async fn cancel(&self, server_id: &McpServerID) {
        let taken = {
            let mut pending = self.pending();
            if pending
                .as_ref()
                .is_some_and(|pending| &pending.server_id == server_id)
            {
                pending.take()
            } else {
                None
            }
        };
        if let Some(pending) = taken {
            pending.stop().await;
        }
    }

    /// Stop any sign-in starting or waiting, for quitting.
    pub(crate) async fn shutdown(&self) {
        let taken = self.pending().take();
        if let Some(pending) = taken {
            pending.stop().await;
        }
    }
}

fn cancelled() -> CoreError {
    CoreError::new("mcpSignInCancelled", "The sign-in was cancelled").recoverable(true)
}

fn listen_failed(error: &std::io::Error) -> CoreError {
    CoreError::new(
        "mcpSignInUnavailable",
        format!("Ottam could not listen for the sign-in: {error}"),
    )
}

/// Whether the browser may open `url` for a sign-in: an https page, or an
/// http one on this Mac. A server's metadata names it, and any other scheme
/// would open a file, a share, or another app.
fn is_web_page(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    let host = url.host_str().unwrap_or_default();
    match url.scheme() {
        "https" => !host.is_empty(),
        "http" => {
            host == "localhost"
                || host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        }
        _ => false,
    }
}

/// Discover the server's authorization server, register with it unless a
/// client ID is given or saved, and build the authorization URL. Also gives
/// back the store when it reused the saved client ID, which a failed
/// sign-in forgets.
async fn authorize(
    url: &str,
    client_id: Option<String>,
    store: StoredOAuth,
    redirect_uri: &str,
) -> Result<(AuthorizationSession, Option<StoredOAuth>), CoreError> {
    let unavailable = |error: &AuthError| {
        CoreError::new(
            "mcpSignInUnavailable",
            format!(
                "The server does not offer a sign-in Ottam can use: {}",
                describe(error)
            ),
        )
        .recoverable(true)
    };
    let (client_id, reused) = match client_id.filter(|id| !id.trim().is_empty()) {
        Some(id) => (Some(id.trim().to_owned()), false),
        None => {
            let saved = store.client_id().await;
            let reused = saved.is_some();
            (saved, reused)
        }
    };
    let reused_client = reused.then(|| store.clone());
    let mut manager = manager_for(url)
        .await
        .map_err(|error| unavailable(&error))?;
    manager.set_credential_store(store);
    let metadata = manager
        .resolve_metadata()
        .await
        .map_err(|error| unavailable(&error))?;
    manager.set_metadata(metadata.metadata);
    let mut request = AuthorizationRequest::new(redirect_uri).with_client_name(CLIENT_NAME);
    if let Some(client_id) = &client_id {
        request = request.with_preregistered_client(client_id);
    }
    let session = AuthorizationSession::new(manager, request)
        .await
        .map_err(|(_, error)| match error {
            AuthError::RegistrationFailed(_) if client_id.is_none() => CoreError::new(
                "mcpClientIDRequired",
                format!(
                    "This server needs Ottam registered by hand. Register an app with the \
                     redirect URI {redirect_uri}, then sign in with the client ID it gives you."
                ),
            )
            .recoverable(true),
            other => unavailable(&other),
        })?;
    Ok((session, reused_client))
}

/// A request that reached the callback path, and where its page goes.
struct Callback {
    url: String,
    page: oneshot::Sender<String>,
}

/// Serve the callback path until the browser returns, the sign-in is
/// cancelled, or it times out; then release the port and report.
/// A started sign-in's wait for the browser.
struct Waiting {
    listener: TcpListener,
    port: u16,
    session: AuthorizationSession,
    cancel: CancellationToken,
    /// The store, when the sign-in reused its saved client ID.
    reused_client: Option<StoredOAuth>,
}

async fn wait(waiting: Waiting, finished: Finished) {
    let Waiting {
        listener,
        port,
        session,
        cancel,
        reused_client,
    } = waiting;
    // A reply carries the `state` the authorization URL sent; any other
    // request to the port, a stray tab or another page, is no reply.
    let expected_state = query_value(session.get_authorization_url(), "state");
    let (callbacks, mut received) = mpsc::channel::<Callback>(4);
    let app = axum::Router::new().route(
        CALLBACK_PATH,
        axum::routing::get(move |uri: axum::http::Uri| {
            let callbacks = callbacks.clone();
            async move {
                let (page, reply) = oneshot::channel();
                let url = format!("http://127.0.0.1:{port}{uri}");
                if callbacks.send(Callback { url, page }).await.is_err() {
                    return axum::response::Html(page_html(
                        "This sign-in has ended. Return to Ottam to try again.",
                    ));
                }
                axum::response::Html(reply.await.unwrap_or_else(|_| {
                    page_html("This sign-in has ended. Return to Ottam to try again.")
                }))
            }
        }),
    );
    let stop = CancellationToken::new();
    let mut server = tokio::spawn(std::future::IntoFuture::into_future(
        axum::serve(listener, app).with_graceful_shutdown(stop.clone().cancelled_owned()),
    ));
    let (outcome, error, denied) = tokio::select! {
        () = cancel.cancelled() => (McpSignInOutcome::Cancelled, None, false),
        () = tokio::time::sleep(SIGN_IN_TIMEOUT) => (
            McpSignInOutcome::Failed,
            Some("The sign-in timed out. Try again.".to_owned()),
            false,
        ),
        outcome = async {
            while let Some(callback) = received.recv().await {
                let reply = query_value(&callback.url, "state")
                    .is_some_and(|state| Some(state) == expected_state);
                let finished = if reply {
                    finish(&session, &callback.url).await
                } else {
                    None
                };
                match finished {
                    Some((outcome, error, page)) => {
                        let _ = callback.page.send(page_html(&page));
                        let denied = query_value(&callback.url, "error").as_deref()
                            == Some("access_denied");
                        return (outcome, error, denied);
                    }
                    None => {
                        let _ = callback.page.send(page_html(
                            "This is not a sign-in reply. Return to Ottam.",
                        ));
                    }
                }
            }
            (McpSignInOutcome::Failed, Some("The sign-in stopped".to_owned()), false)
        } => outcome,
    };
    // Requests still queued get the ended page at once, rather than holding
    // the server open until the grace runs out.
    drop(received);
    stop.cancel();
    if tokio::time::timeout(RELEASE_GRACE, &mut server)
        .await
        .is_err()
    {
        server.abort();
        let _ = server.await;
    }
    drop(session);
    // A saved client ID the server no longer knows fails every sign-in, as
    // the browser shows the server's error and never comes back. Any ending
    // but success or the user's own denial registers Ottam again next
    // time.
    if let Some(store) = reused_client {
        if outcome != McpSignInOutcome::SignedIn && !denied {
            store.forget_client_id().await;
        }
    }
    finished(outcome, error).await;
}

fn query_value(url: &str, name: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// The outcome a browser return means, its error, and the page it gets;
/// `None` for a request that is no reply at all.
async fn finish(
    session: &AuthorizationSession,
    url: &str,
) -> Option<(McpSignInOutcome, Option<String>, String)> {
    if let Some(error) = query_value(url, "error") {
        let message = if error == "access_denied" {
            "The sign-in was denied in the browser".to_owned()
        } else {
            format!("The server refused the sign-in ({error})")
        };
        return Some((
            McpSignInOutcome::Failed,
            Some(message.clone()),
            format!("{message}. Return to Ottam to try again."),
        ));
    }
    let callback = AuthorizationCallback::from_redirect_url(url).ok()?;
    match session
        .handle_callback_with_issuer(
            &callback.code,
            &callback.csrf_token,
            callback.issuer.as_deref(),
        )
        .await
    {
        Ok(_) => Some((
            McpSignInOutcome::SignedIn,
            None,
            "Signed in. You can close this tab and return to Ottam.".to_owned(),
        )),
        Err(error) => {
            tracing::warn!(error = error_kind(&error), "an MCP sign-in failed");
            let message = format!("The sign-in did not finish: {}", describe(&error));
            Some((
                McpSignInOutcome::Failed,
                Some(message.clone()),
                format!("{message}. Return to Ottam to try again."),
            ))
        }
    }
}

fn page_html(message: &str) -> String {
    let message = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<!doctype html><meta charset=utf-8><title>Ottam</title>\
         <body style=\"font: 15px -apple-system, sans-serif; margin: 4em\">\
         <h1>Ottam</h1><p>{message}</p>"
    )
}

/// An `rmcp` auth error as words for the user. Its own text can carry the
/// server's error body, never a token.
fn describe(error: &AuthError) -> String {
    match error {
        AuthError::NoAuthorizationSupport => "it publishes no sign-in details".to_owned(),
        AuthError::PkceUnsupported => "it does not support PKCE".to_owned(),
        AuthError::HttpError(_) => "it could not be reached".to_owned(),
        other => other.to_string(),
    }
}

fn error_kind(error: &AuthError) -> &'static str {
    match error {
        AuthError::AuthorizationRequired => "authorizationRequired",
        AuthError::AuthorizationFailed(_) => "authorizationFailed",
        AuthError::TokenExchangeFailed(_) => "tokenExchangeFailed",
        AuthError::AuthorizationServerMismatch { .. } => "issuerMismatch",
        AuthError::HttpError(_) => "http",
        _ => "other",
    }
}
