use crate::storage::models::{ProjectSource, WorkspaceId};
use crate::storage::repositories::ProjectRepository;
use crate::storage::Database;
use crate::terminal::{
    terminal_output_queue_max_bytes, TerminalEventSender, TerminalManager, TerminalOutputQueue,
};
use ottam_protocol::{
    CoreInstanceID, ProjectID, ProjectSourceID, ProtocolNotification, ProtocolRequest, RequestID,
    METHOD_CONVERSATION_CANCEL_RUN, METHOD_CONVERSATION_CREATE,
    METHOD_CONVERSATION_CREATE_ATTACHMENT, METHOD_CONVERSATION_DELETE,
    METHOD_CONVERSATION_DISCARD_ATTACHMENT, METHOD_CONVERSATION_GET, METHOD_CONVERSATION_LIST,
    METHOD_CONVERSATION_LIST_AGENT_SESSIONS, METHOD_CONVERSATION_LIST_EVENTS,
    METHOD_CONVERSATION_LIST_MESSAGES, METHOD_CONVERSATION_LIST_RUNS,
    METHOD_CONVERSATION_LIST_RUN_INPUTS, METHOD_CONVERSATION_POST_MESSAGE,
    METHOD_CONVERSATION_RESOLVE_RUN_INPUT_DELIVERY,
    METHOD_CONVERSATION_SET_ALWAYS_ALLOW_TOOL_ACTIONS, METHOD_CONVERSATION_SET_MODE,
    METHOD_CONVERSATION_START_RUN, METHOD_CONVERSATION_STEER_RUN,
    METHOD_CONVERSATION_STOP_BACKGROUND_TASK, METHOD_ENGINE_LIST, METHOD_GIT_DIFF_SNAPSHOT,
    METHOD_INITIALIZE, METHOD_INITIALIZED, METHOD_PROJECT_OPEN_PATH, METHOD_RUNTIME_INSTALL,
    METHOD_RUNTIME_INSTALL_CANCEL, METHOD_TERMINAL_CLOSE, METHOD_TERMINAL_DELETE,
    METHOD_TERMINAL_LIST, METHOD_TERMINAL_OPEN, METHOD_TERMINAL_REPLAY, METHOD_TERMINAL_RESIZE,
    METHOD_TERMINAL_UPDATE_METADATA, METHOD_TERMINAL_WRITE, METHOD_TOOL_PERMISSIONS_GET,
    METHOD_TOOL_PERMISSIONS_SET, METHOD_WORKSPACE_ADD_PROJECT, METHOD_WORKSPACE_ARCHIVE,
    METHOD_WORKSPACE_CREATE, METHOD_WORKSPACE_GET, METHOD_WORKSPACE_LIST, METHOD_WORKSPACE_OPEN,
    METHOD_WORKSPACE_REMOVE_PROJECT, METHOD_WORKSPACE_RESTORE, METHOD_WORKSPACE_UPDATE,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc::{channel, unbounded_channel};

mod agents;
mod attach;
mod auth_terminal;
mod chat;
mod client_link;
mod deferred;
mod engines;
pub(crate) use engines::engine_display_name;
mod handlers;
mod mcp;
mod named_agents;
mod outbound;
mod request_params;
mod responses;
mod storage_cache;
mod transport;
mod workspaces;

use agents::AgentRuntimeBridge;
use client_link::ClientLink;
use deferred::DeferredResponses;
use outbound::{AttachedRegistry, ConnectionId, OutboundFrame};
use responses::{
    error_response, request_id_from_invalid_envelope, request_id_kind, response_status,
    success_response, Tap,
};
use transport::{reader_task, writer_task, InboundMessage, PeerClosableOutput};

#[cfg(test)]
pub(crate) use outbound::test_sink;
pub(crate) use outbound::OutboundSink;

/// Bound on inbound lines buffered between the reader task and the dispatcher.
/// The reader completes client responses itself, never through this queue, so
/// no dispatched handler may await a core→client request inline: with the
/// queue full, the answer could not reach it. Spawned work (a run asking for
/// permission) may wait; the reader then rejects excess requests rather than
/// parking ahead of the answer (`transport::reader_task`).
const DISPATCH_CHANNEL_CAPACITY: usize = 256;
/// The client closes stdin, waits ~3s, sends SIGTERM, then SIGKILL 2s later.
/// These caps keep core's own shutdown steps well inside the SIGTERM→SIGKILL
/// window. Adapter shutdown is not capped here.
const TRANSCRIPT_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
/// Blocking tasks still running after `block_on` returns — at least the
/// stdin read on the SIGTERM path — are abandoned after this.
const RUNTIME_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

pub fn serve_stdio() -> Result<(), ottam_protocol::CoreExitError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            ottam_protocol::CoreExitError::server(format!(
                "failed to build tokio runtime: {error}"
            ))
        })?;
    let result = runtime.block_on(serve_stdio_async());
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    result
}

