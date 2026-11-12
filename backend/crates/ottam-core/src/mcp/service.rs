//! The configured servers, their upstream connections, and their proxy
//! paths, kept in step: `mcp.*` requests go through here, so a server's path
//! appears with it, follows a rename, and goes with it. Sign-ins start here
//! too, and a server's tokens go with it.

use super::calls::{self, CallOrigin, UpstreamEvents};
use super::config::McpServers;
use super::ottam::{OttamEndpoint, OttamTools, OTTAM_ROUTE};
use super::entries;
use super::import;
use super::managed::ManagedEndpoint;
use super::oauth::{Finished, SignIns, StoredOAuth};
use super::proxy::{McpSessions, WeakRoutes};
use super::runtime::{McpNotice, McpRuntime};
use super::tokens::{GrantRequest, McpGrant, TokenScope};
use super::upstream::McpWorkspace;
use crate::agent::{
    AgentId, HarnessAdapter, HarnessPermissionBroker, HomeLayout, McpConnections, McpServerEntry,
};
use crate::core_error::CoreError;
use ottam_protocol::{
    ConversationID, McpImportCandidatesResult, McpImportSource, McpServerConfig, McpServerID,
    McpServerSummary, McpServerTransport, McpSignInFinishedParams, McpSignInOutcome,
    McpSignInResult, WorkspaceID,
};
use futures::future::BoxFuture;
use rmcp::model::{ElicitRequestParams, ElicitResult};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// How long adding or changing a remote server waits for trying it.
const PROBE_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub(crate) struct McpService {
    servers: McpServers,
    runtime: McpRuntime,
    sign_ins: Arc<SignIns>,
    proxy: Arc<OnceLock<ProxyLink>>,
}

struct ProxyLink {
    sessions: McpSessions,
    /// Whether the `ottam` path is served, so every session is given it.
    ottam: std::sync::atomic::AtomicBool,
    /// One sync at a time, each reading the list once it holds this, so the
    /// last to run reflects every change already committed.
    sync: tokio::sync::Mutex<()>,
}

/// The session a run is about to open or restore.
pub(crate) struct SessionGrant<'a> {
    pub(crate) adapter: &'a Arc<dyn HarnessAdapter>,
    pub(crate) workspace_id: WorkspaceID,
    pub(crate) conversation_id: ConversationID,
    pub(crate) agent_id: AgentId,
    /// The session's working directory, where an existing setup's
    /// project-scoped servers are looked up.
    pub(crate) cwd: &'a Path,
}

impl McpService {
    /// Sign-ins return to `redirect_port` on loopback; `0` picks a free
    /// port for each, for tests.
    pub(crate) fn new(servers: McpServers, runtime: McpRuntime, redirect_port: u16) -> Self {
        Self {
            servers,
            runtime,
            sign_ins: Arc::new(SignIns::new(redirect_port)),
            proxy: Arc::default(),
        }
    }

    pub(crate) fn runtime(&self) -> &McpRuntime {
        &self.runtime
    }

    /// Stop any sign-in waiting and every connection; returns once every
    /// local process has been reaped.
    pub(crate) async fn shutdown(&self) {
        self.sign_ins.shutdown().await;
        self.runtime.shutdown().await;
    }

    pub(crate) fn redirect_uri(&self) -> String {
        self.sign_ins.redirect_uri()
    }

    /// Serve every configured server on the proxy, putting their questions
    /// to `broker`. Without this, servers are configured but unreachable,
    /// and sessions are granted none.
    pub(crate) async fn attach_proxy(
        &self,
        sessions: McpSessions,
        broker: Arc<dyn HarnessPermissionBroker>,
    ) -> Result<(), CoreError> {
        let events: Arc<dyn UpstreamEvents> = Arc::new(ProxyEvents {
            routes: sessions.routes().downgrade(),
            broker,
        });
        self.runtime.set_events(events);
        let _ = self.proxy.set(ProxyLink {
            sessions,
            ottam: std::sync::atomic::AtomicBool::new(false),
            sync: tokio::sync::Mutex::new(()),
        });
        self.sync_routes().await?;
        self.probe_signed_out();
        Ok(())
    }

