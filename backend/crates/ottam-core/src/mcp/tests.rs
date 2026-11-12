use super::proxy::McpEndpoint;
use super::tokens::{GrantRequest, McpCaller, TokenScope};
use super::{McpProxy, McpTokens};
use crate::agent::{
    AdapterStatus, AgentDefinition, AgentId, HarnessAdapter, HarnessError, HarnessErrorKind,
    HarnessFuture, HarnessPrompt, HarnessSessionId, HarnessSessionState, HarnessSessionSummary,
    HarnessStopReason, HarnessUpdateReceiver, HarnessUpdateSender, SessionConfigRequest,
};
use ottam_protocol::{ConversationID, WorkspaceID};
use futures::future::BoxFuture;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// An adapter whose generation and live sessions a test sets directly.
struct SessionsAdapter {
    definition: AgentDefinition,
    status: watch::Sender<AdapterStatus>,
    updates: HarnessUpdateSender,
    sessions: Mutex<Vec<HarnessSessionSummary>>,
}

impl SessionsAdapter {
    fn new() -> Arc<Self> {
        Self::with_definition(AgentDefinition::for_test("agent"))
    }

    fn with_definition(definition: AgentDefinition) -> Arc<Self> {
        let mut status = AdapterStatus::stopped();
        status.generation = 1;
        Arc::new(Self {
            definition,
            status: watch::channel(status).0,
            updates: HarnessUpdateSender::default(),
            sessions: Mutex::new(Vec::new()),
        })
    }

    fn set_session(&self, session_id: &str, state: HarnessSessionState) {
        let mut sessions = self.sessions.lock().expect("sessions");
        sessions.retain(|session| session.session_id.0 != session_id);
        sessions.push(HarnessSessionSummary {
            session_id: HarnessSessionId::new(session_id),
            cwd: PathBuf::from("/tmp/project"),
            state,
            last_error: None,
            current_mode_id: None,
            current_model_id: None,
            current_thought_level_id: None,
            announced_commands: None,
        });
    }

    fn drop_sessions(&self) {
        self.sessions.lock().expect("sessions").clear();
    }

    fn restart(&self) {
        self.status.send_modify(|status| status.generation += 1);
    }
}

fn unsupported<T: Send + 'static>() -> HarnessFuture<'static, T> {
    Box::pin(std::future::ready(Err(HarnessError::new(
        HarnessErrorKind::NotRunning,
        "not used by these tests",
    ))))
}

impl HarnessAdapter for SessionsAdapter {
    fn definition(&self) -> &AgentDefinition {
        &self.definition
    }
    fn status(&self) -> watch::Receiver<AdapterStatus> {
        self.status.subscribe()
    }
    fn subscribe_updates(&self) -> HarnessUpdateReceiver {
        self.updates.subscribe()
    }
    fn sessions(&self) -> Vec<HarnessSessionSummary> {
        self.sessions.lock().expect("sessions").clone()
    }
    fn start(&self) -> HarnessFuture<'_, AdapterStatus> {
        unsupported()
    }
    fn shutdown(&self) -> HarnessFuture<'_, ()> {
        unsupported()
    }
    fn authenticate(&self, _method_id: String) -> HarnessFuture<'_, ()> {
        unsupported()
    }
    fn new_session(
        &self,
        _cwd: PathBuf,
        _config: SessionConfigRequest,
    ) -> HarnessFuture<'_, HarnessSessionId> {
        unsupported()
    }
    fn load_session(&self, _session_id: HarnessSessionId, _cwd: PathBuf) -> HarnessFuture<'_, ()> {
        unsupported()
    }
    fn delete_session(&self, _session_id: HarnessSessionId) -> HarnessFuture<'_, ()> {
        unsupported()
    }
    fn prompt(
        &self,
        _session_id: HarnessSessionId,
        _prompt: HarnessPrompt,
    ) -> HarnessFuture<'_, HarnessStopReason> {
        unsupported()
    }
    fn steer(
        &self,
        _session_id: HarnessSessionId,
        _prompt: HarnessPrompt,
    ) -> HarnessFuture<'_, ()> {
        unsupported()
    }
    fn cancel(&self, _session_id: HarnessSessionId) -> HarnessFuture<'_, ()> {
        unsupported()
    }
}

/// Lists one tool named for the conversation that called, so a test can
/// see which caller the proxy attributed a request to.
struct CallerEchoEndpoint;

impl McpEndpoint for CallerEchoEndpoint {
    fn info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    fn list_tools(
        &self,
        caller: McpCaller,
        _request: Option<PaginatedRequestParams>,
    ) -> BoxFuture<'_, Result<ListToolsResult, ErrorData>> {
        let tool = Tool::new(
            format!("from_{}", caller.conversation_id.0),
            "echoes the caller",
            Arc::new(serde_json::Map::new()),
        );
        Box::pin(std::future::ready(Ok(ListToolsResult::with_all_items(
            vec![tool],
        ))))
    }

    fn call_tool(
        &self,
        caller: McpCaller,
        _request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> BoxFuture<'_, Result<CallToolResponse, ErrorData>> {
        let text = caller.session_id.map(|id| id.0).unwrap_or_default();
        Box::pin(std::future::ready(Ok(CallToolResult::success(vec![
            ContentBlock::text(text),
        ])
        .into())))
    }
}

async fn started_proxy() -> McpProxy {
    let proxy = McpProxy::start(McpTokens::default())
        .await
        .expect("proxy listens");
    proxy
        .sessions()
        .routes()
        .insert("echo", Arc::new(CallerEchoEndpoint));
    proxy
}

fn session_request(conversation: &str, scope: TokenScope) -> GrantRequest {
    GrantRequest {
        workspace_id: WorkspaceID("workspace".into()),
        conversation_id: ConversationID(conversation.into()),
        agent_id: AgentId::new("agent"),
        scope,
        leases: Vec::new(),
    }
}

fn grant_for(
    proxy: &McpProxy,
    adapter: &Arc<SessionsAdapter>,
    conversation: &str,
) -> (super::McpGrant, String, String) {
    let erased: Arc<dyn HarnessAdapter> = adapter.clone();
    let (grant, entries) = proxy
        .sessions()
        .grant(
            &erased,
            proxy.sessions().routes().names(),
            session_request(conversation, TokenScope::Session),
        )
        .expect("a served route yields entries");
    let [entry] = entries.as_slice() else {
        panic!("one entry per route: {entries:?}");
    };
    assert_eq!(entry.name, "echo");
    assert_eq!(entry.headers[0].0, "Authorization");
    (grant, entry.url.clone(), entry.headers[0].1.clone())
}

/// One JSON-RPC exchange over Streamable HTTP. Returns the status and, on
/// success, the response message for `id` from the SSE or JSON body.
async fn post(
    url: &str,
    authorization: Option<&str>,
    session: Option<&str>,
    extra: &[(&str, &str)],
    body: Value,
) -> (u16, Option<String>, Option<Value>) {
    let mut request = reqwest::Client::new()
        .post(url)
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .json(&body);
    if let Some(authorization) = authorization {
        request = request.header("Authorization", authorization);
    }
    if let Some(session) = session {
        request = request.header("Mcp-Session-Id", session);
    }
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
    let response = request.send().await.expect("request reaches the proxy");
    let status = response.status().as_u16();
    let session = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let www_authenticate = response.headers().get("www-authenticate").is_some();
    assert!(!www_authenticate, "a refusal never offers OAuth metadata");
    let text = response.text().await.unwrap_or_default();
    let id = body.get("id").cloned();
    let message = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:").or(Some(line)))
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .find(|message| id.is_some() && message.get("id") == id.as_ref());
    (status, session, message)
}

async fn initialize(url: &str, authorization: &str) -> String {
    let (status, session, message) = post(
        url,
        Some(authorization),
        None,
        &[],
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"test","version":"1"}}}),
    )
    .await;
    assert_eq!(status, 200);
    assert!(message.expect("initialize answered")["result"]["capabilities"]["tools"].is_object());
    let session = session.expect("a legacy session id");
    let (status, _, _) = post(
        url,
        Some(authorization),
        Some(&session),
        &[],
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await;
    assert_eq!(status, 202);
    session
}

async fn list_tool_names(url: &str, authorization: &str, session: &str) -> (u16, Vec<String>) {
    let (status, _, message) = post(
        url,
        Some(authorization),
        Some(session),
        &[],
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await;
    let names = message
        .and_then(|message| message["result"]["tools"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect();
    (status, names)
}

#[tokio::test]
async fn a_live_token_reaches_the_endpoint_as_its_caller() {
    let proxy = started_proxy().await;
    let adapter = SessionsAdapter::new();
    let (grant, url, authorization) = grant_for(&proxy, &adapter, "chat-a");
    // Agents connect while `session/new` is in flight, before it is bound.
    let session = initialize(&url, &authorization).await;
    assert_eq!(
        list_tool_names(&url, &authorization, &session).await,
        (200, vec!["from_chat-a".to_string()])
    );

    adapter.set_session("s1", HarnessSessionState::Active);
    grant.bind(HarnessSessionId::new("s1"));
    let (status, _, message) = post(
        &url,
        Some(&authorization),
        Some(&session),
        &[],
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
               "params":{"name":"from_chat-a","arguments":{}}}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        message.expect("call answered")["result"]["content"][0]["text"],
        "s1"
    );
    proxy.finish().await;
}

#[tokio::test]
async fn requests_without_a_live_token_get_a_bare_401() {
    let proxy = started_proxy().await;
    let adapter = SessionsAdapter::new();
    let (_grant, url, _) = grant_for(&proxy, &adapter, "chat-a");
    let body = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},
        "clientInfo":{"name":"test","version":"1"}}});
    for authorization in [None, Some("Bearer not-minted"), Some("Basic abc")] {
        let (status, _, _) = post(&url, authorization, None, &[], body.clone()).await;
        assert_eq!(status, 401, "{authorization:?}");
    }
    proxy.finish().await;
}

#[tokio::test]
async fn a_token_ends_with_its_session_adapter_or_grant() {
    let proxy = started_proxy().await;
    let adapter = SessionsAdapter::new();
    let tokens = proxy.sessions().tokens().clone();
    let bare = |authorization: &str| authorization.trim_start_matches("Bearer ").to_owned();

    // Bound, then the session ends.
    let (grant, _, ended) = grant_for(&proxy, &adapter, "chat-a");
    adapter.set_session("s1", HarnessSessionState::Active);
    grant.bind(HarnessSessionId::new("s1"));
    assert!(tokens.resolve(&bare(&ended)).is_some());
    adapter.set_session("s1", HarnessSessionState::Dead);
    assert!(tokens.resolve(&bare(&ended)).is_none());

    // Bound, then the adapter drops every session.
    let (grant, _, dropped) = grant_for(&proxy, &adapter, "chat-a");
    adapter.set_session("s2", HarnessSessionState::Active);
    grant.bind(HarnessSessionId::new("s2"));
    adapter.drop_sessions();
    assert!(tokens.resolve(&bare(&dropped)).is_none());

    // Pending, then the adapter restarts.
    let (_pending, _, restarted) = grant_for(&proxy, &adapter, "chat-a");
    adapter.restart();
    assert!(tokens.resolve(&bare(&restarted)).is_none());

    // Never bound: the request failed or the run continued a live session.
    let (grant, _, unbound) = grant_for(&proxy, &adapter, "chat-a");
    assert!(tokens.resolve(&bare(&unbound)).is_some());
    drop(grant);
    assert!(tokens.resolve(&bare(&unbound)).is_none());

    // Deleting the conversation revokes its tokens only.
    let (grant_a, _, kept_out) = grant_for(&proxy, &adapter, "chat-a");
    let (grant_b, _, kept) = grant_for(&proxy, &adapter, "chat-b");
    adapter.set_session("s3", HarnessSessionState::Active);
    adapter.set_session("s4", HarnessSessionState::Active);
    grant_a.bind(HarnessSessionId::new("s3"));
    grant_b.bind(HarnessSessionId::new("s4"));
    tokens.revoke_conversation(&ConversationID("chat-a".into()));
    assert!(tokens.resolve(&bare(&kept_out)).is_none());
    assert!(tokens.resolve(&bare(&kept)).is_some());
    proxy.finish().await;
}