async fn serve_stdio_async() -> Result<(), ottam_protocol::CoreExitError> {
    let database = tokio::task::spawn_blocking(Database::open_from_environment)
        .await
        .map_err(|error| {
            ottam_protocol::CoreExitError::server(format!(
                "database startup task failed: {error}"
            ))
        })?
        .map_err(crate::storage::StorageError::into_core_exit_error)?;
    let primary_sources = database
        .run("project.preloadPrimarySources", |connection| {
            ProjectRepository::all_active_primary_sources(connection)
        })
        .await
        .map_err(crate::storage::StorageError::into_core_exit_error)?;
    // Startup recovery: runs left non-terminal by a previous core process fail
    // with a typed `coreRestarted` error, and inputs caught mid-send surface as
    // deliveryUnknown for the user to resolve. Never auto-retried.
    let recovered = database
        .run("conversation.recoverOnStartup", |connection| {
            crate::storage::repositories::RunRepository::recover_on_startup(
                connection,
                chrono::Utc::now(),
            )
        })
        .await
        .map_err(crate::storage::StorageError::into_core_exit_error)?;
    if recovered.failed_runs > 0 || recovered.ambiguous_inputs > 0 {
        tracing::info!(
            failed_runs = recovered.failed_runs,
            ambiguous_inputs = recovered.ambiguous_inputs,
            "recovered interrupted conversation state on startup"
        );
    }
    // Attachments are created before their Message exists, so a crash or a
    // force-quit strands them. `workspace.open` sweeps the workspace being
    // opened; this catches every workspace the user never comes back to.
    //
    // Detached, because it walks every workspace's attachment directory and the
    // client is already waiting on `initialize` — reaping day-old bytes must not
    // sit in front of the first request. Archived workspaces are skipped: the
    // sweep creates the directories it reads, and an archived workspace should
    // not be re-materialized on disk just to look for strays.
    let sweep_database = database.clone();
    tokio::spawn(async move {
        let workspaces = sweep_database
            .run("workspace.listForAttachmentSweep", |connection| {
                crate::storage::repositories::WorkspaceRepository::list(connection, false)
            })
            .await;
        let Ok(workspaces) = workspaces else {
            return;
        };
        for workspace in &workspaces {
            chat::sweep_attachments(&sweep_database, &workspace.id).await;
        }
    });

    tracing::info!("ottam-core serving JSONL over stdio");
    AgentServer::new(database, primary_sources)
        .serve()
        .await
        .map_err(ottam_protocol::CoreExitError::server)
}

pub struct AgentServer {
    core_instance_id: CoreInstanceID,
    transcript_outbound: Option<OutboundSink>,
    transcript_coordinator: Arc<chat::TranscriptCoordinator>,
    database: Database,
    project_roots: HashMap<ProjectID, String>,
    source_roots: HashMap<ProjectSourceID, String>,
    terminal_manager: Option<TerminalManager>,
    client_link: Option<ClientLink>,
    agent_bridge: Option<std::sync::Arc<AgentRuntimeBridge>>,
    deferred: Option<DeferredResponses>,
    attachment_sweeps: AttachmentSweeps,
    mcp_servers: crate::mcp::McpServers,
    /// Servers, upstream connections, and proxy paths; set once `serve` has
    /// an outbound sink for their state notifications.
    mcp: Option<crate::mcp::McpService>,
}

