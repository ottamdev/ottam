use super::super::types::{
    AdapterLifecycleState, AdapterStatus, AgentDefinition, AgentModelOption, AgentModelSelects,
    AgentSessionSelects, FormSkip, HarnessAuthMethod, HarnessAuthState, HarnessCapabilities,
    HarnessError, HarnessErrorKind, HarnessPermissionBroker, HarnessPermissionOption,
    HarnessPermissionOptionKind, HarnessPermissionRequest, HarnessPrompt, HarnessQuestionOutcome,
    HarnessQuestionRequest, HarnessSessionId, HarnessSessionState, HarnessSessionSummary,
    HarnessStopReason, HarnessTurnUsage, HarnessUpdate, HarnessUpdateKind, HarnessUpdateSender,
    McpServerEntry, ModelSwitch, SessionConfigRequest, SignInProof, SteeringCapability,
};
use super::async_tasks::StopAsyncTaskRequest;
use super::child_sessions::ChildSessions;
use super::elicitation;
use super::features::{advertised_features, AcpFeature};
use super::protocol::{
    build_transport, decode_session_update, harness_to_acp_error, initialize_wire, map_acp_error,
    map_prompt_execution_error, normalize_auth_method, prompt_content_blocks,
    sanitize_error_message, to_json, validate_permission_outcome, CLIENT_NAME,
};
use super::session_config::{
    apply_live_mode, apply_live_model, apply_restored_mode, apply_session_config,
    extract_session_selects,
    model_option, ConfigSelect, SessionConfigSetter, SessionSelects,
};
use super::steering::{map_steer_error, steering_capability, SteerOutcome, SteerRequest};
use super::subagent_sessions::SubagentSessions;
use crate::diagnostics::{record_unhandled, shape};
use agent_client_protocol::schema::v1::{
    AuthenticateRequest, CancelNotification, CloseSessionRequest, ContentBlock,
    DeleteSessionRequest, HttpHeader, LoadSessionRequest, McpServer, McpServerHttp,
    NewSessionRequest, PromptRequest, RequestPermissionRequest, RequestPermissionResponse,
    SessionConfigOption, SetSessionConfigOptionRequest, StopReason,
};
use agent_client_protocol::{Agent, ConnectionTo, Dispatch, Handled, UntypedMessage};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

pub(super) const COMMAND_CAPACITY: usize = 64;

/// Connection-scoped identity reported by Claude and Codex ACP. Unknown future
/// kinds carry no verdict; never retain or log account details from the payload.
#[derive(
    Debug, Clone, serde::Serialize, serde::Deserialize, agent_client_protocol::JsonRpcNotification,
)]
#[notification(method = "_auth/status_update")]
pub(super) struct AuthStatusUpdate {
    #[serde(rename = "authStatus")]
    pub(super) auth_status: serde_json::Value,
}

impl AuthStatusUpdate {
    pub(super) fn record(&self, shared: &SharedState) {
        let state = match self
            .auth_status
            .get("kind")
            .and_then(serde_json::Value::as_str)
        {
            Some("none") => HarnessAuthState::Missing,
            Some("account" | "api_key" | "gateway" | "external") => HarnessAuthState::Ready,
            _ => return,
        };
        shared.set_auth(state, None);
    }
}

pub(super) type ReadySender =
    Arc<Mutex<Option<oneshot::Sender<Result<AdapterStatus, HarnessError>>>>>;

pub(super) struct SharedState {
    pub(super) status_tx: watch::Sender<AdapterStatus>,
    pub(super) updates_tx: HarnessUpdateSender,
    pub(super) sessions: Mutex<BTreeMap<HarnessSessionId, HarnessSessionSummary>>,
    pub(super) update_sequences: Mutex<BTreeMap<HarnessSessionId, UpdateSequence>>,
    /// Each session's last announced commands. Separate from `sessions`
    /// because agents announce them before `session/new` returns.
    pub(super) announced_commands: Mutex<BTreeMap<HarnessSessionId, serde_json::Value>>,
    /// Rewrites subagent activity the agent tags onto its parent's session
    /// (`AgentDefinition::child_sessions`).
    pub(super) child_sessions: Option<ChildSessions>,
    /// What shows the agent signed in (`AgentDefinition::sign_in_proof`).
    pub(super) sign_in_proof: SignInProof,
}

/// The mode an agent announced on its own: `current_mode_update`, or the
/// mode-category option of `config_option_update`.
fn announced_mode(update: &HarnessUpdateKind) -> Option<String> {
    let HarnessUpdateKind::Other { kind, payload } = update else {
        return None;
    };
    match kind.as_str() {
        "current_mode_update" => {
            let mode = payload.get("currentModeId")?.as_str()?;
            (!mode.is_empty()).then(|| mode.to_owned())
        }
        _ => announced_config(update, "mode"),
    }
}

/// The current value of one category in a `config_option_update` the agent
/// sent on its own.
fn announced_config(update: &HarnessUpdateKind, category: &str) -> Option<String> {
    let HarnessUpdateKind::Other { kind, payload } = update else {
        return None;
    };
    if kind != "config_option_update" {
        return None;
    }
    let value = payload
        .get("configOptions")?
        .as_array()?
        .iter()
        .find(|option| option.get("category").and_then(|c| c.as_str()) == Some(category))?
        .get("currentValue")?
        .as_str()?;
    (!value.is_empty()).then(|| value.to_owned())
}

/// A session's update numbering.
#[derive(Default)]
pub(super) struct UpdateSequence {
    last: u64,
    /// The latest [`HarnessUpdateKind::PromptSettled`] marker's sequence.
    prompt_settled: Option<u64>,
}

impl SharedState {
    pub(super) fn status(&self) -> AdapterStatus {
        self.status_tx.borrow().clone()
    }

    pub(super) fn begin_start(&self) {
        self.update_sequences
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.announced_commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        if let Some(children) = &self.child_sessions {
            children.reset();
        }
        self.status_tx.send_modify(|status| {
            status.lifecycle = AdapterLifecycleState::Starting;
            status.auth_state = HarnessAuthState::Unknown;
            status.credentials_seen = false;
            // Session catalog fields belong to this process generation. A
            // fresh initialize must not re-record the previous process's
            // models/selects as a new live session observation.
            status.models.clear();
            status.default_model_id = None;
            status.session_selects = None;
            status.model_selects.clear();
            status.generation = status.generation.saturating_add(1);
            status.last_error = None;
        });
    }

    pub(super) fn set_ready(&self, ready: &BackendReady) -> Result<AdapterStatus, HarnessError> {
        let mut ready_status = None;
        self.status_tx.send_modify(|status| {
            if status.lifecycle == AdapterLifecycleState::Starting {
                status.lifecycle = AdapterLifecycleState::Ready;
                // An auth-status notification may arrive immediately after
                // initialize, before this continuation processes its response.
                status.auth_methods = ready.auth_methods.clone();
                status.agent_name = ready.agent_name.clone();
                status.agent_version = ready.agent_version.clone();
                status.capabilities = ready.capabilities.clone();
                status.last_error = None;
                ready_status = Some(status.clone());
            }
        });
        ready_status.ok_or_else(|| {
            HarnessError::new(
                HarnessErrorKind::RequestCancelled,
                "adapter initialization was cancelled",
            )
        })
    }

    pub(super) fn set_lifecycle(&self, lifecycle: AdapterLifecycleState, error: Option<String>) {
        self.status_tx.send_modify(|status| {
            status.lifecycle = lifecycle;
            status.last_error = error.map(|message| sanitize_error_message(&message));
        });
    }

    pub(super) fn set_auth(&self, auth_state: HarnessAuthState, error: Option<String>) {
        self.status_tx.send_modify(|status| {
            status.auth_state = auth_state;
            // Latches for the life of the generation: once this adapter has been
            // seen working, a later failure is a lapse rather than an absence.
            status.credentials_seen |= auth_state == HarnessAuthState::Ready;
            status.last_error = error.map(|message| sanitize_error_message(&message));
        });
    }

    /// Record that the agent's steering advertisement was wrong: it named the
    /// extension in `initialize` and then answered the request with
    /// `MethodNotFound`. Demoting corrects the published capability instead of
    /// re-asking a question this adapter has already answered, so `engine.list`
    /// stops reporting `native` for it. The advertised map is corrected with
    /// it, so the compatibility matrix on `agent.list` tells the same story.
    ///
    /// Deliberately not latched across generations: `set_ready` reassigns
    /// capabilities on every start, so a restarted adapter gets to make its
    /// claim again. The demotion is a fact about this process.
    pub(super) fn demote_steering(&self) {
        self.status_tx.send_modify(|status| {
            if status.capabilities.steering == SteeringCapability::Native {
                status.capabilities.steering = SteeringCapability::InterruptThenPrompt;
                status
                    .capabilities
                    .agent_advertises
                    .insert(AcpFeature::SessionSteering, false);
            }
        });
    }

    pub(super) fn set_models(
        &self,
        models: Vec<AgentModelOption>,
        default_model_id: Option<String>,
    ) {
        self.status_tx.send_modify(|status| {
            if status.models != models {
                status.models = models;
            }
            if status.default_model_id != default_model_id {
                status.default_model_id = default_model_id;
            }
        });
    }

