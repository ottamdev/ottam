//! The loopback Streamable HTTP listener every agent session reaches MCP
//! servers through: one path per server, `/mcp/<server>`, each served by an
//! rmcp service in front of that server's [`McpEndpoint`].
//!
//! Every request must carry a live session token; anything else gets a bare
//! 401 with no OAuth metadata, so no agent starts a sign-in against core.
//! rmcp's own `Host` and `Origin` checks guard against DNS rebinding: only
//! loopback hosts pass, and any request that carries an `Origin` is refused,
//! since agents are never browsers.

use super::tokens::{bearer_token, McpCaller, McpTokens};
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use ottam_protocol::{McpServerID, WorkspaceID};
use futures::future::BoxFuture;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ListToolsResult, PaginatedRequestParams, ResultType,
    ServerConfig, SubscriptionFilter,
};
use rmcp::service::{NotificationContext, RequestContext, SubscriptionContext, SubscriptionSink};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, RwLock, Weak};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tower_service::Service;

/// How often tokens whose sessions all ended are forgotten.
const TOKEN_SWEEP: std::time::Duration = std::time::Duration::from_secs(30);

/// What one proxied server answers. Core is the MCP server each agent
/// session talks to; an endpoint decides what is behind it.
pub(crate) trait McpEndpoint: Send + Sync + 'static {
    /// The configured server behind the path, if any.
    fn server_id(&self) -> Option<&McpServerID> {
        None
    }
    /// The `initialize` answer, given at once whatever state the server is in.
    fn info(&self) -> ServerConfig;
    fn list_tools(
        &self,
        caller: McpCaller,
        request: Option<PaginatedRequestParams>,
    ) -> BoxFuture<'_, Result<ListToolsResult, ErrorData>>;
    fn call_tool(
        &self,
        caller: McpCaller,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> BoxFuture<'_, Result<CallToolResponse, ErrorData>>;
}

type RouteService = StreamableHttpService<RouteHandler, LocalSessionManager>;

#[derive(Clone)]
struct Route {
    service: RouteService,
    endpoint: Arc<dyn McpEndpoint>,
    sessions: DownstreamSessions,
}

/// The agent sessions connected to one path, by the workspace their token
/// names, so a server's `list_changed` reaches each of them. A shared
/// token's connection, which may serve several workspaces, hears every one.
/// A legacy client is a peer from its `initialize`; a `2026-07-28` client,
/// which has no `initialize`, is its `subscriptions/listen` stream.
#[derive(Clone, Default)]
struct DownstreamSessions(Arc<Mutex<Vec<DownstreamSession>>>);

struct DownstreamSession {
    workspace_id: Option<WorkspaceID>,
    listener: Listener,
}

enum Listener {
    Peer(rmcp::Peer<RoleServer>),
    Subscription {
        sink: SubscriptionSink,
        open: CancellationToken,
    },
}

impl Listener {
    fn is_closed(&self) -> bool {
        match self {
            Self::Peer(peer) => peer.is_transport_closed(),
            Self::Subscription { open, .. } => open.is_cancelled(),
        }
    }
}

impl DownstreamSessions {
    fn add(&self, workspace_id: Option<WorkspaceID>, listener: Listener) {
        let mut sessions = self.lock();
        sessions.retain(|session| !session.listener.is_closed());
        sessions.push(DownstreamSession {
            workspace_id,
            listener,
        });
    }