#[derive(Clone, Default)]
struct AttachmentSweeps {
    in_flight: Arc<Mutex<HashSet<WorkspaceId>>>,
}

impl AttachmentSweeps {
    fn try_start(&self, workspace_id: WorkspaceId) -> Option<AttachmentSweepPermit> {
        let mut in_flight = self
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        in_flight
            .insert(workspace_id)
            .then(|| AttachmentSweepPermit {
                workspace_id,
                in_flight: Arc::clone(&self.in_flight),
            })
    }
}

struct AttachmentSweepPermit {
    workspace_id: WorkspaceId,
    in_flight: Arc<Mutex<HashSet<WorkspaceId>>>,
}

impl Drop for AttachmentSweepPermit {
    fn drop(&mut self) {
        self.in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.workspace_id);
    }
}

impl AgentServer {
    pub fn new(database: Database, primary_sources: Vec<ProjectSource>) -> Self {
        let project_roots = primary_sources
            .iter()
            .map(|source| {
                (
                    ProjectID(source.project_id.to_string()),
                    source.canonical_path.clone(),
                )
            })
            .collect();
        let source_roots = primary_sources
            .into_iter()
            .map(|source| {
                (
                    ProjectSourceID(source.id.to_string()),
                    source.canonical_path,
                )
            })
            .collect();
        Self {
            core_instance_id: CoreInstanceID(uuid::Uuid::now_v7().to_string()),
            transcript_outbound: None,
            transcript_coordinator: chat::TranscriptCoordinator::new(database.clone()),
            mcp_servers: crate::mcp::McpServers::new(
                database.clone(),
                crate::mcp::default_secret_store(),
            ),
            mcp: None,
            database,
            project_roots,
            source_roots,
            terminal_manager: None,
            client_link: None,
            agent_bridge: None,
            deferred: None,
            attachment_sweeps: AttachmentSweeps::default(),
        }
    }