#[tokio::test]
async fn an_opencode_token_speaks_for_its_directory_sessions_until_the_last_ends() {
    let proxy = started_proxy().await;
    let adapter = SessionsAdapter::new();
    let erased: Arc<dyn HarnessAdapter> = adapter.clone();
    let tokens = proxy.sessions().tokens().clone();
    let grant = |conversation: &str, workspace: &str, directory: &str| {
        let scope = TokenScope::Directory(PathBuf::from(directory));
        let mut request = session_request(conversation, scope);
        request.workspace_id = WorkspaceID(workspace.into());
        let (grant, entries) = proxy
            .sessions()
            .grant(&erased, vec!["echo".into()], request)
            .expect("entries");
        let token = entries[0].headers[0]
            .1
            .trim_start_matches("Bearer ")
            .to_owned();
        (grant, token)
    };

    // OpenCode shares one connection per directory, whatever workspace
    // the directory's project is in.
    let (first, token) = grant("chat-a", "workspace", "/work/app");
    let (second, shared) = grant("chat-b", "other-workspace", "/work/app");
    let (_elsewhere, other) = grant("chat-c", "workspace", "/work/lib");
    assert_eq!(token, shared, "one token per adapter process and directory");
    assert_ne!(token, other);
    adapter.set_session("s1", HarnessSessionState::Active);
    adapter.set_session("s2", HarnessSessionState::Active);
    first.bind(HarnessSessionId::new("s1"));
    second.bind(HarnessSessionId::new("s2"));

    // A call names its session in `_meta`; without one, it has none.
    let caller = tokens.resolve(&token).expect("live");
    assert_eq!(caller.session_id, None);
    let Value::Object(meta) = json!({"ai.opencode/sessionID": "s2"}) else {
        unreachable!()
    };
    let named = caller.clone().attributed(Some(&meta));
    assert_eq!(named.session_id, Some(HarnessSessionId::new("s2")));
    assert_eq!(named.conversation_id, ConversationID("chat-b".into()));
    assert_eq!(
        named.workspace_id,
        WorkspaceID("other-workspace".into()),
        "the call reaches its own workspace's servers"
    );
    let Value::Object(foreign) = json!({"ai.opencode/sessionID": "s9"}) else {
        unreachable!()
    };
    assert_eq!(caller.attributed(Some(&foreign)).session_id, None);

    // One chat ending leaves the other's token working.
    adapter.set_session("s1", HarnessSessionState::Dead);
    let caller = tokens.resolve(&token).expect("still live");
    assert_eq!(caller.workspace_id, WorkspaceID("other-workspace".into()));
    assert_eq!(
        caller.attributed(None).session_id,
        Some(HarnessSessionId::new("s2")),
        "the only live session"
    );
    tokens.revoke_conversation(&ConversationID("chat-b".into()));
    assert!(tokens.resolve(&token).is_none(), "the last session ended");
    proxy.finish().await;
}

#[tokio::test]
async fn foreign_hosts_and_any_origin_are_refused() {
    let proxy = started_proxy().await;
    let adapter = SessionsAdapter::new();
    let (_grant, url, authorization) = grant_for(&proxy, &adapter, "chat-a");
    let body = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},
        "clientInfo":{"name":"test","version":"1"}}});
    for header in [("Host", "evil.example"), ("Origin", "http://127.0.0.1")] {
        let (status, _, _) = post(&url, Some(&authorization), None, &[header], body.clone()).await;
        assert!((400..500).contains(&status), "{header:?} gave {status}");
    }
    proxy.finish().await;
}

#[tokio::test]
async fn no_served_servers_means_no_entries_and_no_token() {
    let proxy = McpProxy::start(McpTokens::default())
        .await
        .expect("proxy listens");
    let adapter: Arc<dyn HarnessAdapter> = SessionsAdapter::new();
    assert!(proxy
        .sessions()
        .grant(
            &adapter,
            Vec::new(),
            session_request("chat-a", TokenScope::Session)
        )
        .is_none());
    proxy.finish().await;
}

mod config {
    use crate::mcp::secrets::{MemorySecretStore, SecretStore};
    use crate::mcp::McpServers;
    use crate::storage::Database;
    use ottam_protocol::{McpConfigValue, McpServerConfig, McpServerTransport};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn header(name: &str, value: Option<&str>, secret: bool) -> McpConfigValue {
        McpConfigValue {
            name: name.into(),
            value: value.map(str::to_owned),
            secret,
        }
    }

    fn remote(headers: Vec<McpConfigValue>) -> McpServerConfig {
        McpServerConfig {
            name: "remote".into(),
            transport: McpServerTransport::Http {
                url: "https://mcp.example.com/mcp".into(),
                headers,
            },
        }
    }

    fn servers() -> (TempDir, McpServers, Arc<MemorySecretStore>) {
        let home = TempDir::new().expect("home");
        let database = Database::open_test_home(home.path().to_owned()).expect("database");
        let secrets = Arc::new(MemorySecretStore::default());
        let store: Arc<dyn SecretStore> = secrets.clone();
        (home, McpServers::new(database, store), secrets)
    }

    #[tokio::test]
    async fn secrets_live_only_in_the_secret_store() {
        let (_home, servers, secrets) = servers();
        let added = servers
            .add(remote(vec![
                header("X-Api-Key", Some("k1"), true),
                header("X-Team", Some("core"), false),
            ]))
            .await
            .expect("adds");
        let id = added.server_id.0.clone();
        let key_account = format!("{id}/header/X-Api-Key");
        assert_eq!(secrets.get(&key_account).unwrap().as_deref(), Some("k1"));
        let McpServerTransport::Http { headers, .. } = &added.config.transport else {
            panic!("http");
        };
        assert_eq!(headers[0], header("X-Api-Key", None, true));
        assert_eq!(headers[1], header("X-Team", Some("core"), false));

        // Kept without a value, then replaced, then renamed away.
        servers
            .update(
                &added.server_id,
                remote(vec![header("X-Api-Key", None, true)]),
            )
            .await
            .expect("keeps");
        assert_eq!(secrets.get(&key_account).unwrap().as_deref(), Some("k1"));
        servers
            .update(
                &added.server_id,
                remote(vec![header("X-Api-Key", Some("k2"), true)]),
            )
            .await
            .expect("replaces");
        assert_eq!(secrets.get(&key_account).unwrap().as_deref(), Some("k2"));
        servers
            .update(
                &added.server_id,
                remote(vec![header("X-Key", Some("k3"), true)]),
            )
            .await
            .expect("renames");
        assert_eq!(secrets.get(&key_account).unwrap(), None);
        let renamed_account = format!("{id}/header/X-Key");
        assert_eq!(
            secrets.get(&renamed_account).unwrap().as_deref(),
            Some("k3")
        );

        // A new secret name needs a value; turning it plain drops the secret.
        let error = servers
            .update(&added.server_id, remote(vec![header("Other", None, true)]))
            .await
            .expect_err("a new secret needs a value");
        assert_eq!(error.kind, "invalidMcpServer");
        servers
            .update(
                &added.server_id,
                remote(vec![header("X-Key", Some("plain"), false)]),
            )
            .await
            .expect("turns plain");
        assert_eq!(secrets.get(&renamed_account).unwrap(), None);

        servers
            .update(
                &added.server_id,
                remote(vec![header("X-Key", Some("k4"), true)]),
            )
            .await
            .expect("secret again");
        servers.remove(&added.server_id).await.expect("removes");
        assert_eq!(secrets.get(&renamed_account).unwrap(), None);
    }

    #[tokio::test]
    async fn a_failed_update_keeps_the_secrets_it_would_have_replaced() {
        let (_home, servers, secrets) = servers();
        let added = servers
            .add(remote(vec![header("X-Api-Key", Some("k1"), true)]))
            .await
            .expect("adds");
        servers
            .add(McpServerConfig {
                name: "other".into(),
                ..remote(Vec::new())
            })
            .await
            .expect("adds another");
        let error = servers
            .update(
                &added.server_id,
                McpServerConfig {
                    name: "other".into(),
                    ..remote(vec![
                        header("X-Api-Key", Some("k2"), true),
                        header("X-New", Some("n1"), true),
                    ])
                },
            )
            .await
            .expect_err("the name is taken");
        assert_eq!(error.kind, "mcpServerNameTaken");
        let id = &added.server_id.0;
        let replaced = secrets.get(&format!("{id}/header/X-Api-Key")).unwrap();
        assert_eq!(replaced.as_deref(), Some("k1"));
        assert_eq!(secrets.get(&format!("{id}/header/X-New")).unwrap(), None);
    }

    #[tokio::test]
    async fn invalid_configs_are_refused_before_anything_is_stored() {
        let (_home, servers, _secrets) = servers();
        let cases = [
            McpServerConfig {
                name: "".into(),
                ..remote(Vec::new())
            },
            McpServerConfig {
                name: "x".repeat(65),
                ..remote(Vec::new())
            },
            McpServerConfig {
                name: "local".into(),
                transport: McpServerTransport::Stdio {
                    command: "  ".into(),
                    args: Vec::new(),
                    env: Vec::new(),
                },
            },
            McpServerConfig {
                name: "local".into(),
                transport: McpServerTransport::Stdio {
                    command: "server".into(),
                    args: Vec::new(),
                    env: vec![header("A=B", Some("x"), false)],
                },
            },
            McpServerConfig {
                name: "remote".into(),
                transport: McpServerTransport::Http {
                    url: "file:///etc/passwd".into(),
                    headers: Vec::new(),
                },
            },
            remote(vec![header("Bad Header", Some("x"), false)]),
            remote(vec![header("Mcp-Session-Id", Some("x"), false)]),
            remote(vec![
                header("X-Key", Some("a"), false),
                header("x-key", Some("b"), false),
            ]),
            remote(vec![header("X-Key", None, false)]),
        ];
        for config in cases {
            let error = servers.add(config.clone()).await.expect_err("refused");
            assert_eq!(error.kind, "invalidMcpServer", "{config:?}");
        }
        // Ottam's own path, in any case.
        let error = servers
            .add(McpServerConfig {
                name: "OTtam".into(),
                ..remote(Vec::new())
            })
            .await
            .expect_err("refused");
        assert_eq!(error.kind, "mcpServerNameReserved");
        assert!(servers.list().await.expect("lists").is_empty());
    }
}

mod runtime {
    use crate::mcp::secrets::MemorySecretStore;
    use crate::mcp::upstream::{McpWorkspace, Upstream, UpstreamError};
    use crate::mcp::{McpNotice, McpRuntime, McpServers};
    use crate::storage::Database;
    use ottam_protocol::{
        McpConfigValue, McpServerConfig, McpServerID, McpServerState, McpServerStatus,
        McpServerTransport, WorkspaceID,
    };
    use rmcp::model::{CallToolRequestParams, CallToolResponse};
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::io::AsyncBufReadExt;

