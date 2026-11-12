//! `--http --oauth`: the HTTP fixture behind its own OAuth 2.1 authorization
//! server, as a remote server that needs sign-in looks to core.
//!
//! It publishes protected resource and authorization server metadata,
//! registers clients dynamically unless `--no-registration` (then only
//! [`PREREGISTERED_CLIENT`] is known), and checks PKCE. `/authorize` shows an
//! approve page unless `--auto-approve` redirects straight back. Access tokens
//! last `--token-ttl` seconds (an hour by default) and refresh tokens rotate.
//! `--static-token <token>` is also accepted, as a header a user pastes
//! would be, and `--authorization-endpoint <url>` publishes another sign-in
//! page. `POST /admin/revoke` voids every token, `POST /admin/forget-clients`
//! every registration, and `GET /admin/stats` counts sign-ins, refreshes,
//! and rejected calls.

use axum::extract::{Form, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The one client `--no-registration` accepts.
pub const PREREGISTERED_CLIENT: &str = "ottam-test-client";

pub struct Options {
    pub registration: bool,
    pub auto_approve: bool,
    pub token_ttl: Duration,
    pub static_token: Option<String>,
    pub authorization_endpoint: Option<String>,
}

struct Grant {
    client_id: String,
    redirect_uri: String,
    challenge: String,
}

#[derive(Default)]
struct Tokens {
    clients: HashSet<String>,
    codes: HashMap<String, Grant>,
    access: HashMap<String, Instant>,
    refresh: HashMap<String, String>,
}

struct Server {
    base: String,
    options: Options,
    tokens: Mutex<Tokens>,
    next: AtomicU64,
    sign_ins: AtomicU64,
    refreshes: AtomicU64,
    rejected: AtomicU64,
}

type Shared = Arc<Server>;
type Params = HashMap<String, String>;

impl Server {
    fn mint(&self, kind: &str) -> String {
        let serial = self.next.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        format!("{kind}-{}-{serial}-{nanos:x}", std::process::id())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Tokens> {
        self.tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn knows(&self, client_id: &str) -> bool {
        if self.options.registration {
            self.lock().clients.contains(client_id)
        } else {
            client_id == PREREGISTERED_CLIENT
        }
    }

    /// A new access and refresh token for `client_id`, as a token response.
    fn issue(&self, client_id: &str) -> Response {
        let (access, refresh) = (self.mint("access"), self.mint("refresh"));
        {
            let mut tokens = self.lock();
            tokens
                .access
                .insert(access.clone(), Instant::now() + self.options.token_ttl);
            tokens.refresh.insert(refresh.clone(), client_id.to_owned());
        }
        json_response(
            StatusCode::OK,
            json!({
                "access_token": access,
                "token_type": "Bearer",
                "expires_in": self.options.token_ttl.as_secs(),
                "refresh_token": refresh,
                "scope": "mcp",
            }),
        )
    }

    fn resource_metadata_url(&self) -> String {
        format!("{}/.well-known/oauth-protected-resource/mcp", self.base)
    }
}

/// The fixture's routes wrapped around `mcp`, the MCP service router.
pub fn router(base: String, options: Options, mcp: Router) -> Router {
    let server: Shared = Arc::new(Server {
        base,
        options,
        tokens: Mutex::default(),
        next: AtomicU64::new(1),
        sign_ins: AtomicU64::new(0),
        refreshes: AtomicU64::new(0),
        rejected: AtomicU64::new(0),
    });
    let mcp = mcp.layer(middleware::from_fn_with_state(server.clone(), bearer));
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_metadata),
        )
        .route("/register", post(register))
        .route("/authorize", get(authorize))
        .route("/approve", get(approve))
        .route("/deny", get(deny))
        .route("/token", post(token))
        .route("/admin/revoke", post(revoke))
        .route("/admin/forget-clients", post(forget_clients))
        .route("/admin/stats", get(stats))
        .with_state(server)
        .merge(mcp)
}

async fn bearer(State(server): State<Shared>, request: Request, next: Next) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned);
    let valid = presented.as_ref().is_some_and(|token| {
        server.options.static_token.as_ref() == Some(token)
            || server
                .lock()
                .access
                .get(token)
                .is_some_and(|expires| Instant::now() < *expires)
    });
    if valid {
        return next.run(request).await;
    }
    server.rejected.fetch_add(1, Ordering::Relaxed);
    let mut challenge = format!(
        "Bearer resource_metadata=\"{}\"",
        server.resource_metadata_url()
    );
    if presented.is_some() {
        challenge.push_str(", error=\"invalid_token\"");
    }
    let mut response = StatusCode::UNAUTHORIZED.into_response();
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

async fn resource_metadata(State(server): State<Shared>) -> Response {
    json_response(
        StatusCode::OK,
        json!({
            "resource": format!("{}/mcp", server.base),
            "authorization_servers": [server.base],
            "scopes_supported": ["mcp"],
            "bearer_methods_supported": ["header"],
        }),
    )
}