    /// Actor split: a reader task streams stdin lines into the dispatcher (this
    /// loop, which owns `AgentServer` and may await an async handler), and a
    /// writer task owns stdout + the terminal-output queue. The reader routes
    /// client responses directly to `ClientLink`, so a core→client request can
    /// await without deadlocking the dispatcher.
    pub async fn serve(&mut self) -> Result<(), String> {
        self.transcript_coordinator.start_payloads();
        // Frames flow sink → router → per-connection writer. Today the primary
        // (stdio) writer is the only registered destination; the attach socket
        // registers more. The router preserves the old failure signal: a dead
        // primary writer makes every subsequent sink send fail.
        let (frame_tx, frame_rx) = unbounded_channel::<OutboundFrame>();
        let outbound = OutboundSink::new(frame_tx);
        self.transcript_outbound = Some(outbound.clone());
        let (primary_tx, primary_rx) = unbounded_channel::<outbound::BudgetedValue>();
        let attached_registry = AttachedRegistry::default();
        let router_handle = tokio::spawn(outbound::router_task(
            frame_rx,
            primary_tx,
            attached_registry.clone(),
        ));
        let (dispatch_tx, mut dispatch_rx) = channel::<InboundMessage>(DISPATCH_CHANNEL_CAPACITY);
        let terminal_output = TerminalOutputQueue::new(terminal_output_queue_max_bytes());
        let client_link = ClientLink::new(outbound.clone());
        let pending_client_requests = client_link.pending_requests();
        self.client_link = Some(client_link);
        self.deferred = Some(DeferredResponses::new(outbound.clone()));
        self.agent_bridge = Some(AgentRuntimeBridge::new(
            outbound.clone(),
            self.client_link
                .as_ref()
                .expect("client link installed")
                .clone(),
        ));
        let instances = self
            .database
            .run(
                "agent.instances.load",
                crate::storage::repositories::AgentInstanceRepository::list,
            )
            .await
            .map_err(|error| error.to_string())?;
        self.agent_bridge
            .as_ref()
            .expect("agent bridge installed")
            .registry()
            .load_instances(instances);
        let bridge = self.agent_bridge.as_ref().expect("agent bridge installed");
        bridge
            .model_catalog()
            .restore_probed_auth(&bridge.registry());
        // The runtime's state sink is an outbound clone; its shutdown lets
        // it go, so the router can exit.
        self.mcp = Some(crate::mcp::McpService::new(
            self.mcp_servers.clone(),
            crate::mcp::McpRuntime::new(
                self.mcp_servers.clone(),
                mcp::notice_sink(outbound.clone()),
            ),
            crate::mcp::redirect_port(),
        ));
        // Delegation reaches any workspace's chats, built as requests build
        // theirs.
        bridge.install_delegation(agents::DelegationHost {
            chats: {
                let (database, registry, coordinator, outbound, instance) = (
                    self.database.clone(),
                    bridge.registry(),
                    self.transcript_coordinator.clone(),
                    self.transcript_outbound.clone(),
                    self.core_instance_id.0.clone(),
                );
                Arc::new(move |workspace_id: &ottam_protocol::WorkspaceID| {
                    chat::ChatService::new(database.clone(), workspace_id.clone())
                        .with_registry(Some(Arc::clone(&registry)))
                        .with_transcript_coordinator(coordinator.clone())
                        .with_transcript_delivery(outbound.clone(), instance.clone())
                })
            },
            database: self.database.clone(),
        });
        // Like the attach socket, the proxy is optional: without it, sessions
        // are sent no MCP servers.
        let mcp_proxy = match crate::mcp::McpProxy::start(crate::mcp::McpTokens::default()).await {
            Ok(proxy) => {
                // Before any request is read, so no `mcp.*` change races it.
                let service = self.mcp.as_ref().expect("MCP service installed");
                if let Err(error) = service
                    .attach_proxy(proxy.sessions(), bridge.question_broker())
                    .await
                {
                    tracing::warn!(kind = error.kind, "MCP servers could not be served");
                }
                let registry = Arc::downgrade(&bridge.registry());
                service.runtime().set_toolchain(Arc::new(move || {
                    registry
                        .upgrade()
                        .and_then(|registry| registry.node_bin_dir())
                }));
                service.serve_ottam(bridge.ottam_tools());
                bridge.install_mcp(service.clone());
                Some(proxy)
            }
            Err(error) => {
                tracing::warn!(%error, "MCP proxy could not listen; sessions get no MCP servers");
                None
            }
        };
        // Model-catalog discovery is driven lazily by `engine.list` (the Chat
        // surface), not at boot: a core started for an unrelated operation should
        // never spawn agent/npx probe subprocesses.

        let terminal_manager = TerminalManager::new(
            TerminalEventSender::new(outbound.clone(), terminal_output.clone()),
            tokio::runtime::Handle::current(),
        );
        // The sign-in service outlives the request that starts it, so it holds a
        // detached spawner rather than borrowing the manager the server owns.
        if let Some(bridge) = self.agent_bridge.as_ref() {
            bridge.install_sign_in(auth_terminal::ServerAuthTerminalHost::new(
                terminal_manager.spawner(),
                self.database.clone(),
            ));
        }
        self.terminal_manager = Some(terminal_manager);

        let pending_for_writer = pending_client_requests.clone();
        // The client closing core's stdout ends the primary reader below, like
        // stdin EOF, instead of failing the writer as a server error.
        let stdout_closed = tokio_util::sync::CancellationToken::new();
        let stdout = PeerClosableOutput::new(tokio::io::stdout(), stdout_closed.clone());
        let writer_handle = tokio::spawn(async move {
            let result = writer_task(primary_rx, terminal_output, stdout).await;
            pending_for_writer.close();
            result
        });
        // The primary is the process lifecycle owner: its reader finishing
        // (stdin EOF) must end the server even while attached connections hold
        // live dispatch senders. The token tells the attach accept loop to
        // shut those connections down so the dispatch queue can drain closed.
        let primary_done = tokio_util::sync::CancellationToken::new();
        let attach_dispatch_tx = dispatch_tx.clone();
        let outbound_for_reader = outbound.clone();
        // SIGTERM ends the primary reader exactly as stdin EOF does, so a
        // terminated core runs the same graceful shutdown below.
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(
                |error| format!("failed to install SIGTERM handler: {error}"),
            )?;
        let reader_handle = tokio::spawn({
            let primary_done = primary_done.clone();
            let pending_on_stop = pending_client_requests.clone();
            async move {
                let result = tokio::select! {
                    result = reader_task(
                        tokio::io::BufReader::new(tokio::io::stdin()),
                        ConnectionId::Primary,
                        dispatch_tx,
                        outbound_for_reader,
                        pending_client_requests,
                    ) => result,
                    _ = sigterm.recv() => {
                        tracing::info!("SIGTERM received; shutting down");
                        pending_on_stop.close();
                        Ok(())
                    }
                    () = stdout_closed.cancelled() => {
                        pending_on_stop.close();
                        Ok(())
                    }
                };
                primary_done.cancel();
                result
            }
        });
        let attach_runtime = attach::spawn(
            self.database.home(),
            attached_registry.clone(),
            attach_dispatch_tx,
            outbound.clone(),
            primary_done.clone(),
        );

        let mut dispatch_result = Ok(());
        while let Some(message) = dispatch_rx.recv().await {
            if let Some(response) = self
                .handle_message(message.origin, &message.line, message.received_at)
                .await
            {
                if outbound.respond(message.origin, response).is_err() {
                    // The router is gone because the primary writer died
                    // (broken stdout); stop and exit non-zero, matching the
                    // old `send_outbound(...)?` path.
                    dispatch_result = Err("JSONL outbound writer terminated".to_string());
                    break;
                }
            }
        }

        // Reached either because the primary reader finished (its senders and
        // the attach loop's are gone) or because the writer died; make the
        // attach teardown unconditional so the second path cannot leave the
        // accept loop running, then wait for it to unlink the socket.
        primary_done.cancel();
        if let Some(attach_runtime) = attach_runtime {
            attach_runtime.finish().await;
        }

        if let Some(agent_bridge) = self.agent_bridge.take() {
            agent_bridge.shutdown().await;
        }
        // After the agents, so none loses its servers while it winds down.
        if let Some(mcp_proxy) = mcp_proxy {
            mcp_proxy.finish().await;
        }
        if let Some(mcp) = self.mcp.take() {
            mcp.shutdown().await;
        }
        if let Some(deferred) = self.deferred.take() {
            deferred.shutdown().await;
        }
        self.client_link = None;
        let _ = tokio::time::timeout(
            TRANSCRIPT_DRAIN_TIMEOUT,
            self.transcript_coordinator.drain(),
        )
        .await;
        self.transcript_coordinator.disconnect();
        if let Some(terminal_manager) = self.terminal_manager.as_mut() {
            terminal_manager.close_all().await;
        }
        self.terminal_manager = None;
        // Every sink clone is gone once the reader task finishes; the router
        // then exits, dropping the primary writer's channel so it drains out.
        self.transcript_outbound = None;
        drop(outbound);

        let reader_result = if dispatch_result.is_ok() {
            // Normal shutdown: the dispatch channel closed because the reader
            // finished (stdin EOF or a stdin read error). Its
            // result has already resolved; preserve it so a genuine read error
            // still exits non-zero, as the old synchronous loop did.
            match reader_handle.await {
                Ok(result) => result,
                Err(join_error) => Err(format!("JSONL reader task panicked: {join_error}")),
            }
        } else {
            // Writer-failure shutdown: the reader may still be parked on stdin.
            // Abort it so its outbound sender drops and the writer terminates;
            // its (cancelled) result is not meaningful here.
            reader_handle.abort();
            let _ = reader_handle.await;
            Ok(())
        };

        if let Err(join_error) = router_handle.await {
            tracing::warn!(error = %join_error, "outbound router task panicked");
        }
        let writer_result = match writer_handle.await {
            Ok(result) => result,
            Err(join_error) => Err(format!("JSONL writer task panicked: {join_error}")),
        };

        tracing::info!("JSONL input closed; stdio server exiting");
        writer_result.and(dispatch_result).and(reader_result)
    }