    fn notify_tools_changed(&self, workspace: Option<&WorkspaceID>) {
        let mut sessions = self.lock();
        sessions.retain(|session| !session.listener.is_closed());
        for session in sessions.iter() {
            let hears = match (workspace, &session.workspace_id) {
                (Some(workspace), Some(workspace_id)) => workspace == workspace_id,
                _ => true,
            };
            if !hears {
                continue;
            }
            // A session that went away meanwhile just misses it.
            match &session.listener {
                Listener::Peer(peer) => {
                    let peer = peer.clone();
                    tokio::spawn(async move {
                        let _ = peer.notify_tool_list_changed().await;
                    });
                }
                Listener::Subscription { sink, .. } => {
                    let sink = sink.clone();
                    tokio::spawn(async move {
                        let _ = sink.notify_tool_list_changed().await;
                    });
                }
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<DownstreamSession>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The servers the proxy currently serves, by path name. A name added here is
/// reachable at once; sessions learn of it at their next `session/new` or
/// `session/load`.
#[derive(Clone, Default)]
pub(crate) struct McpRoutes {
    routes: Arc<RwLock<BTreeMap<String, Route>>>,
    shutdown: CancellationToken,
}

impl McpRoutes {
    pub(crate) fn insert(&self, name: impl Into<String>, endpoint: Arc<dyn McpEndpoint>) {
        let config = StreamableHttpServerConfig::default()
            .with_allowed_hosts(["localhost", "127.0.0.1", "::1"])
            .enforce_origin_validation()
            .with_cancellation_token(self.shutdown.child_token());
        let sessions = DownstreamSessions::default();
        let service = StreamableHttpService::new(
            {
                let (endpoint, sessions) = (Arc::clone(&endpoint), sessions.clone());
                move || {
                    Ok(RouteHandler {
                        endpoint: Arc::clone(&endpoint),
                        sessions: sessions.clone(),
                    })
                }
            },
            Arc::new(LocalSessionManager::default()),
            config,
        );
        self.write().insert(
            name.into(),
            Route {
                service,
                endpoint,
                sessions,
            },
        );
    }

    /// Tell the sessions on `server_id`'s path, in `workspace` when given,
    /// that its tools changed.
    pub(crate) fn notify_tools_changed(
        &self,
        server_id: &McpServerID,
        workspace: Option<&WorkspaceID>,
    ) {
        for route in self.read().values() {
            if route.endpoint.server_id() == Some(server_id) {
                route.sessions.notify_tools_changed(workspace);
            }
        }
    }

    /// Each path name, with the configured server behind it.
    pub(crate) fn served(&self) -> BTreeMap<String, Option<McpServerID>> {
        self.read()
            .iter()
            .map(|(name, route)| (name.clone(), route.endpoint.server_id().cloned()))
            .collect()
    }

    /// Drop every route not in `names`.
    pub(crate) fn retain(&self, names: &std::collections::BTreeSet<String>) {
        self.write().retain(|name, _| names.contains(name));
    }

    /// A handle that does not keep the routes, for what their own
    /// endpoints hold: an endpoint's runtime holds its events, so a strong
    /// handle there would keep every route, and the runtime, alive forever.
    pub(crate) fn downgrade(&self) -> WeakRoutes {
        WeakRoutes {
            routes: Arc::downgrade(&self.routes),
            shutdown: self.shutdown.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn names(&self) -> Vec<String> {
        self.read().keys().cloned().collect()
    }

    /// The path's service, and the configured server behind it.
    fn get(&self, name: &str) -> Option<(RouteService, Option<McpServerID>)> {
        self.read()
            .get(name)
            .map(|route| (route.service.clone(), route.endpoint.server_id().cloned()))
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, Route>> {
        self.routes
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, BTreeMap<String, Route>> {
        self.routes
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// See [`McpRoutes::downgrade`].
#[derive(Clone)]
pub(crate) struct WeakRoutes {
    routes: Weak<RwLock<BTreeMap<String, Route>>>,
    shutdown: CancellationToken,
}

impl WeakRoutes {
    /// The routes, unless every [`McpRoutes`] is gone.
    pub(crate) fn upgrade(&self) -> Option<McpRoutes> {
        Some(McpRoutes {
            routes: self.routes.upgrade()?,
            shutdown: self.shutdown.clone(),
        })
    }
}

#[derive(Clone)]
struct RouteHandler {
    endpoint: Arc<dyn McpEndpoint>,
    sessions: DownstreamSessions,
}

/// Results relayed from an upstream on an older protocol carry no
/// `resultType`, which a `2026-07-28` client requires and rejects the result
/// without; rmcp drops the field again for an older client.
impl ServerHandler for RouteHandler {
    fn get_info(&self) -> ServerConfig {
        self.endpoint.info()
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        let caller = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<McpCaller>());
        if let Some(caller) = caller {
            self.sessions
                .add(listening_workspace(caller), Listener::Peer(context.peer));
        }
    }

    /// Accept the tool list changes a `2026-07-28` client asks to hear, the
    /// only notifications core sends.
    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        Some(requested.supported_by(&self.endpoint.info().capabilities))
    }

    /// Hold the client's stream open, sending it the server's tool list
    /// changes, until the client cancels it or goes away.
    async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
        let caller = request_caller(context.request_context())?;
        let open = CancellationToken::new();
        let _closed = open.clone().drop_guard();
        self.sessions.add(
            listening_workspace(&caller),
            Listener::Subscription {
                sink: context.sink().clone(),
                open,
            },
        );
        context.cancelled().await;
        Ok(())
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let caller = request_caller(&context)?;
        let mut listed = self.endpoint.list_tools(caller, request).await?;
        listed.result_type.get_or_insert(ResultType::COMPLETE);
        Ok(listed)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let caller = request_caller(&context)?;
        let mut response = self.endpoint.call_tool(caller, request, context).await?;
        if let CallToolResponse::Complete(result) = &mut response {
            result.result_type.get_or_insert(ResultType::COMPLETE);
        }
        Ok(response)
    }
}

/// The workspace whose changes a session hears: none for a shared token,
/// which hears every workspace's.
fn listening_workspace(caller: &McpCaller) -> Option<WorkspaceID> {
    (!caller.shared).then(|| caller.workspace_id.clone())
}

/// The caller [`authorize`] attached before the request reached rmcp, which
/// hands the HTTP request parts to the handler.
fn request_caller(context: &RequestContext<RoleServer>) -> Result<McpCaller, ErrorData> {
    context
        .extensions
        .get::<axum::http::request::Parts>()
        .and_then(|parts| parts.extensions.get::<McpCaller>())
        .cloned()
        .ok_or_else(|| ErrorData::internal_error("request reached the proxy unauthorized", None))
}

#[derive(Clone)]
struct ProxyState {
    tokens: McpTokens,
    routes: McpRoutes,
}

/// What opening a session needs from the proxy: its address, the servers it
/// serves, and the tokens it accepts. Cheap to clone.
#[derive(Clone)]
pub(crate) struct McpSessions {
    address: SocketAddr,
    routes: McpRoutes,
    tokens: McpTokens,
}

impl McpSessions {
    pub(crate) fn routes(&self) -> &McpRoutes {
        &self.routes
    }

    pub(crate) fn tokens(&self) -> &McpTokens {
        &self.tokens
    }

    /// The URL an agent session is given for a server's path.
    pub(crate) fn server_url(&self, name: &str) -> String {
        format!("http://{}/mcp/{name}", self.address)
    }
}

/// The running listener. Stop it with [`McpProxy::finish`] after the agents
/// it serves have shut down.
pub(crate) struct McpProxy {
    sessions: McpSessions,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl McpProxy {
    /// Bind a random loopback port and serve until [`Self::finish`].
    pub(crate) async fn start(tokens: McpTokens) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let routes = McpRoutes::default();
        let shutdown = routes.shutdown.clone();
        let router = Router::new()
            .route("/mcp/{server}", any(proxy_request))
            .with_state(ProxyState {
                tokens: tokens.clone(),
                routes: routes.clone(),
            });
        // A token outlives its sessions until someone presents it; the
        // sweep releases its servers' leases soon after instead.
        tokio::spawn({
            let (tokens, shutdown) = (tokens.clone(), shutdown.clone());
            async move {
                let mut sweeps = tokio::time::interval(TOKEN_SWEEP);
                loop {
                    tokio::select! {
                        _ = sweeps.tick() => tokens.sweep(),
                        () = shutdown.cancelled() => break,
                    }
                }
            }
        });
        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                let served = axum::serve(listener, router)
                    .with_graceful_shutdown(shutdown.cancelled_owned())
                    .await;
                if let Err(error) = served {
                    tracing::warn!(%error, "MCP proxy listener stopped");
                }
            }
        });
        Ok(Self {
            sessions: McpSessions {
                address,
                routes,
                tokens,
            },
            shutdown,
            task,
        })
    }

    pub(crate) fn sessions(&self) -> McpSessions {
        self.sessions.clone()
    }

    /// Refuse every token, end open streams, and stop listening.
    pub(crate) async fn finish(self) {
        self.sessions.tokens.revoke_all();
        self.shutdown.cancel();
        let _ = self.task.await;
    }
}

async fn proxy_request(
    State(state): State<ProxyState>,
    Path(server): Path<String>,
    mut request: Request,
) -> Response {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer_token);
    let Some(caller) = token.and_then(|token| state.tokens.resolve(token)) else {
        // Common and harmless when an agent replaces a connection and sends
        // `DELETE` with a token that ended with its session.
        tracing::info!(
            server = %server,
            method = %request.method(),
            presented = token.is_some(),
            "MCP proxy refused a request without a live session token"
        );
        return StatusCode::UNAUTHORIZED.into_response();
    };
    // A token reaches only the servers its session was given: not one
    // turned off for its workspace, or added since.
    let Some((mut service, _)) = state
        .routes
        .get(&server)
        .filter(|(_, server_id)| server_id.as_ref().is_none_or(|id| caller.reaches(id)))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    request.extensions_mut().insert(caller);
    match service.call(request).await {
        Ok(response) => response.map(Body::new),
        Err(never) => match never {},
    }
}