    /// Always `Some` once a `session/new` succeeded, even with nothing in it:
    /// "advertises no modes" is an answer the catalog must be able to cache.
    pub(super) fn set_session_selects(&self, selects: AgentSessionSelects) {
        self.status_tx.send_modify(|status| {
            if status.session_selects.as_ref() != Some(&selects) {
                status.session_selects = Some(selects);
            }
        });
    }

    /// Remember the thought-level select a model brought with it. Newest wins.
    pub(super) fn record_model_selects(&self, observed: Option<(String, AgentModelSelects)>) {
        let Some((model_id, selects)) = observed else {
            return;
        };
        self.status_tx.send_if_modified(|status| {
            if status.model_selects.get(&model_id) == Some(&selects) {
                return false;
            }
            status.model_selects.insert(model_id, selects);
            true
        });
    }

    pub(super) fn mark_dead(&self, reason: &str) {
        self.set_lifecycle(AdapterLifecycleState::Dead, Some(reason.to_string()));
        self.mark_sessions_dead(reason);
    }

    pub(super) fn mark_stopped(&self, reason: &str) {
        self.status_tx.send_modify(|status| {
            status.lifecycle = AdapterLifecycleState::Stopped;
            status.auth_state = HarnessAuthState::Unknown;
            status.last_error = None;
        });
        self.mark_sessions_dead(reason);
    }

    pub(super) fn mark_sessions_dead(&self, reason: &str) {
        let sanitized_reason = sanitize_error_message(reason);
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for session in sessions.values_mut() {
            session.state = HarnessSessionState::Dead;
            session.last_error = Some(sanitized_reason.clone());
        }
    }

    pub(super) fn sessions(&self) -> Vec<HarnessSessionSummary> {
        let commands = self
            .announced_commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .map(|session| HarnessSessionSummary {
                announced_commands: commands.get(&session.session_id).cloned(),
                ..session.clone()
            })
            .collect()
    }

    pub(super) fn send_update(&self, session_id: HarnessSessionId, update: HarnessUpdateKind) {
        self.send_session_update(session_id, update, None);
    }

    /// Publishes an update the agent sent as `params`, after rewriting any
    /// subagent activity it tagged onto its parent's session.
    pub(super) fn send_session_update(
        &self,
        session_id: HarnessSessionId,
        update: HarnessUpdateKind,
        params: Option<&serde_json::Value>,
    ) {
        match &self.child_sessions {
            Some(children) => {
                for (session_id, update) in children.translate(session_id, update, params) {
                    self.publish(session_id, update);
                }
            }
            None => self.publish(session_id, update),
        }
    }

    fn publish(&self, session_id: HarnessSessionId, update: HarnessUpdateKind) {
        if let Some(mode_id) = announced_mode(&update) {
            self.set_session_mode(&session_id, Some(mode_id));
        }
        let model = announced_config(&update, "model");
        let thought_level = announced_config(&update, "thought_level");
        if model.is_some() || thought_level.is_some() {
            self.update_session(&session_id, |session| {
                if model.is_some() {
                    session.current_model_id = model;
                }
                if thought_level.is_some() {
                    session.current_thought_level_id = thought_level;
                }
            });
        }
        if let HarnessUpdateKind::Other { kind, payload } = &update {
            if kind == "available_commands_update" {
                self.announced_commands
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(session_id.clone(), payload.clone());
            }
        }
        let mut sequences = self
            .update_sequences
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sequence = sequences.entry(session_id.clone()).or_default();
        sequence.last = sequence.last.saturating_add(1);
        if matches!(update, HarnessUpdateKind::PromptSettled) {
            sequence.prompt_settled = Some(sequence.last);
        }
        let _ = self.updates_tx.send(HarnessUpdate {
            session_id,
            sequence: sequence.last,
            update,
        });
    }

    pub(super) fn prompt_settled_at(&self, session_id: &HarnessSessionId) -> Option<u64> {
        self.update_sequences
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(session_id)
            .and_then(|sequence| sequence.prompt_settled)
    }

    pub(super) fn insert_session(&self, session_id: HarnessSessionId, cwd: PathBuf) {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                session_id.clone(),
                HarnessSessionSummary {
                    session_id,
                    cwd,
                    state: HarnessSessionState::Active,
                    last_error: None,
                    current_mode_id: None,
                    current_model_id: None,
                    current_thought_level_id: None,
                    announced_commands: None,
                },
            );
    }

    fn update_session(&self, session_id: &HarnessSessionId, update: impl FnOnce(&mut HarnessSessionSummary)) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(session_id)
        {
            update(session);
        }
    }

    /// Record the mode the agent confirmed for a session core holds.
    pub(super) fn set_session_mode(&self, session_id: &HarnessSessionId, mode_id: Option<String>) {
        self.update_session(session_id, |session| session.current_mode_id = mode_id);
    }

    /// Record the model and thought level the agent confirmed for a session
    /// core holds.
    pub(super) fn set_session_model(
        &self,
        session_id: &HarnessSessionId,
        model_id: Option<String>,
        thought_level_id: Option<String>,
    ) {
        self.update_session(session_id, |session| {
            session.current_model_id = model_id;
            session.current_thought_level_id = thought_level_id;
        });
    }

    pub(super) fn remove_session(&self, session_id: &HarnessSessionId) {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_id);
        self.update_sequences
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_id);
        self.announced_commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_id);
    }

    pub(super) fn begin_prompt(&self, session_id: &HarnessSessionId) -> Result<(), HarnessError> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = sessions.get_mut(session_id).ok_or_else(|| {
            HarnessError::new(
                HarnessErrorKind::SessionNotFound,
                format!("unknown session {session_id}"),
            )
        })?;
        match session.state {
            HarnessSessionState::Active => {
                session.state = HarnessSessionState::Prompting;
                session.last_error = None;
                Ok(())
            }
            HarnessSessionState::Prompting => Err(HarnessError::new(
                HarnessErrorKind::SessionBusy,
                format!("session {session_id} already has an active prompt"),
            )),
            HarnessSessionState::Dead => Err(HarnessError::new(
                HarnessErrorKind::AdapterDead,
                format!("session {session_id} is dead"),
            )),
        }
    }

    pub(super) fn finish_prompt(
        &self,
        session_id: &HarnessSessionId,
        result: &Result<HarnessStopReason, HarnessError>,
    ) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(session_id)
        {
            session.state = HarnessSessionState::Active;
            session.last_error = result
                .as_ref()
                .err()
                .map(|error| sanitize_error_message(&error.message));
        }
    }

    pub(super) fn ensure_loadable(
        &self,
        session_id: &HarnessSessionId,
    ) -> Result<(), HarnessError> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match sessions.get(session_id).map(|session| session.state) {
            Some(HarnessSessionState::Prompting) => Err(HarnessError::new(
                HarnessErrorKind::SessionBusy,
                format!("session {session_id} already has an active prompt"),
            )),
            _ => Ok(()),
        }
    }

    /// Whether the session has a prompt in flight, which is what both
    /// `session/cancel` and `_session/steering` need to know before they put
    /// anything on the wire. Errors describe the session itself, not the
    /// request that asked.
    pub(super) fn is_prompting(&self, session_id: &HarnessSessionId) -> Result<bool, HarnessError> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match sessions.get(session_id).map(|session| session.state) {
            Some(HarnessSessionState::Prompting) => Ok(true),
            Some(HarnessSessionState::Active) => Ok(false),
            Some(HarnessSessionState::Dead) => Err(HarnessError::new(
                HarnessErrorKind::AdapterDead,
                format!("session {session_id} is dead"),
            )),
            None => Err(HarnessError::new(
                HarnessErrorKind::SessionNotFound,
                format!("unknown session {session_id}"),
            )),
        }
    }

    pub(super) fn prompting_sessions(&self) -> Vec<HarnessSessionId> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .filter(|session| session.state == HarnessSessionState::Prompting)
            .map(|session| session.session_id.clone())
            .collect()
    }
}