    async fn handle_message(
        &mut self,
        origin: ConnectionId,
        payload: &str,
        received_at: Instant,
    ) -> Option<Value> {
        let Ok(value) = serde_json::from_str::<Value>(payload) else {
            tracing::warn!(bytes = payload.len(), "malformed JSONL request rejected");
            return Some(error_response(RequestID::Null, -32700, "parse error"));
        };

        if value.get("id").is_none() {
            let notification = serde_json::from_value::<ProtocolNotification<Value>>(value);
            if let Ok(notification) = notification {
                if notification.method == METHOD_INITIALIZED {
                    tracing::info!(
                        method = METHOD_INITIALIZED,
                        "client initialized notification received"
                    );
                    return None;
                }
                tracing::debug!(
                    method = %notification.method,
                    "notification ignored"
                );
                return None;
            }
            tracing::warn!("invalid notification ignored");
            return None;
        }

        let request = match serde_json::from_value::<ProtocolRequest<Value>>(value.clone()) {
            Ok(request) => request,
            Err(error) => {
                tracing::warn!(
                    request_id_kind = %request_id_kind(&request_id_from_invalid_envelope(&value)),
                    error = %error,
                    "invalid request envelope rejected"
                );
                return Some(error_response(
                    request_id_from_invalid_envelope(&value),
                    -32600,
                    format!("invalid request: {error}"),
                ));
            }
        };
        self.handle_request(origin, request, received_at).await
    }