async fn authorization_metadata(State(server): State<Shared>) -> Response {
    let base = &server.base;
    let mut metadata = json!({
        "issuer": base,
        "authorization_endpoint": server
            .options
            .authorization_endpoint
            .clone()
            .unwrap_or_else(|| format!("{base}/authorize")),
        "token_endpoint": format!("{base}/token"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": ["mcp"],
        "authorization_response_iss_parameter_supported": true,
    });
    if server.options.registration {
        metadata["registration_endpoint"] = json!(format!("{base}/register"));
    }
    json_response(StatusCode::OK, metadata)
}

async fn register(State(server): State<Shared>, body: String) -> Response {
    if !server.options.registration {
        return StatusCode::NOT_FOUND.into_response();
    }
    let request: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    let client_id = server.mint("client");
    server.lock().clients.insert(client_id.clone());
    json_response(
        StatusCode::CREATED,
        json!({
            "client_id": client_id,
            "client_name": request.get("client_name"),
            "redirect_uris": request.get("redirect_uris").cloned().unwrap_or(json!([])),
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }),
    )
}

async fn authorize(
    State(server): State<Shared>,
    Query(params): Query<Params>,
    request: Request,
) -> Response {
    let client_id = params.get("client_id").cloned().unwrap_or_default();
    if !server.knows(&client_id) || !params.contains_key("redirect_uri") {
        return (StatusCode::BAD_REQUEST, "unknown client or redirect").into_response();
    }
    if params.get("code_challenge_method").map(String::as_str) != Some("S256") {
        return (StatusCode::BAD_REQUEST, "PKCE S256 is required").into_response();
    }
    if server.options.auto_approve {
        return approve(State(server), Query(params)).await;
    }
    let query = request.uri().query().unwrap_or_default();
    Html(format!(
        "<!doctype html><meta charset=utf-8><title>Test OAuth</title>\
         <body style=\"font: 15px -apple-system, sans-serif; margin: 4em\">\
         <h1>Ottam test server</h1>\
         <p>Allow <b>{client_id}</b> to use the test MCP server?</p>\
         <p><a id=approve href=\"/approve?{query}\">Approve</a> \
         &nbsp; <a id=deny href=\"/deny?{query}\">Deny</a></p>"
    ))
    .into_response()
}

async fn approve(State(server): State<Shared>, Query(params): Query<Params>) -> Response {
    let (Some(client_id), Some(redirect_uri), Some(challenge)) = (
        params.get("client_id"),
        params.get("redirect_uri"),
        params.get("code_challenge"),
    ) else {
        return (StatusCode::BAD_REQUEST, "missing parameters").into_response();
    };
    let code = server.mint("code");
    server.lock().codes.insert(
        code.clone(),
        Grant {
            client_id: client_id.clone(),
            redirect_uri: redirect_uri.clone(),
            challenge: challenge.clone(),
        },
    );
    let mut target = format!(
        "{redirect_uri}?code={}&iss={}",
        encode(&code),
        encode(&server.base)
    );
    if let Some(state) = params.get("state") {
        target.push_str(&format!("&state={}", encode(state)));
    }
    Redirect::to(&target).into_response()
}

async fn deny(Query(params): Query<Params>) -> Response {
    let Some(redirect_uri) = params.get("redirect_uri") else {
        return (StatusCode::BAD_REQUEST, "missing redirect").into_response();
    };
    let mut target = format!("{redirect_uri}?error=access_denied");
    if let Some(state) = params.get("state") {
        target.push_str(&format!("&state={}", encode(state)));
    }
    Redirect::to(&target).into_response()
}

async fn token(
    State(server): State<Shared>,
    headers: HeaderMap,
    Form(params): Form<Params>,
) -> Response {
    let invalid = |error: &str| json_response(StatusCode::BAD_REQUEST, json!({"error": error}));
    let field = |name: &str| params.get(name).cloned().unwrap_or_default();
    // A public client names itself in the body, or as Basic auth with no
    // secret.
    let client_id = params.get("client_id").cloned().unwrap_or_else(|| {
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Basic "))
            .and_then(|value| STANDARD.decode(value).ok())
            .and_then(|value| String::from_utf8(value).ok())
            .and_then(|value| value.split(':').next().map(str::to_owned))
            .unwrap_or_default()
    });
    match field("grant_type").as_str() {
        "authorization_code" => {
            let Some(grant) = server.lock().codes.remove(&field("code")) else {
                return invalid("invalid_grant");
            };
            let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(field("code_verifier")));
            if grant.challenge != challenge
                || grant.client_id != client_id
                || grant.redirect_uri != field("redirect_uri")
            {
                return invalid("invalid_grant");
            }
            server.sign_ins.fetch_add(1, Ordering::Relaxed);
            server.issue(&grant.client_id)
        }
        "refresh_token" => {
            let Some(client_id) = server.lock().refresh.remove(&field("refresh_token")) else {
                return invalid("invalid_grant");
            };
            server.refreshes.fetch_add(1, Ordering::Relaxed);
            server.issue(&client_id)
        }
        _ => invalid("unsupported_grant_type"),
    }
}

async fn revoke(State(server): State<Shared>) -> Response {
    let mut tokens = server.lock();
    let count = tokens.access.len() + tokens.refresh.len();
    tokens.access.clear();
    tokens.refresh.clear();
    format!("revoked {count}\n").into_response()
}

async fn forget_clients(State(server): State<Shared>) -> Response {
    let mut tokens = server.lock();
    let count = tokens.clients.len();
    tokens.clients.clear();
    format!("forgot {count}\n").into_response()
}

async fn stats(State(server): State<Shared>) -> Response {
    json_response(
        StatusCode::OK,
        json!({
            "signIns": server.sign_ins.load(Ordering::Relaxed),
            "refreshes": server.refreshes.load(Ordering::Relaxed),
            "rejected": server.rejected.load(Ordering::Relaxed),
        }),
    )
}

fn json_response(status: StatusCode, body: serde_json::Value) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    (status, headers, body.to_string()).into_response()
}

/// Percent-encodes everything but unreserved characters.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}