    /// Serve Ottam's own tools on the reserved path, and give every
    /// session opened from now on its entry. Needs [`Self::attach_proxy`].
    pub(crate) fn serve_ottam(&self, tools: Arc<dyn OttamTools>) {
        if let Some(proxy) = self.proxy.get() {
            proxy
                .sessions
                .routes()
                .insert(OTTAM_ROUTE, Arc::new(OttamEndpoint::new(tools)));
            proxy
                .ottam
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }

    /// Try each remote server without stored tokens once, in the
    /// background, so one that needs a sign-in shows signed out from
    /// launch, as it did when added, rather than not started.
    fn probe_signed_out(&self) {
        let (servers, runtime) = (self.servers.clone(), self.runtime.clone());
        tokio::spawn(async move {
            let Ok(list) = servers.list().await else {
                return;
            };
            for server in list.into_iter().filter(|server| is_remote(&server.config)) {
                if !servers.oauth(&server.server_id).signed_in().await {
                    runtime.probe(&server.server_id).await;
                }
            }
        });
    }

    /// The entries a session is given, and the token they carry: Ottam's
    /// own, and one per server on for its workspace, less the ones an
    /// existing setup already runs itself. The token holds a lease on each
    /// server, starting a local server's process for the workspace. `None`
    /// when there are none.
    pub(crate) async fn grant(
        &self,
        session: SessionGrant<'_>,
    ) -> Option<(McpGrant, Vec<McpServerEntry>)> {
        let proxy = self.proxy.get()?;
        let servers = match self.servers.list().await {
            Ok(servers) => servers,
            Err(error) => {
                tracing::warn!(
                    kind = error.kind,
                    "MCP servers could not be read for a session"
                );
                return None;
            }
        };
        let served = proxy.sessions.routes().served();
        let mut servers: Vec<McpServerSummary> = servers
            .into_iter()
            .filter(|server| {
                !server
                    .disabled_workspace_ids
                    .contains(&session.workspace_id)
            })
            .filter(|server| {
                served.get(&server.config.name) == Some(&Some(server.server_id.clone()))
            })
            .collect();
        let ottam = proxy.ottam.load(std::sync::atomic::Ordering::Acquire);
        if servers.is_empty() && !ottam {
            return None;
        }
        let definition = session.adapter.definition();
        if let Some(HomeLayout::Existing { path, environment }) = definition
            .instance
            .as_ref()
            .map(|instance| &instance.home)
            .filter(|_| !servers.is_empty())
        {
            let (harness, path, environment, cwd) = (
                definition.harness_id().to_owned(),
                path.clone(),
                environment.clone(),
                session.cwd.to_path_buf(),
            );
            let native = tokio::task::spawn_blocking(move || {
                let user_home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default();
                entries::native_servers(&harness, &path, &environment, &user_home, &cwd)
            })
            .await
            .unwrap_or_default();
            servers.retain(|server| !native.has(&server.config));
        }
        let roots = match self.servers.workspace_roots(&session.workspace_id).await {
            Ok(roots) if !roots.is_empty() => roots,
            _ => vec![session.cwd.to_path_buf()],
        };
        let workspace = McpWorkspace {
            id: session.workspace_id.clone(),
            roots,
        };
        let mut names = Vec::new();
        if ottam {
            names.push(OTTAM_ROUTE.to_owned());
        }
        let mut leases = Vec::new();
        for server in servers {
            let local = matches!(server.config.transport, McpServerTransport::Stdio { .. });
            match self.runtime.lease(&server.server_id, local, &workspace) {
                Ok(lease) => {
                    names.push(server.config.name);
                    leases.push((server.server_id, lease));
                }
                Err(error) => {
                    tracing::warn!(
                        kind = error.kind,
                        "an MCP server could not be given to a session"
                    );
                }
            }
        }
        let scope = match definition.mcp_connections {
            McpConnections::PerSession => TokenScope::Session,
            McpConnections::PerDirectory => TokenScope::Directory(session.cwd.to_path_buf()),
        };
        proxy.sessions.grant(
            session.adapter,
            names,
            GrantRequest {
                workspace_id: session.workspace_id,
                conversation_id: session.conversation_id,
                agent_id: session.agent_id,
                scope,
                leases,
            },
        )
    }

    /// The servers an import source defines, for approval before any is
    /// added. Reads files and the login shell, so it runs off the runtime.
    pub(crate) async fn import_candidates(
        &self,
        source: McpImportSource,
    ) -> Result<McpImportCandidatesResult, CoreError> {
        let configured = self.servers.list().await?;
        tokio::task::spawn_blocking(move || import::candidates(&source, &configured))
            .await
            .map_err(|_| CoreError::new("mcpImportUnavailable", "the import stopped"))?
    }

    /// Revoke the tokens a deleted conversation's sessions hold.
    pub(crate) fn revoke_conversation(&self, conversation_id: &ConversationID) {
        if let Some(proxy) = self.proxy.get() {
            proxy.sessions.tokens().revoke_conversation(conversation_id);
        }
    }

    pub(crate) async fn list(&self) -> Result<Vec<McpServerSummary>, CoreError> {
        let servers = self.servers.list().await?;
        let mut summaries = Vec::with_capacity(servers.len());
        for server in servers {
            summaries.push(self.with_state(server).await);
        }
        Ok(summaries)
    }

    /// Adds a server. A remote one is tried at once, so one that needs a
    /// sign-in comes back signed out, ready to sign in, unless it is slow to
    /// answer: then `mcp.serverStateChanged` says so.
    pub(crate) async fn add(&self, config: McpServerConfig) -> Result<McpServerSummary, CoreError> {
        let server = self.servers.add(config).await?;
        self.sync_routes().await?;
        self.runtime.notify(McpNotice::ServersChanged);
        if is_remote(&server.config) {
            self.probe_soon(&server.server_id).await;
        }
        Ok(self.with_state(server).await)
    }

    pub(crate) async fn update(
        &self,
        server_id: &McpServerID,
        config: McpServerConfig,
    ) -> Result<McpServerSummary, CoreError> {
        let local = matches!(config.transport, McpServerTransport::Stdio { .. });
        let previous = self.server(server_id).await?;
        let server = self.servers.update(server_id, config).await?;
        // Tokens are for the URL they were issued for.
        if remote_url(&previous.config) != remote_url(&server.config) {
            self.forget_sign_in(server_id).await;
        } else {
            // New headers may be the cure for a 401, as a fresh access
            // token often is: the server is tried again.
            self.runtime.forget_sign_in(server_id);
        }
        // Running connections use the old config; open sessions' next calls
        // start them again with the new one.
        self.runtime.restart_server(server_id, local).await;
        self.sync_routes().await?;
        self.runtime.notify(McpNotice::ServersChanged);
        if is_remote(&server.config) && !self.oauth(server_id).signed_in().await {
            self.probe_soon(server_id).await;
        }
        Ok(self.with_state(server).await)
    }

    pub(crate) async fn remove(&self, server_id: &McpServerID) -> Result<(), CoreError> {
        self.servers.remove(server_id).await?;
        self.forget_sign_in(server_id).await;
        self.runtime.stop_server(server_id).await;
        self.sync_routes().await?;
        self.runtime.notify(McpNotice::ServersChanged);
        Ok(())
    }

    pub(crate) async fn set_workspace_enabled(
        &self,
        server_id: &McpServerID,
        workspace_id: &WorkspaceID,
        enabled: bool,
    ) -> Result<McpServerSummary, CoreError> {
        let server = self
            .servers
            .set_workspace_enabled(server_id, workspace_id, enabled)
            .await?;
        self.runtime.notify(McpNotice::ServersChanged);
        Ok(self.with_state(server).await)
    }

    /// Start signing in to a remote server. The browser's return finishes
    /// it, reported as `mcp.signInFinished`; a success starts the server's
    /// connections again with the new tokens.
    pub(crate) async fn sign_in(
        &self,
        server_id: &McpServerID,
        client_id: Option<String>,
    ) -> Result<McpSignInResult, CoreError> {
        let server = self.server(server_id).await?;
        let url = remote_url(&server.config)
            .ok_or_else(not_remote)?
            .to_owned();
        let (runtime, id) = (self.runtime.clone(), server_id.clone());
        let finished: Finished = Box::new(move |outcome, error| {
            Box::pin(async move {
                if outcome == McpSignInOutcome::SignedIn {
                    runtime.signed_in(&id).await;
                }
                runtime.notify(McpNotice::SignInFinished(McpSignInFinishedParams {
                    server_id: id,
                    outcome,
                    error,
                }));
            })
        });
        let started = self
            .sign_ins
            .start(
                server_id.clone(),
                &url,
                client_id,
                self.oauth(server_id),
                finished,
            )
            .await?;
        Ok(McpSignInResult {
            authorization_url: started.authorization_url,
            redirect_uri: started.redirect_uri,
        })
    }

    pub(crate) async fn cancel_sign_in(&self, server_id: &McpServerID) {
        self.sign_ins.cancel(server_id).await;
    }

    /// Forget a remote server's tokens; it stays signed out until the next
    /// sign-in.
    pub(crate) async fn sign_out(
        &self,
        server_id: &McpServerID,
    ) -> Result<McpServerSummary, CoreError> {
        let server = self.server(server_id).await?;
        remote_url(&server.config).ok_or_else(not_remote)?;
        self.sign_ins.cancel(server_id).await;
        self.oauth(server_id).forget().await?;
        self.runtime.mark_signed_out(server_id, None).await;
        Ok(self.with_state(server).await)
    }

    /// Try a remote server in the background, waiting briefly for it: a
    /// quick answer comes back in the reply, a slow one as
    /// `mcp.serverStateChanged`.
    async fn probe_soon(&self, server_id: &McpServerID) {
        let (runtime, server_id) = (self.runtime.clone(), server_id.clone());
        let probe = tokio::spawn(async move { runtime.probe(&server_id).await });
        let _ = tokio::time::timeout(PROBE_WAIT, probe).await;
    }

    async fn server(&self, server_id: &McpServerID) -> Result<McpServerSummary, CoreError> {
        self.servers
            .list()
            .await?
            .into_iter()
            .find(|server| &server.server_id == server_id)
            .ok_or_else(|| CoreError::new("mcpServerNotFound", "No MCP server has this id"))
    }

    fn oauth(&self, server_id: &McpServerID) -> StoredOAuth {
        self.servers.oauth(server_id)
    }

    /// Stop a sign-in waiting for the server and drop its tokens. Best
    /// effort: a token left in the Keychain is never used for another URL.
    async fn forget_sign_in(&self, server_id: &McpServerID) {
        self.sign_ins.cancel(server_id).await;
        if let Err(error) = self.oauth(server_id).forget().await {
            tracing::warn!(kind = error.kind, "an MCP server's tokens were not removed");
        }
        self.runtime.forget_sign_in(server_id);
    }

    async fn with_state(&self, mut server: McpServerSummary) -> McpServerSummary {
        server.state = self.runtime.state(&server.server_id);
        if is_remote(&server.config) {
            server.signed_in = self.oauth(&server.server_id).signed_in().await;
        }
        server
    }

    /// One path per configured server, named for it. A renamed server's old
    /// path goes; sessions given it see 404 until they reload.
    async fn sync_routes(&self) -> Result<(), CoreError> {
        let Some(proxy) = self.proxy.get() else {
            return Ok(());
        };
        let _sync = proxy.sync.lock().await;
        let servers = self.servers.list().await?;
        let routes = proxy.sessions.routes();
        let served = routes.served();
        // Ottam's own path stays; a server stored under its name before
        // the name was reserved is not served.
        let mut names = BTreeSet::from([OTTAM_ROUTE.to_owned()]);
        for server in servers {
            let name = server.config.name;
            if name.eq_ignore_ascii_case(OTTAM_ROUTE) {
                continue;
            }
            if served.get(&name) != Some(&Some(server.server_id.clone())) {
                routes.insert(
                    name.clone(),
                    Arc::new(ManagedEndpoint::new(
                        server.server_id,
                        name.clone(),
                        self.runtime.clone(),
                    )),
                );
            }
            names.insert(name);
        }
        routes.retain(&names);
        Ok(())
    }
}

fn is_remote(config: &McpServerConfig) -> bool {
    remote_url(config).is_some()
}

fn remote_url(config: &McpServerConfig) -> Option<&str> {
    match &config.transport {
        McpServerTransport::Http { url, .. } => Some(url),
        McpServerTransport::Stdio { .. } => None,
    }
}

fn not_remote() -> CoreError {
    CoreError::new(
        "mcpServerNotRemote",
        "Only remote servers sign in; a local server's credentials go in its environment",
    )
}

/// Server events, delivered to the proxy's sessions and the question panel.
struct ProxyEvents {
    routes: WeakRoutes,
    broker: Arc<dyn HarnessPermissionBroker>,
}

impl UpstreamEvents for ProxyEvents {
    fn tools_changed(&self, server_id: &McpServerID, workspace: Option<&WorkspaceID>) {
        if let Some(routes) = self.routes.upgrade() {
            routes.notify_tools_changed(server_id, workspace);
        }
    }

    fn elicit(
        &self,
        origin: CallOrigin,
        request: ElicitRequestParams,
    ) -> BoxFuture<'static, ElicitResult> {
        Box::pin(calls::ask(Arc::clone(&self.broker), origin, request))
    }
}