    #[tracing::instrument(
        level = "info",
        skip_all,
        fields(method = %request.method, request_id_kind = %request_id_kind(&request.id))
    )]
    async fn handle_request(
        &mut self,
        origin: ConnectionId,
        request: ProtocolRequest<Value>,
        received_at: Instant,
    ) -> Option<Value> {
        let started = Instant::now();
        // Requests run one at a time; time spent behind earlier ones is
        // queueing the app sees as latency.
        let queued_ms = started.saturating_duration_since(received_at).as_millis();
        let request_id = request.id.clone();
        if request.method == ottam_protocol::METHOD_CONVERSATION_READ_TRANSCRIPT_PAYLOAD {
            let params = match serde_json::from_value::<
                ottam_protocol::ConversationReadTranscriptPayloadParams,
            >(request.params)
            {
                Ok(params) => params,
                Err(error) => return Some(error_response(request.id, -32602, error.to_string())),
            };
            let chat = self.chat_service(&params.workspace_id);
            let id = request.id.clone();
            let response = async move {
                match chat.read_transcript_payload(params).await {
                    Ok(result) => success_response(id, result),
                    Err(error) => responses::core_error_response(id, -32000, error),
                }
            };
            return match self.deferred.as_mut() {
                Some(deferred) => deferred
                    .spawn(origin, response)
                    .err()
                    .map(|_| error_response(request.id, -32000, "Too many pending requests")),
                None => Some(response.await),
            };
        }
        if matches!(
            request.method.as_str(),
            ottam_protocol::METHOD_STORAGE_STATUS
                | ottam_protocol::METHOD_STORAGE_UPDATE
                | ottam_protocol::METHOD_STORAGE_CLEAR_CACHE
        ) {
            let database = self.database.clone();
            if let Some(deferred) = self.deferred.as_mut() {
                let id = request.id.clone();
                return deferred
                    .spawn(origin, async move {
                        storage_cache::handle_storage_request(&database, request).await
                    })
                    .err()
                    .map(|_| error_response(id, -32000, "Too many pending requests"));
            }
            return Some(storage_cache::handle_storage_request(&database, request).await);
        }
        if mcp::is_mcp_method(&request.method) {
            let Some(service) = self.mcp.clone() else {
                return Some(error_response(
                    request.id,
                    -32000,
                    "MCP servers are not available yet",
                ));
            };
            if let Some(deferred) = self.deferred.as_mut() {
                let id = request.id.clone();
                return deferred
                    .spawn(origin, async move {
                        mcp::handle_mcp_request(&service, request).await
                    })
                    .err()
                    .map(|_| error_response(id, -32000, "Too many pending requests"));
            }
            return Some(mcp::handle_mcp_request(&service, request).await);
        }
        if named_agents::is_named_agent_method(&request.method) {
            let database = self.database.clone();
            let registry = self.agent_bridge.as_ref().map(|bridge| bridge.registry());
            if let Some(deferred) = self.deferred.as_mut() {
                let id = request.id.clone();
                return deferred
                    .spawn(origin, async move {
                        named_agents::handle_named_agent_request(&database, registry, request).await
                    })
                    .err()
                    .map(|_| error_response(id, -32000, "Too many pending requests"));
            }
            return Some(
                named_agents::handle_named_agent_request(&self.database, registry, request).await,
            );
        }
        if request.method == ottam_protocol::METHOD_CONVERSATION_CONTINUE_DELEGATION {
            return self.handle_conversation_continue_delegation(origin, request);
        }
        if request.method == ottam_protocol::METHOD_CONVERSATION_SEND_TO_PARENT {
            return self.handle_conversation_send_to_parent(origin, request);
        }
        if request.method == METHOD_CONVERSATION_STOP_BACKGROUND_TASK {
            return self.handle_conversation_stop_background_task(origin, request);
        }
        if request.method == METHOD_CONVERSATION_SET_MODE {
            return self.handle_conversation_set_mode(origin, request);
        }
        if request.method.starts_with("agent.") {
            return self.handle_agent_request(origin, request);
        }
        if request.method == METHOD_ENGINE_LIST {
            return self.handle_engine_list(origin, request).await;
        }
        if request.method == METHOD_RUNTIME_INSTALL {
            return Some(self.handle_runtime_install(request));
        }
        if request.method == METHOD_RUNTIME_INSTALL_CANCEL {
            return Some(self.handle_runtime_install_cancel(request));
        }
        if request.method == METHOD_GIT_DIFF_SNAPSHOT {
            return self.handle_git_diff_snapshot(origin, request).await;
        }
        let response = match request.method.as_str() {
            METHOD_INITIALIZE => self.handle_initialize(origin, request),
            METHOD_PROJECT_OPEN_PATH => self.handle_project_open_path(request).await,
            METHOD_WORKSPACE_LIST => self.handle_workspace_list(request).await,
            METHOD_WORKSPACE_CREATE => self.handle_workspace_create(request).await,
            METHOD_WORKSPACE_OPEN => self.handle_workspace_open(request).await,
            METHOD_WORKSPACE_GET => self.handle_workspace_get(request).await,
            METHOD_WORKSPACE_UPDATE => self.handle_workspace_update(request).await,
            METHOD_WORKSPACE_ARCHIVE => self.handle_workspace_archive(request).await,
            METHOD_WORKSPACE_RESTORE => self.handle_workspace_restore(request).await,
            METHOD_WORKSPACE_ADD_PROJECT => self.handle_workspace_add_project(request).await,
            METHOD_WORKSPACE_REMOVE_PROJECT => self.handle_workspace_remove_project(request).await,
            METHOD_CONVERSATION_CREATE => self.handle_conversation_create(request).await,
            METHOD_CONVERSATION_DELETE => self.handle_conversation_delete(request).await,
            METHOD_CONVERSATION_LIST => self.handle_conversation_list(request).await,
            METHOD_CONVERSATION_GET => self.handle_conversation_get(request).await,
            METHOD_CONVERSATION_LIST_EVENTS => self.handle_conversation_list_events(request).await,
            ottam_protocol::METHOD_CONVERSATION_LIST_TRANSCRIPT => {
                self.handle_conversation_list_transcript(request).await
            }
            ottam_protocol::METHOD_CONVERSATION_READ_TRANSCRIPT_BODY => {
                self.handle_conversation_read_transcript_body(request).await
            }
            ottam_protocol::METHOD_CONVERSATION_READ_TRANSCRIPT_PAYLOAD => {
                self.handle_conversation_read_transcript_payload(request)
                    .await
            }
            METHOD_CONVERSATION_LIST_MESSAGES => {
                self.handle_conversation_list_messages(request).await
            }
            METHOD_CONVERSATION_LIST_RUNS => self.handle_conversation_list_runs(request).await,
            METHOD_CONVERSATION_LIST_RUN_INPUTS => {
                self.handle_conversation_list_run_inputs(request).await
            }
            METHOD_CONVERSATION_LIST_AGENT_SESSIONS => {
                self.handle_conversation_list_agent_sessions(request).await
            }
            METHOD_CONVERSATION_POST_MESSAGE => {
                self.handle_conversation_post_message(request).await
            }
            METHOD_CONVERSATION_CREATE_ATTACHMENT => {
                self.handle_conversation_create_attachment(request).await
            }
            METHOD_CONVERSATION_DISCARD_ATTACHMENT => {
                self.handle_conversation_discard_attachment(request).await
            }
            METHOD_CONVERSATION_START_RUN => self.handle_conversation_start_run(request).await,
            METHOD_CONVERSATION_CANCEL_RUN => self.handle_conversation_cancel_run(request).await,
            METHOD_CONVERSATION_STEER_RUN => self.handle_conversation_steer_run(request).await,
            METHOD_CONVERSATION_RESOLVE_RUN_INPUT_DELIVERY => {
                self.handle_conversation_resolve_run_input_delivery(request)
                    .await
            }
            METHOD_CONVERSATION_SET_ALWAYS_ALLOW_TOOL_ACTIONS => {
                self.handle_conversation_set_always_allow_tool_actions(request)
                    .await
            }
            METHOD_TOOL_PERMISSIONS_GET => self.handle_tool_permissions_get(request).await,
            METHOD_TOOL_PERMISSIONS_SET => self.handle_tool_permissions_set(request).await,
            METHOD_TERMINAL_OPEN => self.handle_terminal_open(request).await,
            METHOD_TERMINAL_WRITE => self.handle_terminal_write(request),
            METHOD_TERMINAL_RESIZE => self.handle_terminal_resize(request).await,
            METHOD_TERMINAL_CLOSE => self.handle_terminal_close(request),
            METHOD_TERMINAL_LIST => self.handle_terminal_list(request).await,
            METHOD_TERMINAL_DELETE => self.handle_terminal_delete(request).await,
            METHOD_TERMINAL_UPDATE_METADATA => self.handle_terminal_update_metadata(request).await,
            METHOD_TERMINAL_REPLAY => self.handle_terminal_replay(request).await,
            _ => error_response(request.id, -32601, "method not found"),
        }
        .tap(|response| {
            tracing::info!(
                request_id_kind = %request_id_kind(&request_id),
                queued_ms,
                elapsed_ms = started.elapsed().as_millis(),
                status = response_status(response),
                "request completed"
            );
        });
        Some(response)
    }
}

#[cfg(test)]
mod attachment_sweep_tests {
    use super::*;

    #[test]
    fn coalesces_one_in_flight_sweep_per_workspace() {
        let sweeps = AttachmentSweeps::default();
        let workspace_id =
            WorkspaceId::try_from("019b0000-0000-7000-8000-000000000001").expect("workspace ID");

        let permit = sweeps.try_start(workspace_id).expect("first sweep starts");
        assert!(sweeps.try_start(workspace_id).is_none());
        drop(permit);
        assert!(sweeps.try_start(workspace_id).is_some());
    }
}