    fn fixture_binary() -> PathBuf {
        let binary = std::env::var_os("OTTAM_TEST_MCP_BINARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/debug/ottam-test-mcp")
            });
        assert!(
            binary.is_file(),
            "ottam-test-mcp is missing at {}; build it with `cargo build -p ottam-test-mcp` or set OTTAM_TEST_MCP_BINARY",
            binary.display()
        );
        binary
    }

    fn value(name: &str, value: &str, secret: bool) -> McpConfigValue {
        McpConfigValue {
            name: name.into(),
            value: Some(value.into()),
            secret,
        }
    }

    fn local(name: &str, args: &[&str]) -> McpServerConfig {
        McpServerConfig {
            name: name.into(),
            transport: McpServerTransport::Stdio {
                command: fixture_binary().display().to_string(),
                args: args.iter().map(|arg| arg.to_string()).collect(),
                env: vec![value("FIXTURE_TOKEN", "local-secret", true)],
            },
        }
    }

    struct Harness {
        _home: TempDir,
        servers: McpServers,
        runtime: McpRuntime,
        states: Arc<Mutex<Vec<(String, McpServerState)>>>,
        roots: TempDir,
    }

    impl Harness {
        fn new(idle_after: Duration) -> Self {
            let home = TempDir::new().expect("home");
            let database = Database::open_test_home(home.path().to_owned()).expect("database");
            let servers = McpServers::new(database, Arc::new(MemorySecretStore::default()));
            let states = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&states);
            let runtime = McpRuntime::with_idle_after(
                servers.clone(),
                Arc::new(move |notice| {
                    if let McpNotice::State(server, state) = notice {
                        recorded.lock().unwrap().push((server.0, state));
                    }
                }),
                idle_after,
            );
            Self {
                _home: home,
                servers,
                runtime,
                states,
                roots: TempDir::new().expect("roots"),
            }
        }

        async fn add(&self, config: McpServerConfig) -> McpServerID {
            self.servers.add(config).await.expect("adds").server_id
        }

        fn workspace(&self, id: &str) -> McpWorkspace {
            let root = self.roots.path().join(id);
            std::fs::create_dir_all(&root).expect("creates root");
            McpWorkspace {
                id: WorkspaceID(id.into()),
                roots: vec![root.canonicalize().expect("root")],
            }
        }

        fn statuses(&self) -> Vec<McpServerStatus> {
            let states = self.states.lock().unwrap();
            states.iter().map(|(_, state)| state.status).collect()
        }

        async fn until_status(
            &self,
            server: &McpServerID,
            status: McpServerStatus,
        ) -> McpServerState {
            eventually(|| {
                let state = self.runtime.state(server);
                (state.status == status).then_some(state)
            })
            .await
        }
    }

    async fn eventually<T>(mut check: impl FnMut() -> Option<T>) -> T {
        for _ in 0..200 {
            if let Some(found) = check() {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("condition never held");
    }

    async fn call(upstream: &Upstream, tool: &str, arguments: serde_json::Value) -> String {
        let serde_json::Value::Object(arguments) = arguments else {
            panic!("arguments are an object");
        };
        let response = upstream
            .peer
            .call_tool_once(CallToolRequestParams::new(tool.to_owned()).with_arguments(arguments))
            .await
            .expect("calls the tool");
        let CallToolResponse::Complete(result) = response else {
            panic!("a complete result");
        };
        result.content[0]
            .as_text()
            .expect("text content")
            .text
            .clone()
    }

    fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    fn file_uri(path: &Path) -> String {
        reqwest::Url::from_directory_path(path).unwrap().to_string()
    }

    #[tokio::test]
    async fn a_local_server_runs_once_per_workspace_with_its_roots_and_secrets() {
        let harness = Harness::new(Duration::from_secs(600));
        let server = harness.add(local("fixture", &[])).await;
        let (alpha, beta) = (harness.workspace("alpha"), harness.workspace("beta"));
        let _alpha_lease = harness
            .runtime
            .acquire(&server, &alpha)
            .await
            .expect("acquires");
        let _second_alpha = harness
            .runtime
            .acquire(&server, &alpha)
            .await
            .expect("acquires");
        let _beta_lease = harness
            .runtime
            .acquire(&server, &beta)
            .await
            .expect("acquires");

        let peer = harness
            .runtime
            .upstream(&server, &alpha.id)
            .await
            .expect("starts");
        let tools = peer.peer.list_all_tools().await.expect("lists tools");
        assert!(tools.iter().any(|tool| tool.name == "echo"));
        assert_eq!(call(&peer, "echo", json!({"text": "hi"})).await, "hi");
        assert_eq!(
            call(&peer, "env", json!({"name": "FIXTURE_TOKEN"})).await,
            "local-secret",
            "the secret reaches the process environment"
        );
        assert_eq!(
            call(&peer, "roots", json!({})).await,
            file_uri(&alpha.roots[0])
        );

        let again = harness
            .runtime
            .upstream(&server, &alpha.id)
            .await
            .expect("reuses");
        let beta_peer = harness
            .runtime
            .upstream(&server, &beta.id)
            .await
            .expect("starts");
        let alpha_pid = call(&peer, "pid", json!({})).await;
        assert_eq!(
            call(&again, "pid", json!({})).await,
            alpha_pid,
            "one process per workspace"
        );
        assert_ne!(call(&beta_peer, "pid", json!({})).await, alpha_pid);
        assert_eq!(
            call(&beta_peer, "roots", json!({})).await,
            file_uri(&beta.roots[0])
        );

        assert_eq!(
            harness.runtime.state(&server).status,
            McpServerStatus::Running
        );
        assert_eq!(
            harness.statuses().first(),
            Some(&McpServerStatus::Starting),
            "starting is reported before running"
        );
        assert_eq!(harness.statuses().last(), Some(&McpServerStatus::Running));
        harness.runtime.shutdown().await;
    }

    #[tokio::test]
    async fn a_crashed_server_shows_failed_and_restarts_after_its_backoff() {
        let harness = Harness::new(Duration::from_secs(600));
        let server = harness.add(local("fixture", &[])).await;
        let workspace = harness.workspace("alpha");
        let _lease = harness
            .runtime
            .acquire(&server, &workspace)
            .await
            .expect("acquires");
        let peer = harness
            .runtime
            .upstream(&server, &workspace.id)
            .await
            .expect("starts");
        let first_pid = call(&peer, "pid", json!({})).await;

        let _ = peer
            .peer
            .call_tool_once(CallToolRequestParams::new("crash"))
            .await
            .expect_err("the process exits mid-call");
        let failed = harness.until_status(&server, McpServerStatus::Failed).await;
        let error = failed.error.expect("a failure reason");
        assert!(error.contains("exit status: 3"), "{error}");
        assert!(error.contains("fixture crashed on request"), "{error}");
        let refused = harness
            .runtime
            .upstream(&server, &workspace.id)
            .await
            .expect_err("still backing off");
        assert_eq!(refused, UpstreamError::Failed(error));

        tokio::time::sleep(Duration::from_millis(1100)).await;
        let restarted = harness
            .runtime
            .upstream(&server, &workspace.id)
            .await
            .expect("restarts");
        assert_ne!(call(&restarted, "pid", json!({})).await, first_pid);
        assert_eq!(
            harness.runtime.state(&server).status,
            McpServerStatus::Running
        );
        harness.runtime.shutdown().await;
    }

    #[tokio::test]
    async fn a_server_that_cannot_start_shows_why() {
        let harness = Harness::new(Duration::from_secs(600));
        let refuses = harness.add(local("refuses", &["--fail-start"])).await;
        let mut missing = local("missing", &[]);
        let McpServerTransport::Stdio { command, .. } = &mut missing.transport else {
            unreachable!()
        };
        *command = "/nonexistent/ottam-mcp-server".into();
        let missing = harness.add(missing).await;
        let workspace = harness.workspace("alpha");
        for (server, expected) in [
            (&refuses, "fixture refused to start"),
            (
                &missing,
                "Could not start /nonexistent/ottam-mcp-server",
            ),
        ] {
            let _lease = harness
                .runtime
                .acquire(server, &workspace)
                .await
                .expect("acquires");
            let failed = harness.until_status(server, McpServerStatus::Failed).await;
            let error = failed.error.expect("a reason");
            assert!(error.contains(expected), "{error}");
        }
        harness.runtime.shutdown().await;
    }

    #[tokio::test]
    async fn idle_servers_and_quitting_leave_no_process_behind() {
        let harness = Harness::new(Duration::from_millis(200));
        let server = harness.add(local("fixture", &[])).await;
        let workspace = harness.workspace("alpha");

        let lease = harness
            .runtime
            .acquire(&server, &workspace)
            .await
            .expect("acquires");
        let peer = harness
            .runtime
            .upstream(&server, &workspace.id)
            .await
            .expect("starts");
        let pid: i32 = call(&peer, "pid", json!({})).await.parse().unwrap();
        let child: i32 = call(&peer, "spawn_child", json!({})).await.parse().unwrap();
        assert!(alive(pid) && alive(child));
        drop(lease);
        harness
            .until_status(&server, McpServerStatus::NotStarted)
            .await;
        eventually(|| (!alive(pid) && !alive(child)).then_some(())).await;
        harness
            .runtime
            .upstream(&server, &workspace.id)
            .await
            .expect_err("an idle-stopped slot needs a new session");

        let _lease = harness
            .runtime
            .acquire(&server, &workspace)
            .await
            .expect("acquires");
        let peer = harness
            .runtime
            .upstream(&server, &workspace.id)
            .await
            .expect("starts");
        let pid: i32 = call(&peer, "pid", json!({})).await.parse().unwrap();
        let child: i32 = call(&peer, "spawn_child", json!({})).await.parse().unwrap();
        harness.runtime.shutdown().await;
        assert!(!alive(pid), "the server is reaped before shutdown returns");
        eventually(|| (!alive(child)).then_some(())).await;
        harness
            .runtime
            .acquire(&server, &workspace)
            .await
            .err()
            .expect("no leases after quitting");
    }

    #[tokio::test]
    async fn a_tokens_leases_end_with_its_sessions() {
        let harness = Harness::new(Duration::from_millis(200));
        let server = harness.add(local("fixture", &[])).await;
        let workspace = harness.workspace("alpha");
        let lease = harness
            .runtime
            .acquire(&server, &workspace)
            .await
            .expect("acquires");
        let adapter = super::SessionsAdapter::new();
        let erased: Arc<dyn crate::agent::HarnessAdapter> = adapter.clone();
        let tokens = crate::mcp::McpTokens::default();
        let grant = tokens.mint(
            &erased,
            crate::mcp::tokens::GrantRequest {
                workspace_id: workspace.id.clone(),
                conversation_id: ottam_protocol::ConversationID("chat".into()),
                agent_id: crate::agent::AgentId::new("agent"),
                scope: crate::mcp::tokens::TokenScope::Session,
                leases: vec![(server.clone(), lease)],
            },
        );
        adapter.set_session("s1", crate::agent::HarnessSessionState::Active);
        grant.bind(crate::agent::HarnessSessionId::new("s1"));
        harness
            .until_status(&server, McpServerStatus::Running)
            .await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        tokens.sweep();
        assert_eq!(
            harness.runtime.state(&server).status,
            McpServerStatus::Running,
            "a live session keeps its servers"
        );

        adapter.set_session("s1", crate::agent::HarnessSessionState::Dead);
        tokens.sweep();
        harness
            .until_status(&server, McpServerStatus::NotStarted)
            .await;
        harness.runtime.shutdown().await;
    }

    #[tokio::test]
    async fn a_shared_tokens_leases_end_with_their_workspaces_sessions() {
        let harness = Harness::new(Duration::from_millis(200));
        let first = harness.add(local("first", &[])).await;
        let second = harness.add(local("second", &[])).await;
        let (alpha, beta) = (harness.workspace("alpha"), harness.workspace("beta"));
        let adapter = super::SessionsAdapter::new();
        let erased: Arc<dyn crate::agent::HarnessAdapter> = adapter.clone();
        let tokens = crate::mcp::McpTokens::default();
        let mint = |server: &McpServerID, workspace: &McpWorkspace, lease, chat: &str| {
            tokens.mint(
                &erased,
                crate::mcp::tokens::GrantRequest {
                    workspace_id: workspace.id.clone(),
                    conversation_id: ottam_protocol::ConversationID(chat.into()),
                    agent_id: crate::agent::AgentId::new("agent"),
                    scope: crate::mcp::tokens::TokenScope::Directory(PathBuf::from("/work/app")),
                    leases: vec![(server.clone(), lease)],
                },
            )
        };
        let lease = harness.runtime.acquire(&first, &alpha).await.unwrap();
        adapter.set_session("s1", crate::agent::HarnessSessionState::Active);
        mint(&first, &alpha, lease, "chat-a").bind(crate::agent::HarnessSessionId::new("s1"));

        // A session that never opened gives back what it was leased.
        let lease = harness.runtime.acquire(&second, &beta).await.unwrap();
        let given_up = mint(&second, &beta, lease, "chat-b");
        harness
            .until_status(&second, McpServerStatus::Running)
            .await;
        drop(given_up);
        harness
            .until_status(&second, McpServerStatus::NotStarted)
            .await;

        let lease = harness.runtime.acquire(&second, &beta).await.unwrap();
        adapter.set_session("s2", crate::agent::HarnessSessionState::Active);
        mint(&second, &beta, lease, "chat-b").bind(crate::agent::HarnessSessionId::new("s2"));
        harness
            .until_status(&second, McpServerStatus::Running)
            .await;

        // The token outlives one workspace's session, but not its leases.
        adapter.set_session("s2", crate::agent::HarnessSessionState::Dead);
        tokens.sweep();
        harness
            .until_status(&second, McpServerStatus::NotStarted)
            .await;
        assert_eq!(
            harness.runtime.state(&first).status,
            McpServerStatus::Running,
            "the other workspace's session keeps its servers"
        );

        adapter.set_session("s1", crate::agent::HarnessSessionState::Dead);
        tokens.sweep();
        harness
            .until_status(&first, McpServerStatus::NotStarted)
            .await;
        harness.runtime.shutdown().await;
    }

    #[tokio::test]
    async fn local_servers_find_the_agents_node_first_on_path() {
        let harness = Harness::new(Duration::from_secs(600));
        harness
            .runtime
            .set_toolchain(Arc::new(|| Some(PathBuf::from("/managed/node/bin"))));
        let server = harness.add(local("fixture", &[])).await;
        let workspace = harness.workspace("alpha");
        let _lease = harness
            .runtime
            .acquire(&server, &workspace)
            .await
            .expect("acquires");
        let peer = harness
            .runtime
            .upstream(&server, &workspace.id)
            .await
            .expect("starts");
        let path = call(&peer, "env", json!({"name": "PATH"})).await;
        let inherited = std::env::var("PATH").unwrap_or_default();
        assert_eq!(path, format!("/managed/node/bin:{inherited}"));
        harness.runtime.shutdown().await;
    }

    #[tokio::test]
    async fn a_changed_server_or_archived_workspace_stops_its_processes() {
        let harness = Harness::new(Duration::from_secs(600));
        let server = harness.add(local("fixture", &[])).await;
        let workspace = harness.workspace("alpha");
        let _lease = harness
            .runtime
            .acquire(&server, &workspace)
            .await
            .expect("acquires");
        let peer = harness
            .runtime
            .upstream(&server, &workspace.id)
            .await
            .expect("starts");
        let pid: i32 = call(&peer, "pid", json!({})).await.parse().unwrap();
        harness.runtime.stop_workspace(&workspace.id).await;
        assert!(!alive(pid));
        assert_eq!(
            harness.runtime.state(&server).status,
            McpServerStatus::NotStarted
        );

        let _lease = harness
            .runtime
            .acquire(&server, &workspace)
            .await
            .expect("acquires");
        let peer = harness
            .runtime
            .upstream(&server, &workspace.id)
            .await
            .expect("starts");
        let pid: i32 = call(&peer, "pid", json!({})).await.parse().unwrap();
        harness.runtime.stop_server(&server).await;
        assert!(!alive(pid));
        assert_eq!(
            harness.statuses().last(),
            Some(&McpServerStatus::NotStarted)
        );
    }

    #[tokio::test]
    async fn a_changed_server_starts_again_with_its_new_config_for_its_sessions() {
        let harness = Harness::new(Duration::from_millis(100));
        let server = harness.add(local("fixture", &[])).await;
        let workspace = harness.workspace("alpha");
        let lease = harness
            .runtime
            .acquire(&server, &workspace)
            .await
            .expect("acquires");
        let upstream = harness
            .runtime
            .upstream(&server, &workspace.id)
            .await
            .expect("starts");
        let old: i32 = call(&upstream, "pid", json!({})).await.parse().unwrap();

        let mut changed = local("fixture", &[]);
        let McpServerTransport::Stdio { env, .. } = &mut changed.transport else {
            panic!("stdio");
        };
        *env = vec![value("FIXTURE_TOKEN", "rotated", true)];
        harness
            .servers
            .update(&server, changed)
            .await
            .expect("updates");
        harness.runtime.restart_server(&server, true).await;
        assert!(!alive(old));

        // The session's lease carried over, so its next call starts it.
        let upstream = harness
            .runtime
            .upstream(&server, &workspace.id)
            .await
            .expect("starts again");
        let token = call(&upstream, "env", json!({"name": "FIXTURE_TOKEN"})).await;
        assert_eq!(token, "rotated");
        let new: i32 = call(&upstream, "pid", json!({})).await.parse().unwrap();

        // Releasing the carried lease idles the new process away.
        drop(lease);
        eventually(|| (!alive(new)).then_some(())).await;
    }

    #[tokio::test]
    async fn stderr_that_is_not_utf8_keeps_being_drained() {
        let harness = Harness::new(Duration::from_secs(600));
        let server = harness.add(local("fixture", &[])).await;
        let workspace = harness.workspace("alpha");
        let _lease = harness
            .runtime
            .acquire(&server, &workspace)
            .await
            .expect("acquires");
        let upstream = harness
            .runtime
            .upstream(&server, &workspace.id)
            .await
            .expect("starts");
        for _ in 0..3 {
            assert_eq!(call(&upstream, "garble", json!({})).await, "ok");
        }
        // Still read to the end: a crash's last words make the failed state.
        let _ = upstream
            .peer
            .call_tool_once(CallToolRequestParams::new("crash".to_owned()))
            .await;
        let state = harness.until_status(&server, McpServerStatus::Failed).await;
        let error = state.error.expect("a reason");
        assert!(
            error.ends_with("fixture crashed on request"),
            "unexpected reason: {error}"
        );
    }

    #[tokio::test]
    async fn a_remote_server_is_one_connection_carrying_its_headers() {
        let mut http = tokio::process::Command::new(fixture_binary())
            .arg("--http")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("starts the HTTP fixture");
        let mut lines = tokio::io::BufReader::new(http.stdout.take().unwrap()).lines();
        let url = lines.next_line().await.unwrap().expect("prints its URL");

        let harness = Harness::new(Duration::from_secs(600));
        let server = harness
            .add(McpServerConfig {
                name: "remote".into(),
                transport: McpServerTransport::Http {
                    url,
                    headers: vec![
                        value("X-Fixture-Key", "remote-secret", true),
                        value("X-Team", "core", false),
                    ],
                },
            })
            .await;
        let (alpha, beta) = (harness.workspace("alpha"), harness.workspace("beta"));
        let _alpha = harness
            .runtime
            .acquire(&server, &alpha)
            .await
            .expect("acquires");
        let _beta = harness
            .runtime
            .acquire(&server, &beta)
            .await
            .expect("acquires");
        let alpha_peer = harness
            .runtime
            .upstream(&server, &alpha.id)
            .await
            .expect("connects");
        let beta_peer = harness
            .runtime
            .upstream(&server, &beta.id)
            .await
            .expect("connects");
        assert_eq!(
            call(&alpha_peer, "header", json!({"name": "x-fixture-key"})).await,
            "remote-secret"
        );
        assert_eq!(
            call(&alpha_peer, "header", json!({"name": "x-team"})).await,
            "core"
        );
        let session = call(&alpha_peer, "header", json!({"name": "mcp-session-id"})).await;
        assert!(!session.is_empty());
        assert_eq!(
            call(&beta_peer, "header", json!({"name": "mcp-session-id"})).await,
            session,
            "every workspace shares one MCP session"
        );
        assert_eq!(
            call(&beta_peer, "roots", json!({})).await,
            "",
            "a shared connection declares no roots"
        );
        harness.runtime.shutdown().await;
        let _ = http.kill().await;
    }
}

/// Agents on proxy paths backed by the fixture server.
mod proxied {
    use super::SessionsAdapter;
    use crate::agent::{
        AgentDefinition, AgentId, AgentInstance, HarnessAdapter, HarnessId,
        HarnessPermissionBroker, HarnessPermissionFuture, HarnessPermissionOutcome,
        HarnessPermissionRequest, HarnessQuestionFuture, HarnessQuestionOutcome,
        HarnessQuestionRequest, HarnessSessionState, HomeLayout, McpConnections,
    };
    use crate::mcp::secrets::MemorySecretStore;
    use crate::mcp::upstream::McpWorkspace;
    use crate::mcp::{
        McpGrant, McpNotice, McpProxy, McpRuntime, McpServers, McpService, McpTokens, SessionGrant,
    };
    use crate::storage::repositories::WorkspaceRepository;
    use crate::storage::Database;
    use ottam_protocol::{
        AgentQuestionAnswer, ConversationID, McpServerConfig, McpServerID, McpServerTransport,
        WorkspaceID,
    };
    use rmcp::model::{
        CallToolRequest, CallToolRequestParams, CallToolResponse, ClientRequest, ProtocolVersion,
        ResultType, ServerNotification, SubscriptionFilter,
    };
    use rmcp::service::{NotificationContext, PeerRequestOptions, RunningService};
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt, RoleClient};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tempfile::TempDir;

    fn fixture_binary() -> PathBuf {
        let binary = std::env::var_os("OTTAM_TEST_MCP_BINARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/debug/ottam-test-mcp")
            });
        assert!(binary.is_file(), "build ottam-test-mcp first");
        binary
    }

    fn local(name: &str, args: &[&str]) -> McpServerConfig {
        McpServerConfig {
            name: name.into(),
            transport: McpServerTransport::Stdio {
                command: fixture_binary().display().to_string(),
                args: args.iter().map(|arg| arg.to_string()).collect(),
                env: Vec::new(),
            },
        }
    }

    /// Answers every question with its first question's `blue`, recording
    /// who was asked.
    #[derive(Default)]
    struct RecordingBroker {
        asked: Mutex<Vec<(String, Option<String>, String)>>,
    }

    impl HarnessPermissionBroker for RecordingBroker {
        fn request_permission(
            &self,
            _request: HarnessPermissionRequest,
        ) -> HarnessPermissionFuture<'_> {
            Box::pin(async { HarnessPermissionOutcome::Cancelled })
        }

        fn ask_question(&self, request: HarnessQuestionRequest) -> HarnessQuestionFuture<'_> {
            self.asked.lock().unwrap().push((
                request.session_id.0.clone(),
                request.provider_tool_call_id.clone(),
                request.title.clone(),
            ));
            let answer = AgentQuestionAnswer {
                question_id: request.questions[0].id.clone(),
                values: vec!["blue".into()],
                other: None,
            };
            Box::pin(async move { HarnessQuestionOutcome::Submitted(vec![answer]) })
        }
    }

    /// The agent side of a session: counts `list_changed` notifications.
    #[derive(Clone, Default)]
    struct Agent {
        list_changed: Arc<AtomicUsize>,
    }

    impl ClientHandler for Agent {
        async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
            self.list_changed.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct Chat {
        client: RunningService<RoleClient, Agent>,
        agent: Agent,
    }

    impl Chat {
        async fn call(&self, tool: &str, arguments: serde_json::Value) -> Result<String, String> {
            self.call_with_meta(tool, arguments, None).await
        }

        async fn call_with_meta(
            &self,
            tool: &str,
            arguments: serde_json::Value,
            meta: Option<serde_json::Value>,
        ) -> Result<String, String> {
            let serde_json::Value::Object(arguments) = arguments else {
                panic!("arguments are an object");
            };
            let mut params = CallToolRequestParams::new(tool.to_owned()).with_arguments(arguments);
            if let Some(meta) = meta {
                params.meta = Some(serde_json::from_value(meta).expect("meta"));
            }
            match self.client.peer().call_tool_once(params).await {
                Ok(CallToolResponse::Complete(result)) => {
                    Ok(result.content[0].as_text().expect("text").text.clone())
                }
                Ok(_) => panic!("core always completes a call"),
                Err(error) => Err(error.to_string()),
            }
        }
    }

    struct Harness {
        _home: TempDir,
        database: Database,
        roots: TempDir,
        service: McpService,
        proxy: McpProxy,
        broker: Arc<RecordingBroker>,
        adapter: Arc<SessionsAdapter>,
        notices: Arc<Mutex<Vec<McpNotice>>>,
    }

    impl Harness {
        async fn new() -> Self {
            Self::with_redirect_port(0).await
        }

        async fn with_redirect_port(redirect_port: u16) -> Self {
            let home = TempDir::new().expect("home");
            let database = Database::open_test_home(home.path().to_owned()).expect("database");
            let servers = McpServers::new(database.clone(), Arc::new(MemorySecretStore::default()));
            let notices = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&notices);
            let runtime = McpRuntime::new(
                servers.clone(),
                Arc::new(move |notice| recorded.lock().unwrap().push(notice)),
            );
            let service = McpService::new(servers, runtime, redirect_port);
            let proxy = McpProxy::start(McpTokens::default())
                .await
                .expect("listens");
            let broker = Arc::new(RecordingBroker::default());
            service
                .attach_proxy(proxy.sessions(), broker.clone())
                .await
                .expect("attaches");
            Self {
                _home: home,
                database,
                roots: TempDir::new().expect("roots"),
                service,
                proxy,
                broker,
                adapter: SessionsAdapter::new(),
                notices,
            }
        }

        /// The first notice `pick` accepts, once it arrives.
        async fn notice<T>(&self, pick: impl Fn(&McpNotice) -> Option<T>) -> T {
            for _ in 0..400 {
                if let Some(found) = self.notices.lock().unwrap().iter().find_map(&pick) {
                    return found;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            panic!("no such notice: {:?}", self.notices.lock().unwrap());
        }

        fn workspace(&self, id: &str) -> McpWorkspace {
            let root = self.roots.path().join(id);
            std::fs::create_dir_all(&root).expect("root");
            McpWorkspace {
                id: WorkspaceID(id.into()),
                roots: vec![root],
            }
        }

        async fn add(&self, config: McpServerConfig) -> McpServerID {
            self.service.add(config).await.expect("adds").server_id
        }

        /// A chat in `workspace` connected to the path named `server`.
        async fn chat(&self, server: &str, workspace: &str, session: &str) -> Chat {
            self.chat_with(server, workspace, session, ClientLifecycleMode::Initialize)
                .await
        }

        /// A session speaking `lifecycle`: `initialize` for a legacy agent,
        /// `server/discover` for a `2026-07-28` one such as Claude Code.
        async fn chat_with(
            &self,
            server: &str,
            workspace: &str,
            session: &str,
            lifecycle: ClientLifecycleMode,
        ) -> Chat {
            self.adapter
                .set_session(session, HarnessSessionState::Active);
            let erased: Arc<dyn HarnessAdapter> = self.adapter.clone();
            let cwd = self.workspace(workspace).roots.remove(0);
            let (grant, entries) = self
                .service
                .grant(SessionGrant {
                    adapter: &erased,
                    workspace_id: WorkspaceID(workspace.into()),
                    conversation_id: ConversationID(format!("conversation-{session}")),
                    agent_id: AgentId::new("agent"),
                    cwd: &cwd,
                })
                .await
                .expect("entries");
            // Bound, the token lives as long as the adapter's session.
            grant.bind(crate::agent::HarnessSessionId::new(session));
            let entry = entries
                .iter()
                .find(|entry| entry.name == server)
                .expect("an entry for the server");
            let token = entry.headers[0]
                .1
                .strip_prefix("Bearer ")
                .unwrap()
                .to_owned();
            // OpenCode adds a query string; the path is what matters.
            let url = format!("{}?codemode=false", entry.url);
            let transport = StreamableHttpClientTransport::from_config(
                StreamableHttpClientTransportConfig::with_uri(url).auth_header(token),
            );
            let agent = Agent::default();
            let client = agent
                .clone()
                .serve_with_lifecycle(transport, lifecycle)
                .await
                .expect("initializes");
            Chat { client, agent }
        }

        /// The grant a session of `adapter` in `workspace` is given, and its
        /// entries' names.
        async fn entries(
            &self,
            adapter: &Arc<SessionsAdapter>,
            workspace: &str,
            conversation: &str,
        ) -> Option<(McpGrant, Vec<String>, String)> {
            let erased: Arc<dyn HarnessAdapter> = adapter.clone();
            let cwd = self.workspace(workspace).roots.remove(0);
            let (grant, entries) = self
                .service
                .grant(SessionGrant {
                    adapter: &erased,
                    workspace_id: WorkspaceID(workspace.into()),
                    conversation_id: ConversationID(conversation.into()),
                    agent_id: AgentId::new("agent"),
                    cwd: &cwd,
                })
                .await?;
            let authorization = entries[0].headers[0].1.clone();
            let names = entries.into_iter().map(|entry| entry.name).collect();
            Some((grant, names, authorization))
        }

        async fn finish(self) {
            self.service.runtime().shutdown().await;
            self.proxy.finish().await;
        }
    }

    /// Ottam's tools as a test sets them: whether every caller is a
    /// delegated child.
    struct StubOttamTools {
        child: bool,
    }

    impl crate::mcp::OttamTools for StubOttamTools {
        fn is_child(
            &self,
            _caller: &crate::mcp::McpCaller,
        ) -> futures::future::BoxFuture<'_, bool> {
            Box::pin(async move { self.child })
        }
        fn delegation_targets(
            &self,
        ) -> futures::future::BoxFuture<'_, crate::mcp::DelegationTargets> {
            Box::pin(async {
                crate::mcp::DelegationTargets {
                    named: vec!["review-bot".to_owned()],
                    installed: vec![("codex".to_owned(), "Codex".to_owned())],
                }
            })
        }
        fn delegate(
            &self,
            caller: crate::mcp::McpCaller,
            request: crate::mcp::DelegateRequest,
        ) -> futures::future::BoxFuture<'_, Result<String, String>> {
            Box::pin(async move {
                let agent = request
                    .agent
                    .or(request.engine)
                    .unwrap_or_else(|| "own".to_owned());
                Ok(format!(
                    "{agent} from {}: {}",
                    caller.conversation_id.0, request.prompt
                ))
            })
        }
        fn list_children(
            &self,
            _caller: crate::mcp::McpCaller,
        ) -> futures::future::BoxFuture<'_, Result<String, String>> {
            Box::pin(async { Ok("none".to_owned()) })
        }
        fn conversation_info(
            &self,
            caller: crate::mcp::McpCaller,
        ) -> futures::future::BoxFuture<'_, Result<String, String>> {
            Box::pin(async move { Ok(format!("Conversation: {}", caller.conversation_id.0)) })
        }
        fn message_child(
            &self,
            _caller: crate::mcp::McpCaller,
            child: String,
            message: String,
        ) -> futures::future::BoxFuture<'_, Result<String, String>> {
            Box::pin(async move { Ok(format!("to {child}: {message}")) })
        }
        fn stop_child(
            &self,
            _caller: crate::mcp::McpCaller,
            child: String,
        ) -> futures::future::BoxFuture<'_, Result<String, String>> {
            Box::pin(async move { Ok(format!("stopped {child}")) })
        }
        fn message_parent(
            &self,
            _caller: crate::mcp::McpCaller,
            message: String,
        ) -> futures::future::BoxFuture<'_, Result<String, String>> {
            Box::pin(async move { Ok(format!("to parent: {message}")) })
        }
        fn session_messages(
            &self,
            _caller: crate::mcp::McpCaller,
            conversation: Option<String>,
            before: Option<u64>,
            limit: Option<u64>,
        ) -> futures::future::BoxFuture<'_, Result<String, String>> {
            Box::pin(async move { Ok(format!("{conversation:?} {before:?} {limit:?}")) })
        }
        fn toolcall_output(
            &self,
            _caller: crate::mcp::McpCaller,
            conversation: Option<String>,
            tool_call_id: String,
            offset: Option<u64>,
        ) -> futures::future::BoxFuture<'_, Result<String, String>> {
            Box::pin(async move { Ok(format!("{conversation:?} {tool_call_id} {offset:?}")) })
        }
    }

    const PARENT_TOOLS: [&str; 7] = [
        "delegate",
        "message_child",
        "list_children",
        "stop_child",
        "get_conversation_info",
        "get_session_messages",
        "get_session_toolcall_output",
    ];

    /// A shared token's listing can't tell which session asks, so it offers
    /// a parent's tools and a child's even when the token's conversation is
    /// a child; a call outside the caller's role is refused.
    #[tokio::test]
    async fn a_shared_listing_offers_the_delegation_tools() {
        use crate::mcp::proxy::McpEndpoint;
        let endpoint =
            crate::mcp::ottam::OttamEndpoint::new(Arc::new(StubOttamTools {
                child: true,
            }));
        let caller = crate::mcp::McpCaller {
            workspace_id: ottam_protocol::WorkspaceID("alpha".into()),
            conversation_id: ottam_protocol::ConversationID("child".into()),
            agent_id: crate::agent::AgentId("opencode".into()),
            generation: 1,
            session_id: None,
            shared: true,
            sessions: Vec::new(),
            servers: Vec::new(),
        };
        let listed = endpoint
            .list_tools(caller.clone(), None)
            .await
            .expect("lists");
        let names: Vec<String> = listed
            .tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        let mut expected = PARENT_TOOLS.to_vec();
        expected.push("message_parent");
        assert_eq!(names, expected);
        let unshared = crate::mcp::McpCaller {
            shared: false,
            ..caller
        };
        let listed = endpoint.list_tools(unshared, None).await.expect("lists");
        let names: Vec<String> = listed
            .tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert_eq!(
            names,
            [
                "message_parent",
                "get_conversation_info",
                "get_session_messages",
                "get_session_toolcall_output"
            ]
        );
    }

    /// With no servers configured, a session still gets Ottam's path. A
    /// parent sees the delegation tools; a child sees `message_parent` and
    /// the history tools, and is refused the rest.
    #[tokio::test]
    async fn every_session_gets_ottam_and_only_parents_may_delegate() {
        for child in [false, true] {
            let harness = Harness::new().await;
            harness
                .service
                .serve_ottam(Arc::new(StubOttamTools { child }));
            let chat = harness.chat("ottam", "alpha", "session-1").await;
            let mut names: Vec<String> = chat
                .client
                .peer()
                .list_all_tools()
                .await
                .expect("lists")
                .into_iter()
                .map(|tool| tool.name.to_string())
                .collect();
            names.sort();
            if !child {
                let tools = chat.client.peer().list_all_tools().await.expect("lists");
                let delegate = tools
                    .iter()
                    .find(|tool| tool.name == "delegate")
                    .expect("delegate is listed");
                let schema = serde_json::Value::Object((*delegate.input_schema).clone());
                assert_eq!(schema["required"], json!(["prompt"]));
                assert!(schema["properties"]["engine"]["description"]
                    .as_str()
                    .is_some_and(|text| text.contains("Codex (codex)")));
                assert!(schema["properties"]["agent"]["description"]
                    .as_str()
                    .is_some_and(|text| text.contains("review-bot")));
            }
            if child {
                assert_eq!(
                    names,
                    [
                        "get_conversation_info",
                        "get_session_messages",
                        "get_session_toolcall_output",
                        "message_parent"
                    ]
                );
                assert_eq!(
                    chat.call("get_conversation_info", json!({}))
                        .await
                        .expect("completes"),
                    "Conversation: conversation-session-1"
                );
                assert_eq!(
                    chat.call("message_parent", json!({"message": "Blocked on X"}))
                        .await
                        .expect("completes"),
                    "to parent: Blocked on X"
                );
                // A child reads its parent's history by default; numbers
                // may come as strings.
                assert_eq!(
                    chat.call("get_session_messages", json!({"before": "40", "limit": 5}))
                        .await
                        .expect("completes"),
                    "None Some(40) Some(5)"
                );
                assert_eq!(
                    chat.call("stop_child", json!({"child": "review-bot"}))
                        .await
                        .expect("completes"),
                    "A delegated agent cannot delegate; report back to your parent instead"
                );
                assert_eq!(
                    chat.call(
                        "delegate",
                        json!({"agent": "review-bot", "prompt": "Review it"})
                    )
                    .await
                    .expect("completes"),
                    "A delegated agent cannot delegate; report back to your parent instead"
                );
            } else {
                let mut expected = PARENT_TOOLS.to_vec();
                expected.sort();
                assert_eq!(names, expected);
                assert_eq!(
                    chat.call("message_parent", json!({"message": "Hi"}))
                        .await
                        .expect("completes"),
                    "This conversation was not delegated, so it has no parent to message."
                );
                // A parent may leave the conversation out to read its own.
                assert_eq!(
                    chat.call(
                        "get_session_toolcall_output",
                        json!({"tool_call_id": "call-3"})
                    )
                    .await
                    .expect("completes"),
                    "None call-3 None"
                );
                assert_eq!(
                    chat.call(
                        "message_child",
                        json!({"child": "review-bot", "message": "Which check?"})
                    )
                    .await
                    .expect("completes"),
                    "to review-bot: Which check?"
                );
                assert_eq!(
                    chat.call(
                        "get_session_toolcall_output",
                        json!({"conversation": "child-1", "tool_call_id": "call-7", "offset": 16000})
                    )
                    .await
                    .expect("completes"),
                    "Some(\"child-1\") call-7 Some(16000)"
                );
                assert_eq!(
                    chat.call(
                        "delegate",
                        json!({"agent": "review-bot", "prompt": "Review it"})
                    )
                    .await
                    .expect("completes"),
                    "review-bot from conversation-session-1: Review it"
                );
                // Without a named agent, an installed agent or the
                // caller's own runs the task; a named agent takes no engine.
                assert_eq!(
                    chat.call(
                        "delegate",
                        json!({"engine": "codex", "prompt": "Review it"})
                    )
                    .await
                    .expect("completes"),
                    "codex from conversation-session-1: Review it"
                );
                assert_eq!(
                    chat.call("delegate", json!({"prompt": "Review it"}))
                        .await
                        .expect("completes"),
                    "own from conversation-session-1: Review it"
                );
                assert!(chat
                    .call(
                        "delegate",
                        json!({"agent": "review-bot", "mode": "plan", "prompt": "Review it"})
                    )
                    .await
                    .expect("completes")
                    .starts_with("A named agent has its own engine, model and mode"));
            }
            drop(chat);
            harness.finish().await;
        }
    }

    fn harness_definition(harness: &str, home: HomeLayout) -> AgentDefinition {
        let mut definition = AgentDefinition::for_test(harness);
        definition.instance = Some(AgentInstance {
            id: AgentId::new(harness),
            harness_id: HarnessId(harness.into()),
            label: harness.into(),
            home,
            claimed: true,
        });
        definition
    }

    #[tokio::test]
    async fn sessions_get_their_workspaces_servers_less_their_existing_setups_own() {
        let harness = Harness::new().await;
        let workspace = harness
            .database
            .run("test.workspace", |connection| {
                WorkspaceRepository::create(connection, "Alpha", chrono::Utc::now())
            })
            .await
            .expect("workspace");
        let alpha = workspace.id.to_string();
        harness.add(local("docs", &[])).await;
        let off = harness.add(local("off", &[])).await;
        harness.add(local("notes", &[])).await;
        harness.add(local("search", &["--native"])).await;
        harness
            .service
            .set_workspace_enabled(&off, &WorkspaceID(alpha.clone()), false)
            .await
            .expect("turns off");

        // Codex's own config has `notes` by name and `search` by command.
        let codex_home = TempDir::new().expect("codex home");
        std::fs::write(
            codex_home.path().join("config.toml"),
            format!(
                "[mcp_servers.notes]\nurl = 'https://notes.example.com'\n\
                 [mcp_servers.native_search]\ncommand = '{}'\nargs = ['--native']\n",
                fixture_binary().display()
            ),
        )
        .expect("writes config");
        let existing = HomeLayout::Existing {
            path: codex_home.path().to_owned(),
            environment: Default::default(),
        };
        let codex = SessionsAdapter::with_definition(harness_definition("codex", existing.clone()));
        let managed_codex =
            SessionsAdapter::with_definition(harness_definition("codex", HomeLayout::Managed));
        // Oh-My-Pi reads no MCP config of its own under ACP.
        let omp = SessionsAdapter::with_definition(harness_definition("omp", existing));

        let names = |entries: Option<(McpGrant, Vec<String>, String)>| {
            entries.map(|(_, names, _)| names).unwrap_or_default()
        };
        assert_eq!(names(harness.entries(&codex, &alpha, "c1").await), ["docs"]);
        assert_eq!(
            names(harness.entries(&managed_codex, &alpha, "c2").await),
            ["docs", "notes", "search"]
        );
        assert_eq!(
            names(harness.entries(&omp, &alpha, "c3").await),
            ["docs", "notes", "search"]
        );
        assert_eq!(
            names(harness.entries(&managed_codex, "beta", "c4").await),
            ["docs", "off", "notes", "search"],
            "off only where it was turned off"
        );

        // A token reaches only the paths it was given.
        let (_grant, _, authorization) = harness
            .entries(&codex, &alpha, "c5")
            .await
            .expect("entries");
        let initialize = |name: &str| {
            let url = harness.proxy.sessions().server_url(name);
            let authorization = authorization.clone();
            async move {
                super::post(
                    &url,
                    Some(&authorization),
                    None,
                    &[],
                    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                        "protocolVersion":"2025-06-18","capabilities":{},
                        "clientInfo":{"name":"test","version":"1"}}}),
                )
                .await
                .0
            }
        };
        assert_eq!(initialize("docs").await, 200);
        assert_eq!(initialize("off").await, 404, "turned off for the workspace");
        assert_eq!(initialize("notes").await, 404, "the setup's own");
        harness.finish().await;
    }

    #[tokio::test]
    async fn opencode_sessions_in_one_directory_share_a_token() {
        let harness = Harness::new().await;
        harness.add(local("docs", &[])).await;
        let mut definition = harness_definition("opencode", HomeLayout::Managed);
        definition.mcp_connections = McpConnections::PerDirectory;
        let opencode = SessionsAdapter::with_definition(definition);
        let (_first, _, token) = harness
            .entries(&opencode, "alpha", "c1")
            .await
            .expect("entries");
        let (_second, _, shared) = harness
            .entries(&opencode, "alpha", "c2")
            .await
            .expect("entries");
        let (_other, _, other) = harness
            .entries(&opencode, "beta", "c3")
            .await
            .expect("entries");
        assert_eq!(token, shared);
        assert_ne!(token, other);

        let codex =
            SessionsAdapter::with_definition(harness_definition("codex", HomeLayout::Managed));
        let (_first, _, token) = harness
            .entries(&codex, "alpha", "c4")
            .await
            .expect("entries");
        let (_second, _, own) = harness
            .entries(&codex, "alpha", "c5")
            .await
            .expect("entries");
        assert_ne!(token, own, "other agents get one token per session");
        harness.finish().await;
    }

    #[tokio::test]
    async fn two_chats_on_one_path_call_concurrently_without_crossed_replies() {
        let harness = Harness::new().await;
        let server = harness.add(local("fixture", &[])).await;
        let workspace = harness.workspace("alpha");
        let _lease = harness
            .service
            .runtime()
            .acquire(&server, &workspace)
            .await
            .unwrap();
        let (first, second) = (
            harness.chat("fixture", "alpha", "s1").await,
            harness.chat("fixture", "alpha", "s2").await,
        );
        let tools = first.client.peer().list_all_tools().await.expect("lists");
        assert!(tools.iter().any(|tool| tool.name == "echo"));

        let calls = (0..20).map(|index| {
            let chat = if index % 2 == 0 { &first } else { &second };
            async move {
                let text = format!("call-{index}");
                let reply = chat
                    .call("echo", json!({"text": text}))
                    .await
                    .expect("echoes");
                (text, reply)
            }
        });
        for (sent, received) in futures::future::join_all(calls).await {
            assert_eq!(sent, received);
        }
        assert_eq!(
            first.call("pid", json!({})).await,
            second.call("pid", json!({})).await,
            "one process serves the workspace"
        );
        harness.finish().await;
    }

    #[tokio::test]
    async fn a_question_reaches_only_the_chat_whose_call_asked_it() {
        let harness = Harness::new().await;
        let server = harness.add(local("fixture", &[])).await;
        let workspace = harness.workspace("alpha");
        let _lease = harness
            .service
            .runtime()
            .acquire(&server, &workspace)
            .await
            .unwrap();
        let (first, second) = (
            harness.chat("fixture", "alpha", "s1").await,
            harness.chat("fixture", "alpha", "s2").await,
        );

        let reply = first
            .call_with_meta(
                "ask",
                json!({}),
                Some(json!({"claudecode/toolUseId": "tool-1"})),
            )
            .await
            .expect("asks");
        assert!(reply.contains("blue"), "{reply}");
        assert_eq!(
            *harness.broker.asked.lock().unwrap(),
            [(
                "s1".to_owned(),
                Some("tool-1".to_owned()),
                "Pick a color".to_owned()
            )]
        );

        // With another chat's call in flight, a request that names no call
        // could be either chat's, so it is declined and nobody is asked.
        let (slow, asked) = tokio::join!(first.call("slow", json!({"millis": 1500})), async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            second.call("ask", json!({})).await
        });
        assert_eq!(slow.as_deref(), Ok("done"));
        let asked = asked.expect("asks");
        assert!(asked.contains("decline"), "{asked}");
        assert_eq!(harness.broker.asked.lock().unwrap().len(), 1);
        harness.finish().await;
    }

    #[tokio::test]
    async fn cancelling_a_call_cancels_it_upstream() {
        let harness = Harness::new().await;
        let server = harness.add(local("fixture", &[])).await;
        let workspace = harness.workspace("alpha");
        let _lease = harness
            .service
            .runtime()
            .acquire(&server, &workspace)
            .await
            .unwrap();
        let chat = harness.chat("fixture", "alpha", "s1").await;

        let serde_json::Value::Object(arguments) = json!({"millis": 30_000}) else {
            unreachable!()
        };
        let handle = chat
            .client
            .peer()
            .send_cancellable_request(
                ClientRequest::CallToolRequest(CallToolRequest::new(
                    CallToolRequestParams::new("slow").with_arguments(arguments),
                )),
                PeerRequestOptions::no_options(),
            )
            .await
            .expect("sends");
        tokio::time::sleep(Duration::from_millis(300)).await;
        handle.cancel(None).await.expect("cancels");
        let mut cancelled = String::new();
        for _ in 0..100 {
            cancelled = chat.call("cancelled", json!({})).await.expect("counts");
            if cancelled == "1" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(cancelled, "1", "the server saw the cancellation");
        harness.finish().await;
    }

    #[tokio::test]
    async fn a_restarted_server_tells_its_workspace_its_tools_changed() {
        let harness = Harness::new().await;
        let server = harness.add(local("fixture", &[])).await;
        let (alpha, beta) = (harness.workspace("alpha"), harness.workspace("beta"));
        let _alpha = harness
            .service
            .runtime()
            .acquire(&server, &alpha)
            .await
            .unwrap();
        let _beta = harness
            .service
            .runtime()
            .acquire(&server, &beta)
            .await
            .unwrap();
        let here = harness.chat("fixture", "alpha", "s1").await;
        let there = harness.chat("fixture", "beta", "s2").await;
        here.call("pid", json!({})).await.expect("starts alpha");
        there.call("pid", json!({})).await.expect("starts beta");
        let (here_before, there_before) = (
            here.agent.list_changed.load(Ordering::SeqCst),
            there.agent.list_changed.load(Ordering::SeqCst),
        );
        assert_eq!(
            (here_before, there_before),
            (0, 0),
            "a first start lists its tools anyway"
        );

        let crashed = here.call("crash", json!({})).await.expect_err("crashes");
        assert!(crashed.contains("could not be reached"), "{crashed}");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        here.call("pid", json!({}))
            .await
            .expect("restarts after its backoff");
        for _ in 0..100 {
            if here.agent.list_changed.load(Ordering::SeqCst) > here_before {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(here.agent.list_changed.load(Ordering::SeqCst) > here_before);
        assert_eq!(
            there.agent.list_changed.load(Ordering::SeqCst),
            there_before,
            "another workspace's process did not restart"
        );
        harness.finish().await;
    }

    #[tokio::test]
    async fn a_2026_session_hears_tool_changes_on_its_subscription() {
        let harness = Harness::new().await;
        let server = harness.add(local("fixture", &[])).await;
        let alpha = harness.workspace("alpha");
        let _lease = harness
            .service
            .runtime()
            .acquire(&server, &alpha)
            .await
            .unwrap();
        let chat = harness
            .chat_with(
                "fixture",
                "alpha",
                "s1",
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                },
            )
            .await;
        let mut subscription = chat
            .client
            .peer()
            .listen(SubscriptionFilter::builder().tools_list_changed().build())
            .await
            .expect("the proxy accepts the subscription");
        assert_eq!(subscription.acknowledged().tools_list_changed, Some(true));
        // A `2026-07-28` client rejects a result without `resultType`, which
        // an older upstream never sends.
        let listed = chat.client.peer().list_tools(None).await.expect("lists");
        assert_eq!(listed.result_type, Some(ResultType::COMPLETE));
        let called = chat
            .client
            .peer()
            .call_tool_once(CallToolRequestParams::new("pid"))
            .await
            .expect("starts");
        let CallToolResponse::Complete(called) = called else {
            panic!("a complete result");
        };
        assert_eq!(called.result_type, Some(ResultType::COMPLETE));

        chat.call("crash", json!({})).await.expect_err("crashes");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        chat.call("pid", json!({}))
            .await
            .expect("restarts after its backoff");
        let heard = tokio::time::timeout(Duration::from_secs(5), subscription.next())
            .await
            .expect("hears in time")
            .expect("the subscription stays open");
        assert!(
            matches!(
                heard,
                Some(ServerNotification::ToolListChangedNotification(_))
            ),
            "{heard:?}"
        );
        subscription.cancel().await.expect("cancels");
        harness.finish().await;
    }

    #[tokio::test]
    async fn upstream_failures_reach_the_agent_as_mcp_errors() {
        let harness = Harness::new().await;
        let server = harness.add(local("refuses", &["--fail-start"])).await;
        let workspace = harness.workspace("alpha");
        let _lease = harness
            .service
            .runtime()
            .acquire(&server, &workspace)
            .await
            .unwrap();
        let chat = harness.chat("refuses", "alpha", "s1").await;
        let error = chat
            .client
            .peer()
            .list_tools(None)
            .await
            .expect_err("the server never started");
        let rmcp::ServiceError::McpError(error) = error else {
            panic!("an MCP error, not a transport one: {error}");
        };
        assert!(
            error.message.contains("fixture refused to start"),
            "{}",
            error.message
        );
        harness.finish().await;
    }

    #[tokio::test]
    async fn paths_follow_the_configured_servers() {
        let harness = Harness::new().await;
        let routes = harness.proxy.sessions().routes().clone();
        let server = harness.add(local("first", &[])).await;
        assert_eq!(routes.names(), ["first"]);
        harness
            .service
            .update(&server, local("renamed", &[]))
            .await
            .expect("renames");
        assert_eq!(routes.names(), ["renamed"]);
        harness.service.remove(&server).await.expect("removes");
        assert!(routes.names().is_empty());
        harness.finish().await;
    }

    /// Remote servers behind the fixture's OAuth server: sign-in, its port,
    /// refresh, and losing it.
    mod signing_in {
        use super::{fixture_binary, Harness, RecordingBroker};
        use crate::mcp::secrets::MemorySecretStore;
        use crate::mcp::{McpNotice, McpProxy, McpRuntime, McpServers, McpService, McpTokens};
        use ottam_protocol::{
            McpServerConfig, McpServerID, McpServerStatus, McpServerTransport, McpSignInOutcome,
        };
        use serde_json::json;
        use std::sync::{Arc, Mutex};
        use std::time::Duration;
        use tokio::io::AsyncBufReadExt;

        /// The fixture's OAuth mode with `flags`, and its `/mcp` URL.
        async fn oauth_server(flags: &[&str]) -> (tokio::process::Child, String) {
            let mut child = tokio::process::Command::new(fixture_binary())
                .args(["--http", "--oauth"])
                .args(flags)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("starts the OAuth fixture");
            let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
            let url = lines.next_line().await.unwrap().expect("prints its URL");
            (child, url)
        }

        fn remote(url: &str) -> McpServerConfig {
            McpServerConfig {
                name: "remote".into(),
                transport: McpServerTransport::Http {
                    url: url.to_owned(),
                    headers: Vec::new(),
                },
            }
        }

        /// What a browser does with the authorization URL when the server
        /// approves at once: follow it back to core, and read the page.
        async fn browse(url: &str) -> String {
            reqwest::get(url)
                .await
                .expect("reaches the sign-in")
                .text()
                .await
                .expect("reads the page")
        }

        async fn stats(url: &str) -> serde_json::Value {
            let base = url.trim_end_matches("/mcp");
            reqwest::get(format!("{base}/admin/stats"))
                .await
                .expect("reads stats")
                .json()
                .await
                .expect("stats are JSON")
        }

        fn port_of(redirect_uri: &str) -> u16 {
            reqwest::Url::parse(redirect_uri)
                .expect("a URL")
                .port()
                .expect("a port")
        }

        fn finished(
            server: &McpServerID,
        ) -> impl Fn(&McpNotice) -> Option<(McpSignInOutcome, Option<String>)> + '_ {
            move |notice| match notice {
                McpNotice::SignInFinished(params) if &params.server_id == server => {
                    Some((params.outcome, params.error.clone()))
                }
                _ => None,
            }
        }

        async fn summary(
            harness: &Harness,
            server: &McpServerID,
        ) -> ottam_protocol::McpServerSummary {
            harness
                .service
                .list()
                .await
                .expect("lists")
                .into_iter()
                .find(|summary| &summary.server_id == server)
                .expect("listed")
        }

        #[tokio::test]
        async fn a_remote_server_signs_in_and_its_chat_calls_it() {
            let (_fixture, url) = oauth_server(&["--auto-approve"]).await;
            let harness = Harness::new().await;
            let added = harness.service.add(remote(&url)).await.expect("adds");
            assert_eq!(added.state.status, McpServerStatus::SignedOut);
            assert!(!added.signed_in);
            let server = added.server_id;

            let chat = harness.chat("remote", "alpha", "s1").await;
            let refused = chat
                .call("echo", json!({"text": "hi"}))
                .await
                .expect_err("needs a sign-in");
            assert!(refused.contains("sign in"), "{refused}");
            let (name, conversation) = harness
                .notice(|notice| match notice {
                    McpNotice::SignInNeeded(params) => {
                        Some((params.server_name.clone(), params.conversation_id.0.clone()))
                    }
                    _ => None,
                })
                .await;
            assert_eq!(
                (name.as_str(), conversation.as_str()),
                ("remote", "conversation-s1")
            );

            let started = harness
                .service
                .sign_in(&server, None)
                .await
                .expect("starts signing in");
            assert!(started.redirect_uri.ends_with("/oauth/callback"));
            let page = browse(&started.authorization_url).await;
            assert!(page.contains("Signed in"), "{page}");
            let (outcome, error) = harness.notice(finished(&server)).await;
            assert_eq!(outcome, McpSignInOutcome::SignedIn, "{error:?}");
            std::net::TcpListener::bind(("127.0.0.1", port_of(&started.redirect_uri)))
                .expect("the redirect port is released");
            assert!(summary(&harness, &server).await.signed_in);
            assert_ne!(
                summary(&harness, &server).await.state.status,
                McpServerStatus::SignedOut
            );

            assert_eq!(
                chat.call("echo", json!({"text": "hi"}))
                    .await
                    .expect("calls"),
                "hi",
                "the same chat's next call succeeds"
            );
            assert_eq!(stats(&url).await["signIns"], 1);

            let signed_out = harness.service.sign_out(&server).await.expect("signs out");
            assert_eq!(signed_out.state.status, McpServerStatus::SignedOut);
            assert!(!signed_out.signed_in);
            let refused = chat
                .call("echo", json!({"text": "hi"}))
                .await
                .expect_err("signed out");
            assert!(refused.contains("sign in"), "{refused}");
            harness.finish().await;
        }

        #[tokio::test]
        async fn a_waiting_sign_in_holds_its_port_until_cancelled() {
            let (_fixture, url) = oauth_server(&[]).await;
            let harness = Harness::new().await;
            let server = harness.add(remote(&url)).await;
            let started = harness
                .service
                .sign_in(&server, None)
                .await
                .expect("starts signing in");
            let page = browse(&started.authorization_url).await;
            assert!(
                page.contains("Approve"),
                "the browser waits on the server's page"
            );
            let port = port_of(&started.redirect_uri);
            assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_err());
            // Another page's request, without the sign-in's state, is no
            // reply: the sign-in keeps waiting.
            for forged in ["?code=forged&state=forged", "?error=access_denied", ""] {
                let page = browse(&format!("{}{forged}", started.redirect_uri)).await;
                assert!(page.contains("not a sign-in reply"), "{page}");
            }
            assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_err());

            harness.service.cancel_sign_in(&server).await;
            let (outcome, _) = harness.notice(finished(&server)).await;
            assert_eq!(outcome, McpSignInOutcome::Cancelled);
            std::net::TcpListener::bind(("127.0.0.1", port)).expect("the port is released");
            assert_eq!(
                summary(&harness, &server).await.state.status,
                McpServerStatus::SignedOut
            );
            harness.finish().await;
        }

        #[tokio::test]
        async fn a_held_redirect_port_fails_the_sign_in_saying_so() {
            let (_fixture, url) = oauth_server(&["--auto-approve"]).await;
            let held = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("holds a port");
            let port = held.local_addr().unwrap().port();
            let harness = Harness::with_redirect_port(port).await;
            let server = harness.add(remote(&url)).await;
            let error = harness
                .service
                .sign_in(&server, None)
                .await
                .expect_err("the port is taken");
            assert_eq!(error.kind, "mcpRedirectPortInUse");
            assert!(
                error.message.contains(&port.to_string()),
                "{}",
                error.message
            );
            drop(held);
            harness.finish().await;
        }

        #[tokio::test]
        async fn a_server_never_signed_in_reads_as_signed_out_after_a_restart() {
            let (_fixture, url) = oauth_server(&["--auto-approve"]).await;
            let harness = Harness::new().await;
            let server = harness.add(remote(&url)).await;

            // The next launch: the same database, a new service.
            let servers = McpServers::new(
                harness.database.clone(),
                Arc::new(MemorySecretStore::default()),
            );
            let notices = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&notices);
            let runtime = McpRuntime::new(
                servers.clone(),
                Arc::new(move |notice| recorded.lock().unwrap().push(notice)),
            );
            let relaunched = McpService::new(servers, runtime, 0);
            let proxy = McpProxy::start(McpTokens::default())
                .await
                .expect("listens");
            relaunched
                .attach_proxy(proxy.sessions(), Arc::new(RecordingBroker::default()))
                .await
                .expect("attaches");
            let mut status = None;
            for _ in 0..400 {
                status = relaunched
                    .list()
                    .await
                    .expect("lists")
                    .into_iter()
                    .find(|summary| summary.server_id == server)
                    .map(|summary| summary.state.status);
                if status == Some(McpServerStatus::SignedOut) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            assert_eq!(status, Some(McpServerStatus::SignedOut));
            relaunched.shutdown().await;
            proxy.finish().await;
            harness.finish().await;
        }

        #[tokio::test]
        async fn a_server_without_registration_signs_in_with_a_client_id() {
            let (_fixture, url) = oauth_server(&["--auto-approve", "--no-registration"]).await;
            let harness = Harness::new().await;
            let server = harness.add(remote(&url)).await;
            let error = harness
                .service
                .sign_in(&server, None)
                .await
                .expect_err("needs a client ID");
            assert_eq!(error.kind, "mcpClientIDRequired");
            assert!(
                error.message.contains("/oauth/callback"),
                "{}",
                error.message
            );

            let started = harness
                .service
                .sign_in(&server, Some("ottam-test-client".into()))
                .await
                .expect("starts with the client ID");
            browse(&started.authorization_url).await;
            let (outcome, error) = harness.notice(finished(&server)).await;
            assert_eq!(outcome, McpSignInOutcome::SignedIn, "{error:?}");
            harness.finish().await;
        }

        #[tokio::test]
        async fn tokens_refresh_and_a_revoked_sign_in_reads_as_signed_out() {
            let (_fixture, url) = oauth_server(&["--auto-approve", "--token-ttl", "31"]).await;
            let harness = Harness::new().await;
            let server = harness.add(remote(&url)).await;
            let started = harness
                .service
                .sign_in(&server, None)
                .await
                .expect("starts signing in");
            browse(&started.authorization_url).await;
            let (outcome, error) = harness.notice(finished(&server)).await;
            assert_eq!(outcome, McpSignInOutcome::SignedIn, "{error:?}");
            let chat = harness.chat("remote", "alpha", "s1").await;
            assert_eq!(
                chat.call("echo", json!({"text": "one"})).await.unwrap(),
                "one"
            );

            // Within 30 s of expiry, the next request refreshes first.
            tokio::time::sleep(Duration::from_millis(2100)).await;
            assert_eq!(
                chat.call("echo", json!({"text": "two"})).await.unwrap(),
                "two"
            );
            assert!(stats(&url).await["refreshes"].as_u64().unwrap() >= 1);

            let base = url.trim_end_matches("/mcp");
            reqwest::Client::new()
                .post(format!("{base}/admin/revoke"))
                .send()
                .await
                .expect("revokes");
            let refused = chat
                .call("echo", json!({"text": "three"}))
                .await
                .expect_err("revoked");
            assert!(refused.contains("sign in"), "{refused}");
            harness
                .notice(|notice| matches!(notice, McpNotice::SignInNeeded(_)).then_some(()))
                .await;
            let after = summary(&harness, &server).await;
            assert_eq!(after.state.status, McpServerStatus::SignedOut);
            assert!(!after.signed_in, "rejected tokens are dropped");
            harness.finish().await;
        }

        fn with_token(url: &str, token: &str) -> McpServerConfig {
            McpServerConfig {
                name: "remote".into(),
                transport: McpServerTransport::Http {
                    url: url.to_owned(),
                    headers: vec![ottam_protocol::McpConfigValue {
                        name: "Authorization".into(),
                        value: Some(format!("Bearer {token}")),
                        secret: true,
                    }],
                },
            }
        }

        async fn admin(url: &str, action: &str) {
            let base = url.trim_end_matches("/mcp");
            reqwest::Client::new()
                .post(format!("{base}/admin/{action}"))
                .send()
                .await
                .expect("reaches the admin route");
        }

        #[tokio::test]
        async fn a_signed_out_server_given_a_working_header_is_tried_again() {
            let (_fixture, url) = oauth_server(&["--static-token", "pasted-token"]).await;
            let harness = Harness::new().await;
            let server = harness.add(remote(&url)).await;
            let chat = harness.chat("remote", "alpha", "s1").await;
            let refused = chat
                .call("echo", json!({"text": "hi"}))
                .await
                .expect_err("needs a sign-in");
            assert!(refused.contains("sign in"), "{refused}");

            // The user pastes a token as a header instead, at the same URL.
            let updated = harness
                .service
                .update(&server, with_token(&url, "pasted-token"))
                .await
                .expect("updates");
            assert_ne!(updated.state.status, McpServerStatus::SignedOut);
            assert_eq!(
                chat.call("echo", json!({"text": "hi"}))
                    .await
                    .expect("calls with the header"),
                "hi"
            );
            harness.finish().await;
        }

        #[tokio::test]
        async fn a_sign_in_page_that_is_not_a_web_page_is_never_opened() {
            let (_fixture, url) = oauth_server(&[
                "--authorization-endpoint",
                "file:///Applications/Calculator.app",
            ])
            .await;
            let harness = Harness::new().await;
            let server = harness.add(remote(&url)).await;
            let error = harness
                .service
                .sign_in(&server, None)
                .await
                .expect_err("not a web page");
            assert_eq!(error.kind, "mcpSignInUnavailable");
            assert!(error.message.contains("web page"), "{}", error.message);
            harness.finish().await;
        }

        #[tokio::test]
        async fn cancelling_a_start_stuck_on_its_server_returns_at_once() {
            // A server that accepts connections and never answers.
            let stuck = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .expect("binds");
            let url = format!("http://{}/mcp", stuck.local_addr().unwrap());
            let held = tokio::spawn(async move {
                let mut sockets = Vec::new();
                while let Ok((socket, _)) = stuck.accept().await {
                    sockets.push(socket);
                }
            });
            let harness = Harness::new().await;
            let adding = std::time::Instant::now();
            let server = harness.add(remote(&url)).await;
            assert!(
                adding.elapsed() < Duration::from_secs(5),
                "adding waits only briefly for the server: {:?}",
                adding.elapsed()
            );

            let service = harness.service.clone();
            let starting = {
                let server = server.clone();
                tokio::spawn(async move { service.sign_in(&server, None).await })
            };
            tokio::time::sleep(Duration::from_millis(300)).await;
            tokio::time::timeout(
                Duration::from_secs(2),
                harness.service.cancel_sign_in(&server),
            )
            .await
            .expect("cancelling never waits on the server");
            let error = tokio::time::timeout(Duration::from_secs(2), starting)
                .await
                .expect("the start stops")
                .expect("joins")
                .expect_err("cancelled");
            assert_eq!(error.kind, "mcpSignInCancelled");
            let (outcome, _) = harness.notice(finished(&server)).await;
            assert_eq!(outcome, McpSignInOutcome::Cancelled);
            held.abort();
            harness.finish().await;
        }

        #[tokio::test]
        async fn a_saved_client_id_the_server_forgot_is_registered_again() {
            let (_fixture, url) = oauth_server(&["--auto-approve"]).await;
            let harness = Harness::new().await;
            let server = harness.add(remote(&url)).await;
            let started = harness.service.sign_in(&server, None).await.unwrap();
            browse(&started.authorization_url).await;
            let (outcome, error) = harness.notice(finished(&server)).await;
            assert_eq!(outcome, McpSignInOutcome::SignedIn, "{error:?}");

            // Refresh is refused, which keeps the client ID; then the server
            // forgets that registration.
            admin(&url, "revoke").await;
            let chat = harness.chat("remote", "alpha", "s1").await;
            chat.call("echo", json!({"text": "hi"}))
                .await
                .expect_err("revoked");
            admin(&url, "forget-clients").await;
            harness.notices.lock().unwrap().clear();

            let stale = harness.service.sign_in(&server, None).await.unwrap();
            let page = browse(&stale.authorization_url).await;
            assert!(page.contains("unknown client"), "{page}");
            harness.service.cancel_sign_in(&server).await;
            harness.notice(finished(&server)).await;
            harness.notices.lock().unwrap().clear();

            let fresh = harness.service.sign_in(&server, None).await.unwrap();
            browse(&fresh.authorization_url).await;
            let (outcome, error) = harness.notice(finished(&server)).await;
            assert_eq!(outcome, McpSignInOutcome::SignedIn, "{error:?}");
            assert_eq!(stats(&url).await["signIns"], 2);
            harness.finish().await;
        }

        #[tokio::test]
        async fn a_refusal_from_before_a_sign_in_leaves_it_signed_in() {
            let harness = Harness::new().await;
            let runtime = harness.service.runtime();
            let server = McpServerID("mcp_remote".into());
            let before = runtime.sign_in_epoch(&server);
            runtime.signed_in(&server).await;
            assert!(!runtime.mark_signed_out(&server, Some(before)).await);
            assert_ne!(runtime.state(&server).status, McpServerStatus::SignedOut);
            let current = runtime.sign_in_epoch(&server);
            assert!(runtime.mark_signed_out(&server, Some(current)).await);
            assert_eq!(runtime.state(&server).status, McpServerStatus::SignedOut);
            harness.finish().await;
        }

        /// Sign in to `server`, which approves at once.
        async fn sign_in(harness: &Harness, server: &McpServerID) {
            harness.notices.lock().unwrap().clear();
            let started = harness.service.sign_in(server, None).await.unwrap();
            browse(&started.authorization_url).await;
            let (outcome, error) = harness.notice(finished(server)).await;
            assert_eq!(outcome, McpSignInOutcome::SignedIn, "{error:?}");
        }

        /// A server named `name` at `url`, signed in.
        async fn signed_in(harness: &Harness, url: &str, name: &str) -> McpServerID {
            let server = harness
                .add(McpServerConfig {
                    name: name.into(),
                    ..remote(url)
                })
                .await;
            sign_in(harness, &server).await;
            server
        }

        #[tokio::test]
        async fn a_sign_in_to_one_server_leaves_another_able_to_sign_out() {
            let (_first, first_url) = oauth_server(&["--auto-approve"]).await;
            let (_second, second_url) = oauth_server(&["--auto-approve"]).await;
            let harness = Harness::new().await;
            let first = signed_in(&harness, &first_url, "first").await;
            let second = signed_in(&harness, &second_url, "second").await;
            let chat = harness.chat("second", "alpha", "s1").await;
            assert_eq!(
                chat.call("echo", json!({"text": "hi"})).await.unwrap(),
                "hi"
            );

            // The other server signs in again while this connection is live.
            let before = harness.service.runtime().sign_in_epoch(&second);
            sign_in(&harness, &first).await;
            assert_eq!(harness.service.runtime().sign_in_epoch(&second), before);

            admin(&second_url, "revoke").await;
            let refused = chat
                .call("echo", json!({"text": "hi"}))
                .await
                .expect_err("revoked");
            assert!(refused.contains("sign in"), "{refused}");
            assert_eq!(
                summary(&harness, &second).await.state.status,
                McpServerStatus::SignedOut
            );
            harness.finish().await;
        }

        #[tokio::test]
        async fn a_connection_from_before_a_change_starts_again() {
            let (_fixture, url) = oauth_server(&["--auto-approve"]).await;
            let harness = Harness::new().await;
            let server = signed_in(&harness, &url, "remote").await;
            let chat = harness.chat("remote", "alpha", "s1").await;
            assert_eq!(
                chat.call("echo", json!({"text": "hi"})).await.unwrap(),
                "hi"
            );

            // A change that leaves the live connection running.
            harness.service.runtime().forget_sign_in(&server);
            admin(&url, "revoke").await;
            let changed = chat
                .call("echo", json!({"text": "hi"}))
                .await
                .expect_err("revoked");
            assert!(
                changed.contains(crate::mcp::runtime::SIGN_IN_CHANGED),
                "{changed}"
            );
            let refused = chat
                .call("echo", json!({"text": "hi"}))
                .await
                .expect_err("revoked");
            assert!(refused.contains("sign in"), "{refused}");
            assert_eq!(
                summary(&harness, &server).await.state.status,
                McpServerStatus::SignedOut
            );
            harness.finish().await;
        }

        /// A secret store that counts its reads.
        #[derive(Default)]
        struct CountingStore {
            inner: MemorySecretStore,
            reads: std::sync::atomic::AtomicUsize,
        }

        impl crate::mcp::secrets::SecretStore for CountingStore {
            fn get(&self, account: &str) -> Result<Option<String>, crate::core_error::CoreError> {
                self.reads
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.inner.get(account)
            }
            fn set(&self, account: &str, value: &str) -> Result<(), crate::core_error::CoreError> {
                self.inner.set(account, value)
            }
            fn delete(&self, account: &str) -> Result<(), crate::core_error::CoreError> {
                self.inner.delete(account)
            }
        }

        #[tokio::test]
        async fn credentials_are_read_from_the_store_once() {
            use crate::mcp::oauth::{OAuthCache, StoredOAuth};
            let store = Arc::new(CountingStore::default());
            let cache = OAuthCache::default();
            let server = McpServerID("mcp_remote".into());
            let oauth = StoredOAuth::new(store.clone(), cache.clone(), &server);
            for _ in 0..3 {
                assert!(!oauth.signed_in().await);
            }
            // Another handle on the same server shares what was read.
            let again = StoredOAuth::new(store.clone(), cache, &server);
            assert!(!again.signed_in().await);
            again.forget().await.expect("forgets");
            assert!(!oauth.signed_in().await);
            assert_eq!(store.reads.load(std::sync::atomic::Ordering::Relaxed), 1);
        }
    }
}