pub(super) enum AdapterCommand {
    Authenticate {
        method_id: String,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    CancelAuthenticate {
        method_id: String,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    NewSession {
        cwd: PathBuf,
        config: SessionConfigRequest,
        reply: oneshot::Sender<Result<HarnessSessionId, HarnessError>>,
    },
    LoadSession {
        session_id: HarnessSessionId,
        cwd: PathBuf,
        mode_id: Option<String>,
        mcp_servers: Vec<McpServerEntry>,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    ApplyMode {
        session_id: HarnessSessionId,
        mode_id: String,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    ApplyModel {
        session_id: HarnessSessionId,
        switch: ModelSwitch,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    DeleteSession {
        session_id: HarnessSessionId,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    Prompt {
        session_id: HarnessSessionId,
        prompt: HarnessPrompt,
        reply: oneshot::Sender<Result<HarnessStopReason, HarnessError>>,
    },
    Steer {
        session_id: HarnessSessionId,
        prompt: HarnessPrompt,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    Cancel {
        session_id: HarnessSessionId,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    /// `session/cancel` for a session the agent is working in on its own,
    /// only while core has no prompt in flight there.
    CancelOutOfTurn {
        session_id: HarnessSessionId,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    /// AIR `_session/async_task/stop`; answers whether the agent stopped it.
    StopAsyncTask {
        session_id: HarnessSessionId,
        async_task_id: String,
        reply: oneshot::Sender<Result<bool, HarnessError>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
pub(super) struct BackendReady {
    pub(super) auth_methods: Vec<HarnessAuthMethod>,
    pub(super) agent_name: Option<String>,
    pub(super) agent_version: Option<String>,
    pub(super) capabilities: HarnessCapabilities,
}

pub(super) struct BackendContext {
    pub(super) definition: AgentDefinition,
    pub(super) command_rx: mpsc::Receiver<AdapterCommand>,
    pub(super) ready_tx: ReadySender,
    pub(super) shared: Arc<SharedState>,
    pub(super) permission_broker: Arc<dyn HarnessPermissionBroker>,
}

pub(super) trait AdapterBackend: Send + Sync {
    fn run(
        self: Arc<Self>,
        context: BackendContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'static>>;
}

pub(super) struct AcpBackend;

impl AdapterBackend for AcpBackend {
    fn run(
        self: Arc<Self>,
        context: BackendContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'static>> {
        Box::pin(async move { run_acp_backend(context).await })
    }
}

async fn run_acp_backend(context: BackendContext) -> Result<(), HarnessError> {
    let BackendContext {
        definition,
        mut command_rx,
        ready_tx,
        shared,
        permission_broker,
    } = context;
    let (transport, child_return) = build_transport(&definition);
    let refresh_lock_path = definition.refresh_lock_path();
    let update_shared = Arc::clone(&shared);
    // Per process, like its sessions.
    let update_subagents = SubagentSessions::default();
    let update_agent_id = definition.agent_id.clone();
    let unclaimed_agent_id = definition.agent_id.clone();
    let auth_shared = Arc::clone(&shared);
    let connection_ready_tx = Arc::clone(&ready_tx);
    let connection_shared = Arc::clone(&shared);
    let permission_agent_id = definition.agent_id.clone();
    let auto_approvable = definition.permission_bypass_mode().is_none();
    let permission_shared = Arc::clone(&shared);
    let question_broker = Arc::clone(&permission_broker);
    let question_shared = Arc::clone(&shared);
    let form_skip = definition.form_skip;
    let omp = definition.harness_id() == crate::agent::registry::OMP_AGENT_ID;
    // Oh-My-Pi runs MCP tools as `xd://` device writes (`xd_devices`).
    let devices = move |update| {
        if omp {
            super::xd_devices::rewrite_update(update)
        } else {
            update
        }
    };
    let result = agent_client_protocol::Client
        .builder()
        .name(CLIENT_NAME)
        .on_receive_notification(
            async move |notification: AuthStatusUpdate, _connection| {
                notification.record(&auth_shared);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        // Raw, not typed: the SDK's typed parse rejects `async_task_*` and any
        // kind it doesn't know, then logs the whole frame with its payload.
        // This claims every `session/update` and records what it can't use
        // (`decode_session_update`). Everything else is declined unchanged,
        // never with `Err` (which would end the chain) or `retry` (which parks
        // it for a handler core never adds). Handlers share one dispatch
        // task, so sequences stay in order.
        .on_receive_notification(
            async move |notification: UntypedMessage, connection: ConnectionTo<Agent>| {
                if notification.method != "session/update" {
                    return Ok(Handled::No {
                        message: (notification, connection),
                        retry: false,
                    });
                }
                let translation = update_subagents.translate(&notification.params);
                for (session_id, update) in translation.updates {
                    update_shared.send_session_update(session_id, devices(update), None);
                }
                if translation.consumed {
                    return Ok(Handled::Yes);
                }
                if let Some((session_id, update)) =
                    decode_session_update(&update_agent_id.0, &notification.params)
                {
                    update_shared.send_session_update(
                        session_id,
                        devices(update),
                        Some(&notification.params),
                    );
                }
                Ok(Handled::Yes)
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, connection| {
                let options = request
                    .options
                    .iter()
                    .map(|option| HarnessPermissionOption {
                        option_id: option.option_id.0.to_string(),
                        name: option.name.clone(),
                        kind: match option.kind {
                            agent_client_protocol::schema::v1::PermissionOptionKind::AllowOnce => {
                                HarnessPermissionOptionKind::AllowOnce
                            }
                            agent_client_protocol::schema::v1::PermissionOptionKind::AllowAlways => {
                                HarnessPermissionOptionKind::AllowAlways
                            }
                            agent_client_protocol::schema::v1::PermissionOptionKind::RejectOnce => {
                                HarnessPermissionOptionKind::RejectOnce
                            }
                            agent_client_protocol::schema::v1::PermissionOptionKind::RejectAlways => {
                                HarnessPermissionOptionKind::RejectAlways
                            }
                            _ => HarnessPermissionOptionKind::Other,
                        },
                    })
                    .collect::<Vec<_>>();
                let allowed_option_ids = options
                    .iter()
                    .map(|option| option.option_id.clone())
                    .collect::<Vec<_>>();
                let mut tool_call = to_json(&request.tool_call);
                if omp {
                    tool_call = super::xd_devices::rewrite(tool_call);
                }
                let mut session_id = HarnessSessionId::new(request.session_id.0.to_string());
                if let Some(children) = &permission_shared.child_sessions {
                    session_id = children.permission_session(session_id, &mut tool_call);
                }
                // The user may take minutes to answer. Waiting here would hold
                // the SDK's dispatch loop, so every later message from the
                // agent, including replies to core's own requests (a live mode
                // switch), would queue behind the card.
                let broker = Arc::clone(&permission_broker);
                let withdrawn = tokio_util::sync::CancellationToken::new();
                let request = HarnessPermissionRequest {
                    agent_id: permission_agent_id.clone(),
                    session_id,
                    tool_call,
                    options,
                    withdrawn: withdrawn.clone(),
                    auto_approvable,
                };
                connection.spawn(async move {
                    // Requests wait without a timeout, so an agent withdrawing
                    // its request is what releases one it gave up on.
                    let cancellation = responder.cancellation();
                    let asking = broker.request_permission(request);
                    tokio::pin!(asking);
                    let outcome = tokio::select! {
                        outcome = &mut asking => outcome,
                        () = cancellation.cancelled() => {
                            // The broker settles the panel and the record.
                            withdrawn.cancel();
                            asking.await
                        }
                    };
                    // A spawned task's error shuts the connection down; a
                    // reply the agent can no longer take is not worth that.
                    let result = if cancellation.is_cancelled() {
                        responder.respond_with_result(Err(agent_client_protocol::Error::request_cancelled()))
                    } else {
                        let outcome = validate_permission_outcome(outcome, &allowed_option_ids);
                        responder.respond(RequestPermissionResponse::new(outcome))
                    };
                    if let Err(error) = result {
                        tracing::warn!(%error, "could not answer the agent's permission request");
                    }
                    Ok(())
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        // Last: claim whatever every handler above declined. The SDK's v1
        // default retries anything carrying a `sessionId` until a dynamic
        // handler is added, and core adds none, so an unclaimed request would
        // never be answered and the adapter's turn would hang. Requests get
        // the `method_not_found` the SDK sends for sessionless ones;
        // notifications are dropped; both are recorded as unhandled.
        // Responses pass on to their awaiting request. This runs before
        // dynamic handlers, so it goes if core adopts the SDK's session
        // helpers.
        .on_receive_dispatch(
            async move |dispatch: Dispatch<UntypedMessage, UntypedMessage>,
                        connection: ConnectionTo<Agent>| match dispatch {
                // Raw, so the AIR markers on each property survive. The answer
                // waits on the user, so it waits off the dispatch loop.
                Dispatch::Request(request, responder) if request.method == "elicitation/create" => {
                    let form = match elicitation::parse_form(&request.params, omp) {
                        Ok(form) => form,
                        Err(refusal) => {
                            record_unhandled(&unclaimed_agent_id.0, refusal.label(), || {
                                shape(&request.params)
                            });
                            return responder
                                .respond(elicitation::decline())
                                .map(|()| Handled::Yes);
                        }
                    };
                    let mut title = form.title.clone();
                    let mut session_id = HarnessSessionId::new(form.session_id.clone());
                    if let Some(children) = &question_shared.child_sessions {
                        session_id = children.question_session(
                            session_id,
                            form.tool_call_id.as_deref(),
                            &mut title,
                        );
                    }
                    let withdrawn = tokio_util::sync::CancellationToken::new();
                    let question = HarnessQuestionRequest {
                        agent_id: unclaimed_agent_id.clone(),
                        session_id,
                        provider_tool_call_id: form.tool_call_id.clone(),
                        title,
                        questions: form.questions.clone(),
                        withdrawn: withdrawn.clone(),
                    };
                    let broker = Arc::clone(&question_broker);
                    connection
                        .spawn(async move {
                            let cancellation = responder.cancellation();
                            let asking = broker.ask_question(question);
                            tokio::pin!(asking);
                            let outcome = tokio::select! {
                                outcome = &mut asking => outcome,
                                () = cancellation.cancelled() => {
                                    // The broker settles the panel and the record.
                                    withdrawn.cancel();
                                    asking.await
                                }
                            };
                            let response = if cancellation.is_cancelled() {
                                Err(agent_client_protocol::Error::request_cancelled())
                            } else {
                                Ok(match outcome {
                                    HarnessQuestionOutcome::Submitted(answers) => {
                                        elicitation::accept(form.content(&answers))
                                    }
                                    HarnessQuestionOutcome::Skipped => match form_skip {
                                        FormSkip::Decline => elicitation::decline(),
                                        FormSkip::EmptyAccept => {
                                            elicitation::accept(Default::default())
                                        }
                                    },
                                    HarnessQuestionOutcome::Cancelled => elicitation::cancel(),
                                })
                            };
                            // The adapter may be gone; nothing else waits on this.
                            let _ = responder.respond_with_result(response);
                            Ok(())
                        })
                        .map(|()| Handled::Yes)
                }
                Dispatch::Request(request, responder) => {
                    record_unhandled(
                        &unclaimed_agent_id.0,
                        &format!("request:{}", request.method),
                        || shape(&request.params),
                    );
                    responder
                        .respond_with_error(
                            agent_client_protocol::Error::method_not_found().data(request.method),
                        )
                        .map(|()| Handled::Yes)
                }
                Dispatch::Notification(notification) => {
                    record_unhandled(
                        &unclaimed_agent_id.0,
                        &format!("notification:{}", notification.method),
                        || shape(&notification.params),
                    );
                    Ok(Handled::Yes)
                }
                response @ Dispatch::Response(..) => Ok(Handled::No {
                    message: response,
                    retry: false,
                }),
            },
            agent_client_protocol::on_receive_dispatch!(),
        )
        .connect_with(transport, async move |connection: ConnectionTo<Agent>| {
            let initializing = connection.send_request(initialize_wire()).block_task();
            tokio::pin!(initializing);
            // Startup can hold the registry's admission gate. EOF shutdown
            // must reach this actor even if initialize never answers.
            let initialization = 'initialize: loop {
                tokio::select! {
                    result = &mut initializing => break result,
                    command = command_rx.recv() => match command {
                        Some(AdapterCommand::Shutdown { mut reply }) => {
                            while super::super::refresh_lock::is_refreshing(refresh_lock_path.as_deref()) {
                                tokio::select! {
                                    _ = reply.closed() => continue 'initialize,
                                    _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                                }
                            }
                            if reply.send(()).is_ok() { return Ok(()) }
                        }
                        None => {
                            super::super::refresh_lock::wait_for_release(refresh_lock_path.as_deref()).await;
                            return Ok(())
                        }
                        Some(_) => {} // only Shutdown is admitted before Ready
                    }
                }
            };
            let initialization = match initialization {
                Ok(initialization) => initialization,
                Err(error) => {
                    super::super::refresh_lock::wait_for_release(refresh_lock_path.as_deref()).await;
                    send_ready(&connection_ready_tx, Err(map_acp_error(&error)));
                    return Err(error);
                }
            };
            let ready = BackendReady {
                auth_methods: initialization
                    .auth_methods
                    .iter()
                    .map(normalize_auth_method)
                    .collect(),
                agent_name: initialization
                    .agent_info
                    .as_ref()
                    .map(|info| info.name.clone()),
                agent_version: initialization
                    .agent_info
                    .as_ref()
                    .map(|info| info.version.clone()),
                capabilities: {
                    // The named bits are views onto the advertised map, so
                    // they cannot disagree with it. `steering` keeps its
                    // three-way answer because the map only records the
                    // native case.
                    let agent_advertises = advertised_features(&initialization);
                    let advertised =
                        |feature| agent_advertises.get(&feature).copied().unwrap_or(false);
                    HarnessCapabilities {
                        auth_status: advertised(AcpFeature::AuthStatusUpdate),
                        load_session: advertised(AcpFeature::LoadSession),
                        delete_session: advertised(AcpFeature::SessionDelete),
                        steering: steering_capability(initialization.meta.as_ref()),
                        prompt_image: advertised(AcpFeature::PromptImage),
                        agent_advertises,
                    }
                },
            };
            let status = loop {
                match connection_shared.set_ready(&ready) {
                    Ok(status) => break status,
                    Err(error) => {
                        super::super::refresh_lock::wait_for_release(refresh_lock_path.as_deref()).await;
                        // Cancel may restore Starting while a refresh fences
                        // teardown. Reuse the initialization on this transport.
                        if connection_shared.status().lifecycle != AdapterLifecycleState::Starting {
                            send_ready(&connection_ready_tx, Err(error.clone()));
                            return Err(harness_to_acp_error(error));
                        }
                    }
                }
            };
            send_ready(&connection_ready_tx, Ok(status));
            let result = run_command_loop(&connection, &mut command_rx, &connection_shared, refresh_lock_path.as_deref()).await;
            super::super::refresh_lock::wait_for_release(refresh_lock_path.as_deref()).await;
            result.map_err(harness_to_acp_error)
        })
        .await;

    // The SDK may cancel its transport future rather than awaiting it. Reap
    // the returned child before publishing Dead/Stopped or replacing it.
    let exited = match child_return.await {
        Ok(mut child) => child.finish().await,
        Err(_) => Ok(()), // managed transport, or spawn failed before ownership
    };
    let result = result.and(exited);

    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            let mapped = map_acp_error(&error);
            send_ready(&ready_tx, Err(mapped.clone()));
            Err(mapped)
        }
    }
}

/// The wire operations used by the command actor. Keeping the actor generic
/// over this narrow boundary lets tests drive the production command loop with
/// controlled request completions instead of maintaining a second fake loop.
type ObservedPromptResult = Result<
    (
        StopReason,
        Option<ottam_protocol::ConversationSessionSignal>,
    ),
    agent_client_protocol::Error,
>;
type ObservedPromptFuture<'a> = Pin<Box<dyn Future<Output = ObservedPromptResult> + Send + 'a>>;

/// Runs when the prompt's response arrives, with the response's session
/// signal, before the connection dispatches anything the agent sent after the
/// response. Whatever it publishes is therefore ordered ahead of the agent's
/// later updates.
pub(super) type PromptSettledHook = Box<
    dyn FnOnce(Option<&ottam_protocol::ConversationSessionSignal>, Option<HarnessTurnUsage>)
        + Send,
>;

pub(super) type CommandLoadSessionResponse = Option<Vec<SessionConfigOption>>;

pub(super) trait AcpCommandConnection:
    SessionConfigSetter + Clone + Send + Sync + 'static
{
    fn authenticate(
        &self,
        method_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), agent_client_protocol::Error>> + Send + '_>>;

    fn new_session(
        &self,
        cwd: PathBuf,
        mcp_servers: Vec<McpServerEntry>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<CommandNewSessionResponse, agent_client_protocol::Error>>
                + Send
                + '_,
        >,
    >;

    fn load_session(
        &self,
        session_id: String,
        cwd: PathBuf,
        mcp_servers: Vec<McpServerEntry>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<CommandLoadSessionResponse, agent_client_protocol::Error>>
                + Send
                + '_,
        >,
    >;

    fn delete_session(
        &self,
        session_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), agent_client_protocol::Error>> + Send + '_>>;

    fn close_session(
        &self,
        session_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), agent_client_protocol::Error>> + Send + '_>>;

    fn prompt(
        &self,
        session_id: String,
        blocks: Vec<ContentBlock>,
    ) -> Pin<Box<dyn Future<Output = Result<StopReason, agent_client_protocol::Error>> + Send + '_>>;

    fn observed_prompt(
        &self,
        session_id: String,
        blocks: Vec<ContentBlock>,
        settled: PromptSettledHook,
    ) -> ObservedPromptFuture<'_> {
        Box::pin(async move {
            let result = self
                .prompt(session_id, blocks)
                .await
                .map(|reason| (reason, None));
            settled(None, None);
            result
        })
    }

    fn steer(
        &self,
        session_id: String,
        blocks: Vec<ContentBlock>,
    ) -> Pin<Box<dyn Future<Output = Result<SteerOutcome, agent_client_protocol::Error>> + Send + '_>>;

    fn stop_async_task(
        &self,
        session_id: String,
        async_task_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<bool, agent_client_protocol::Error>> + Send + '_>>;

    fn cancel(&self, session_id: String) -> Result<(), agent_client_protocol::Error>;
}

/// Every entry is a path on core's proxy, so every one goes out as http.
pub(super) fn acp_mcp_servers(entries: Vec<McpServerEntry>) -> Vec<McpServer> {
    entries
        .into_iter()
        .map(|entry| {
            let mut server = McpServerHttp::new(entry.name, entry.url);
            server.headers = entry
                .headers
                .into_iter()
                .map(|(name, value)| HttpHeader::new(name, value))
                .collect();
            McpServer::Http(server)
        })
        .collect()
}

pub(super) struct CommandNewSessionResponse {
    pub(super) session_id: String,
    pub(super) config_options: Option<Vec<SessionConfigOption>>,
}

impl AcpCommandConnection for ConnectionTo<Agent> {
    fn authenticate(
        &self,
        method_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), agent_client_protocol::Error>> + Send + '_>> {
        Box::pin(async move {
            self.send_request(AuthenticateRequest::new(method_id))
                .block_task()
                .await
                .map(|_| ())
        })
    }

    fn new_session(
        &self,
        cwd: PathBuf,
        mcp_servers: Vec<McpServerEntry>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<CommandNewSessionResponse, agent_client_protocol::Error>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            self.send_request(NewSessionRequest::new(cwd).mcp_servers(acp_mcp_servers(mcp_servers)))
                .block_task()
                .await
                .map(|response| CommandNewSessionResponse {
                    session_id: response.session_id.0.to_string(),
                    config_options: response.config_options,
                })
        })
    }

    fn load_session(
        &self,
        session_id: String,
        cwd: PathBuf,
        mcp_servers: Vec<McpServerEntry>,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<Option<Vec<SessionConfigOption>>, agent_client_protocol::Error>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            self.send_request(
                LoadSessionRequest::new(session_id, cwd).mcp_servers(acp_mcp_servers(mcp_servers)),
            )
            .block_task()
            .await
            .map(|response| response.config_options)
        })
    }

    fn delete_session(
        &self,
        session_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), agent_client_protocol::Error>> + Send + '_>> {
        Box::pin(async move {
            self.send_request(DeleteSessionRequest::new(session_id))
                .block_task()
                .await
                .map(|_| ())
        })
    }

    fn close_session(
        &self,
        session_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), agent_client_protocol::Error>> + Send + '_>> {
        Box::pin(async move {
            self.send_request(CloseSessionRequest::new(session_id))
                .block_task()
                .await
                .map(|_| ())
        })
    }

    fn prompt(
        &self,
        session_id: String,
        blocks: Vec<ContentBlock>,
    ) -> Pin<Box<dyn Future<Output = Result<StopReason, agent_client_protocol::Error>> + Send + '_>>
    {
        Box::pin(async move {
            self.send_request(PromptRequest::new(session_id, blocks))
                .block_task()
                .await
                .map(|response| response.stop_reason)
        })
    }

    fn observed_prompt(
        &self,
        session_id: String,
        blocks: Vec<ContentBlock>,
        settled: PromptSettledHook,
    ) -> ObservedPromptFuture<'_> {
        Box::pin(async move {
            let (observed_tx, observed_rx) = oneshot::channel();
            let signal_session = session_id.clone();
            // Ordered consumption: the connection finishes this callback
            // before it dispatches the agent's next frame, which is what puts
            // the hook's output ahead of anything sent after the response.
            self.send_request(PromptRequest::new(session_id, blocks))
                .on_receiving_result(async move |result| {
                    let usage = result.as_ref().ok().and_then(|response| {
                        response
                            .usage
                            .as_ref()
                            // claude-agent-acp answers a turn that failed
                            // before the model ran, or a cancelled one, with
                            // all zeros: nothing was reported.
                            .filter(|usage| usage.total_tokens > 0)
                            .map(|usage| HarnessTurnUsage {
                                cancelled: response.stop_reason == StopReason::Cancelled,
                                ..turn_usage(usage)
                            })
                    });
                    let observed = result.and_then(|response| {
                        let mut signal = crate::agent::metadata::session_signal(
                            &serde_json::json!({"_meta": response.meta}),
                        );
                        if let Some(signal) = &mut signal {
                            signal.session_id = signal_session;
                        }
                        // Never published: the run reports Ottam's message.
                        match super::protocol::refresh_lock_turn(
                            response.stop_reason,
                            signal.as_ref(),
                        ) {
                            Some(error) => Err(error),
                            None => Ok((response.stop_reason, signal)),
                        }
                    });
                    settled(
                        observed
                            .as_ref()
                            .ok()
                            .and_then(|(_, signal)| signal.as_ref()),
                        usage,
                    );
                    let _ = observed_tx.send(observed);
                    Ok(())
                })?;
            observed_rx.await.map_err(|_| {
                agent_client_protocol::util::internal_error("prompt response was dropped")
            })?
        })
    }

    fn steer(
        &self,
        session_id: String,
        blocks: Vec<ContentBlock>,
    ) -> Pin<Box<dyn Future<Output = Result<SteerOutcome, agent_client_protocol::Error>> + Send + '_>>
    {
        Box::pin(async move {
            self.send_request(SteerRequest::new(session_id, blocks))
                .block_task()
                .await
                .map(|response| response.outcome())
        })
    }

    fn stop_async_task(
        &self,
        session_id: String,
        async_task_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<bool, agent_client_protocol::Error>> + Send + '_>> {
        Box::pin(async move {
            self.send_request(StopAsyncTaskRequest {
                session_id,
                async_task_id,
            })
            .block_task()
            .await
            .map(|response| response.stopped)
        })
    }

    fn cancel(&self, session_id: String) -> Result<(), agent_client_protocol::Error> {
        self.send_notification(CancelNotification::new(session_id))
    }
}

impl SessionConfigSetter for ConnectionTo<Agent> {
    fn set_session_config(
        &self,
        request: SetSessionConfigOptionRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<SessionConfigOption>, agent_client_protocol::Error>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            self.send_request(request)
                .block_task()
                .await
                .map(|response| response.config_options)
        })
    }
}

/// Publishes the response's observations first, so a run's update fence
/// drains them before finalization, and then the turn boundary.
fn prompt_settled_hook(
    shared: Arc<SharedState>,
    session_id: HarnessSessionId,
) -> PromptSettledHook {
    Box::new(move |signal, usage| {
        if let Some(signal) = signal {
            shared.send_update(
                session_id.clone(),
                HarnessUpdateKind::SessionSignal(signal.clone()),
            );
        }
        if let Some(usage) = usage {
            shared.send_update(session_id.clone(), HarnessUpdateKind::TurnUsage(usage));
        }
        shared.send_update(session_id, HarnessUpdateKind::PromptSettled);
    })
}

pub(super) fn turn_usage(usage: &agent_client_protocol::schema::v1::Usage) -> HarnessTurnUsage {
    HarnessTurnUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        total_tokens: usage.total_tokens,
        thought_tokens: usage.thought_tokens,
        cached_read_tokens: usage.cached_read_tokens,
        cached_write_tokens: usage.cached_write_tokens,
        cancelled: false,
    }
}

/// How long shutdown waits for cancelled prompts to answer. Inside the
/// adapter's grace period for the actor to finish (`adapter.rs`).
#[cfg(not(test))]
const SHUTDOWN_PROMPT_DRAIN: Duration = Duration::from_millis(1500);
#[cfg(test)]
const SHUTDOWN_PROMPT_DRAIN: Duration = Duration::from_millis(30);

struct PromptTasks {
    tasks: JoinSet<PromptCompletion>,
    replies: BTreeMap<HarnessSessionId, oneshot::Sender<Result<HarnessStopReason, HarnessError>>>,
}

impl PromptTasks {
    fn new() -> Self {
        Self {
            tasks: JoinSet::new(),
            replies: BTreeMap::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    fn start(
        &mut self,
        connection: &impl AcpCommandConnection,
        shared: &Arc<SharedState>,
        session_id: HarnessSessionId,
        prompt: HarnessPrompt,
        reply: oneshot::Sender<Result<HarnessStopReason, HarnessError>>,
    ) {
        match shared.begin_prompt(&session_id) {
            Ok(()) => {
                let connection = connection.clone();
                self.replies.insert(session_id.clone(), reply);
                let settled = prompt_settled_hook(Arc::clone(shared), session_id.clone());
                self.tasks.spawn(async move {
                    let blocks = match prompt_content_blocks(prompt).await {
                        Ok(blocks) => blocks,
                        Err(error) => {
                            return PromptCompletion {
                                session_id,
                                result: Err(error),
                            };
                        }
                    };
                    let result = connection
                        .observed_prompt(session_id.0.clone(), blocks, settled)
                        .await
                        .map_err(|error| map_prompt_execution_error(&error))
                        .and_then(super::protocol::map_observed_prompt);
                    PromptCompletion { session_id, result }
                });
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    async fn join_next(&mut self) -> Option<Result<PromptCompletion, tokio::task::JoinError>> {
        self.tasks.join_next().await
    }

    fn reconcile(
        &mut self,
        completion: Option<Result<PromptCompletion, tokio::task::JoinError>>,
        shared: &SharedState,
    ) {
        if let Some(Ok(completion)) = completion {
            if let Some(reply) = self.replies.remove(&completion.session_id) {
                // The response's observations and the turn boundary are
                // already published (see `PromptSettledHook`).
                shared.finish_prompt(&completion.session_id, &completion.result);
                match &completion.result {
                    Ok(_) => shared.set_auth(HarnessAuthState::Ready, None),
                    Err(error) if error.kind == HarnessErrorKind::AuthRequired => {
                        shared.set_auth(HarnessAuthState::Missing, Some(error.message.clone()));
                    }
                    Err(error) => {
                        shared.set_auth(HarnessAuthState::Unknown, Some(error.message.clone()));
                    }
                }
                let _ = reply.send(completion.result);
            }
        }
    }

    fn cancel_remote_for_shutdown(
        &self,
        connection: &impl AcpCommandConnection,
        shared: &SharedState,
    ) {
        for session_id in shared.prompting_sessions() {
            let _ = connection.cancel(session_id.0);
        }
    }

    /// Lets cancelled prompts answer before the connection closes, so each
    /// turn ends on the agent's own `stopReason`. An agent that drops
    /// requests still in flight when stdin closes would otherwise lose the
    /// turn without an answer. Whatever is still open at the deadline is
    /// answered `Cancelled` by [`Self::reply_cancelled_for_shutdown`].
    async fn drain_for_shutdown(&mut self, shared: &SharedState) {
        let deadline = tokio::time::sleep(SHUTDOWN_PROMPT_DRAIN);
        tokio::pin!(deadline);
        while !self.is_empty() {
            tokio::select! {
                _ = &mut deadline => break,
                completion = self.join_next() => self.reconcile(completion, shared),
            }
        }
    }

    fn reply_cancelled_for_shutdown(&mut self, shared: &SharedState) {
        for (session_id, prompt_reply) in std::mem::take(&mut self.replies) {
            let stopped = Ok(HarnessStopReason::Cancelled);
            shared.finish_prompt(&session_id, &stopped);
            let _ = prompt_reply.send(stopped);
        }
    }

    fn abort_all(&mut self) {
        self.tasks.abort_all();
    }
}

/// Extension requests in flight (steers, background-task stops, and live mode
/// switches), kept only so the loop owns them: each answers its own caller from
/// inside the task. Only a mode switch touches session state on the way back,
/// recording the mode the agent acknowledged.
///
/// Spawned rather than awaited inline because the adapters make these calls
/// slow. codex serialises steers per session behind its own queue and, when it
/// misses the running turn, waits out the previous prompt before answering;
/// awaiting here would put Stop behind that wait.
struct ExtensionTasks {
    tasks: JoinSet<()>,
}

impl ExtensionTasks {
    fn new() -> Self {
        Self {
            tasks: JoinSet::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    fn start(
        &mut self,
        connection: &impl AcpCommandConnection,
        shared: &Arc<SharedState>,
        session_id: HarnessSessionId,
        prompt: HarnessPrompt,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    ) {
        // Nothing goes on the wire for a session that is not prompting. This
        // is load-bearing rather than an optimization: codex-acp drops the
        // `_meta` that asks for the message back, so an idle session there
        // answers a steer by starting a turn nobody is waiting on.
        match shared.is_prompting(&session_id) {
            Ok(true) => {}
            Ok(false) => {
                let _ = reply.send(Err(HarnessError::new(
                    HarnessErrorKind::NoActivePrompt,
                    format!("session {session_id} has no active prompt to steer"),
                )));
                return;
            }
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        }
        let connection = connection.clone();
        let shared = Arc::clone(shared);
        self.tasks.spawn(async move {
            let blocks = match prompt_content_blocks(prompt).await {
                Ok(blocks) => blocks,
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            };
            let outcome = connection
                .steer(session_id.0.clone(), blocks)
                .await
                .map_err(|error| map_steer_error(&error));
            let result = match outcome {
                Ok(SteerOutcome::Injected) => Ok(()),
                Ok(SteerOutcome::PromptRequired) => Err(HarnessError::new(
                    HarnessErrorKind::NoActivePrompt,
                    format!("session {session_id} settled its turn before the steer landed"),
                )),
                Ok(SteerOutcome::StartedNewTurn) => {
                    // The turn settled under us and the agent answered by
                    // starting one of its own — with the user's message in it.
                    // No run owns that turn, so end it, best effort.
                    //
                    // `session/cancel` is session-scoped, not turn-scoped, so
                    // this is only safe because the answer itself says the
                    // earlier turn is over: an agent reaches this outcome only
                    // after finding no turn to inject into. The detached turn
                    // is the only one left to cancel.
                    let _ = connection.cancel(session_id.0.clone());
                    // Ambiguous, not undelivered. The cancel is an
                    // unacknowledged notification racing a turn that already
                    // holds the message, so tools may have run before it
                    // landed. Reporting this as never-sent would send it a
                    // second time, and a steer is exactly the kind of
                    // instruction that must not run twice.
                    Err(HarnessError {
                        detached_turn: true,
                        ..HarnessError::new(
                            HarnessErrorKind::PromptExecution,
                            format!(
                                "session {session_id} settled its turn and started a detached one for the steer"
                            ),
                        )
                    })
                }
                // codex-acp's `failed` is a catch-all around an unexpected
                // throw part-way through delivering, so whether the message
                // reached the turn is genuinely unknown. `PromptExecution` is
                // this crate's name for that — an error during turn execution
                // that callers must not read as a definitive rejection. An
                // outcome this client has never heard of is no more decidable.
                Ok(SteerOutcome::Unrecognized(outcome)) => Err(HarnessError::new(
                    HarnessErrorKind::PromptExecution,
                    format!("the agent answered the steer with {outcome}"),
                )),
                Err(error) => {
                    if error.kind == HarnessErrorKind::SteeringUnsupported {
                        shared.demote_steering();
                    }
                    Err(error)
                }
            };
            let _ = reply.send(result);
        });
    }

    /// A stop answers from its own task too: the agent stops a shell or a
    /// subagent before it replies, and Stop must not wait behind that.
    fn stop_async_task(
        &mut self,
        connection: &impl AcpCommandConnection,
        session_id: HarnessSessionId,
        async_task_id: String,
        reply: oneshot::Sender<Result<bool, HarnessError>>,
    ) {
        let connection = connection.clone();
        self.tasks.spawn(async move {
            let result = connection
                .stop_async_task(session_id.0, async_task_id)
                .await
                .map_err(|error| map_acp_error(&error));
            let _ = reply.send(result);
        });
    }

    /// A mode switch can land mid-turn, where the adapter answers from behind
    /// its own query; awaiting it inline would hold Cancel behind that answer.
    /// The timeout bounds a switch the adapter never acknowledges.
    fn apply_mode(
        &mut self,
        connection: &impl AcpCommandConnection,
        shared: &Arc<SharedState>,
        session_id: HarnessSessionId,
        select: Option<ConfigSelect>,
        mode_id: String,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    ) {
        let connection = connection.clone();
        let shared = Arc::clone(shared);
        self.tasks.spawn(async move {
            let applied = tokio::time::timeout(
                APPLY_MODE_TIMEOUT,
                apply_live_mode(&connection, &session_id, select.as_ref(), &mode_id),
            )
            .await
            .unwrap_or_else(|_| {
                Err(HarnessError::new(
                    HarnessErrorKind::SelectionUnavailable,
                    format!("The agent did not confirm permission mode \"{mode_id}\" in time. Try again."),
                ))
            });
            let result = applied.map(|()| shared.set_session_mode(&session_id, Some(mode_id)));
            let _ = reply.send(result);
        });
    }

    async fn join_next(&mut self) -> Option<Result<(), tokio::task::JoinError>> {
        self.tasks.join_next().await
    }

    fn abort_all(&mut self) {
        self.tasks.abort_all();
    }
}

const APPLY_MODE_TIMEOUT: Duration = Duration::from_secs(15);

struct AuthenticationTasks {
    tasks: JoinSet<AuthCompletion>,
    attempts: BTreeMap<String, AuthAttempt>,
}

impl AuthenticationTasks {
    fn new() -> Self {
        Self {
            tasks: JoinSet::new(),
            attempts: BTreeMap::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    fn start(
        &mut self,
        connection: &impl AcpCommandConnection,
        shared: &Arc<SharedState>,
        method_id: String,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    ) {
        // Two callers can reach this at once — the failed-run card and
        // the engine picker both offer the same button. A second attempt
        // on a method already in flight is the user asking again, not a
        // new login, so refuse it rather than open a second browser tab.
        if self.attempts.contains_key(&method_id) {
            let _ = reply.send(Err(HarnessError::new(
                HarnessErrorKind::SessionBusy,
                format!("authentication for {method_id:?} is already in progress"),
            )));
            return;
        }
        let previous_auth_state = shared.status().auth_state;
        shared.set_auth(HarnessAuthState::Authenticating, None);
        let connection = connection.clone();
        let shared = Arc::clone(shared);
        let spawned_method_id = method_id.clone();
        let abort = self.tasks.spawn(async move {
            let result = connection
                .authenticate(spawned_method_id.clone())
                .await
                .map_err(|error| map_and_record_auth_error(&shared, &error));
            AuthCompletion {
                method_id: spawned_method_id,
                result,
            }
        });
        self.attempts.insert(
            method_id,
            AuthAttempt {
                reply,
                abort,
                previous_auth_state,
            },
        );
    }

    fn cancel(&mut self, shared: &SharedState, method_id: &str) -> Result<(), HarnessError> {
        // ACP has no cancel for `authenticate`, so this abandons the
        // local future while the request stays outstanding on the
        // adapter side. That is tolerable only because the loop is no
        // longer blocked on it: the orphaned request costs nothing but
        // an ignored reply, and dies with the adapter process.
        match self.attempts.remove(method_id) {
            Some(attempt) => {
                attempt.abort.abort();
                // A notification may have settled sign-in before the RPC
                // completes. Cancel only the local pending state, atomically
                // preserving any newer verdict delivered by the adapter.
                shared.status_tx.send_modify(|status| {
                    if status.auth_state == HarnessAuthState::Authenticating {
                        status.auth_state = attempt.previous_auth_state;
                        status.last_error = None;
                    }
                });
                let _ = attempt.reply.send(Err(HarnessError::new(
                    HarnessErrorKind::RequestCancelled,
                    "authentication cancelled",
                )));
                Ok(())
            }
            // Idempotent: a login that finished on its own moments
            // before the user hit cancel is not an error.
            None => Ok(()),
        }
    }

    async fn join_next(&mut self) -> Option<Result<AuthCompletion, tokio::task::JoinError>> {
        self.tasks.join_next().await
    }

    fn reconcile(
        &mut self,
        completion: Option<Result<AuthCompletion, tokio::task::JoinError>>,
        shared: &SharedState,
    ) {
        if let Some(Ok(completion)) = completion {
            if let Some(attempt) = self.attempts.remove(&completion.method_id) {
                // Failures already recorded their own state in
                // `map_and_record_auth_error` on the spawned task.
                // Under `ModelSelector` a method can succeed without adding a
                // provider; the next session's models decide.
                if completion.result.is_ok() && shared.sign_in_proof == SignInProof::Session {
                    shared.set_auth(HarnessAuthState::Ready, None);
                }
                let _ = attempt.reply.send(completion.result);
            }
        }
    }

    fn reply_cancelled_for_shutdown(&mut self) {
        // Sign-ins waiting on a browser lose their process, so answer
        // them here rather than let `request` report the adapter as
        // having exited before replying.
        for (_, attempt) in std::mem::take(&mut self.attempts) {
            let _ = attempt.reply.send(Err(HarnessError::new(
                HarnessErrorKind::RequestCancelled,
                "adapter shut down during authentication",
            )));
        }
    }

    fn abort_all(&mut self) {
        self.tasks.abort_all();
    }
}

/// Switch a held session's model and level before its next prompt. Awaited on
/// the command loop like `session/new`'s own config requests: the run waits on
/// it anyway, and no prompt is in flight on this session. The timeout bounds a
/// switch the agent never acknowledges.
///
/// A failed switch still records what the agent took before the failure, and
/// forgets what it may have taken unconfirmed, so the next run on this session
/// compares against the agent's real model.
async fn apply_model(
    connection: &impl AcpCommandConnection,
    shared: &SharedState,
    session_selects: &mut BTreeMap<HarnessSessionId, SessionSelects>,
    session_id: &HarnessSessionId,
    switch: &ModelSwitch,
) -> Result<(), HarnessError> {
    let Some(selects) = session_selects.get(session_id) else {
        // Nothing advertised for this session: no select to switch.
        return Ok(());
    };
    let confirmed = shared
        .sessions()
        .into_iter()
        .find(|session| &session.session_id == session_id);
    let (model, level) = confirmed
        .map(|session| (session.current_model_id, session.current_thought_level_id))
        .unwrap_or_default();
    let Ok(applied) = tokio::time::timeout(
        APPLY_MODE_TIMEOUT,
        apply_live_model(connection, session_id, selects, model.as_deref(), level.as_deref(), switch),
    )
    .await
    else {
        // The agent may yet take what went out.
        shared.set_session_model(session_id, None, None);
        return Err(HarnessError::new(
            HarnessErrorKind::SelectionUnavailable,
            "The agent did not confirm the model change in time. Try again.",
        ));
    };
    if applied.switched {
        shared.record_model_selects(applied.selects.model_selects());
        // The new model's options can reset the mode; the caller compares
        // its pick against this before prompting.
        if let Some(mode) = &applied.selects.mode {
            shared.set_session_mode(session_id, Some(mode.current_value.clone()));
        }
    }
    shared.set_session_model(session_id, applied.model, applied.thought_level);
    session_selects.insert(session_id.clone(), applied.selects);
    applied.outcome
}

/// Before reloading a session the adapter still holds, close it, so the load
/// applies the session's new config: Codex ignores it for a thread it still
/// has loaded. Core does not reload held sessions today; this keeps a future
/// path from silently keeping stale MCP entries. Without `session/close`,
/// the load goes ahead as before.
async fn close_held_session(
    connection: &impl AcpCommandConnection,
    shared: &SharedState,
    session_selects: &mut BTreeMap<HarnessSessionId, SessionSelects>,
    session_id: &HarnessSessionId,
) {
    let held = shared.sessions().iter().any(|session| {
        session.session_id == *session_id && session.state != HarnessSessionState::Dead
    });
    let can_close = shared
        .status()
        .capabilities
        .agent_advertises
        .get(&AcpFeature::SessionClose)
        .copied()
        .unwrap_or(false);
    if !held || !can_close {
        return;
    }
    match connection.close_session(session_id.0.clone()).await {
        Ok(()) => {
            session_selects.remove(session_id);
            shared.remove_session(session_id);
        }
        Err(error) => {
            tracing::warn!(code = ?error.code, "session/close before a reload failed; loading anyway");
        }
    }
}

pub(super) async fn run_command_loop(
    connection: &impl AcpCommandConnection,
    commands: &mut mpsc::Receiver<AdapterCommand>,
    shared: &Arc<SharedState>,
    refresh_lock_path: Option<&std::path::Path>,
) -> Result<(), HarnessError> {
    let session_proves_auth = !shared.status().capabilities.auth_status;
    let mut prompts = PromptTasks::new();
    // `authenticate` is spawned rather than awaited inline for the same reason
    // `prompt` is: an adapter-executed browser login blocks until the human
    // finishes with the browser, and ACP gives it no timeout. Awaiting it here
    // would leave every other command — including the `new_session` the login
    // exists to unblock — queued behind a tab the user may never return to.
    let mut authentications = AuthenticationTasks::new();
    let mut extensions = ExtensionTasks::new();
    // Each held session's mode selector, for switching a live session.
    let mut session_selects = BTreeMap::<HarnessSessionId, SessionSelects>::new();
    'commands: loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    prompts.abort_all();
                    authentications.abort_all();
                    extensions.abort_all();
                    return Ok(());
                };
                match command {
                    AdapterCommand::Authenticate { method_id, reply } => {
                        authentications.start(connection, shared, method_id, reply);
                    }
                    AdapterCommand::CancelAuthenticate { method_id, reply } => {
                        let result = authentications.cancel(shared, &method_id);
                        let _ = reply.send(result);
                    }
                    AdapterCommand::NewSession { cwd, config, reply } => {
                        let result = match connection.new_session(cwd.clone(), config.mcp_servers.clone()).await
                        {
                            Ok(response) => {
                                let session_id = HarnessSessionId::new(response.session_id);
                                let selects = extract_session_selects(response.config_options.as_deref());
                                if let Some(select) = &selects.model {
                                    shared.set_models(
                                        select.options.iter().map(model_option).collect(),
                                        Some(select.current_value.clone()),
                                    );
                                }
                                shared.set_session_selects(selects.advertised());
                                shared.record_model_selects(selects.model_selects());
                                // Adapters with explicit auth status can open
                                // sessions without proving their credentials.
                                if session_proves_auth {
                                    record_session_sign_in(
                                        shared,
                                        &selects,
                                        response.config_options.is_some(),
                                    );
                                }
                                let applied = apply_session_config(connection, &session_id, &selects, &config).await;
                                // Recorded even when a later request is refused,
                                // so the caller's next pick sees the new model's levels.
                                if applied.switched {
                                    shared.record_model_selects(applied.selects.model_selects());
                                }
                                match applied.mode {
                                    Ok(mode) => {
                                        shared.insert_session(session_id.clone(), cwd);
                                        shared.set_session_mode(&session_id, mode);
                                        shared.set_session_model(&session_id, applied.model, applied.thought_level);
                                        session_selects.insert(session_id.clone(), applied.selects);
                                        Ok(session_id)
                                    }
                                    // The agent-side session is left to die with
                                    // the adapter: nothing was prompted into it,
                                    // and the models it advertised are already
                                    // recorded above for the caller's next pick.
                                    Err(error) => Err(error),
                                }
                            }
                            Err(error) => Err(map_and_record_auth_error(shared, &error)),
                        };
                        let _ = reply.send(result);
                    }
                    AdapterCommand::LoadSession { session_id, cwd, mode_id, mcp_servers, reply } => {
                        if let Err(error) = shared.ensure_loadable(&session_id) {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                        close_held_session(connection, shared, &mut session_selects, &session_id).await;
                        let result = match connection.load_session(session_id.0.clone(), cwd.clone(), mcp_servers).await {
                            Ok(options) => {
                                let selects = extract_session_selects(options.as_deref());
                                if options.is_some() {
                                    // A loaded session's level is a past pick, not
                                    // a default: keep the thought half from session/new.
                                    let mut advertised = selects.advertised();
                                    let previous = shared.status().session_selects;
                                    advertised.thought_levels = previous
                                        .as_ref()
                                        .map_or(advertised.thought_levels, |previous| previous.thought_levels.clone());
                                    advertised.default_thought_level_id =
                                        previous.and_then(|previous| previous.default_thought_level_id);
                                    shared.set_session_selects(advertised);
                                }
                                match apply_restored_mode(connection, &session_id, &selects, mode_id.as_deref()).await {
                                    Ok(mode) => {
                                        shared.insert_session(session_id.clone(), cwd);
                                        shared.set_session_mode(&session_id, mode);
                                        // What the load reported, so a run whose pick
                                        // differs switches before it prompts.
                                        shared.set_session_model(
                                            &session_id,
                                            selects.model.as_ref().map(|select| select.current_value.clone()),
                                            selects.thought_level().map(|select| select.current_value.clone()),
                                        );
                                        if session_proves_auth {
                                            record_session_sign_in(shared, &selects, options.is_some());
                                        }
                                        session_selects.insert(session_id.clone(), selects);
                                        Ok(())
                                    }
                                    Err(error) => Err(error),
                                }
                            }
                            Err(error) => Err(map_and_record_auth_error(shared, &error)),
                        };
                        let _ = reply.send(result);
                    }
                    AdapterCommand::DeleteSession { session_id, reply } => {
                        if let Err(error) = shared.ensure_loadable(&session_id) {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                        let result = connection
                            .delete_session(session_id.0.clone())
                            .await
                            .map(|_| {
                                session_selects.remove(&session_id);
                                shared.remove_session(&session_id)
                            })
                            .map_err(|error| map_acp_error(&error));
                        let _ = reply.send(result);
                    }
                    AdapterCommand::ApplyMode { session_id, mode_id, reply } => {
                        let select = session_selects.get(&session_id).and_then(|selects| selects.mode.clone());
                        extensions.apply_mode(connection, shared, session_id, select, mode_id, reply);
                    }
                    AdapterCommand::ApplyModel { session_id, switch, reply } => {
                        let result = apply_model(connection, shared, &mut session_selects, &session_id, &switch).await;
                        let _ = reply.send(result);
                    }
                    AdapterCommand::Prompt { session_id, prompt, reply } => {
                        prompts.start(connection, shared, session_id, prompt, reply);
                    }
                    AdapterCommand::Steer { session_id, prompt, reply } => {
                        extensions.start(connection, shared, session_id, prompt, reply);
                    }
                    AdapterCommand::Cancel { session_id, reply } => {
                        let result = match shared.is_prompting(&session_id) {
                            Ok(false) => Ok(()),
                            Ok(true) => {
                                let result = connection
                                    .cancel(session_id.0.clone())
                                    .map_err(|error| map_acp_error(&error));
                                // Cancellation is a notification, not an acknowledgement. Keep
                                // the prompt and session live until the agent resolves the prompt;
                                // otherwise a follow-up prompt can overlap work still running in
                                // the remote session.
                                result
                            }
                            Err(error) => Err(error),
                        };
                        let _ = reply.send(result);
                    }
                    AdapterCommand::CancelOutOfTurn { session_id, reply } => {
                        let result = match shared.is_prompting(&session_id) {
                            Ok(false) => connection
                                .cancel(session_id.0.clone())
                                .map_err(|error| map_acp_error(&error)),
                            Ok(true) => Ok(()),
                            Err(error) => Err(error),
                        };
                        let _ = reply.send(result);
                    }
                    AdapterCommand::StopAsyncTask { session_id, async_task_id, reply } => {
                        extensions.stop_async_task(connection, session_id, async_task_id, reply);
                    }
                    AdapterCommand::Shutdown { mut reply } => {
                        while super::super::refresh_lock::is_refreshing(refresh_lock_path) {
                            tokio::select! {
                                _ = reply.closed() => continue 'commands,
                                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                            }
                        }
                        // Sending the acknowledgement is the shutdown commit.
                        // A cancelled receiver must leave the actor untouched.
                        if reply.send(()).is_err() { continue 'commands }
                        prompts.cancel_remote_for_shutdown(connection, shared);
                        prompts.drain_for_shutdown(shared).await;
                        prompts.reply_cancelled_for_shutdown(shared);
                        authentications.reply_cancelled_for_shutdown();
                        prompts.abort_all();
                        authentications.abort_all();
                        // Extension requests answer from inside their own
                        // task, so an aborted one drops its reply sender and the caller
                        // reads the adapter as having exited mid-request —
                        // which is exactly what happened.
                        extensions.abort_all();
                        return Ok(());
                    }
                }
            }
            completion = prompts.join_next(), if !prompts.is_empty() => {
                prompts.reconcile(completion, shared);
            }
            completion = authentications.join_next(), if !authentications.is_empty() => {
                authentications.reconcile(completion, shared);
            }
            _ = extensions.join_next(), if !extensions.is_empty() => {}
        }
    }
}

struct PromptCompletion {
    session_id: HarnessSessionId,
    result: Result<HarnessStopReason, HarnessError>,
}

struct AuthCompletion {
    method_id: String,
    result: Result<(), HarnessError>,
}

/// A sign-in the loop has handed off to a task: who to answer, how to abandon
/// it, and the auth state to put back if it is abandoned — cancelling a login
/// should leave the adapter looking exactly as signed-out as it did before.
struct AuthAttempt {
    reply: oneshot::Sender<Result<(), HarnessError>>,
    abort: tokio::task::AbortHandle,
    previous_auth_state: HarnessAuthState,
}

/// Records what an opened session shows about sign-in. Under
/// [`SignInProof::ModelSelector`] a session that lists its options without a
/// model means no provider is set up; one that lists none says nothing.
fn record_session_sign_in(shared: &SharedState, selects: &SessionSelects, listed_options: bool) {
    match shared.sign_in_proof {
        SignInProof::Session => shared.set_auth(HarnessAuthState::Ready, None),
        SignInProof::ModelSelector if selects.model.is_some() => {
            shared.set_auth(HarnessAuthState::Ready, None)
        }
        SignInProof::ModelSelector if listed_options => shared.set_auth(
            HarnessAuthState::Missing,
            Some(NO_MODEL_PROVIDER_MESSAGE.to_string()),
        ),
        SignInProof::ModelSelector => {}
    }
}

const NO_MODEL_PROVIDER_MESSAGE: &str = "No model provider is set up. Sign in to add one.";

pub(super) fn map_and_record_auth_error(
    shared: &SharedState,
    error: &agent_client_protocol::Error,
) -> HarnessError {
    let error = map_acp_error(error);
    let state = if error.kind == HarnessErrorKind::AuthRequired {
        HarnessAuthState::Missing
    } else {
        HarnessAuthState::Unknown
    };
    shared.set_auth(state, Some(error.message.clone()));
    error
}

pub(super) fn send_ready(sender: &ReadySender, result: Result<AdapterStatus, HarnessError>) {
    if let Some(sender) = sender
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        let _ = sender.send(result);
    }
}

#[cfg(test)]
mod prompt_observation_tests {
    use super::*;

    #[tokio::test]
    async fn response_observations_and_the_turn_boundary_precede_completion() {
        let fixtures: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../fixtures/session-signals.json")).unwrap();
        for (fixture, reason) in [
            (Some(3), StopReason::EndTurn),
            (Some(4), StopReason::EndTurn),
            (Some(4), StopReason::Cancelled),
            (None, StopReason::EndTurn),
        ] {
            let signal = fixture
                .map(|fixture| crate::agent::metadata::session_signal(&fixtures[fixture]).unwrap());
            let (status_tx, _) = watch::channel(AdapterStatus::stopped());
            let updates_tx = HarnessUpdateSender::default();
            let mut updates = updates_tx.subscribe();
            let shared = Arc::new(SharedState {
                status_tx,
                updates_tx,
                sessions: Mutex::new(BTreeMap::new()),
                update_sequences: Mutex::new(BTreeMap::new()),
                announced_commands: Mutex::new(BTreeMap::new()),
                child_sessions: None,
                sign_in_proof: SignInProof::Session,
            });
            let session_id = HarnessSessionId("session".into());
            let (reply, response) = oneshot::channel();
            let mut tasks = PromptTasks::new();
            tasks.replies.insert(session_id.clone(), reply);
            // The connection runs the hook as the response arrives, before it
            // hands the result to the prompt task.
            prompt_settled_hook(Arc::clone(&shared), session_id.clone())(signal.as_ref(), None);
            let result = super::super::protocol::map_observed_prompt((reason, signal.clone()));
            tasks.reconcile(Some(Ok(PromptCompletion { session_id, result })), &shared);
            let published: Vec<_> = std::iter::from_fn(|| updates.try_recv().ok())
                .map(|update| update.update)
                .collect();
            let expected: Vec<_> = signal
                .into_iter()
                .map(HarnessUpdateKind::SessionSignal)
                .chain([HarnessUpdateKind::PromptSettled])
                .collect();
            assert_eq!(published, expected);
            assert!(response.await.unwrap().is_ok());
        }
    }
    #[tokio::test]
    async fn turn_usage_publishes_before_settling_even_when_no_one_awaits_the_reply() {
        let (status_tx, _) = watch::channel(AdapterStatus::stopped());
        let updates_tx = HarnessUpdateSender::default();
        let mut updates = updates_tx.subscribe();
        let shared = Arc::new(SharedState {
            status_tx,
            updates_tx,
            sessions: Mutex::new(BTreeMap::new()),
            update_sequences: Mutex::new(BTreeMap::new()),
            announced_commands: Mutex::new(BTreeMap::new()),
            child_sessions: None,
            sign_in_proof: SignInProof::Session,
        });
        let session_id = HarnessSessionId("session".into());
        let (reply, response) = oneshot::channel();
        let mut tasks = PromptTasks::new();
        tasks.replies.insert(session_id.clone(), reply);
        // The runner stopped waiting: a cancelled run finalizes at once.
        drop(response);
        let usage = turn_usage(&agent_client_protocol::schema::v1::Usage::new(
            1_500, 1_200, 300,
        ));
        prompt_settled_hook(Arc::clone(&shared), session_id.clone())(None, Some(usage.clone()));
        let result = super::super::protocol::map_observed_prompt((StopReason::Cancelled, None));
        tasks.reconcile(Some(Ok(PromptCompletion { session_id, result })), &shared);
        let published: Vec<_> = std::iter::from_fn(|| updates.try_recv().ok())
            .map(|update| update.update)
            .collect();
        assert_eq!(
            published,
            vec![
                HarnessUpdateKind::TurnUsage(usage),
                HarnessUpdateKind::PromptSettled
            ]
        );
    }
}
