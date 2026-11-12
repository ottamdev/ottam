use super::*;
use crate::agent::{
    AdapterFactory, AdapterLifecycleState, AdapterStatus, AgentDefinition, AgentLaunchSpec,
    AgentRuntimeStatus, HarnessAuthState, HarnessFuture, HarnessPermissionOption,
    HarnessPermissionOptionKind, HarnessPermissionOutcome, HarnessPermissionRequest, HarnessPrompt,
    HarnessSessionId, HarnessSessionSummary, HarnessStopReason, HarnessUpdate, HarnessUpdateKind,
    HarnessUpdateReceiver, HarnessUpdateSender, ModelSwitch, RuntimeProbe, SessionConfigRequest,
    SteeringCapability,
};
use crate::server::agents::attribution::{AttributionOp, DropReason, Resolution, RunEnd};
use crate::server::agents::conversation_recorder::{
    merge_progress, ConversationRecorder, TOOL_PROGRESS_INTERVAL,
};
use crate::server::agents::conversation_runner::{ConversationRunContext, FollowUpAttribution};
use crate::server::agents::task_index::TaskOwner;
use crate::server::client_link::ClientResponse;
use crate::server::responses::success_response;
use ottam_protocol::{
    AgentQuestion, AgentQuestionAnswer, AgentQuestionKind, AgentQuestionOption,
    ConversationSessionCommand, METHOD_AGENT_ASK_QUESTION, NOTIFICATION_CANCEL_REQUEST,
};

/// The wire package mirrors the definition's pin; an agent without one
/// (the in-process test engine) puts nothing on the wire.
#[test]
fn adapter_package_mirrors_the_definitions_pin() {
    let definition = |adapter: Option<crate::agent::AdapterPackage>| AgentDefinition {
        adapter,
        ..AgentDefinition::for_test("test")
    };

    let scoped = adapter_package(
        &definition(Some(crate::agent::AdapterPackage {
            name: "@agentclientprotocol/codex-acp".to_string(),
            version: "1.8.0".to_string(),
            source: crate::agent::AdapterSource::Npm,
        })),
        AgentAdapterInstallState::NotInstalled,
    )
    .expect("a pinned adapter maps");
    assert_eq!(scoped.name, "@agentclientprotocol/codex-acp");
    assert_eq!(scoped.version, "1.8.0");
    assert_eq!(scoped.install_state, AgentAdapterInstallState::NotInstalled);

    assert!(adapter_package(&definition(None), AgentAdapterInstallState::Installed).is_none());
}

/// The verdict is the join of the two columns; `Partial` counts as the
/// client having the feature, and no agent entry is never guessed.
#[test]
fn compatibility_verdict_joins_agent_and_client_columns() {
    use crate::agent::ClientSupport::{Partial, Supported, Unsupported};
    use ottam_protocol::AgentFeatureVerdict::*;
    let cases = [
        (Some(true), Supported, Works),
        (Some(true), Partial("title only"), Works),
        (Some(true), Unsupported, AgentOnly),
        (Some(false), Supported, ClientOnly),
        (Some(false), Unsupported, Neither),
        (None, Supported, Unknown),
        (None, Unsupported, Unknown),
    ];
    for (agent, client, expected) in cases {
        assert_eq!(
            super::wire::verdict(agent, client),
            expected,
            "{agent:?} + {client:?}"
        );
    }
}

/// Everything a client shows about an agent comes from its definition, so a
/// new agent needs no per-id switch in the app; an absent home or brand stays
/// absent rather than becoming an empty string.
#[test]
fn agent_summary_ships_the_definitions_presentation() {
    let summary = |presentation| {
        super::wire::map_definition(
            AgentDefinition {
                presentation,
                ..AgentDefinition::for_test("newcomer")
            },
            Some(AgentRuntimeStatus::ready("v22.0.0", "10.0.0")),
            AdapterStatus::stopped(),
            Vec::new(),
            None,
            &[],
            AgentAdapterInstallState::Installed,
        )
    };
    let full = summary(crate::agent::AgentPresentation {
        product_name: "Newcomer Code",
        vendor_name: "Newcomer Labs",
        vendor_home: Some("~/.newcomer"),
        brand: Some("newcomer"),
        account_label: "Newcomer providers",
        account_kind: crate::agent::AccountKind::MultiProvider {
            keyless_providers: &[],
        },
    });
    assert_eq!(full.display_name, "Newcomer Code");
    assert_eq!(full.vendor_name, "Newcomer Labs");
    assert_eq!(full.vendor_home.as_deref(), Some("~/.newcomer"));
    assert_eq!(full.brand.as_deref(), Some("newcomer"));
    assert_eq!(full.account_label, "Newcomer providers");
    assert_eq!(
        full.account_kind,
        ottam_protocol::AgentAccountKind::MultiProvider
    );

    let bare = summary(AgentDefinition::for_test("newcomer").presentation);
    assert_eq!(bare.vendor_home, None);
    assert_eq!(bare.brand, None);
}

/// omp lists Apple's on-device model with nothing set up, so a provider
/// count that included it would read as set up when it is not.
#[test]
fn model_providers_leave_out_keyless_providers() {
    let summary = |account_kind, models: &[&str]| {
        let models: Vec<crate::agent::AgentModelOption> = models
            .iter()
            .map(|id| crate::agent::AgentModelOption {
                model_id: id.to_string(),
                display_name: id.to_string(),
            })
            .collect();
        let mut definition = AgentDefinition::for_test("omp");
        definition.presentation.account_kind = account_kind;
        super::wire::map_definition(
            definition,
            Some(AgentRuntimeStatus::ready("v22.0.0", "10.0.0")),
            AdapterStatus::stopped(),
            Vec::new(),
            None,
            &models,
            AgentAdapterInstallState::Installed,
        )
        .model_providers
    };
    let omp = crate::agent::AccountKind::MultiProvider {
        keyless_providers: &["apple"],
    };

    assert_eq!(summary(omp, &["apple/on-device"]), Some(vec![]));
    assert_eq!(
        summary(
            omp,
            &[
                "opencode-go/kimi-k2",
                "apple/on-device",
                "anthropic/claude-opus",
                "opencode-go/glm-5",
            ]
        ),
        Some(vec!["anthropic".to_string(), "opencode-go".to_string()])
    );
    assert_eq!(summary(omp, &[]), None, "never seen is unknown, not none");
    assert_eq!(
        summary(crate::agent::AccountKind::Subscription, &["openai/gpt"]),
        None
    );
}

/// A stopped adapter knows nothing first-hand, so its agent column comes
/// from the catalog; a running one answers from its own `initialize`.
#[test]
fn compatibility_agent_column_falls_back_to_the_catalog_when_stopped() {
    use crate::agent::AcpFeature;
    let definition = AgentDefinition {
        adapter: Some(crate::agent::AdapterPackage {
            name: "@agentclientprotocol/claude-agent-acp".to_string(),
            version: "0.81.0".to_string(),
            source: crate::agent::AdapterSource::Npm,
        }),
        ..AgentDefinition::for_test("claude")
    };
    let cached: std::collections::BTreeMap<AcpFeature, bool> =
        [(AcpFeature::LoadSession, true)].into_iter().collect();
    let row = |summary: &ottam_protocol::AgentSummary, feature: &str| {
        summary
            .adapter
            .compatibility
            .iter()
            .find(|row| row.feature == feature)
            .cloned()
            .expect("every feature has a row")
    };

    let stopped = super::wire::map_definition(
        definition.clone(),
        Some(AgentRuntimeStatus::ready("v22.0.0", "10.0.0")),
        AdapterStatus::stopped(),
        Vec::new(),
        Some(cached.clone()),
        &[],
        AgentAdapterInstallState::Installed,
    );
    let features: Vec<&str> = stopped
        .adapter
        .compatibility
        .iter()
        .map(|row| row.feature.as_str())
        .collect();
    assert_eq!(
        features.len(),
        AcpFeature::ALL.len() - 4,
        "every row but the four codex-only _meta rows is present for claude"
    );
    assert!(!features.iter().any(|name| name.starts_with("_meta.codex.")));
    assert!(features.contains(&"_meta._claude/rateLimit"));
    assert!(features.contains(&"jetbrains.air.asyncTasks"));
    let steering = row(&stopped, "_session/steering");
    assert_eq!(
        steering.sdk_detail.as_deref(),
        Some("claude-agent-acp"),
        "shared adapter rows name this agent's own adapter"
    );
    assert_eq!(
        row(&stopped, "jetbrains.air.asyncTasks")
            .sdk_detail
            .as_deref(),
        Some("jetbrains.air")
    );
    let load_session = row(&stopped, "loadSession");
    assert_eq!(load_session.agent_advertises, Some(true));
    assert_eq!(
        load_session.verdict,
        ottam_protocol::AgentFeatureVerdict::Works
    );
    let usage = row(&stopped, "usage_update");
    assert_eq!(usage.agent_advertises, None);
    assert_eq!(
        usage.client_detail.as_deref(),
        Some("context gauge; cost not surfaced")
    );
    assert_eq!(
        usage.verdict,
        ottam_protocol::AgentFeatureVerdict::Unknown
    );
    assert_eq!(
        row(&stopped, "elicitation.form").sdk_status,
        ottam_protocol::AgentFeatureSdkStatus::Stable
    );
    let compaction = row(&stopped, "session.compaction");
    assert_eq!(
        compaction.sdk_status,
        ottam_protocol::AgentFeatureSdkStatus::UnstableOff
    );
    assert_eq!(
        compaction.sdk_detail.as_deref(),
        Some("unstable_session_compaction")
    );

    let unprobed = super::wire::map_definition(
        definition.clone(),
        Some(AgentRuntimeStatus::ready("v22.0.0", "10.0.0")),
        AdapterStatus::stopped(),
        Vec::new(),
        None,
        &[],
        AgentAdapterInstallState::Installed,
    );
    assert!(unprobed
        .adapter
        .compatibility
        .iter()
        .all(|row| row.agent_advertises.is_none()));

    let mut live = AdapterStatus::stopped();
    live.lifecycle = AdapterLifecycleState::Ready;
    live.capabilities.agent_advertises = [(AcpFeature::LoadSession, false)].into_iter().collect();
    let running = super::wire::map_definition(
        definition,
        Some(AgentRuntimeStatus::ready("v22.0.0", "10.0.0")),
        live,
        Vec::new(),
        Some(cached),
        &[],
        AgentAdapterInstallState::Installed,
    );
    assert_eq!(
        row(&running, "loadSession").agent_advertises,
        Some(false),
        "a running adapter's own answer beats the catalog"
    );
}
use crate::agent::HarnessAdapter;
use crate::storage::repositories::WorkspaceRepository;
use crate::storage::Database;
use ottam_protocol::{
    AgentEmptyResult, AgentStopReason, AgentUpdate, ConversationEventDeltaParams as WireEventDelta,
    ConversationInitialMessage, ConversationRunInputID, ConversationRunStatus,
    ConversationRunSummary, EngineID, HarnessSessionCleanup, HarnessSessionCleanupStatus,
    ProtocolRequest, RunInputStatus, RunOrigin, SessionOrigin, WorkingRootSelection,
    METHOD_AGENT_REQUEST_PERMISSION, METHOD_CONVERSATION_EVENT_DELTA,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Shared-database chat fixture: a tempdir home, one Workspace, and a
/// ChatService bound to it. Conversations use the workspace-files root so
/// no Project rows are required.
struct ChatEnv {
    _home: TempDir,
    database: Database,
    workspace_id: WorkspaceID,
    chat: ChatService,
}

struct CreatedConversation {
    conversation_id: ConversationID,
    input_id: ConversationRunInputID,
    root: PathBuf,
}

async fn chat_env() -> ChatEnv {
    let home = TempDir::new().expect("temporary home");
    let database = Database::open_test_home(home.path().to_path_buf()).expect("opens database");
    let workspace = database
        .run("test.createWorkspace", |connection| {
            WorkspaceRepository::create(connection, "Chat tests", chrono::Utc::now())
        })
        .await
        .expect("creates workspace");
    let workspace_id = WorkspaceID(workspace.id.to_string());
    let chat = ChatService::new(database.clone(), workspace_id.clone());
    ChatEnv {
        _home: home,
        database,
        workspace_id,
        chat,
    }
}

impl ChatEnv {
    async fn create_conversation(&self, message: &str) -> CreatedConversation {
        let result = self
            .chat
            .create_conversation(
                WorkingRootSelection::WorkspaceFiles,
                Vec::new(),
                None,
                Some(ConversationInitialMessage {
                    body: message.to_string(),
                    idempotency_key: format!("key-{}", uuid::Uuid::now_v7()),
                    attachment_ids: Vec::new(),
                }),
            )
            .await
            .expect("creates conversation");
        CreatedConversation {
            conversation_id: result.conversation.conversation.conversation_id.clone(),
            root: PathBuf::from(&result.conversation.conversation.working_root.canonical_path),
            input_id: result
                .initial_input
                .expect("initial pending input")
                .run_input_id,
        }
    }

    async fn post_message(
        &self,
        conversation_id: &ConversationID,
        body: &str,
    ) -> ConversationRunInputID {
        self.chat
            .post_message(
                conversation_id,
                body.to_string(),
                Vec::new(),
                format!("key-{}", uuid::Uuid::now_v7()),
            )
            .await
            .expect("posts message")
            .input
            .run_input_id
    }

    fn start_params(
        &self,
        conversation_id: &ConversationID,
        engine: &str,
        model_id: Option<&str>,
        input_id: &ConversationRunInputID,
    ) -> ConversationStartRunParams {
        ConversationStartRunParams {
            model_id: model_id.map(str::to_string),
            mode_id: None,
            thought_level_id: None,
            workspace_id: self.workspace_id.clone(),
            conversation_id: conversation_id.clone(),
            engine_id: EngineID(engine.to_string()),
            run_input_id: input_id.clone(),
        }
    }

    async fn run(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
    ) -> ConversationRunSummary {
        self.chat
            .conversation_run(conversation_id, run_id)
            .await
            .expect("reads run")
    }

    async fn transcript(
        &self,
        conversation_id: &ConversationID,
    ) -> Vec<ottam_protocol::ConversationTranscriptItem> {
        self.chat
            .list_transcript(ottam_protocol::ConversationListTranscriptParams {
                workspace_id: self.workspace_id.clone(),
                conversation_id: conversation_id.clone(),
                limit: Some(200),
                cursor: None,
                subagent_thread_id: None,
            })
            .await
            .expect("normalized transcript")
            .items
    }

    async fn events(&self, conversation_id: &ConversationID) -> Vec<WireEventDelta> {
        self.chat
            .list_events(conversation_id, None, Some(500), None)
            .await
            .expect("conversation events")
            .events
    }

    async fn wait_for_terminal(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
    ) -> ConversationRunSummary {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let run = self.run(conversation_id, run_id).await;
                if run.status != ConversationRunStatus::Queued
                    && run.status != ConversationRunStatus::Running
                {
                    return run;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("run reaches a terminal state")
    }

    async fn wait_for_running(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
    ) -> ConversationRunSummary {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let run = self.run(conversation_id, run_id).await;
                if run.status == ConversationRunStatus::Running && run.agent_session_id.is_some() {
                    return run;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("run reaches running with a session")
    }
}

struct ReadyProbe;

impl RuntimeProbe for ReadyProbe {
    fn probe(&self, _definition: &AgentDefinition) -> AgentRuntimeStatus {
        AgentRuntimeStatus::ready("v25.0.0", "11.0.0")
    }
}

struct FakeFactory {
    load_session_supported: bool,
    steering: SteeringCapability,
    created: StdMutex<Vec<Arc<FakeConversationAdapter>>>,
    /// Shared with every adapter, so a release reaches one not yet created.
    pause_release: Arc<tokio::sync::Semaphore>,
}

impl FakeFactory {
    fn new(load_session_supported: bool) -> Arc<Self> {
        Self::with_steering(
            load_session_supported,
            SteeringCapability::InterruptThenPrompt,
        )
    }

    fn with_steering(load_session_supported: bool, steering: SteeringCapability) -> Arc<Self> {
        Arc::new(Self {
            load_session_supported,
            steering,
            created: StdMutex::new(Vec::new()),
            pause_release: Arc::new(tokio::sync::Semaphore::new(0)),
        })
    }

    fn adapters(&self) -> Vec<Arc<FakeConversationAdapter>> {
        self.created
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl AdapterFactory for FakeFactory {
    fn create(&self, definition: AgentDefinition, generation: u64) -> Arc<dyn HarnessAdapter> {
        let adapter = Arc::new(FakeConversationAdapter {
            pause_release: Arc::clone(&self.pause_release),
            ..FakeConversationAdapter::new(
                definition,
                generation,
                self.load_session_supported,
                self.steering,
            )
        });
        self.created
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Arc::clone(&adapter));
        adapter
    }
}

struct FakeConversationAdapter {
    definition: AgentDefinition,
    status_tx: watch::Sender<AdapterStatus>,
    updates_tx: HarnessUpdateSender,
    next_session: AtomicU64,
    cancelled: CancellationToken,
    live_sessions: StdMutex<Vec<crate::agent::HarnessSessionId>>,
    deleted_sessions: StdMutex<Vec<crate::agent::HarnessSessionId>>,
    load_calls: StdMutex<Vec<crate::agent::HarnessSessionId>>,
    loaded_modes: StdMutex<Vec<Option<String>>>,
    prompts: StdMutex<Vec<String>>,
    steers: StdMutex<Vec<String>>,
    new_session_models: StdMutex<Vec<Option<String>>>,
    fail_loads: AtomicBool,
    vanish_next_prompt: AtomicBool,
    /// Prompts to reject as busy before taking one, as oh-my-pi does.
    busy_prompts: AtomicU64,
    /// Prompts to fail as a refresh blocked by a lock, abandoned in this home
    /// when one is set, as a CLI that died mid-refresh leaves it.
    refresh_busy_prompts: AtomicU64,
    abandon_refresh_in: StdMutex<Option<PathBuf>>,
    replay_on_load: AtomicBool,
    announce_new_sessions: AtomicBool,
    /// The held prompt answers its cancel with usage, after the run stopped.
    late_usage: AtomicBool,
    /// Sends that late answer; the test releases it once the cancel returned.
    late_usage_release: Arc<tokio::sync::Notify>,
    /// A "pause" prompt works until the test closes this.
    pause_release: Arc<tokio::sync::Semaphore>,
    prompt_answers: StdMutex<HashMap<String, u64>>,
    /// Updates the next prompt streams, numbered from 1, before `end_turn`.
    scripted_turn: StdMutex<Vec<HarnessUpdateKind>>,
    /// Sessions core asked to stop working on their own.
    out_of_turn_cancels: StdMutex<Vec<String>>,
    /// Each session's mode, as the actor would track it.
    session_modes: StdMutex<HashMap<String, String>>,
    /// Each session's announced commands, as the actor would track them.
    session_commands: StdMutex<HashMap<String, Value>>,
    /// Modes core switched live sessions to.
    applied_modes: StdMutex<Vec<String>>,
    /// Each session's model and level, as the actor would track them.
    session_models: StdMutex<HashMap<String, SessionModel>>,
    /// Model switches core asked of held sessions.
    applied_models: StdMutex<Vec<ModelSwitch>>,
}

/// A fake session's model and thought level.
type SessionModel = (Option<String>, Option<String>);

impl FakeConversationAdapter {
    fn new(
        definition: AgentDefinition,
        generation: u64,
        load_session_supported: bool,
        steering: SteeringCapability,
    ) -> Self {
        let mut status = AdapterStatus::stopped();
        status.generation = generation;
        status.capabilities.load_session = load_session_supported;
        status.capabilities.delete_session = true;
        status.capabilities.steering = steering;
        let (status_tx, _) = watch::channel(status);
        let updates_tx = HarnessUpdateSender::default();
        Self {
            definition,
            status_tx,
            updates_tx,
            next_session: AtomicU64::new(1),
            cancelled: CancellationToken::new(),
            live_sessions: StdMutex::new(Vec::new()),
            deleted_sessions: StdMutex::new(Vec::new()),
            load_calls: StdMutex::new(Vec::new()),
            loaded_modes: StdMutex::new(Vec::new()),
            prompts: StdMutex::new(Vec::new()),
            steers: StdMutex::new(Vec::new()),
            new_session_models: StdMutex::new(Vec::new()),
            fail_loads: AtomicBool::new(false),
            vanish_next_prompt: AtomicBool::new(false),
            busy_prompts: AtomicU64::new(0),
            refresh_busy_prompts: AtomicU64::new(0),
            abandon_refresh_in: StdMutex::new(None),
            replay_on_load: AtomicBool::new(false),
            announce_new_sessions: AtomicBool::new(false),
            late_usage: AtomicBool::new(false),
            late_usage_release: Arc::new(tokio::sync::Notify::new()),
            pause_release: Arc::new(tokio::sync::Semaphore::new(0)),
            prompt_answers: StdMutex::new(HashMap::new()),
            scripted_turn: StdMutex::new(Vec::new()),
            out_of_turn_cancels: StdMutex::new(Vec::new()),
            session_modes: StdMutex::new(HashMap::new()),
            session_commands: StdMutex::new(HashMap::new()),
            applied_modes: StdMutex::new(Vec::new()),
            session_models: StdMutex::new(HashMap::new()),
            applied_models: StdMutex::new(Vec::new()),
        }
    }

    fn set_session_model(
        &self,
        session_id: &crate::agent::HarnessSessionId,
        model_id: Option<String>,
        thought_level_id: Option<String>,
    ) {
        self.session_models
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_id.0.clone(), (model_id, thought_level_id));
    }

    fn session_model(&self, session_id: &crate::agent::HarnessSessionId) -> SessionModel {
        self.session_models
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&session_id.0)
            .cloned()
            .unwrap_or_default()
    }

    fn recorded_applied_models(&self) -> Vec<ModelSwitch> {
        self.applied_models
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_session_mode(&self, session_id: &crate::agent::HarnessSessionId, mode_id: &str) {
        self.session_modes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_id.0.clone(), mode_id.to_string());
    }

    fn recorded_applied_modes(&self) -> Vec<String> {
        self.applied_modes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn recorded_out_of_turn_cancels(&self) -> Vec<String> {
        self.out_of_turn_cancels
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn script_next_turn(&self, updates: impl IntoIterator<Item = HarnessUpdateKind>) {
        *self
            .scripted_turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = updates.into_iter().collect();
    }

    /// Simulate an adapter restart: in-memory sessions are gone, only
    /// `session/load` (when supported) can bring one back.
    fn forget_sessions(&self) {
        self.live_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    fn recorded_load_calls(&self) -> Vec<crate::agent::HarnessSessionId> {
        self.load_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn recorded_deleted_sessions(&self) -> Vec<crate::agent::HarnessSessionId> {
        self.deleted_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn recorded_prompts(&self) -> Vec<String> {
        self.prompts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn recorded_steers(&self) -> Vec<String> {
        self.steers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn recorded_new_session_models(&self) -> Vec<Option<String>> {
        self.new_session_models
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn remember_session(&self, session_id: &crate::agent::HarnessSessionId) {
        self.live_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(session_id.clone());
    }
}

impl HarnessAdapter for FakeConversationAdapter {
    fn definition(&self) -> &AgentDefinition {
        &self.definition
    }
    fn status(&self) -> watch::Receiver<AdapterStatus> {
        self.status_tx.subscribe()
    }
    fn subscribe_updates(&self) -> HarnessUpdateReceiver {
        self.updates_tx.subscribe()
    }
    fn sessions(&self) -> Vec<HarnessSessionSummary> {
        self.live_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|session_id| HarnessSessionSummary {
                session_id: session_id.clone(),
                cwd: PathBuf::from("/tmp/fake"),
                state: crate::agent::HarnessSessionState::Active,
                last_error: None,
                current_mode_id: self
                    .session_modes
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .get(&session_id.0)
                    .cloned(),
                current_model_id: self.session_model(session_id).0,
                current_thought_level_id: self.session_model(session_id).1,
                announced_commands: self
                    .session_commands
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .get(&session_id.0)
                    .cloned(),
            })
            .collect()
    }
    fn prompt_settled_at(&self, session_id: &HarnessSessionId) -> Option<u64> {
        self.prompt_answers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&session_id.0)
            .copied()
    }
    fn start(&self) -> HarnessFuture<'_, AdapterStatus> {
        Box::pin(async move {
            self.status_tx.send_modify(|status| {
                status.lifecycle = AdapterLifecycleState::Ready;
                status.auth_state = HarnessAuthState::Ready;
            });
            Ok(self.status_tx.borrow().clone())
        })
    }
    fn shutdown(&self) -> HarnessFuture<'_, ()> {
        Box::pin(async move {
            self.status_tx
                .send_modify(|status| status.lifecycle = AdapterLifecycleState::Stopped);
            Ok(())
        })
    }
    fn authenticate(&self, _method_id: String) -> HarnessFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn new_session(
        &self,
        _cwd: PathBuf,
        config: SessionConfigRequest,
    ) -> HarnessFuture<'_, crate::agent::HarnessSessionId> {
        Box::pin(async move {
            self.new_session_models
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(config.model_id.clone());
            let id = self.next_session.fetch_add(1, Ordering::Relaxed);
            let session_id = crate::agent::HarnessSessionId(format!(
                "fake-{}-session-{id}",
                self.definition.agent_id.0
            ));
            self.set_session_model(
                &session_id,
                config.model_id.clone(),
                config.thought_level_id.clone(),
            );
            if let Some(mode_id) = &config.mode_id {
                self.set_session_mode(&session_id, mode_id);
            }
            if self.announce_new_sessions.load(Ordering::Relaxed) {
                // Real ACP agents announce a new session's commands while
                // `session/new` is still in flight.
                let payload = json!({
                    "sessionUpdate": "available_commands_update",
                    "availableCommands": [
                        {"name": "review", "description": "Review changes", "input": {"hint": "what to review"}},
                        {"name": "compact", "description": "Summarize the conversation"}
                    ]
                });
                self.session_commands
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(session_id.0.clone(), payload.clone());
                let _ = self.updates_tx.send(HarnessUpdate {
                    session_id: session_id.clone(),
                    sequence: 1,
                    update: HarnessUpdateKind::Other {
                        kind: "available_commands_update".to_string(),
                        payload,
                    },
                });
            }
            self.remember_session(&session_id);
            Ok(session_id)
        })
    }
    fn load_session(
        &self,
        session_id: crate::agent::HarnessSessionId,
        _cwd: PathBuf,
    ) -> HarnessFuture<'_, ()> {
        Box::pin(async move {
            self.load_calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(session_id.clone());
            if self.fail_loads.load(Ordering::Relaxed) {
                return Err(HarnessError::new(
                    HarnessErrorKind::Internal,
                    "simulated load failure",
                ));
            }
            if self.replay_on_load.load(Ordering::Relaxed) {
                // Real ACP agents replay session history as ordinary session
                // updates while `session/load` is in flight.
                let _ = self.updates_tx.send(HarnessUpdate {
                    session_id: session_id.clone(),
                    sequence: 1,
                    update: HarnessUpdateKind::AgentMessageChunk("REPLAYED ".to_string(), None),
                });
                let _ = self.updates_tx.send(HarnessUpdate {
                    session_id: session_id.clone(),
                    sequence: 2,
                    update: HarnessUpdateKind::AsyncTaskStateUpdate(
                        ottam_protocol::AsyncTaskState::Completed,
                        async_task("REPLAYED-task", Some("REPLAYED-tool")),
                    ),
                });
            }
            self.remember_session(&session_id);
            Ok(())
        })
    }
    fn load_session_with_mode(
        &self,
        session_id: crate::agent::HarnessSessionId,
        cwd: PathBuf,
        mode_id: Option<String>,
    ) -> HarnessFuture<'_, ()> {
        self.loaded_modes.lock().unwrap().push(mode_id.clone());
        if mode_id.as_deref() == Some("unavailable") {
            return Box::pin(async {
                Err(HarnessError::new(
                    HarnessErrorKind::SelectionUnavailable,
                    "mode unavailable",
                ))
            });
        }
        self.load_session(session_id, cwd)
    }
    fn apply_mode(
        &self,
        session_id: crate::agent::HarnessSessionId,
        mode_id: String,
    ) -> HarnessFuture<'_, ()> {
        Box::pin(async move {
            self.applied_modes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(mode_id.clone());
            if mode_id == "unavailable" {
                return Err(HarnessError::new(
                    HarnessErrorKind::SelectionUnavailable,
                    "mode unavailable",
                ));
            }
            self.set_session_mode(&session_id, &mode_id);
            Ok(())
        })
    }
    fn apply_model(
        &self,
        session_id: crate::agent::HarnessSessionId,
        switch: ModelSwitch,
    ) -> HarnessFuture<'_, ()> {
        Box::pin(async move {
            self.applied_models
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(switch.clone());
            if switch.model_id.as_deref() == Some("unavailable") {
                return Err(HarnessError::new(
                    HarnessErrorKind::SelectionUnavailable,
                    "model unavailable",
                ));
            }
            let (model, level) = self.session_model(&session_id);
            self.set_session_model(
                &session_id,
                switch.model_id.or(model),
                switch.thought_level_id.or(level),
            );
            Ok(())
        })
    }
    fn delete_session(&self, session_id: crate::agent::HarnessSessionId) -> HarnessFuture<'_, ()> {
        Box::pin(async move {
            self.live_sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .retain(|candidate| candidate != &session_id);
            self.deleted_sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(session_id);
            Ok(())
        })
    }
    fn prompt(
        &self,
        session_id: crate::agent::HarnessSessionId,
        prompt: HarnessPrompt,
    ) -> HarnessFuture<'_, HarnessStopReason> {
        Box::pin(async move {
            let prompt = prompt.text;
            self.prompts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(prompt.clone());
            if self
                .busy_prompts
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
            {
                return Err(HarnessError::new(
                    HarnessErrorKind::RemoteSessionBusy,
                    "simulated agent still settling",
                ));
            }
            if self
                .refresh_busy_prompts
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(1))
                .is_ok()
            {
                if let Some(home) = self.abandon_refresh_in.lock().unwrap().as_deref() {
                    crate::agent::refresh_lock::fixture::abandoned_refresh(home);
                }
                return Err(HarnessError::new(
                    HarnessErrorKind::AuthRefreshBusy,
                    "simulated blocked token refresh",
                ));
            }
            if self.vanish_next_prompt.swap(false, Ordering::Relaxed) {
                self.forget_sessions();
                return Err(HarnessError::new(
                    HarnessErrorKind::SessionNotFound,
                    "simulated adapter restart race",
                ));
            }
            if prompt.contains("panic") {
                panic!("simulated Chat adapter panic");
            }
            let scripted = std::mem::take(
                &mut *self
                    .scripted_turn
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            );
            if !scripted.is_empty() {
                for (index, update) in scripted.into_iter().enumerate() {
                    let _ = self.updates_tx.send(HarnessUpdate {
                        session_id: session_id.clone(),
                        sequence: index as u64 + 1,
                        update,
                    });
                }
                return Ok(HarnessStopReason::EndTurn);
            }
            if prompt.contains("typed-terminal") {
                let _ = self.updates_tx.send(HarnessUpdate {
                    session_id: session_id.clone(),
                    sequence: 1,
                    update: HarnessUpdateKind::AgentMessageChunk(
                        "retained partial output".into(),
                        None,
                    ),
                });
                let fixtures: Vec<Value> =
                    serde_json::from_str(include_str!("../../agent/fixtures/session-signals.json"))
                        .unwrap();
                let mut signal = crate::agent::metadata::session_signal(&fixtures[5]).unwrap();
                signal.session_id = session_id.0;
                let mut error = HarnessError::new(
                    HarnessErrorKind::PromptExecution,
                    "Provider temporarily unavailable",
                );
                error.terminal_signal = Some(Box::new(signal));
                return Err(error);
            }
            if prompt.contains("shutdown-partial") {
                let _ = self.updates_tx.send(HarnessUpdate {
                    session_id: session_id.clone(),
                    sequence: 1,
                    update: HarnessUpdateKind::AgentMessageChunk(
                        "partial before shutdown".to_string(),
                        None,
                    ),
                });
                std::future::pending::<()>().await;
            }
            if prompt.contains("stuck") {
                std::future::pending::<()>().await;
            }
            if prompt.contains("usage-late") {
                self.late_usage.store(true, Ordering::Relaxed);
                self.cancelled.cancelled().await;
                return Ok(HarnessStopReason::Cancelled);
            }
            if prompt.contains("usage") {
                for (sequence, update) in [
                    HarnessUpdateKind::AgentMessageChunk("counted".to_string(), None),
                    HarnessUpdateKind::TurnUsage(fake_turn_usage()),
                    HarnessUpdateKind::PromptSettled,
                ]
                .into_iter()
                .enumerate()
                {
                    let _ = self.updates_tx.send(HarnessUpdate {
                        session_id: session_id.clone(),
                        sequence: sequence as u64 + 1,
                        update,
                    });
                }
                return Ok(HarnessStopReason::EndTurn);
            }
            if prompt.contains("nothing to add") {
                // A parent that acknowledges its notes and says nothing.
                let _ = self.updates_tx.send(HarnessUpdate {
                    session_id: session_id.clone(),
                    sequence: 1,
                    update: HarnessUpdateKind::AgentMessageChunk(
                        ottam_protocol::DELEGATION_ACK_DIRECTIVE.to_string(),
                        None,
                    ),
                });
                return Ok(HarnessStopReason::EndTurn);
            }
            if prompt.contains("hold") {
                self.cancelled.cancelled().await;
                return Ok(HarnessStopReason::Cancelled);
            }
            if prompt.contains("pause") {
                // Works until released, then ends its turn as usual.
                let _ = self.pause_release.acquire().await;
            }
            if prompt.contains("edit a file") {
                // An edit whose new text core saves as a file change.
                let _ = self.updates_tx.send(HarnessUpdate {
                    session_id: session_id.clone(),
                    sequence: 1,
                    update: HarnessUpdateKind::ToolCall(json!({
                        "toolCallId": "edit-1",
                        "kind": "edit",
                        "title": "Edit notes.md",
                        "status": "completed",
                        "rawInput": {
                            "file_path": "notes.md",
                            "old_string": "old line",
                            "new_string": "the new line"
                        }
                    })),
                });
                return Ok(HarnessStopReason::EndTurn);
            }
            if prompt.contains("boundary") {
                // One assistant message, a tool call, then a fresh message whose
                // chunks carry no separating newline — mirrors real ACP streams.
                let updates = [
                    HarnessUpdateKind::AgentMessageChunk("Intro paragraph.".to_string(), None),
                    HarnessUpdateKind::ToolCall(json!({"toolCallID": "tool-1"})),
                    HarnessUpdateKind::AgentMessageChunk("## Heading".to_string(), None),
                    HarnessUpdateKind::AgentMessageChunk(" continues".to_string(), None),
                ];
                for (index, update) in updates.into_iter().enumerate() {
                    let _ = self.updates_tx.send(HarnessUpdate {
                        session_id: session_id.clone(),
                        sequence: index as u64 + 1,
                        update,
                    });
                }
                return Ok(HarnessStopReason::EndTurn);
            }
            if prompt.contains("retitle") {
                // Mirrors an ACP `session_info_update`: the payload shape matches
                // what acp.rs normalizes (title at the top level).
                let _ = self.updates_tx.send(HarnessUpdate {
                    session_id: session_id.clone(),
                    sequence: 1,
                    update: HarnessUpdateKind::Other {
                        kind: "session_info_update".to_string(),
                        payload: json!({
                            "sessionUpdate": "session_info_update",
                            "title": "Harness generated title"
                        }),
                    },
                });
                return Ok(HarnessStopReason::EndTurn);
            }
            if prompt.contains("fallback-title") {
                // Mirrors codex-acp when the Codex thread has no explicit name:
                // its fallback title is the entire first prompt with whitespace
                // collapsed, including Ottam's private prompt wrapper.
                let fallback_title = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
                let _ = self.updates_tx.send(HarnessUpdate {
                    session_id: session_id.clone(),
                    sequence: 1,
                    update: HarnessUpdateKind::Other {
                        kind: "session_info_update".to_string(),
                        payload: json!({
                            "sessionUpdate": "session_info_update",
                            "title": fallback_title
                        }),
                    },
                });
                return Ok(HarnessStopReason::EndTurn);
            }
            for (sequence, text) in [(1, "Hello "), (2, "from fake")] {
                let _ = self.updates_tx.send(HarnessUpdate {
                    session_id: session_id.clone(),
                    sequence,
                    update: HarnessUpdateKind::AgentMessageChunk(text.to_string(), None),
                });
            }
            Ok(HarnessStopReason::EndTurn)
        })
    }
    fn steer(
        &self,
        session_id: crate::agent::HarnessSessionId,
        prompt: HarnessPrompt,
    ) -> HarnessFuture<'_, ()> {
        Box::pin(async move {
            let prompt = prompt.text;
            self.steers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(prompt.clone());
            if prompt.contains("detached-steer") {
                // codex-acp's order: the turn's answer, then the turn it
                // starts for the steer streaming, then the steer's answer.
                for (sequence, update) in [
                    HarnessUpdateKind::PromptSettled,
                    HarnessUpdateKind::AgentMessageChunk("DETACHED OUTPUT".to_string(), None),
                ]
                .into_iter()
                .enumerate()
                {
                    let _ = self.updates_tx.send(HarnessUpdate {
                        session_id: session_id.clone(),
                        sequence: sequence as u64 + 1,
                        update,
                    });
                }
                return Err(HarnessError {
                    detached_turn: true,
                    ..HarnessError::new(
                        HarnessErrorKind::PromptExecution,
                        "simulated detached steer turn",
                    )
                });
            }
            if prompt.contains("transport-error") {
                return Err(HarnessError::new(
                    HarnessErrorKind::AdapterExited,
                    "simulated transport failure after send",
                ));
            }
            if prompt.contains("reject-steer") {
                return Err(HarnessError::new(
                    HarnessErrorKind::Internal,
                    "simulated definitive steer rejection",
                ));
            }
            Ok(())
        })
    }
    fn cancel(&self, session_id: crate::agent::HarnessSessionId) -> HarnessFuture<'_, ()> {
        Box::pin(async move {
            self.cancelled.cancel();
            // An agent answers the cancelled prompt after core stopped
            // waiting for it, with the turn's usage.
            if self.late_usage.swap(false, Ordering::Relaxed) {
                let updates = self.updates_tx.clone();
                let release = self.late_usage_release.clone();
                tokio::spawn(async move {
                    release.notified().await;
                    for (sequence, update) in [
                        HarnessUpdateKind::TurnUsage(crate::agent::HarnessTurnUsage {
                            cancelled: true,
                            ..fake_turn_usage()
                        }),
                        HarnessUpdateKind::PromptSettled,
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        let _ = updates.send(HarnessUpdate {
                            session_id: session_id.clone(),
                            sequence: sequence as u64 + 1,
                            update,
                        });
                    }
                });
            }
            Ok(())
        })
    }
    fn cancel_out_of_turn(
        &self,
        session_id: &crate::agent::HarnessSessionId,
    ) -> HarnessFuture<'_, ()> {
        self.out_of_turn_cancels
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(session_id.0.clone());
        Box::pin(std::future::ready(Ok(())))
    }
}

fn fake_turn_usage() -> crate::agent::HarnessTurnUsage {
    crate::agent::HarnessTurnUsage {
        input_tokens: 1_200,
        output_tokens: 300,
        total_tokens: 1_500,
        thought_tokens: None,
        cached_read_tokens: Some(800),
        cached_write_tokens: None,
        cancelled: false,
    }
}

/// Waits until the fake holds a `usage-late` prompt. The run reads as running
/// before the prompt reaches the agent, and a cancel that lands first leaves
/// no prompt to answer late.
async fn wait_for_late_usage_prompt(factory: &FakeFactory) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !factory
            .adapters()
            .iter()
            .any(|adapter| adapter.late_usage.load(Ordering::Relaxed))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the fake holds the usage-late prompt");
}

/// Lets the fake send the late reply its cancel is holding.
fn release_late_usage(factory: &FakeFactory) {
    for adapter in factory.adapters() {
        adapter.late_usage_release.notify_one();
    }
}

/// Lets every "pause" prompt end its turn.
fn release_paused_prompts(factory: &FakeFactory) {
    factory.pause_release.close();
}

fn fake_registry() -> AgentRegistry {
    fake_registry_with(FakeFactory::new(true))
}

fn fake_registry_with(factory: Arc<FakeFactory>) -> AgentRegistry {
    let definitions = ["codex", "claude", "omp"]
        .into_iter()
        .map(fake_definition)
        .collect();
    AgentRegistry::with_components(definitions, Arc::new(ReadyProbe), factory)
}

fn fake_definition(id: &str) -> AgentDefinition {
    let definition = AgentDefinition::for_test(id);
    AgentDefinition {
        launch: AgentLaunchSpec {
            command: "fake".to_string(),
            ..definition.launch.clone()
        },
        ..definition
    }
}

fn permission_request() -> HarnessPermissionRequest {
    HarnessPermissionRequest {
        agent_id: AgentId("codex".to_string()),
        session_id: crate::agent::HarnessSessionId("session-1".to_string()),
        tool_call: json!({"toolCallID": "tool-1"}),
        options: vec![HarnessPermissionOption {
            option_id: "allow-once".to_string(),
            name: "Allow once".to_string(),
            kind: HarnessPermissionOptionKind::AllowOnce,
        }],
        withdrawn: Default::default(),
        auto_approvable: false,
    }
}

fn permission_attribution() -> SessionAttribution {
    SessionAttribution::default()
}

#[tokio::test]
async fn permission_broker_round_trips_selected_option() {
    let (outbound_tx, mut outbound_rx) = crate::server::outbound::test_sink();
    let client_link = ClientLink::new(outbound_tx.clone());
    let pending = client_link.pending_requests();
    let broker = ServerPermissionBroker::new(client_link, permission_attribution());
    broker.set_supported(true);
    let task_broker = broker.clone();
    let task =
        tokio::spawn(async move { task_broker.request_permission(permission_request()).await });

    let callback = outbound_rx.recv().await.expect("permission callback").frame;
    assert_eq!(callback["method"], METHOD_AGENT_REQUEST_PERMISSION);
    let id = serde_json::from_value::<RequestID>(callback["id"].clone()).expect("request id");
    assert!(pending.complete(ClientResponse::Success {
        id,
        result: json!({"outcome": "selected", "optionID": "allow-once"}),
    }));

    assert_eq!(
        task.await.expect("broker task"),
        HarnessPermissionOutcome::Selected {
            option_id: "allow-once".to_string()
        }
    );
}

#[tokio::test]
async fn permission_broker_cancels_pending_session_request() {
    let (outbound_tx, mut outbound_rx) = crate::server::outbound::test_sink();
    let client_link = ClientLink::new(outbound_tx.clone());
    let broker = ServerPermissionBroker::new(client_link, permission_attribution());
    broker.set_supported(true);
    let task_broker = broker.clone();
    let task =
        tokio::spawn(async move { task_broker.request_permission(permission_request()).await });
    outbound_rx.recv().await.expect("permission callback");

    broker.cancel_session(&AgentId("codex".to_string()), "session-1");
    assert_eq!(
        task.await.expect("broker task"),
        HarnessPermissionOutcome::Cancelled
    );
}

#[tokio::test]
async fn permission_broker_cancels_when_client_does_not_advertise_support() {
    let (outbound_tx, mut outbound_rx) = crate::server::outbound::test_sink();
    let broker = ServerPermissionBroker::new(
        ClientLink::new(outbound_tx.clone()),
        permission_attribution(),
    );
    assert_eq!(
        broker.request_permission(permission_request()).await,
        HarnessPermissionOutcome::Cancelled
    );
    assert!(outbound_rx.try_recv().is_err());
}

/// An unanswered permission waits for the user until a Stop releases it.
#[tokio::test]
async fn permission_broker_waits_for_the_user_without_timeout() {
    let (outbound_tx, mut outbound_rx) = crate::server::outbound::test_sink();
    let broker = ServerPermissionBroker::new(
        ClientLink::new(outbound_tx.clone()),
        permission_attribution(),
    );
    broker.set_supported(true);
    let task_broker = broker.clone();
    let task =
        tokio::spawn(async move { task_broker.request_permission(permission_request()).await });
    outbound_rx.recv().await.expect("permission callback");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!task.is_finished(), "a permission must not time out");

    broker.cancel_session(&AgentId("codex".to_string()), "session-1");
    assert_eq!(
        task.await.expect("broker task"),
        HarnessPermissionOutcome::Cancelled
    );
}

/// The app gets a plan's markdown with its request.
#[tokio::test]
async fn permission_broker_sends_a_plan_with_its_request() {
    let (outbound_tx, mut outbound_rx) = crate::server::outbound::test_sink();
    let client_link = ClientLink::new(outbound_tx.clone());
    let pending = client_link.pending_requests();
    let broker = ServerPermissionBroker::new(client_link, permission_attribution());
    broker.set_supported(true);
    let mut request = permission_request();
    request.tool_call = json!({
        "toolCallId": "tool-1",
        "title": "Approve Plan",
        "rawInput": {"plan": "# Add TODO.md", "planFilePath": "/tmp/plan.md"},
    });
    let task_broker = broker.clone();
    let task = tokio::spawn(async move { task_broker.request_permission(request).await });

    let callback = outbound_rx.recv().await.expect("permission callback").frame;
    assert_eq!(callback["params"]["plan"], "# Add TODO.md");

    let id = serde_json::from_value::<RequestID>(callback["id"].clone()).expect("request id");
    assert!(pending.complete(ClientResponse::Success {
        id,
        result: json!({"outcome": "selected", "optionID": "allow-once"}),
    }));
    assert_eq!(
        task.await.expect("broker task"),
        HarnessPermissionOutcome::Selected {
            option_id: "allow-once".to_string()
        }
    );
}

/// With no timeout, the agent withdrawing its request is what settles it.
#[tokio::test]
async fn a_withdrawn_plan_is_cancelled_in_the_client() {
    let (outbound_tx, mut outbound_rx) = crate::server::outbound::test_sink();
    let broker =
        ServerPermissionBroker::new(ClientLink::new(outbound_tx), permission_attribution());
    broker.set_supported(true);
    let withdrawn = tokio_util::sync::CancellationToken::new();
    let mut request = permission_request();
    request.tool_call = json!({"toolCallId": "tool-1", "rawInput": {"plan": "# Add TODO.md"}});
    request.withdrawn = withdrawn.clone();
    let task_broker = broker.clone();
    let task = tokio::spawn(async move { task_broker.request_permission(request).await });
    let callback = outbound_rx.recv().await.expect("permission callback").frame;

    withdrawn.cancel();
    assert_eq!(
        task.await.expect("broker task"),
        HarnessPermissionOutcome::Cancelled
    );
    let cancel = outbound_rx.recv().await.expect("cancel notification").frame;
    assert_eq!(cancel["method"], NOTIFICATION_CANCEL_REQUEST);
    assert_eq!(cancel["params"]["id"], callback["id"]);
}

#[test]
fn plan_detection_matches_codex_and_claude_shapes() {
    use super::permission::plan_markdown;
    let plan = json!({"plan": "Steps"});
    assert_eq!(
        plan_markdown(&json!({"kind": "switch_mode", "rawInput": plan})),
        Some("Steps")
    );
    assert_eq!(plan_markdown(&json!({"rawInput": plan})), Some("Steps"));
    assert_eq!(
        plan_markdown(&json!({"kind": "execute", "rawInput": plan})),
        None
    );
    assert_eq!(plan_markdown(&json!({"rawInput": {"plan": "  \n"}})), None);
    assert_eq!(plan_markdown(&json!({"rawInput": {"command": "ls"}})), None);
}

/// A broker whose session "session-1" belongs to a running conversation,
/// with that conversation's "Always allow tool actions" set as given.
async fn always_allow_fixture(
    enabled: bool,
    client_supported: bool,
) -> (
    AttributionFixture,
    ServerPermissionBroker,
    tokio::sync::mpsc::UnboundedReceiver<crate::server::outbound::OutboundFrame>,
) {
    let mut fixture = AttributionFixture::new().await;
    let context = fixture.context("run-a");
    fixture.attribution.apply(AttributionOp::Claim {
        key: session_key("session-1"),
        context,
        generation: 1,
    });
    fixture
        .env
        .chat
        .set_always_allow_tool_actions(&fixture.conversation_id, enabled)
        .await
        .expect("set always allow");
    let (outbound_tx, outbound_rx) = crate::server::outbound::test_sink();
    let broker =
        ServerPermissionBroker::new(ClientLink::new(outbound_tx), fixture.attribution.clone());
    broker.set_supported(client_supported);
    (fixture, broker, outbound_rx)
}

fn covered_request() -> HarnessPermissionRequest {
    HarnessPermissionRequest {
        auto_approvable: true,
        ..permission_request()
    }
}

/// Core answers allow once itself, with no client to ask.
#[tokio::test]
async fn always_allow_answers_a_covered_agent_without_the_client() {
    let (_fixture, broker, mut outbound_rx) = always_allow_fixture(true, false).await;
    assert_eq!(
        broker.request_permission(covered_request()).await,
        HarnessPermissionOutcome::Selected {
            option_id: "allow-once".to_string(),
        }
    );
    while let Ok(frame) = outbound_rx.try_recv() {
        assert_ne!(frame.frame["method"], METHOD_AGENT_REQUEST_PERMISSION);
    }
}

/// Each case still reaches the sheet: the switch is off, the agent has its
/// own bypass mode, or nothing offers allow once.
#[tokio::test]
async fn always_allow_leaves_uncovered_requests_to_the_user() {
    let only_always = HarnessPermissionRequest {
        options: vec![HarnessPermissionOption {
            option_id: "allow-always".to_string(),
            name: "Always allow".to_string(),
            kind: HarnessPermissionOptionKind::AllowAlways,
        }],
        ..covered_request()
    };
    for (enabled, request) in [
        (false, covered_request()),
        (true, permission_request()),
        (true, only_always),
    ] {
        let (_fixture, broker, mut outbound_rx) = always_allow_fixture(enabled, true).await;
        let asking = broker.clone();
        let task = tokio::spawn(async move { asking.request_permission(request).await });
        let callback = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let frame = outbound_rx.recv().await.expect("outbound frame").frame;
                if frame["method"] == METHOD_AGENT_REQUEST_PERMISSION {
                    break frame;
                }
            }
        })
        .await
        .expect("the request reaches the client");
        assert_eq!(callback["params"]["runID"], "run-a");
        broker.cancel_session(&AgentId("codex".to_string()), "session-1");
        assert_eq!(
            task.await.expect("broker task"),
            HarnessPermissionOutcome::Cancelled
        );
    }
}

/// A plan offering allow once still goes to the user to review.
#[tokio::test]
async fn always_allow_leaves_a_plan_to_the_user() {
    let (_fixture, broker, mut outbound_rx) = always_allow_fixture(true, true).await;
    let request = HarnessPermissionRequest {
        tool_call: json!({"toolCallId": "tool-1", "rawInput": {"plan": "# Add TODO.md"}}),
        ..covered_request()
    };
    let task = tokio::spawn(async move { broker.request_permission(request).await });
    let callback = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let frame = outbound_rx.recv().await.expect("outbound frame").frame;
            if frame["method"] == METHOD_AGENT_REQUEST_PERMISSION {
                break frame;
            }
        }
    })
    .await
    .expect("the plan reaches the client");
    assert_eq!(callback["params"]["plan"], "# Add TODO.md");
    task.abort();
}

/// A request no conversation holds has no switch to read.
#[tokio::test]
async fn always_allow_ignores_an_unattributed_request() {
    let (_fixture, broker, _outbound_rx) = always_allow_fixture(true, false).await;
    let request = HarnessPermissionRequest {
        session_id: HarnessSessionId("session-unknown".to_string()),
        ..covered_request()
    };
    assert_eq!(
        broker.request_permission(request).await,
        HarnessPermissionOutcome::Cancelled
    );
}

/// Setting one conversation's switch sets the default new ones start from.
#[tokio::test]
async fn a_new_conversation_starts_from_the_last_always_allow_value() {
    let env = chat_env().await;
    let first = env.create_conversation("first").await;
    let before = env.create_conversation("before").await;
    env.chat
        .set_always_allow_tool_actions(&first.conversation_id, true)
        .await
        .expect("set always allow");
    let after = env.create_conversation("after").await;
    for (conversation, expected) in [(&first, true), (&before, false), (&after, true)] {
        assert_eq!(
            env.chat
                .always_allow_tool_actions(&conversation.conversation_id)
                .await
                .expect("read always allow"),
            expected
        );
    }
}

fn question_request(
    withdrawn: tokio_util::sync::CancellationToken,
) -> crate::agent::HarnessQuestionRequest {
    crate::agent::HarnessQuestionRequest {
        agent_id: AgentId("codex".to_string()),
        session_id: crate::agent::HarnessSessionId("session-1".to_string()),
        provider_tool_call_id: Some("tool-1".to_string()),
        title: "Codex needs your input to continue.".to_string(),
        questions: vec![
            AgentQuestion {
                id: "q0".to_string(),
                header: Some("Approach".to_string()),
                prompt: "Which approach?".to_string(),
                required: true,
                kind: AgentQuestionKind::SingleSelect {
                    options: vec![AgentQuestionOption {
                        value: "refactor".to_string(),
                        label: "Refactor".to_string(),
                        description: None,
                    }],
                    allows_other: true,
                    default: None,
                },
            },
            AgentQuestion {
                id: "q1".to_string(),
                header: None,
                prompt: "Token".to_string(),
                required: false,
                kind: AgentQuestionKind::Text {
                    secure: true,
                    default: None,
                },
            },
        ],
        withdrawn,
    }
}

fn question_broker() -> (
    ServerPermissionBroker,
    crate::server::client_link::PendingClientRequests,
    tokio::sync::mpsc::UnboundedReceiver<crate::server::outbound::OutboundFrame>,
) {
    let (outbound_tx, outbound_rx) = crate::server::outbound::test_sink();
    let client_link = ClientLink::new(outbound_tx);
    let pending = client_link.pending_requests();
    let broker = ServerPermissionBroker::new(client_link, permission_attribution());
    broker.set_questions_supported(true);
    (broker, pending, outbound_rx)
}

#[tokio::test]
async fn question_broker_round_trips_answers() {
    let (broker, pending, mut outbound_rx) = question_broker();
    let task_broker = broker.clone();
    let task = tokio::spawn(async move {
        task_broker
            .ask_question(question_request(Default::default()))
            .await
    });

    let callback = outbound_rx.recv().await.expect("question callback").frame;
    assert_eq!(callback["method"], METHOD_AGENT_ASK_QUESTION);
    assert_eq!(callback["params"]["providerToolCallID"], "tool-1");
    assert_eq!(
        callback["params"]["questions"][0]["kind"]["type"],
        "singleSelect"
    );
    assert_eq!(callback["params"]["questions"][1]["kind"]["secure"], true);
    let id = serde_json::from_value::<RequestID>(callback["id"].clone()).expect("request id");
    assert!(pending.complete(ClientResponse::Success {
        id,
        result: json!({"outcome": "submitted", "answers": [
            {"questionID": "q0", "values": [], "other": "Both"}
        ]}),
    }));
    assert_eq!(
        task.await.expect("broker task"),
        crate::agent::HarnessQuestionOutcome::Submitted(vec![AgentQuestionAnswer {
            question_id: "q0".to_string(),
            values: Vec::new(),
            other: Some("Both".to_string()),
        }])
    );
    // Answered: nothing to withdraw.
    assert!(outbound_rx.try_recv().is_err());
}

#[tokio::test]
async fn question_broker_reports_a_skip() {
    let (broker, pending, mut outbound_rx) = question_broker();
    let task_broker = broker.clone();
    let task = tokio::spawn(async move {
        task_broker
            .ask_question(question_request(Default::default()))
            .await
    });
    let callback = outbound_rx.recv().await.expect("question callback").frame;
    let id = serde_json::from_value::<RequestID>(callback["id"].clone()).expect("request id");
    assert!(pending.complete(ClientResponse::Success {
        id,
        result: json!({"outcome": "skipped"}),
    }));
    assert_eq!(
        task.await.expect("broker task"),
        crate::agent::HarnessQuestionOutcome::Skipped
    );
}

#[tokio::test]
async fn a_withdrawn_question_is_cancelled_in_the_client() {
    let (broker, _pending, mut outbound_rx) = question_broker();
    let withdrawn = tokio_util::sync::CancellationToken::new();
    let task_broker = broker.clone();
    let request = question_request(withdrawn.clone());
    let task = tokio::spawn(async move { task_broker.ask_question(request).await });
    let callback = outbound_rx.recv().await.expect("question callback").frame;

    withdrawn.cancel();
    assert_eq!(
        task.await.expect("broker task"),
        crate::agent::HarnessQuestionOutcome::Cancelled
    );
    let cancel = outbound_rx.recv().await.expect("cancel notification").frame;
    assert_eq!(cancel["method"], NOTIFICATION_CANCEL_REQUEST);
    assert_eq!(cancel["params"]["id"], callback["id"]);
}

#[tokio::test]
async fn stopping_the_session_cancels_its_question() {
    let (broker, _pending, mut outbound_rx) = question_broker();
    let task_broker = broker.clone();
    let task = tokio::spawn(async move {
        task_broker
            .ask_question(question_request(Default::default()))
            .await
    });
    let callback = outbound_rx.recv().await.expect("question callback").frame;
    broker.cancel_session(&AgentId("codex".to_string()), "session-1");
    assert_eq!(
        task.await.expect("broker task"),
        crate::agent::HarnessQuestionOutcome::Cancelled
    );
    let cancel = outbound_rx.recv().await.expect("cancel notification").frame;
    assert_eq!(cancel["params"]["id"], callback["id"]);
}

#[tokio::test]
async fn question_broker_cancels_when_client_does_not_advertise_support() {
    let (outbound_tx, mut outbound_rx) = crate::server::outbound::test_sink();
    let broker =
        ServerPermissionBroker::new(ClientLink::new(outbound_tx), permission_attribution());
    broker.set_supported(true);
    assert_eq!(
        broker
            .ask_question(question_request(Default::default()))
            .await,
        crate::agent::HarnessQuestionOutcome::Cancelled
    );
    assert!(outbound_rx.try_recv().is_err());
}

#[test]
fn recorded_answers_keep_labels_and_never_a_secret() {
    let questions = question_request(Default::default()).questions;
    let recorded = super::permission::recorded_answers(
        &questions,
        Some(&[
            AgentQuestionAnswer {
                question_id: "q0".to_string(),
                values: vec!["refactor".to_string()],
                other: Some(" carefully ".to_string()),
            },
            AgentQuestionAnswer {
                question_id: "q1".to_string(),
                values: vec!["s3cret".to_string()],
                other: None,
            },
        ]),
    );
    assert_eq!(
        recorded[0].values,
        vec!["Refactor".to_string(), "carefully".to_string()]
    );
    assert!(recorded[1].secure && recorded[1].values.is_empty());
    assert!(!serde_json::to_string(&recorded).unwrap().contains("s3cret"));
    let unanswered = super::permission::recorded_answers(&questions, None);
    assert!(unanswered.iter().all(|answer| answer.values.is_empty()));
}

#[tokio::test]
async fn completed_run_session_does_not_attribute_later_permission_request() {
    let env = chat_env().await;
    let conversation = env.create_conversation("permission attribution").await;
    let (outbound_tx, mut outbound_rx) = crate::server::outbound::test_sink();
    let client_link = ClientLink::new(outbound_tx.clone());
    let pending = client_link.pending_requests();
    let bridge = AgentRuntimeBridge::new_with_registry(outbound_tx, client_link, fake_registry());
    bridge.permission_broker.set_supported(true);
    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "codex",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root,
            None,
        )
        .await
        .expect("run starts");
    let run = env
        .wait_for_terminal(&conversation.conversation_id, &started.run.run_id)
        .await;
    assert_conversation_run_released(&bridge, &run.run_id).await;
    let events_before = env.events(&conversation.conversation_id).await.len();

    let session_id = run.agent_session_id.expect("completed run session");
    let broker = Arc::clone(&bridge.permission_broker);
    let permission = tokio::spawn(async move {
        broker
            .request_permission(HarnessPermissionRequest {
                agent_id: AgentId("codex".to_string()),
                session_id: HarnessSessionId(session_id.0),
                ..permission_request()
            })
            .await
    });
    let callback = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let frame = outbound_rx.recv().await.expect("outbound frame").frame;
            if frame["method"] == METHOD_AGENT_REQUEST_PERMISSION {
                break frame;
            }
        }
    })
    .await
    .expect("permission callback arrives");
    for field in ["workspaceID", "projectID", "conversationID", "runID"] {
        assert!(
            callback["params"].get(field).is_none(),
            "completed run leaked {field} into permission callback"
        );
    }
    let id = serde_json::from_value::<RequestID>(callback["id"].clone()).expect("request id");
    assert!(pending.complete(ClientResponse::Success {
        id,
        result: json!({"outcome": "selected", "optionID": "allow-once"}),
    }));
    assert_eq!(
        permission.await.expect("permission task"),
        HarnessPermissionOutcome::Selected {
            option_id: "allow-once".to_string(),
        }
    );
    assert_eq!(
        env.events(&conversation.conversation_id).await.len(),
        events_before,
        "completed run received permission lifecycle events"
    );
    bridge.shutdown().await;
}

#[tokio::test]
async fn conversation_runs_share_an_adapter_and_persist_ordered_replies() {
    let env = chat_env().await;
    let first = env.create_conversation("First prompt").await;
    let second = env.create_conversation("Second prompt").await;
    let (outbound_tx, mut outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry(),
    );

    let first_start = bridge
        .start_conversation_run(
            env.start_params(&first.conversation_id, "codex", None, &first.input_id),
            env.chat.clone(),
            first.root.clone(),
            None,
        )
        .await
        .expect("first run starts");
    let second_start = bridge
        .start_conversation_run(
            env.start_params(&second.conversation_id, "codex", None, &second.input_id),
            env.chat.clone(),
            second.root.clone(),
            None,
        )
        .await
        .expect("second run starts");
    assert_eq!(first_start.run.status, ConversationRunStatus::Queued);
    assert_eq!(second_start.run.status, ConversationRunStatus::Queued);

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let first_run = env
                .run(&first.conversation_id, &first_start.run.run_id)
                .await;
            let second_run = env
                .run(&second.conversation_id, &second_start.run.run_id)
                .await;
            if first_run.status == ConversationRunStatus::Completed
                && second_run.status == ConversationRunStatus::Completed
            {
                // Session continuity is scoped to one conversation; two
                // conversations must never share a harness session.
                assert_ne!(first_run.agent_session_id, second_run.agent_session_id);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("runs complete");

    for conversation_id in [
        first.conversation_id.clone(),
        second.conversation_id.clone(),
    ] {
        let items = env.transcript(&conversation_id).await;
        assert_eq!(
            items
                .iter()
                .find(|item| item.kind == ottam_protocol::TranscriptItemKind::AssistantText)
                .map(|item| item.body_preview.as_str()),
            Some("Hello from fake")
        );
    }
    let mut conversation_events = 0;
    let mut transcript_events = 0;
    let mut pending_completions = std::collections::HashSet::from([
        first_start.run.run_id.clone(),
        second_start.run.run_id.clone(),
    ]);
    // Durable execution status is observable before the runner persists and
    // sends its terminal lifecycle event. Await delivery itself; draining only
    // the frames already queued races that asynchronous publication.
    tokio::time::timeout(Duration::from_secs(2), async {
        while !pending_completions.is_empty() || transcript_events < 2 {
            let value = outbound_rx
                .recv()
                .await
                .expect("outbound remains open")
                .frame;
            if value["method"] == METHOD_CONVERSATION_EVENT_DELTA {
                conversation_events += 1;
                let event: WireEventDelta =
                    serde_json::from_value(value["params"].clone()).unwrap();
                if event.event_kind == ottam_protocol::ConversationEventKind::RunStatus
                    && event.payload["status"] == "completed"
                {
                    pending_completions.remove(&event.run_id);
                }
            }
            if value["method"] == ottam_protocol::NOTIFICATION_CONVERSATION_TRANSCRIPT_DELTA {
                transcript_events += 1;
            }
        }
    })
    .await
    .expect("both runs publish completion and transcript notifications");
    assert!(conversation_events >= 4);
    assert!(transcript_events >= 2);
    bridge.shutdown().await;
}

struct ContinuityHarness {
    env: ChatEnv,
    root: PathBuf,
    initial_input: StdMutex<Option<ConversationRunInputID>>,
    bridge: Arc<AgentRuntimeBridge>,
    factory: Arc<FakeFactory>,
    conversation_id: ConversationID,
}

async fn continuity_harness(_name: &str, factory: Arc<FakeFactory>) -> ContinuityHarness {
    continuity_harness_with_lane(factory, MAX_DEFERRED_AGENT_REQUESTS).await
}

/// A harness whose bridge has `regular_limit` regular request slots.
async fn continuity_harness_with_lane(
    factory: Arc<FakeFactory>,
    regular_limit: usize,
) -> ContinuityHarness {
    let env = chat_env().await;
    let conversation = env.create_conversation("Initial request").await;
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry_and_regular_limit(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry_with(Arc::clone(&factory)),
        regular_limit,
    );
    ContinuityHarness {
        env,
        root: conversation.root,
        initial_input: StdMutex::new(Some(conversation.input_id)),
        bridge,
        factory,
        conversation_id: conversation.conversation_id,
    }
}

impl ContinuityHarness {
    async fn run_to_terminal(&self, engine: &str) -> ConversationRunSummary {
        self.run_to_terminal_with_model(engine, None).await
    }

    async fn run_to_terminal_with_model(
        &self,
        engine: &str,
        model_id: Option<&str>,
    ) -> ConversationRunSummary {
        self.run_to_terminal_with(engine, model_id, None).await
    }

    async fn run_to_terminal_with(
        &self,
        engine: &str,
        model_id: Option<&str>,
        mode_id: Option<&str>,
    ) -> ConversationRunSummary {
        let initial_input = self
            .initial_input
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let input_id = match initial_input {
            Some(input_id) => input_id,
            None => {
                self.env
                    .post_message(&self.conversation_id, "Initial request")
                    .await
            }
        };
        self.run_input_to_terminal(engine, model_id, mode_id, input_id)
            .await
    }

    /// Posts `body` and runs it on `engine`. The conversation's first
    /// message is still "Initial request".
    async fn run_saying(&self, engine: &str, body: &str) -> ConversationRunSummary {
        let initial_input = self
            .initial_input
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let input_id = match initial_input {
            Some(input_id) => input_id,
            None => self.env.post_message(&self.conversation_id, body).await,
        };
        self.run_input_to_terminal(engine, None, None, input_id)
            .await
    }

    async fn run_input_to_terminal(
        &self,
        engine: &str,
        model_id: Option<&str>,
        mode_id: Option<&str>,
        input_id: ConversationRunInputID,
    ) -> ConversationRunSummary {
        let started = self
            .bridge
            .start_conversation_run(
                ConversationStartRunParams {
                    mode_id: mode_id.map(str::to_string),
                    ..self
                        .env
                        .start_params(&self.conversation_id, engine, model_id, &input_id)
                },
                self.env.chat.clone(),
                self.root.clone(),
                None,
            )
            .await
            .expect("run starts");
        self.env
            .wait_for_terminal(&self.conversation_id, &started.run.run_id)
            .await
    }

    fn adapter(&self) -> Arc<FakeConversationAdapter> {
        self.factory
            .adapters()
            .into_iter()
            .next()
            .expect("adapter created")
    }
}

async fn assert_conversation_run_released(bridge: &AgentRuntimeBridge, run_id: &ConversationRunID) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !bridge.conversation.contains_run(run_id).await {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("terminal run remains in active runtime map: {}", run_id.0));
    let attributed_sessions = bridge.conversation.attributed_sessions_for_run(run_id);
    assert!(
        attributed_sessions.is_empty(),
        "terminal run still owns session attribution: {attributed_sessions:?}"
    );
}

#[tokio::test]
async fn deleting_conversation_cancels_active_run_and_deletes_harness_session() {
    let harness = continuity_harness("conversation_delete", FakeFactory::new(true)).await;
    let input_id = harness
        .env
        .post_message(&harness.conversation_id, "hold")
        .await;
    let started = harness
        .bridge
        .start_conversation_run(
            harness
                .env
                .start_params(&harness.conversation_id, "codex", None, &input_id),
            harness.env.chat.clone(),
            harness.root.clone(),
            None,
        )
        .await
        .expect("run starts");
    let session_id = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let run = harness
                .env
                .run(&harness.conversation_id, &started.run.run_id)
                .await;
            if let Some(session_id) = run.agent_session_id {
                break session_id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("run obtains harness session");

    let result = harness
        .bridge
        .delete_conversation_with_children(
            &harness.env.chat,
            &harness.env.workspace_id,
            &harness.conversation_id,
            ottam_protocol::ConversationDeleteChildren::Keep,
        )
        .await
        .expect("deletes conversation");
    assert_eq!(
        result.harness_session_cleanup,
        vec![HarnessSessionCleanup {
            engine_id: EngineID("codex".to_string()),
            agent_session_id: session_id.clone(),
            status: HarnessSessionCleanupStatus::Deleted,
            message: None,
        }]
    );
    assert_eq!(
        harness
            .env
            .chat
            .conversation_detail(&harness.conversation_id)
            .await
            .expect_err("conversation is deleted")
            .kind,
        "unknownConversationID"
    );
    assert_eq!(
        harness.adapter().recorded_deleted_sessions(),
        vec![HarnessSessionId(session_id.0)]
    );
    harness.bridge.shutdown().await;
}

#[tokio::test]
async fn deleting_conversation_reports_unsupported_harness_cleanup_but_deletes_locally() {
    let harness =
        continuity_harness("conversation_delete_unsupported", FakeFactory::new(true)).await;
    let run = harness.run_to_terminal("codex").await;
    let adapter = harness.adapter();
    adapter
        .status_tx
        .send_modify(|status| status.capabilities.delete_session = false);

    let result = harness
        .bridge
        .delete_conversation_with_children(
            &harness.env.chat,
            &harness.env.workspace_id,
            &harness.conversation_id,
            ottam_protocol::ConversationDeleteChildren::Keep,
        )
        .await
        .expect("deletes local conversation");
    assert_eq!(
        result.harness_session_cleanup,
        vec![HarnessSessionCleanup {
            engine_id: EngineID("codex".to_string()),
            agent_session_id: run.agent_session_id.expect("session id"),
            status: HarnessSessionCleanupStatus::Unsupported,
            message: Some("The harness does not advertise ACP session deletion.".to_string()),
        }]
    );
    assert!(adapter.recorded_deleted_sessions().is_empty());
    assert_eq!(
        harness
            .env
            .chat
            .conversation_detail(&harness.conversation_id)
            .await
            .expect_err("conversation is deleted")
            .kind,
        "unknownConversationID"
    );
    harness.bridge.shutdown().await;
}

#[tokio::test]
async fn conversation_runs_continue_the_live_session_within_a_conversation() {
    let harness = continuity_harness("conversation_continue", FakeFactory::new(true)).await;
    let first = harness.run_to_terminal("codex").await;
    let second = harness.run_to_terminal("codex").await;

    assert_eq!(first.status, ConversationRunStatus::Completed);
    assert_eq!(second.status, ConversationRunStatus::Completed);
    assert_eq!(first.session_origin, Some(SessionOrigin::New));
    assert_eq!(second.session_origin, Some(SessionOrigin::Continued));
    assert_eq!(first.agent_session_id, second.agent_session_id);

    let adapter = harness.adapter();
    assert!(adapter.recorded_load_calls().is_empty());
    let prompts = adapter.recorded_prompts();
    assert_eq!(prompts.len(), 2);
    assert!(prompts[0].starts_with("You are running inside Ottam Chat."));
    assert!(prompts[1].starts_with("Continue the same Ottam Chat session."));
    assert!(prompts[1].contains("Initial request"));
    harness.bridge.shutdown().await;
}

#[tokio::test]
async fn conversation_model_applies_to_new_sessions_only() {
    let harness = continuity_harness("conversation_model", FakeFactory::new(true)).await;
    let first = harness
        .run_to_terminal_with_model("codex", Some("gpt-fast"))
        .await;
    // The live session is continued; the changed model must not trigger a
    // fresh session and therefore never reaches the adapter.
    let second = harness
        .run_to_terminal_with_model("codex", Some("gpt-smart"))
        .await;

    assert_eq!(first.status, ConversationRunStatus::Completed);
    assert_eq!(second.status, ConversationRunStatus::Completed);
    assert_eq!(first.session_origin, Some(SessionOrigin::New));
    assert_eq!(second.session_origin, Some(SessionOrigin::Continued));
    let adapter = harness.adapter();
    assert_eq!(
        adapter.recorded_new_session_models(),
        vec![Some("gpt-fast".to_string())]
    );

    // A dead session forces a fresh one, which picks up the new model.
    adapter.forget_sessions();
    adapter.fail_loads.store(true, Ordering::Relaxed);
    let third = harness
        .run_to_terminal_with_model("codex", Some("gpt-smart"))
        .await;
    assert_eq!(third.status, ConversationRunStatus::Completed);
    assert_eq!(third.session_origin, Some(SessionOrigin::New));
    assert_eq!(
        adapter.recorded_new_session_models(),
        vec![Some("gpt-fast".to_string()), Some("gpt-smart".to_string())]
    );
    harness.bridge.shutdown().await;
}

/// The conversation's mode follows the agent: an announced change (leaving
/// plan mode) persists on the session, a live session switches only when the
/// requested mode differs from the confirmed one, and a refused switch blocks
/// the prompt.
#[tokio::test]
async fn conversation_mode_follows_the_agent_and_switches_live_sessions() {
    let harness = continuity_harness("conversation_mode_follows", FakeFactory::new(true)).await;
    let confirmed_mode = || async {
        harness
            .env
            .chat
            .conversation_detail(&harness.conversation_id)
            .await
            .expect("detail")
            .latest_agent_session_page
            .sessions
            .first()
            .and_then(|session| session.current_mode_id.clone())
    };
    let first = harness
        .run_to_terminal_with("codex", None, Some("plan"))
        .await;
    assert_eq!(first.session_origin, Some(SessionOrigin::New));
    assert_eq!(confirmed_mode().await.as_deref(), Some("plan"));
    let adapter = harness.adapter();
    let session = crate::agent::HarnessSessionId(first.agent_session_id.clone().unwrap().0);

    // The agent leaves plan mode on its own during the next turn.
    adapter.set_session_mode(&session, "default");
    adapter.script_next_turn([HarnessUpdateKind::Other {
        kind: "current_mode_update".into(),
        payload: json!({"sessionUpdate": "current_mode_update", "currentModeId": "default"}),
    }]);
    let second = harness
        .run_to_terminal_with("codex", None, Some("default"))
        .await;
    assert_eq!(second.session_origin, Some(SessionOrigin::Continued));
    assert!(
        adapter.recorded_applied_modes().is_empty(),
        "the confirmed mode is not resent"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while confirmed_mode().await.as_deref() != Some("default") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "announced mode was not persisted"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Re-picking plan switches the live session even though plan was the
    // session's starting mode.
    let third = harness
        .run_to_terminal_with("codex", None, Some("plan"))
        .await;
    assert_eq!(third.session_origin, Some(SessionOrigin::Continued));
    assert_eq!(adapter.recorded_applied_modes(), vec!["plan".to_string()]);
    assert_eq!(confirmed_mode().await.as_deref(), Some("plan"));

    let refused = harness
        .run_to_terminal_with("codex", None, Some("unavailable"))
        .await;
    assert_eq!(refused.status, ConversationRunStatus::Failed);
    assert_eq!(
        adapter.recorded_prompts().len(),
        3,
        "a refused switch never prompts"
    );
    harness.bridge.shutdown().await;
}

/// `conversation.setMode` switches a live session now and persists what the
/// agent acknowledged, skips a switch the session already has, leaves a
/// refused switch unrecorded, rejects a replaced session, and defers when
/// nothing is live to take it.
#[tokio::test]
async fn conversation_set_mode_applies_to_live_sessions_and_defers_otherwise() {
    use ottam_protocol::{
        AgentSessionID, ConversationSetModeOutcome, ConversationSetModeParams,
    };
    let harness = continuity_harness("conversation_set_mode", FakeFactory::new(true)).await;
    let first = harness
        .run_to_terminal_with("codex", None, Some("default"))
        .await;
    assert_eq!(first.status, ConversationRunStatus::Completed);
    let session = || async {
        harness
            .env
            .chat
            .conversation_detail(&harness.conversation_id)
            .await
            .expect("detail")
            .latest_agent_session_page
            .sessions
            .into_iter()
            .next()
            .expect("session")
    };
    let agent_session_id = session().await.agent_session_id;
    let params =
        |agent_session_id: Option<&AgentSessionID>, mode_id: &str| ConversationSetModeParams {
            workspace_id: harness.env.workspace_id.clone(),
            conversation_id: harness.conversation_id.clone(),
            agent_session_id: agent_session_id.cloned(),
            mode_id: mode_id.into(),
        };
    let harness = &harness;
    let set_mode = |params: ConversationSetModeParams| async move {
        harness
            .bridge
            .set_conversation_mode(&harness.env.chat, &params)
            .await
    };
    let adapter = harness.adapter();

    let applied = set_mode(params(Some(&agent_session_id), "acceptEdits"))
        .await
        .unwrap();
    assert_eq!(applied.outcome, ConversationSetModeOutcome::Applied);
    assert_eq!(applied.current_mode_id.as_deref(), Some("acceptEdits"));
    assert_eq!(
        adapter.recorded_applied_modes(),
        vec!["acceptEdits".to_string()]
    );
    assert_eq!(
        session().await.current_mode_id.as_deref(),
        Some("acceptEdits")
    );

    let again = set_mode(params(Some(&agent_session_id), "acceptEdits"))
        .await
        .unwrap();
    assert_eq!(again.outcome, ConversationSetModeOutcome::Applied);
    assert_eq!(
        adapter.recorded_applied_modes().len(),
        1,
        "a current mode is not resent"
    );

    // A client that has not yet seen the session names none; Core targets
    // the conversation's active session.
    let unnamed = set_mode(params(None, "plan")).await.unwrap();
    assert_eq!(unnamed.outcome, ConversationSetModeOutcome::Applied);
    assert_eq!(session().await.current_mode_id.as_deref(), Some("plan"));
    set_mode(params(None, "acceptEdits")).await.unwrap();
    assert_eq!(adapter.recorded_applied_modes().len(), 3);

    let refused = set_mode(params(Some(&agent_session_id), "unavailable")).await;
    assert!(refused.is_err(), "a refused switch is an error");
    assert_eq!(
        session().await.current_mode_id.as_deref(),
        Some("acceptEdits"),
        "a refused switch is not recorded"
    );

    let stale = set_mode(params(
        Some(&AgentSessionID("not-a-session".into())),
        "plan",
    ))
    .await;
    assert_eq!(stale.unwrap_err().kind, "staleAgentSession");

    adapter.forget_sessions();
    let applied_count = adapter.recorded_applied_modes().len();
    let deferred = set_mode(params(Some(&agent_session_id), "plan"))
        .await
        .unwrap();
    assert_eq!(deferred.outcome, ConversationSetModeOutcome::Deferred);
    assert_eq!(deferred.current_mode_id.as_deref(), Some("acceptEdits"));
    let unnamed = set_mode(params(None, "plan")).await.unwrap();
    assert_eq!(unnamed.outcome, ConversationSetModeOutcome::Deferred);
    assert_eq!(
        adapter.recorded_applied_modes().len(),
        applied_count,
        "nothing is sent to a dead session"
    );
    harness.bridge.shutdown().await;
}

#[tokio::test]
async fn conversation_restore_passes_mode_and_does_not_replace_session_on_config_failure() {
    let harness = continuity_harness("conversation_restore_mode", FakeFactory::new(true)).await;
    let first = harness.run_to_terminal("codex").await;
    let adapter = harness.adapter();
    adapter.forget_sessions();
    let prior = first.agent_session_id.unwrap();
    let erased: Arc<dyn HarnessAdapter> = adapter.clone();
    let config = SessionConfigRequest {
        mode_id: Some("auto".into()),
        ..SessionConfigRequest::default()
    };
    let (restored, origin) = super::conversation_runner::resolve_conversation_session(
        &erased,
        Some(&prior),
        Some("plan"),
        &harness.root,
        &config,
        None,
    )
    .await
    .unwrap();
    assert_eq!(restored.0, prior.0);
    assert_eq!(origin, SessionOrigin::Restored);
    assert_eq!(
        *adapter.loaded_modes.lock().unwrap(),
        vec![Some("auto".into())],
        "a requested mode wins over the persisted one"
    );
    // A follow-up requests no mode: the restore reapplies the session's
    // persisted confirmed mode rather than the agent's startup default.
    adapter.forget_sessions();
    super::conversation_runner::resolve_conversation_session(
        &erased,
        Some(&prior),
        Some("plan"),
        &harness.root,
        &SessionConfigRequest::default(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        adapter
            .loaded_modes
            .lock()
            .unwrap()
            .last()
            .cloned()
            .flatten()
            .as_deref(),
        Some("plan")
    );
    adapter.forget_sessions();
    let config = SessionConfigRequest {
        mode_id: Some("unavailable".into()),
        ..config
    };
    let error = super::conversation_runner::resolve_conversation_session(
        &erased,
        Some(&prior),
        None,
        &harness.root,
        &config,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, HarnessErrorKind::SelectionUnavailable);
    assert_eq!(adapter.recorded_new_session_models().len(), 1);
    harness.bridge.shutdown().await;
}

/// A model or effort picked since the last run reaches a held session before
/// the prompt, rather than the run recording a model the agent never took.
#[tokio::test]
async fn conversation_session_applies_a_changed_model_before_prompting() {
    let harness = continuity_harness("conversation_live_model", FakeFactory::new(true)).await;
    let first = harness.run_to_terminal("codex").await;
    let adapter = harness.adapter();
    let prior = first.agent_session_id.unwrap();
    let erased: Arc<dyn HarnessAdapter> = adapter.clone();
    let resolve = |config: SessionConfigRequest| {
        let erased = Arc::clone(&erased);
        let prior = prior.clone();
        let root = harness.root.clone();
        async move {
            super::conversation_runner::resolve_conversation_session(
                &erased,
                Some(&prior),
                None,
                &root,
                &config,
                None,
            )
            .await
        }
    };
    let pick = |model: &str, level: &str| SessionConfigRequest {
        model_id: Some(model.into()),
        thought_level_id: Some(level.into()),
        ..SessionConfigRequest::default()
    };
    let switch = |model: Option<&str>, level: &str, restored: bool| ModelSwitch {
        model_id: model.map(str::to_owned),
        thought_level_id: Some(level.into()),
        restored,
    };

    let (_, origin) = resolve(pick("model-b", "high")).await.unwrap();
    assert_eq!(origin, SessionOrigin::Continued);
    assert_eq!(
        adapter.recorded_applied_models(),
        vec![switch(Some("model-b"), "high", false)],
        "a new model goes out with the level, which the switch can move"
    );
    resolve(pick("model-b", "high")).await.unwrap();
    assert_eq!(
        adapter.recorded_applied_models().len(),
        1,
        "an unchanged pick sends nothing"
    );
    resolve(pick("model-b", "low")).await.unwrap();
    assert_eq!(
        adapter.recorded_applied_models().last(),
        Some(&switch(None, "low", false)),
        "a level change alone leaves the model alone"
    );

    let error = resolve(pick("unavailable", "low")).await.unwrap_err();
    assert_eq!(error.kind, HarnessErrorKind::SelectionUnavailable);
    let session_id = crate::agent::HarnessSessionId(prior.0.clone());
    assert_eq!(
        adapter.session_model(&session_id).0.as_deref(),
        Some("model-b"),
        "a refused switch leaves the session on the model it had"
    );

    // A restored session is compared with what the load reported.
    adapter.forget_sessions();
    let (_, origin) = resolve(pick("model-c", "low")).await.unwrap();
    assert_eq!(origin, SessionOrigin::Restored);
    assert_eq!(
        adapter.recorded_applied_models().last(),
        Some(&switch(Some("model-c"), "low", true))
    );
    harness.bridge.shutdown().await;
}

#[tokio::test]
async fn conversation_run_restores_the_session_after_an_adapter_restart() {
    let harness = continuity_harness("conversation_restore", FakeFactory::new(true)).await;
    let first = harness.run_to_terminal("codex").await;
    harness.adapter().forget_sessions();
    let second = harness.run_to_terminal("codex").await;

    assert_eq!(second.status, ConversationRunStatus::Completed);
    assert_eq!(second.session_origin, Some(SessionOrigin::Restored));
    assert_eq!(first.agent_session_id, second.agent_session_id);
    let expected =
        crate::agent::HarnessSessionId(first.agent_session_id.clone().expect("session id").0);
    assert_eq!(harness.adapter().recorded_load_calls(), vec![expected]);
    harness.bridge.shutdown().await;
}

#[tokio::test]
async fn conversation_run_starts_fresh_when_load_session_is_unsupported() {
    let harness = continuity_harness("conversation_no_load", FakeFactory::new(false)).await;
    let first = harness.run_to_terminal("codex").await;
    harness.adapter().forget_sessions();
    let second = harness.run_to_terminal("codex").await;

    assert_eq!(second.status, ConversationRunStatus::Completed);
    assert_eq!(second.session_origin, Some(SessionOrigin::New));
    assert_ne!(first.agent_session_id, second.agent_session_id);
    assert!(harness.adapter().recorded_load_calls().is_empty());
    harness.bridge.shutdown().await;
}

#[tokio::test]
async fn conversation_run_falls_back_to_a_fresh_session_when_load_fails() {
    let harness = continuity_harness("conversation_load_fail", FakeFactory::new(true)).await;
    let first = harness.run_to_terminal("codex").await;
    let adapter = harness.adapter();
    adapter.forget_sessions();
    adapter.fail_loads.store(true, Ordering::Relaxed);
    let second = harness.run_to_terminal("codex").await;

    assert_eq!(second.status, ConversationRunStatus::Completed);
    assert_eq!(second.session_origin, Some(SessionOrigin::New));
    assert_ne!(first.agent_session_id, second.agent_session_id);
    assert_eq!(adapter.recorded_load_calls().len(), 1);
    // The fresh session never saw the first turn, so it is sent it.
    assert!(second.history_replay.is_some());
    let prompts = adapter.recorded_prompts();
    assert!(
        prompts
            .last()
            .is_some_and(|prompt| prompt.contains("user:\nInitial request")
                && prompt.contains("Earlier in this conversation")),
        "{prompts:?}"
    );
    harness.bridge.shutdown().await;
}

#[tokio::test]
async fn conversation_run_starts_fresh_when_the_engine_changes() {
    let harness = continuity_harness("conversation_engine_switch", FakeFactory::new(true)).await;
    let first = harness.run_to_terminal("codex").await;
    let second = harness.run_to_terminal("claude").await;

    assert_eq!(second.status, ConversationRunStatus::Completed);
    assert_eq!(second.session_origin, Some(SessionOrigin::New));
    assert_ne!(first.agent_session_id, second.agent_session_id);
    // The claude adapter is a separate instance; no load was attempted anywhere.
    for adapter in harness.factory.adapters() {
        assert!(adapter.recorded_load_calls().is_empty());
    }
    harness.bridge.shutdown().await;
}

/// Every prompt each adapter received, in order, keyed by nothing: the
/// tests find a session's prompts by what they say.
fn all_prompts(harness: &ContinuityHarness) -> Vec<String> {
    harness
        .factory
        .adapters()
        .iter()
        .flat_map(|adapter| adapter.recorded_prompts())
        .collect()
}

/// After a switch, the new agent's first prompt carries the earlier turns,
/// once, and stops before the message it delivers. Switching back to a
/// session that saw them replays nothing.
#[tokio::test]
async fn a_switched_agent_is_sent_the_earlier_turns_once() {
    let harness = continuity_harness("conversation_switch_replay", FakeFactory::new(true)).await;
    let first = harness.run_to_terminal("codex").await;
    assert_eq!(first.history_replay, None);
    let switched = harness.run_saying("claude", "What did we discuss?").await;
    assert_eq!(switched.session_origin, Some(SessionOrigin::New));
    assert_eq!(
        switched.history_replay,
        Some(ottam_protocol::ConversationHistoryReplay {
            entries: 2,
            truncated: false,
        })
    );
    let prompts = all_prompts(&harness);
    let replayed = prompts
        .iter()
        .find(|prompt| prompt.contains("What did we discuss?"))
        .expect("claude was prompted");
    assert!(
        replayed.starts_with("You are running inside Ottam Chat.")
            && replayed.contains(&format!(
                "Ottam conversation: {}",
                harness.conversation_id.0
            ))
            && replayed.contains("Earlier in this conversation, before this session began")
            && replayed.contains("They ran on codex.")
            && replayed.contains("user:\nInitial request")
            && replayed.contains("agent (codex):\nHello from fake"),
        "{replayed}"
    );
    assert_eq!(replayed.matches("What did we discuss?").count(), 1);
    // The request comes before the earlier turns.
    assert!(
        replayed
            .find("User request:\nWhat did we discuss?")
            .unwrap()
            < replayed.find("Earlier in this conversation").unwrap()
    );

    let continued = harness.run_saying("claude", "And then?").await;
    assert_eq!(continued.session_origin, Some(SessionOrigin::Continued));
    assert_eq!(continued.history_replay, None);
    let back = harness.run_saying("codex", "Back to codex").await;
    assert_eq!(back.history_replay, None);
    for prompt in all_prompts(&harness)
        .iter()
        .filter(|prompt| prompt.contains("And then?") || prompt.contains("Back to codex"))
    {
        assert!(
            prompt.starts_with("Continue the same Ottam Chat session.")
                && !prompt.contains("Earlier in this conversation"),
            "{prompt}"
        );
    }
    harness.bridge.shutdown().await;
}

/// A command goes out as typed, so the session's first wrapped prompt
/// carries the earlier turns, and the full preamble, instead.
#[tokio::test]
async fn a_command_first_after_a_switch_defers_the_earlier_turns() {
    let harness = continuity_harness("conversation_switch_command", FakeFactory::new(true)).await;
    harness.run_to_terminal("codex").await;
    let command = harness.run_saying("claude", "/compact").await;
    assert_eq!(command.history_replay, None);
    let next = harness.run_saying("claude", "Where were we?").await;
    assert_eq!(next.session_origin, Some(SessionOrigin::Continued));
    assert!(next.history_replay.is_some());
    let prompts = all_prompts(&harness);
    assert!(prompts.iter().any(|prompt| prompt == "/compact"));
    let wrapped = prompts
        .iter()
        .find(|prompt| prompt.contains("Where were we?"))
        .expect("claude was prompted");
    assert!(
        wrapped.starts_with("You are running inside Ottam Chat.")
            && wrapped.contains("user:\nInitial request"),
        "{wrapped}"
    );
    harness.bridge.shutdown().await;
}

/// A fresh session after a failed load, whose earlier session the agent
/// can't bring back.
async fn fresh_session_harness() -> ContinuityHarness {
    let harness = continuity_harness("conversation_seed", FakeFactory::new(true)).await;
    harness.run_to_terminal("codex").await;
    let adapter = harness.adapter();
    adapter.forget_sessions();
    adapter.fail_loads.store(true, Ordering::Relaxed);
    harness
}

/// The prompt that carried `body`.
fn prompt_saying(harness: &ContinuityHarness, body: &str) -> String {
    all_prompts(harness)
        .into_iter()
        .rfind(|prompt| prompt.contains(&format!("User request:\n{body}")))
        .unwrap_or_else(|| panic!("no prompt said {body}"))
}

/// A prompt the agent rejects never reached it, so the session's next
/// prompt carries the earlier turns again.
#[tokio::test]
async fn a_rejected_prompt_leaves_the_session_unseeded() {
    let harness = fresh_session_harness().await;
    harness
        .adapter()
        .refresh_busy_prompts
        .store(1, Ordering::Relaxed);
    let rejected = harness.run_saying("codex", "First try").await;
    assert_eq!(rejected.status, ConversationRunStatus::Failed);
    assert_eq!(rejected.history_replay, None);

    let next = harness.run_saying("codex", "Second try").await;
    assert_eq!(next.session_origin, Some(SessionOrigin::Continued));
    assert!(next.history_replay.is_some());
    let prompt = prompt_saying(&harness, "Second try");
    assert!(
        prompt.starts_with("You are running inside Ottam Chat.")
            && prompt.contains("user:\nInitial request"),
        "{prompt}"
    );
    harness.bridge.shutdown().await;
}

/// An error the agent signals from inside its turn means it had the
/// prompt, so the turns are not sent twice.
#[tokio::test]
async fn an_error_inside_the_turn_keeps_the_seed() {
    let harness = fresh_session_harness().await;
    let failed = harness.run_saying("codex", "typed-terminal").await;
    assert_eq!(failed.status, ConversationRunStatus::Failed);
    assert!(failed.history_replay.is_some());

    let next = harness.run_saying("codex", "After the error").await;
    assert_eq!(next.session_origin, Some(SessionOrigin::Continued));
    assert_eq!(next.history_replay, None);
    let prompt = prompt_saying(&harness, "After the error");
    assert!(
        prompt.starts_with("Continue the same Ottam Chat session.")
            && !prompt.contains("Earlier in this conversation"),
        "{prompt}"
    );
    harness.bridge.shutdown().await;
}

/// A stop while waiting out a busy agent sent nothing, so the session's
/// next prompt carries the earlier turns.
#[tokio::test]
async fn a_stop_before_the_agent_takes_the_prompt_leaves_the_session_unseeded() {
    let harness = fresh_session_harness().await;
    let adapter = harness.adapter();
    adapter.busy_prompts.store(u64::MAX, Ordering::Relaxed);
    let prompts_before = adapter.recorded_prompts().len();
    let input_id = harness
        .env
        .post_message(&harness.conversation_id, "Stopped early")
        .await;
    let started = harness
        .bridge
        .start_conversation_run(
            harness
                .env
                .start_params(&harness.conversation_id, "codex", None, &input_id),
            harness.env.chat.clone(),
            harness.root.clone(),
            None,
        )
        .await
        .expect("run starts");
    tokio::time::timeout(Duration::from_secs(2), async {
        while adapter.recorded_prompts().len() == prompts_before {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the agent turned the prompt away");
    harness
        .bridge
        .cancel_conversation_run(
            &harness.env.chat,
            &harness.conversation_id,
            &started.run.run_id,
        )
        .await
        .expect("the run cancels");
    let stopped = harness
        .env
        .wait_for_terminal(&harness.conversation_id, &started.run.run_id)
        .await;
    assert_eq!(stopped.status, ConversationRunStatus::Canceled);
    assert_eq!(stopped.history_replay, None);

    adapter.busy_prompts.store(0, Ordering::Relaxed);
    let next = harness.run_saying("codex", "After the stop").await;
    assert!(next.history_replay.is_some());
    let prompt = prompt_saying(&harness, "After the stop");
    assert!(
        prompt.starts_with("You are running inside Ottam Chat."),
        "{prompt}"
    );
    harness.bridge.shutdown().await;
}

#[tokio::test]
async fn restored_session_replay_is_not_recorded_as_run_output() {
    let harness = continuity_harness("conversation_replay", FakeFactory::new(true)).await;
    harness.run_to_terminal("codex").await;
    let adapter = harness.adapter();
    adapter.forget_sessions();
    adapter.replay_on_load.store(true, Ordering::Relaxed);
    let second = harness.run_to_terminal("codex").await;

    assert_eq!(second.status, ConversationRunStatus::Completed);
    assert_eq!(second.session_origin, Some(SessionOrigin::Restored));
    let items = harness.env.transcript(&harness.conversation_id).await;
    assert!(items
        .iter()
        .all(|item| !item.body_preview.contains("REPLAYED")));
    assert!(items
        .iter()
        .any(|item| item.body_preview == "Hello from fake"));
    let events = harness.env.events(&harness.conversation_id).await;
    // Name the run each event landed on: the replay leaks onto the *previous*
    // run when that run's session key outlives its terminal status, and the
    // run id is the only thing that tells the two failures apart.
    let leaked = events
        .iter()
        .filter(|event| {
            serde_json::to_string(&event.payload)
                .unwrap_or_default()
                .contains("REPLAYED")
        })
        .map(|event| format!("run={} sequence={}", event.run_id.0, event.sequence))
        .collect::<Vec<_>>();
    assert!(
        leaked.is_empty(),
        "replayed history recorded as run output: {leaked:?}"
    );
    // The replayed chunk and the replayed async task state: replay drops a
    // lifecycle update like any other, whatever its kind.
    let attribution = harness.bridge.conversation.attribution();
    assert_eq!(attribution.dropped("codex", DropReason::Replay), 2);
    assert_eq!(attribution.dropped("codex", DropReason::AfterEnd), 0);
    assert_eq!(attribution.dropped("codex", DropReason::Unattributed), 0);
    assert!(items.iter().all(|item| item
        .tool
        .as_ref()
        .is_none_or(|tool| tool.background_task.is_none())));
    harness.bridge.shutdown().await;
}

fn async_task(id: &str, tool_call_id: Option<&str>) -> crate::agent::AsyncTaskUpdate {
    crate::agent::AsyncTaskUpdate {
        async_task_id: id.into(),
        tool_call_id: tool_call_id.map(str::to_owned),
        ..Default::default()
    }
}

fn background_tool_call(id: &str) -> HarnessUpdateKind {
    HarnessUpdateKind::ToolCall(json!({
        "toolCallId": id,
        "title": "npm run dev",
        "kind": "execute",
        "status": "pending",
    }))
}

/// The tool result both adapters stamp; Codex leaves out the AIR version.
fn backgrounded_stamp(id: &str, air: Value) -> HarnessUpdateKind {
    let mut air = air;
    air["asyncTasks"] = json!({"backgrounded": true});
    HarnessUpdateKind::ToolCallUpdate(json!({
        "toolCallId": id,
        "status": "completed",
        "_meta": {"jetbrains": {"air": air}},
    }))
}

fn transcript_tool<'a>(
    items: &'a [ottam_protocol::ConversationTranscriptItem],
    tool_call_id: &str,
) -> (
    &'a ottam_protocol::ConversationTranscriptItem,
    &'a ottam_protocol::TranscriptTool,
) {
    items
        .iter()
        .find_map(|item| {
            let tool = item.tool.as_ref()?;
            (tool.provider_tool_call_id.as_deref() == Some(tool_call_id)).then_some((item, tool))
        })
        .unwrap_or_else(|| panic!("no tool item for {tool_call_id}"))
}

fn background(
    id: Option<&str>,
    state: ottam_protocol::AsyncTaskState,
    output_file_path: Option<&str>,
) -> Option<ottam_protocol::TranscriptBackgroundTask> {
    Some(ottam_protocol::TranscriptBackgroundTask {
        id: id.map(str::to_owned),
        state,
        output_file_path: output_file_path.map(str::to_owned),
    })
}

/// Claude stamps the tool result after the spawn, and can announce a task
/// that already ended before the stamp arrives. Either way the lifecycle
/// owns the state, and only `backgroundTask` changes on the tool.
#[tokio::test]
async fn async_task_lifecycle_revises_the_running_runs_tool_in_either_stamp_order() {
    use ottam_protocol::AsyncTaskState;
    let harness = continuity_harness("async_task_orders", FakeFactory::new(true)).await;
    // The adapter exists once the first run starts it; script that run's turn
    // by starting the registry's adapter up front.
    harness
        .bridge
        .registry()
        .get_or_start(&AgentId("codex".to_string()))
        .await
        .expect("adapter starts");
    let air_v1 = || json!({"version": 1});
    harness.adapter().script_next_turn([
        // In-turn: spawn, stamp, progress, state.
        background_tool_call("toolu_a"),
        HarnessUpdateKind::AsyncTaskSpawned(crate::agent::AsyncTaskUpdate {
            output_file_path: Some("/tmp/a.output".into()),
            description: Some("Start the dev server".into()),
            ..async_task("task_a", Some("toolu_a"))
        }),
        backgrounded_stamp("toolu_a", air_v1()),
        HarnessUpdateKind::AsyncTaskProgress(crate::agent::AsyncTaskUpdate {
            summary: Some("Compiled 12 modules".into()),
            ..async_task("task_a", None)
        }),
        HarnessUpdateKind::AsyncTaskStateUpdate(
            AsyncTaskState::Completed,
            async_task("task_a", None),
        ),
        // Terminal before the stamp: the stamp must not revive it.
        background_tool_call("toolu_b"),
        HarnessUpdateKind::AsyncTaskSpawned(async_task("task_b", Some("toolu_b"))),
        HarnessUpdateKind::AsyncTaskStateUpdate(
            AsyncTaskState::Failed,
            async_task("task_b", Some("toolu_b")),
        ),
        backgrounded_stamp("toolu_b", air_v1()),
        // Codex: the stamp alone marks the tool before any task id.
        background_tool_call("call_c"),
        backgrounded_stamp("call_c", json!({})),
        HarnessUpdateKind::AgentMessageChunk("Started.".to_string(), None),
    ]);
    let run = harness.run_to_terminal("codex").await;
    assert_eq!(run.status, ConversationRunStatus::Completed);

    let items = harness.env.transcript(&harness.conversation_id).await;
    let (item_a, tool_a) = transcript_tool(&items, "toolu_a");
    assert_eq!(
        tool_a.background_task,
        background(
            Some("task_a"),
            AsyncTaskState::Completed,
            Some("/tmp/a.output")
        )
    );
    assert_eq!(tool_a.title.as_deref(), Some("npm run dev"));
    assert_eq!(tool_a.status.as_deref(), Some("completed"));
    assert_eq!(
        item_a.completion,
        ottam_protocol::TranscriptCompletion::Complete
    );
    let (item_b, tool_b) = transcript_tool(&items, "toolu_b");
    assert_eq!(
        tool_b.background_task,
        background(Some("task_b"), AsyncTaskState::Failed, None)
    );
    // Both took a call, spawn, stamp, and state; A's progress beat changed
    // nothing the row records, so it revised nothing.
    assert_eq!(item_a.revision, item_b.revision);
    let (_, tool_c) = transcript_tool(&items, "call_c");
    assert_eq!(
        tool_c.background_task,
        background(None, AsyncTaskState::Running, None)
    );
    // Lifecycle updates revise tools; they never add a row.
    assert_eq!(
        items
            .iter()
            .filter(|item| item.kind == ottam_protocol::TranscriptItemKind::Tool)
            .count(),
        3
    );
    harness.bridge.shutdown().await;
}

/// A run's live background tasks as `conversation.listTranscript` reports
/// them, if this core has seen any of the run's tasks.
async fn live_tasks(
    env: &ChatEnv,
    conversation_id: &ConversationID,
    run_id: &ConversationRunID,
) -> Option<u32> {
    env.chat
        .list_transcript(ottam_protocol::ConversationListTranscriptParams {
            workspace_id: env.workspace_id.clone(),
            conversation_id: conversation_id.clone(),
            limit: Some(200),
            cursor: None,
            subagent_thread_id: None,
        })
        .await
        .expect("normalized transcript")
        .background_tasks
        .into_iter()
        .find(|count| count.run_id == *run_id)
        .map(|count| count.live)
}

impl ContinuityHarness {
    /// Sends updates for `session` after its turn ended, numbered on from
    /// `after`, and waits until the forwarder has handled them.
    async fn send_after_turn(
        &self,
        session: &ottam_protocol::AgentSessionID,
        after: u64,
        updates: impl IntoIterator<Item = HarnessUpdateKind>,
    ) {
        for (update, sequence) in updates.into_iter().zip(after + 1..) {
            let _ = self.adapter().updates_tx.send(HarnessUpdate {
                session_id: HarnessSessionId(session.0.clone()),
                sequence,
                update,
            });
        }
        self.bridge
            .flush_forwarder(&AgentId("codex".to_string()))
            .await;
    }

    /// Waits for core to write `state` on the tool's background task; loss
    /// writes follow adapter status, not the update stream.
    async fn wait_for_task_state(
        &self,
        tool_call_id: &str,
        state: ottam_protocol::AsyncTaskState,
    ) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let items = self.env.transcript(&self.conversation_id).await;
                let (_, tool) = transcript_tool(&items, tool_call_id);
                if tool.background_task.as_ref().map(|task| task.state) == Some(state) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{tool_call_id} never became {state:?}"));
    }
}

/// Rule 3: a lifecycle update reaches the run that owns its tool call or task,
/// whichever run holds the session now. An update naming no indexed tool is
/// left to the holder, which anchors it only on a tool of its own.
#[tokio::test]
async fn async_task_updates_reach_the_run_that_owns_their_tool() {
    use ottam_protocol::AsyncTaskState;
    let harness = continuity_harness("async_task_anchor", FakeFactory::new(true)).await;
    harness
        .bridge
        .registry()
        .get_or_start(&AgentId("codex".to_string()))
        .await
        .expect("adapter starts");
    harness.adapter().script_next_turn([
        background_tool_call("toolu_prior"),
        HarnessUpdateKind::AsyncTaskSpawned(async_task("task_prior", Some("toolu_prior"))),
    ]);
    let prior_run = harness.run_to_terminal("codex").await;
    let before = harness.env.transcript(&harness.conversation_id).await;
    let (prior_item, prior_tool) = transcript_tool(&before, "toolu_prior");
    let (prior_item, prior_tool) = (prior_item.clone(), prior_tool.clone());
    assert_eq!(
        prior_tool.background_task,
        background(Some("task_prior"), AsyncTaskState::Running, None)
    );
    assert_eq!(
        live_tasks(&harness.env, &harness.conversation_id, &prior_run.run_id).await,
        Some(1)
    );

    harness.adapter().script_next_turn([
        // The previous run's task, by call id and then by task id alone.
        HarnessUpdateKind::AsyncTaskStateUpdate(
            AsyncTaskState::Stopped,
            async_task("task_prior", Some("toolu_prior")),
        ),
        HarnessUpdateKind::AsyncTaskProgress(crate::agent::AsyncTaskUpdate {
            output_file_path: Some("/tmp/prior.output".into()),
            ..async_task("task_prior", None)
        }),
        // Claude's held task: spawned, stamped, and ended before its call id
        // arrived.
        background_tool_call("toolu_held"),
        HarnessUpdateKind::AsyncTaskSpawned(async_task("task_held", None)),
        backgrounded_stamp("toolu_held", json!({"version": 1})),
        HarnessUpdateKind::AsyncTaskStateUpdate(
            AsyncTaskState::Completed,
            async_task("task_held", None),
        ),
        // The late link anchors it; later frames find it by task id alone.
        HarnessUpdateKind::AsyncTaskStateUpdate(
            AsyncTaskState::Completed,
            async_task("task_held", Some("toolu_held")),
        ),
        HarnessUpdateKind::AsyncTaskProgress(crate::agent::AsyncTaskUpdate {
            output_file_path: Some("/tmp/held.output".into()),
            ..async_task("task_held", None)
        }),
        HarnessUpdateKind::AsyncTaskSpawned(async_task("task_unknown", Some("toolu_unknown"))),
    ]);
    let run = harness.run_to_terminal("codex").await;

    let items = harness.env.transcript(&harness.conversation_id).await;
    let (item, tool) = transcript_tool(&items, "toolu_prior");
    assert_eq!(item.revision, prior_item.revision + 2);
    assert_eq!(item.run_id, prior_item.run_id);
    assert_eq!(
        tool.background_task,
        background(
            Some("task_prior"),
            AsyncTaskState::Stopped,
            Some("/tmp/prior.output")
        )
    );
    assert_eq!(tool.status, prior_tool.status);
    let (held_item, held) = transcript_tool(&items, "toolu_held");
    assert_eq!(held_item.run_id.as_ref(), Some(&run.run_id));
    assert_eq!(
        held.background_task,
        background(
            Some("task_held"),
            AsyncTaskState::Completed,
            Some("/tmp/held.output")
        )
    );
    assert_eq!(
        items
            .iter()
            .filter(|item| item.kind == ottam_protocol::TranscriptItemKind::Tool)
            .count(),
        2
    );
    assert_eq!(
        live_tasks(&harness.env, &harness.conversation_id, &prior_run.run_id).await,
        Some(0)
    );
    let attribution = harness.bridge.conversation.attribution();
    assert_eq!(attribution.dropped("codex", DropReason::AfterEnd), 0);
    // Every task ended, so no entry is left live: the held task's stamp
    // landed on the entry that took its end.
    assert!(attribution.take_lost_tasks(|_, _, _| true).is_empty());
    harness.bridge.shutdown().await;
}

/// Codex reconciles after `end_turn`, so its stamp, spawn, and terminal state
/// all follow the run's release (rules 3 and 4). The closed run takes only
/// the background task; the status the stamp carries stays out.
#[tokio::test]
async fn async_task_updates_after_release_reach_the_released_run() {
    use ottam_protocol::AsyncTaskState;
    let harness = continuity_harness("async_task_after_end", FakeFactory::new(true)).await;
    harness
        .bridge
        .registry()
        .get_or_start(&AgentId("codex".to_string()))
        .await
        .expect("adapter starts");
    harness
        .adapter()
        .script_next_turn([background_tool_call("call_bg")]);
    let run = harness.run_to_terminal("codex").await;
    let session = run
        .agent_session_id
        .clone()
        .expect("the run bound a session");
    let ended = harness.env.transcript(&harness.conversation_id).await;
    let (_, ended_tool) = transcript_tool(&ended, "call_bg");
    let ended_tool = ended_tool.clone();

    harness
        .send_after_turn(
            &session,
            1,
            [
                backgrounded_stamp("call_bg", json!({})),
                HarnessUpdateKind::AsyncTaskSpawned(async_task("call_bg", Some("call_bg"))),
            ],
        )
        .await;
    let items = harness.env.transcript(&harness.conversation_id).await;
    let (_, tool) = transcript_tool(&items, "call_bg");
    assert_eq!(
        tool.background_task,
        background(Some("call_bg"), AsyncTaskState::Running, None)
    );
    assert_eq!(tool.status, ended_tool.status);
    assert_eq!(
        live_tasks(&harness.env, &harness.conversation_id, &run.run_id).await,
        Some(1)
    );

    harness
        .send_after_turn(
            &session,
            3,
            [HarnessUpdateKind::AsyncTaskStateUpdate(
                AsyncTaskState::Stopped,
                async_task("call_bg", Some("call_bg")),
            )],
        )
        .await;
    let items = harness.env.transcript(&harness.conversation_id).await;
    let (_, tool) = transcript_tool(&items, "call_bg");
    assert_eq!(
        tool.background_task,
        background(Some("call_bg"), AsyncTaskState::Stopped, None)
    );
    assert_eq!(
        live_tasks(&harness.env, &harness.conversation_id, &run.run_id).await,
        Some(0)
    );
    let attribution = harness.bridge.conversation.attribution();
    assert_eq!(attribution.dropped("codex", DropReason::AfterEnd), 0);
    harness.bridge.shutdown().await;
}

/// Claude can spawn before naming the tool call. The spawn is held for the
/// run that started it, and a frame that names the call after the run ended
/// still anchors it there.
#[tokio::test]
async fn async_task_spawn_without_a_call_anchors_on_the_released_run() {
    use ottam_protocol::AsyncTaskState;
    let harness = continuity_harness("async_task_held", FakeFactory::new(true)).await;
    harness
        .bridge
        .registry()
        .get_or_start(&AgentId("codex".to_string()))
        .await
        .expect("adapter starts");
    harness.adapter().script_next_turn([
        background_tool_call("toolu_x"),
        HarnessUpdateKind::AsyncTaskSpawned(crate::agent::AsyncTaskUpdate {
            output_file_path: Some("/tmp/x.output".into()),
            ..async_task("task_x", None)
        }),
    ]);
    let run = harness.run_to_terminal("codex").await;
    let session = run
        .agent_session_id
        .clone()
        .expect("the run bound a session");
    let items = harness.env.transcript(&harness.conversation_id).await;
    assert_eq!(transcript_tool(&items, "toolu_x").1.background_task, None);

    harness
        .send_after_turn(
            &session,
            2,
            [
                HarnessUpdateKind::AsyncTaskProgress(async_task("task_x", None)),
                HarnessUpdateKind::AsyncTaskStateUpdate(
                    AsyncTaskState::Completed,
                    async_task("task_x", Some("toolu_x")),
                ),
            ],
        )
        .await;
    let items = harness.env.transcript(&harness.conversation_id).await;
    assert_eq!(
        transcript_tool(&items, "toolu_x").1.background_task,
        background(
            Some("task_x"),
            AsyncTaskState::Completed,
            Some("/tmp/x.output")
        )
    );
    assert_eq!(
        live_tasks(&harness.env, &harness.conversation_id, &run.run_id).await,
        Some(0)
    );
    harness.bridge.shutdown().await;
}

/// Rule 7: when the adapter exits or restarts, nothing can end the tasks it
/// was running. Core marks them lost and leaves an ended task as it was.
#[tokio::test]
async fn async_task_running_when_the_adapter_goes_away_is_marked_lost() {
    use ottam_protocol::AsyncTaskState;
    // Unwatched: no forwarder sees the change, and the next one judges it at
    // start, by generation or by the status it finds.
    for (restart, watched) in [(false, true), (true, true), (false, false), (true, false)] {
        let harness = continuity_harness("async_task_lost", FakeFactory::new(true)).await;
        let adapter = harness
            .bridge
            .registry()
            .get_or_start(&AgentId("codex".to_string()))
            .await
            .expect("adapter starts");
        harness.adapter().script_next_turn([
            background_tool_call("call_live"),
            backgrounded_stamp("call_live", json!({})),
            background_tool_call("call_done"),
            HarnessUpdateKind::AsyncTaskSpawned(async_task("task_done", Some("call_done"))),
            HarnessUpdateKind::AsyncTaskStateUpdate(
                AsyncTaskState::Completed,
                async_task("task_done", Some("call_done")),
            ),
        ]);
        let run = harness.run_to_terminal("codex").await;
        assert_eq!(
            live_tasks(&harness.env, &harness.conversation_id, &run.run_id).await,
            Some(1)
        );
        if !watched {
            harness.bridge.conversation.forwarders().shutdown().await;
        }
        harness.adapter().status_tx.send_modify(|status| {
            if restart {
                status.generation += 1;
                status.lifecycle = AdapterLifecycleState::Starting;
            } else {
                status.lifecycle = AdapterLifecycleState::Dead;
            }
        });
        if !watched {
            harness.bridge.ensure_forwarder(adapter).await;
        }
        harness
            .wait_for_task_state("call_live", AsyncTaskState::Lost)
            .await;
        let items = harness.env.transcript(&harness.conversation_id).await;
        assert_eq!(
            transcript_tool(&items, "call_done").1.background_task,
            background(Some("task_done"), AsyncTaskState::Completed, None)
        );
        assert_eq!(
            live_tasks(&harness.env, &harness.conversation_id, &run.run_id).await,
            Some(0)
        );
        assert_eq!(
            harness
                .bridge
                .conversation
                .attribution()
                .indexed_tasks("codex"),
            1,
            "only the lost task leaves the index"
        );
        harness.bridge.shutdown().await;
    }
}

/// Rule 7: a prior session the adapter no longer has cannot end its tasks,
/// even when `session/load` restores its history for the next run.
#[tokio::test]
async fn async_task_running_in_a_session_that_did_not_survive_is_marked_lost() {
    use ottam_protocol::AsyncTaskState;
    let harness = continuity_harness("async_task_session_gone", FakeFactory::new(true)).await;
    harness
        .bridge
        .registry()
        .get_or_start(&AgentId("codex".to_string()))
        .await
        .expect("adapter starts");
    harness.adapter().script_next_turn([
        background_tool_call("call_bg"),
        backgrounded_stamp("call_bg", json!({})),
    ]);
    let first = harness.run_to_terminal("codex").await;
    harness.adapter().forget_sessions();
    let second = harness.run_to_terminal("codex").await;
    assert_eq!(second.status, ConversationRunStatus::Completed);
    let items = harness.env.transcript(&harness.conversation_id).await;
    assert_eq!(
        transcript_tool(&items, "call_bg").1.background_task,
        background(None, AsyncTaskState::Lost, None)
    );
    assert_eq!(
        live_tasks(&harness.env, &harness.conversation_id, &first.run_id).await,
        Some(0)
    );
    harness.bridge.shutdown().await;
}

impl ContinuityHarness {
    /// Opens an autonomous follow-up of `continues` for output on `session`
    /// from the adapter's `generation`.
    async fn open_autonomous(
        &self,
        continues: &ConversationRunID,
        session: &str,
        generation: u64,
    ) -> Result<ConversationRunSummary, CoreError> {
        // A follow-up waits for any other run of the conversation to leave.
        tokio::time::timeout(Duration::from_secs(2), async {
            while self.bridge.conversation.contains_run(continues).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the continued run leaves");
        let continues = TaskOwner {
            chat: self.env.chat.clone(),
            workspace_id: self.env.workspace_id.clone(),
            project_id: None,
            conversation_id: self.conversation_id.clone(),
            run_id: continues.clone(),
        };
        self.bridge
            .conversation
            .open_autonomous_run(
                &continues,
                &("codex".to_string(), session.to_string()),
                generation,
            )
            .await
    }

    /// Runs a user turn to its end, then opens an autonomous follow-up of it
    /// on its session, as a background task's completion would.
    async fn open_wake_up(&self) -> (ConversationRunSummary, ConversationRunSummary) {
        let adapter = self
            .bridge
            .registry()
            .get_or_start(&AgentId("codex".to_string()))
            .await
            .expect("adapter starts");
        let continued = self.run_to_terminal("codex").await;
        let session = continued
            .agent_session_id
            .clone()
            .expect("the run bound a session");
        // A prompted run's worker frees its slot just after the run ends.
        tokio::time::timeout(Duration::from_secs(2), async {
            while self.bridge.regular_lane.try_acquire().is_err() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the continued run frees its lane slot");
        let generation = adapter.status().borrow().generation;
        let wake_up = self
            .open_autonomous(&continued.run_id, &session.0, generation)
            .await
            .expect("the follow-up opens");
        (continued, wake_up)
    }

    async fn run_count(&self) -> usize {
        self.env
            .chat
            .list_runs(&self.conversation_id, Some(50), None)
            .await
            .expect("lists runs")
            .runs
            .len()
    }
}

/// Item 3: what the agent sends on its own after a turn ended lands in an
/// autonomous run on the same session. Closing it ends it `completed`, and
/// the user's next run continues the session.
#[tokio::test]
async fn autonomous_follow_up_records_out_of_turn_output_and_completes_on_close() {
    let harness = continuity_harness("autonomous_close", FakeFactory::new(true)).await;
    let (continued, wake_up) = harness.open_wake_up().await;
    let session = continued
        .agent_session_id
        .clone()
        .expect("the run bound a session");
    assert_eq!(wake_up.origin, RunOrigin::Autonomous);
    assert_eq!(wake_up.status, ConversationRunStatus::Running);
    assert_eq!(wake_up.agent_session_id.as_ref(), Some(&session));
    assert_eq!(wake_up.session_origin, Some(SessionOrigin::Continued));
    assert_eq!(
        harness
            .bridge
            .conversation
            .attributed_sessions_for_run(&wake_up.run_id),
        vec![("codex".to_string(), session.0.clone())]
    );

    harness
        .send_after_turn(
            &session,
            2,
            [HarnessUpdateKind::AgentMessageChunk(
                "The build finished.".to_string(),
                None,
            )],
        )
        .await;
    harness
        .bridge
        .conversation
        .close_autonomous_run(&harness.conversation_id)
        .await;
    let closed = harness
        .env
        .run(&harness.conversation_id, &wake_up.run_id)
        .await;
    assert_eq!(closed.status, ConversationRunStatus::Completed);
    assert_eq!(closed.stop_reason, None);
    assert_conversation_run_released(&harness.bridge, &wake_up.run_id).await;
    let items = harness.env.transcript(&harness.conversation_id).await;
    let reply = items
        .iter()
        .find(|item| item.body_preview == "The build finished.")
        .expect("the follow-up's reply");
    assert_eq!(reply.run_id.as_ref(), Some(&wake_up.run_id));

    let next = harness.run_to_terminal("codex").await;
    assert_eq!(next.status, ConversationRunStatus::Completed);
    assert_eq!(next.origin, RunOrigin::User);
    assert_eq!(next.agent_session_id, Some(session));
    harness.bridge.shutdown().await;
}

/// A user start preempts a live follow-up, which ends `completed` with what
/// the agent already sent. The start takes its session and, on a lane of
/// one, its slot.
#[tokio::test]
async fn autonomous_follow_up_is_preempted_by_a_user_start() {
    let harness = continuity_harness_with_lane(FakeFactory::new(true), 1).await;
    let (continued, wake_up) = harness.open_wake_up().await;
    let session = continued
        .agent_session_id
        .clone()
        .expect("the run bound a session");
    harness
        .send_after_turn(
            &session,
            2,
            [HarnessUpdateKind::AgentMessageChunk(
                "Tests passed.".to_string(),
                None,
            )],
        )
        .await;

    let next = harness.run_to_terminal("codex").await;
    assert_eq!(next.status, ConversationRunStatus::Completed);
    assert_eq!(next.origin, RunOrigin::User);
    assert_eq!(next.agent_session_id, Some(session));
    let preempted = harness
        .env
        .run(&harness.conversation_id, &wake_up.run_id)
        .await;
    assert_eq!(preempted.status, ConversationRunStatus::Completed);
    assert_eq!(preempted.stop_reason, None);
    assert_conversation_run_released(&harness.bridge, &wake_up.run_id).await;
    let items = harness.env.transcript(&harness.conversation_id).await;
    let owner = |body: &str, run: &ConversationRunID| {
        items
            .iter()
            .any(|item| item.body_preview == body && item.run_id.as_ref() == Some(run))
    };
    assert!(owner("Tests passed.", &wake_up.run_id));
    assert!(owner("Hello from fake", &next.run_id));
    assert_eq!(harness.bridge.conversation.launch_locks(), 0);
    harness.bridge.shutdown().await;
}

/// A steer against a follow-up is not steered, whatever the engine supports:
/// it preempts the follow-up and starts a user run carrying the message.
#[tokio::test]
async fn autonomous_follow_up_steer_becomes_a_preempting_start() {
    for steering in [SteeringCapability::Native, SteeringCapability::Unsupported] {
        let harness = continuity_harness(
            "autonomous_steer",
            FakeFactory::with_steering(true, steering),
        )
        .await;
        let (continued, wake_up) = harness.open_wake_up().await;
        let steer = || async {
            harness
                .bridge
                .steer_conversation_run(
                    ConversationSteerRunParams {
                        workspace_id: harness.env.workspace_id.clone(),
                        conversation_id: harness.conversation_id.clone(),
                        run_id: wake_up.run_id.clone(),
                        message: "use the release build".to_string(),
                        idempotency_key: "wake-up-steer".to_string(),
                        attachment_ids: Vec::new(),
                    },
                    harness.env.chat.clone(),
                    harness.root.clone(),
                    None,
                )
                .await
        };
        let steered = steer()
            .await
            .unwrap_or_else(|error| panic!("{steering:?}: the steer starts a run: {error}"));
        assert_ne!(steered.run.run_id, wake_up.run_id);
        assert_eq!(steered.run.origin, RunOrigin::User);

        let next = harness
            .env
            .wait_for_terminal(&harness.conversation_id, &steered.run.run_id)
            .await;
        assert_eq!(next.status, ConversationRunStatus::Completed);
        assert_eq!(next.agent_session_id, continued.agent_session_id);
        let preempted = harness
            .env
            .run(&harness.conversation_id, &wake_up.run_id)
            .await;
        assert_eq!(preempted.status, ConversationRunStatus::Completed);
        let adapter = harness.adapter();
        assert!(adapter.recorded_steers().is_empty());
        assert!(adapter
            .recorded_prompts()
            .last()
            .is_some_and(|prompt| prompt.contains("use the release build")));
        assert_eq!(harness.bridge.conversation.launch_locks(), 0);

        // A retry of the same steer, now that the follow-up has ended, is
        // answered with what the first one did, on either engine.
        let prompts = adapter.recorded_prompts().len();
        let replayed = steer()
            .await
            .unwrap_or_else(|error| panic!("{steering:?}: the steer replays: {error}"));
        assert_eq!(replayed.input.run_input_id, steered.input.run_input_id);
        assert_eq!(replayed.run.run_id, steered.run.run_id);
        assert_eq!(adapter.recorded_prompts().len(), prompts);
        assert!(adapter.recorded_steers().is_empty());
        assert_eq!(harness.run_count().await, 3);
        harness.bridge.shutdown().await;
    }
}

#[tokio::test]
async fn autonomous_follow_up_cancel_ends_canceled() {
    let harness = continuity_harness("autonomous_cancel", FakeFactory::new(true)).await;
    let (_, wake_up) = harness.open_wake_up().await;
    let canceled = harness
        .bridge
        .cancel_conversation_run(&harness.env.chat, &harness.conversation_id, &wake_up.run_id)
        .await
        .expect("cancels the follow-up")
        .run;
    assert_eq!(canceled.status, ConversationRunStatus::Canceled);
    assert_eq!(canceled.stop_reason, Some(AgentStopReason::Cancelled));
    assert_conversation_run_released(&harness.bridge, &wake_up.run_id).await;
    harness.bridge.shutdown().await;
}

/// A follow-up opens only on a session that the adapter which sent the
/// output still runs, and that the continued run ran on. A refusal writes
/// nothing.
#[tokio::test]
async fn autonomous_follow_up_refuses_a_lost_session() {
    let harness = continuity_harness("autonomous_lost", FakeFactory::new(true)).await;
    let adapter = harness
        .bridge
        .registry()
        .get_or_start(&AgentId("codex".to_string()))
        .await
        .expect("adapter starts");
    let continued = harness.run_to_terminal("codex").await;
    let session = continued
        .agent_session_id
        .clone()
        .expect("the run bound a session");
    let generation = adapter.status().borrow().generation;
    let other = HarnessSessionId("another-session".to_string());
    harness.adapter().remember_session(&other);

    let earlier_generation = harness
        .open_autonomous(&continued.run_id, &session.0, generation + 1)
        .await;
    let other_session = harness
        .open_autonomous(&continued.run_id, &other.0, generation)
        .await;
    harness.adapter().forget_sessions();
    let gone = harness
        .open_autonomous(&continued.run_id, &session.0, generation)
        .await;
    for refused in [earlier_generation, other_session, gone] {
        let error = refused.expect_err("the follow-up is refused");
        assert_eq!(error.kind, "sessionNotFound");
    }
    assert_eq!(harness.run_count().await, 1);
    harness.bridge.shutdown().await;
}

/// An opener checks the session once it holds the launch lock, so a session
/// the agent drops while the opener waits for that lock refuses the open.
#[tokio::test]
async fn autonomous_follow_up_checks_the_session_under_the_launch_lock() {
    let harness = continuity_harness("autonomous_lock", FakeFactory::new(true)).await;
    let adapter = harness
        .bridge
        .registry()
        .get_or_start(&AgentId("codex".to_string()))
        .await
        .expect("adapter starts");
    let continued = harness.run_to_terminal("codex").await;
    let session = continued
        .agent_session_id
        .clone()
        .expect("the run bound a session");
    let generation = adapter.status().borrow().generation;

    let conversation = &harness.bridge.conversation;
    let launch = conversation.lock_launch(&harness.conversation_id).await;
    let lose_session = async {
        while conversation.launch_lock_users(&harness.conversation_id) < 2 {
            tokio::task::yield_now().await;
        }
        harness.adapter().forget_sessions();
        drop(launch);
    };
    let (opened, ()) = tokio::join!(
        harness.open_autonomous(&continued.run_id, &session.0, generation),
        lose_session,
    );
    let error = opened.expect_err("the follow-up is refused");
    assert_eq!(error.kind, "sessionNotFound");
    assert_eq!(harness.run_count().await, 1);
    assert_eq!(conversation.launch_locks(), 0);
    harness.bridge.shutdown().await;
}

/// A user start that `begin_run` would refuse leaves a live follow-up alone:
/// unresolved delivery is checked before the follow-up is closed.
#[tokio::test]
async fn autonomous_follow_up_outlives_a_start_refused_for_unresolved_delivery() {
    let env = chat_env().await;
    let conversation = env.create_conversation("hold for an ambiguous steer").await;
    let factory = FakeFactory::with_steering(true, SteeringCapability::Native);
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry_with(factory),
    );
    let conversation_id = &conversation.conversation_id;
    let started = bridge
        .start_conversation_run(
            env.start_params(conversation_id, "codex", None, &conversation.input_id),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("run starts");
    let held = env
        .wait_for_running(conversation_id, &started.run.run_id)
        .await;
    let session = held.agent_session_id.expect("the run bound a session");
    // A steer whose transport fails after sending leaves its delivery unknown.
    let transport = bridge
        .steer_conversation_run(
            ConversationSteerRunParams {
                workspace_id: env.workspace_id.clone(),
                conversation_id: conversation_id.clone(),
                run_id: started.run.run_id.clone(),
                message: "transport-error".to_string(),
                idempotency_key: "transport-key".to_string(),
                attachment_ids: Vec::new(),
            },
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect_err("transport ambiguity is returned");
    assert_eq!(transport.kind, "adapterExited");
    let ambiguous = env
        .chat
        .list_run_inputs(conversation_id, Some(100), None)
        .await
        .expect("lists inputs")
        .run_inputs
        .into_iter()
        .find(|input| input.status == RunInputStatus::DeliveryUnknown)
        .expect("deliveryUnknown input");
    bridge
        .cancel_conversation_run(&env.chat, conversation_id, &started.run.run_id)
        .await
        .expect("the held run cancels");
    assert_conversation_run_released(&bridge, &started.run.run_id).await;

    let adapter = bridge
        .registry()
        .get_or_start(&AgentId("codex".to_string()))
        .await
        .expect("adapter is running");
    let generation = adapter.status().borrow().generation;
    let continues = TaskOwner {
        chat: env.chat.clone(),
        workspace_id: env.workspace_id.clone(),
        project_id: None,
        conversation_id: conversation_id.clone(),
        run_id: started.run.run_id.clone(),
    };
    let wake_up = bridge
        .conversation
        .open_autonomous_run(
            &continues,
            &("codex".to_string(), session.0.clone()),
            generation,
        )
        .await
        .expect("the follow-up opens");

    let refused = bridge
        .start_conversation_run(
            env.start_params(conversation_id, "codex", None, &ambiguous.run_input_id),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect_err("unresolved delivery refuses the start");
    assert_eq!(refused.kind, "deliveryUnresolved");
    assert!(bridge.conversation.contains_run(&wake_up.run_id).await);
    assert_eq!(
        env.run(conversation_id, &wake_up.run_id).await.status,
        ConversationRunStatus::Running
    );
    assert_eq!(bridge.conversation.launch_locks(), 0);
    bridge.shutdown().await;
}

/// The adapter that ran the session stops, dies, or restarts: the
/// follow-up ends `failed` and lets go of the session.
#[tokio::test]
async fn autonomous_follow_up_ends_failed_when_the_adapter_exits() {
    for restart in [false, true] {
        let harness = continuity_harness("autonomous_exit", FakeFactory::new(true)).await;
        let (_, wake_up) = harness.open_wake_up().await;
        harness.adapter().status_tx.send_modify(|status| {
            if restart {
                status.generation += 1;
                status.lifecycle = AdapterLifecycleState::Starting;
            } else {
                status.lifecycle = AdapterLifecycleState::Dead;
            }
        });
        let ended = harness
            .env
            .wait_for_terminal(&harness.conversation_id, &wake_up.run_id)
            .await;
        assert_eq!(ended.status, ConversationRunStatus::Failed);
        assert_eq!(ended.error["kind"], "adapterExited");
        assert_conversation_run_released(&harness.bridge, &wake_up.run_id).await;
        harness.bridge.shutdown().await;
    }
}

/// `conversation.get` answers with five pages that a client reads as one
/// state. Taken outside a snapshot they straddle concurrent commits, and the
/// run status and the assistant reply are written in the *same* transaction —
/// so a reader that catches the seam sees a completed run whose answer has
/// not arrived, and keeps that view until something else refreshes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conversation_detail_never_reports_a_completed_run_without_its_reply() {
    let harness = continuity_harness("conversation_detail_seam", FakeFactory::new(true)).await;
    let chat = harness.env.chat.clone();
    let conversation_id = harness.conversation_id.clone();
    let stop = CancellationToken::new();
    let stop_reader = stop.clone();
    let torn = Arc::new(AtomicU64::new(0));
    let torn_reader = Arc::clone(&torn);
    let reader = tokio::spawn(async move {
        while !stop_reader.is_cancelled() {
            if let Ok(detail) = chat.conversation_detail(&conversation_id).await {
                let completed = detail
                    .latest_run_page
                    .runs
                    .iter()
                    .filter(|run| run.status == ConversationRunStatus::Completed)
                    .count();
                let items = chat
                    .list_transcript(ottam_protocol::ConversationListTranscriptParams {
                        workspace_id: detail.workspace.workspace_id.clone(),
                        conversation_id: conversation_id.clone(),
                        limit: Some(200),
                        cursor: None,
                        subagent_thread_id: None,
                    })
                    .await
                    .expect("transcript after observed terminal status")
                    .items;
                let assistant = items
                    .iter()
                    .filter(|item| {
                        item.kind == ottam_protocol::TranscriptItemKind::AssistantText
                    })
                    .count();
                if assistant < completed {
                    torn_reader.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    });
    for _ in 0..40 {
        harness.run_to_terminal("codex").await;
    }
    stop.cancel();
    let _ = reader.await;
    assert_eq!(
        torn.load(Ordering::Relaxed),
        0,
        "conversation.get reported a completed run without its assistant message"
    );
    harness.bridge.shutdown().await;
}

/// The window the release closes: `wait_for_terminal` reads the persisted
/// status, which is exactly what a client polls before sending its next
/// message. If the run still owned its session at that point, the next run's
/// `session/load` replay would be recorded against this one.
#[tokio::test]
async fn a_finished_run_releases_its_session_before_reporting_terminal_status() {
    let harness = continuity_harness("conversation_session_release", FakeFactory::new(true)).await;
    let run = harness.run_to_terminal("codex").await;

    assert_eq!(run.status, ConversationRunStatus::Completed);
    let held = harness.bridge.conversation.attributed_session_keys();
    assert!(
        held.is_empty(),
        "a terminal run still owns its agent session: {held:?}"
    );
    assert_conversation_run_released(&harness.bridge, &run.run_id).await;
    harness.bridge.shutdown().await;
}

#[tokio::test]
async fn continued_prompt_recovers_when_the_session_vanished_in_flight() {
    let harness = continuity_harness("conversation_vanish", FakeFactory::new(true)).await;
    let first = harness.run_to_terminal("codex").await;
    let adapter = harness.adapter();
    adapter.vanish_next_prompt.store(true, Ordering::Relaxed);
    let second = harness.run_to_terminal("codex").await;

    assert_eq!(second.status, ConversationRunStatus::Completed);
    // The run started Continued, raced the simulated restart, and recovered
    // by restoring the same session via session/load.
    assert_eq!(second.session_origin, Some(SessionOrigin::Restored));
    assert_eq!(first.agent_session_id, second.agent_session_id);
    assert_eq!(adapter.recorded_load_calls().len(), 1);
    assert_eq!(adapter.recorded_prompts().len(), 3);
    // The restored session saw every turn, so nothing is replayed.
    assert_eq!(second.history_replay, None);
    harness.bridge.shutdown().await;
}

/// When the vanished session cannot be restored either, the retry lands on
/// a fresh session, which is sent the earlier turns.
#[tokio::test]
async fn a_vanished_session_that_cannot_be_restored_is_replaced_and_seeded() {
    let harness = continuity_harness("conversation_vanish_fresh", FakeFactory::new(true)).await;
    harness.run_to_terminal("codex").await;
    let adapter = harness.adapter();
    adapter.vanish_next_prompt.store(true, Ordering::Relaxed);
    adapter.fail_loads.store(true, Ordering::Relaxed);
    let second = harness.run_saying("codex", "Still there?").await;

    assert_eq!(second.status, ConversationRunStatus::Completed);
    assert_eq!(second.session_origin, Some(SessionOrigin::New));
    assert!(second.history_replay.is_some());
    let prompts = adapter.recorded_prompts();
    let retried = prompts.last().expect("the retry prompted");
    assert!(
        retried.starts_with("You are running inside Ottam Chat.")
            && retried.contains("user:\nInitial request")
            && retried.matches("Still there?").count() == 1,
        "{retried}"
    );
    harness.bridge.shutdown().await;
}

/// An agent that rejects a prompt while it settles (oh-my-pi, #10707) never
/// took the message, so the run resends it rather than failing.
#[tokio::test]
async fn a_prompt_the_agent_rejects_as_busy_is_resent() {
    let harness = continuity_harness("conversation_remote_busy", FakeFactory::new(true)).await;
    harness.run_to_terminal("codex").await;
    let adapter = harness.adapter();
    adapter.busy_prompts.store(2, Ordering::Relaxed);
    let second = harness.run_to_terminal("codex").await;

    assert_eq!(second.status, ConversationRunStatus::Completed);
    assert_eq!(second.session_origin, Some(SessionOrigin::Continued));
    assert_eq!(adapter.recorded_prompts().len(), 4);
    harness.bridge.shutdown().await;
}

/// A harness whose Claude has an Ottam-owned home under `homes`.
async fn claude_home_harness(
    factory: Arc<FakeFactory>,
    homes: &std::path::Path,
) -> ContinuityHarness {
    const CLAUDE_HOME: &[crate::agent::HomeVariable] = &[crate::agent::HomeVariable {
        name: "CLAUDE_CONFIG_DIR",
        target: crate::agent::HomeTarget::Root,
    }];
    let env = chat_env().await;
    let conversation = env.create_conversation("Initial request").await;
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let definitions = ["codex", "claude"]
        .into_iter()
        .map(|id| AgentDefinition {
            home_variables: if id == "claude" { CLAUDE_HOME } else { &[] },
            ..fake_definition(id)
        })
        .collect();
    let registry = AgentRegistry::with_components(
        definitions,
        Arc::new(ReadyProbe),
        Arc::clone(&factory) as Arc<dyn crate::agent::AdapterFactory>,
    )
    .with_home_root(homes.to_path_buf());
    let bridge = AgentRuntimeBridge::new_with_registry_and_regular_limit(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        registry,
        MAX_DEFERRED_AGENT_REQUESTS,
    );
    ContinuityHarness {
        env,
        root: conversation.root,
        initial_input: StdMutex::new(Some(conversation.input_id)),
        bridge,
        factory,
        conversation_id: conversation.conversation_id,
    }
}

/// A turn blocked by a refresh lock whose holder exited never reached the
/// model: core clears the lock and sends it once more. With nothing to clear,
/// or after that one resend, the failure stands.
#[tokio::test]
async fn a_prompt_blocked_by_an_abandoned_refresh_lock_is_resent_once() {
    use crate::agent::refresh_lock::CLAUDE_REFRESH_LOCK;
    let homes = tempfile::tempdir().unwrap();
    let harness = claude_home_harness(FakeFactory::new(true), homes.path()).await;
    harness.run_to_terminal("claude").await;
    let adapter = harness.adapter();
    let home = homes.path().join("claude").join("home");

    *adapter.abandon_refresh_in.lock().unwrap() = Some(home.clone());
    adapter.refresh_busy_prompts.store(1, Ordering::Relaxed);
    let resent = harness.run_to_terminal("claude").await;
    assert_eq!(resent.status, ConversationRunStatus::Completed);
    assert_eq!(adapter.recorded_prompts().len(), 3);
    assert!(!home.join(CLAUDE_REFRESH_LOCK).exists());

    *adapter.abandon_refresh_in.lock().unwrap() = None;
    adapter.refresh_busy_prompts.store(1, Ordering::Relaxed);
    let live_holder = harness.run_to_terminal("claude").await;
    assert_eq!(live_holder.status, ConversationRunStatus::Failed);
    assert_eq!(adapter.recorded_prompts().len(), 4, "nothing cleared, nothing resent");

    *adapter.abandon_refresh_in.lock().unwrap() = Some(home.clone());
    adapter.refresh_busy_prompts.store(2, Ordering::Relaxed);
    let still_blocked = harness.run_to_terminal("claude").await;
    assert_eq!(still_blocked.status, ConversationRunStatus::Failed);
    assert_eq!(adapter.recorded_prompts().len(), 6, "resent once, not looped");
    harness.bridge.shutdown().await;
}

#[tokio::test]
async fn in_flight_recovery_does_not_record_replayed_history_as_run_output() {
    let harness = continuity_harness("conversation_vanish_replay", FakeFactory::new(true)).await;
    harness.run_to_terminal("codex").await;
    let adapter = harness.adapter();
    adapter.vanish_next_prompt.store(true, Ordering::Relaxed);
    adapter.replay_on_load.store(true, Ordering::Relaxed);
    let second = harness.run_to_terminal("codex").await;

    assert_eq!(second.status, ConversationRunStatus::Completed);
    // The recovery restores the SAME session id, so the run's session key is
    // unchanged; the replay must still be dropped rather than re-recorded.
    assert_eq!(second.session_origin, Some(SessionOrigin::Restored));
    let detail = harness
        .env
        .chat
        .conversation_detail(&harness.conversation_id)
        .await
        .expect("conversation detail");
    assert!(
        detail
            .latest_message_page
            .messages
            .iter()
            .all(|message| !message.body.contains("REPLAYED")),
        "replayed history must not be persisted as fresh output"
    );
    let events = harness.env.events(&harness.conversation_id).await;
    assert!(events.iter().all(|event| {
        !serde_json::to_string(&event.payload)
            .unwrap_or_default()
            .contains("REPLAYED")
    }));
    harness.bridge.shutdown().await;
}

#[tokio::test]
async fn conversation_run_inserts_paragraph_break_when_tool_activity_splits_messages() {
    let env = chat_env().await;
    let conversation = env.create_conversation("boundary markdown prompt").await;
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry(),
    );

    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "codex",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("run starts");

    let run = env
        .wait_for_terminal(&conversation.conversation_id, &started.run.run_id)
        .await;
    assert_eq!(run.status, ConversationRunStatus::Completed);

    let items = env.transcript(&conversation.conversation_id).await;
    assert_eq!(
        items.iter().rev().map(|item| item.kind).collect::<Vec<_>>(),
        vec![
            ottam_protocol::TranscriptItemKind::Message,
            ottam_protocol::TranscriptItemKind::AssistantText,
            ottam_protocol::TranscriptItemKind::Tool,
            ottam_protocol::TranscriptItemKind::AssistantText,
            ottam_protocol::TranscriptItemKind::RunBoundary
        ]
    );
    assert_eq!(items[1].body_preview, "## Heading continues");
    assert_eq!(items[3].body_preview, "Intro paragraph.");
    bridge.shutdown().await;
}

#[tokio::test]
async fn typed_terminal_failure_keeps_delivered_input_and_partial_output() {
    let env = chat_env().await;
    let conversation = env.create_conversation("typed-terminal prompt").await;
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry(),
    );
    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "codex",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .unwrap();
    let run = env
        .wait_for_terminal(&conversation.conversation_id, &started.run.run_id)
        .await;
    assert_eq!(run.status, ConversationRunStatus::Failed);
    let detail = env
        .chat
        .conversation_detail(&conversation.conversation_id)
        .await
        .unwrap();
    let input = detail
        .latest_run_input_page
        .run_inputs
        .iter()
        .find(|input| input.run_input_id == conversation.input_id)
        .unwrap();
    assert_eq!(input.status, RunInputStatus::Delivered);
    assert!(env
        .transcript(&conversation.conversation_id)
        .await
        .iter()
        .any(|item| item.body_preview == "retained partial output"));
    bridge.shutdown().await;
}

#[tokio::test]
async fn conversation_run_adopts_harness_session_title() {
    let env = chat_env().await;
    let conversation = env.create_conversation("retitle this prompt").await;
    let initial = env
        .chat
        .conversation_detail(&conversation.conversation_id)
        .await
        .expect("initial conversation detail");
    assert_eq!(initial.conversation.title, "retitle this prompt");
    let (outbound_tx, mut outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry(),
    );

    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "codex",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("run starts");

    let completion = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let run = env
                .run(&conversation.conversation_id, &started.run.run_id)
                .await;
            match run.status {
                ConversationRunStatus::Completed => break,
                ConversationRunStatus::Failed | ConversationRunStatus::Canceled => {
                    panic!(
                        "run reached unexpected terminal state {:?}: {}",
                        run.status, run.error
                    );
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if completion.is_err() {
        let run = env
            .run(&conversation.conversation_id, &started.run.run_id)
            .await;
        let events = env.events(&conversation.conversation_id).await;
        panic!(
            "run did not complete: status={:?}, error={}, events={}",
            run.status,
            run.error,
            events.len()
        );
    }

    // The recorder persists the title before notifying every connected client.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let detail = env
                .chat
                .conversation_detail(&conversation.conversation_id)
                .await
                .expect("conversation detail");
            if detail.conversation.title == "Harness generated title" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("harness title adopted");
    let mut title_notification = false;
    while let Ok(frame) = outbound_rx.try_recv() {
        let frame = frame.frame;
        if frame["method"] == "conversation.eventDelta"
            && frame["params"]["eventKind"] == "sessionMetadata"
            && frame["params"]["payload"]["title"] == "Harness generated title"
        {
            title_notification = true;
        }
    }
    assert!(
        title_notification,
        "persisted title must also be delivered live"
    );
    bridge.shutdown().await;
}

#[tokio::test]
async fn conversation_run_rejects_prompt_wrapper_as_fallback_session_title() {
    let env = chat_env().await;
    let conversation = env
        .create_conversation("fallback-title actual request")
        .await;
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry(),
    );

    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "codex",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("run starts");

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let run = env
                .run(&conversation.conversation_id, &started.run.run_id)
                .await;
            if run.status == ConversationRunStatus::Completed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("run completes");

    let detail = env
        .chat
        .conversation_detail(&conversation.conversation_id)
        .await
        .expect("conversation detail");
    assert_eq!(detail.conversation.title, "fallback-title actual request");

    assert!(env
        .events(&conversation.conversation_id)
        .await
        .iter()
        .all(|event| event.payload.get("kind").is_none()));
    bridge.shutdown().await;
}

#[tokio::test]
async fn conversation_run_cancellation_finishes_and_preserves_terminal_state() {
    let env = chat_env().await;
    let conversation = env.create_conversation("hold this prompt").await;
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry(),
    );
    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "codex",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("run starts");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if env
                .run(&conversation.conversation_id, &started.run.run_id)
                .await
                .status
                == ConversationRunStatus::Running
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("run reaches running");
    let canceled = bridge
        .cancel_conversation_run(
            &env.chat,
            &conversation.conversation_id,
            &started.run.run_id,
        )
        .await
        .expect("run cancels");
    assert_eq!(canceled.run.status, ConversationRunStatus::Canceled);
    assert_eq!(canceled.run.stop_reason, Some(AgentStopReason::Cancelled));
    bridge.shutdown().await;
}

#[tokio::test]
async fn completed_run_records_turn_usage_with_the_agent_scope() {
    let env = chat_env().await;
    let conversation = env.create_conversation("usage please").await;
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry(),
    );
    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "codex",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("run starts");
    let run = env
        .wait_for_terminal(&conversation.conversation_id, &started.run.run_id)
        .await;
    assert_eq!(run.status, ConversationRunStatus::Completed);
    assert!(!run.usage_unavailable);
    let usage = run.usage.expect("a completed run keeps its usage");
    assert_eq!(
        usage.scope,
        ottam_protocol::ConversationRunUsageScope::LastRequest
    );
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.total_tokens),
        (1_200, 300, 1_500)
    );
    assert_eq!(usage.cached_read_tokens, Some(800));
    assert_eq!(usage.thought_tokens, None);
    let events = env.events(&conversation.conversation_id).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_kind == ottam_protocol::ConversationEventKind::Usage)
            .count(),
        1
    );

    // A second report for the same run is ignored.
    let deltas = env
        .chat
        .record_turn_usage(
            &conversation.conversation_id,
            &started.run.run_id,
            None,
            ottam_protocol::ConversationRunUsage {
                input_tokens: 9,
                output_tokens: 9,
                total_tokens: 18,
                ..usage.clone()
            },
        )
        .await
        .expect("duplicate usage is not an error");
    assert!(deltas.is_empty());
    let run = env
        .run(&conversation.conversation_id, &started.run.run_id)
        .await;
    assert_eq!(run.usage, Some(usage));
    bridge.shutdown().await;
}

#[tokio::test]
async fn stopped_run_records_usage_from_the_late_cancelled_reply() {
    let env = chat_env().await;
    let conversation = env.create_conversation("usage-late prompt").await;
    let factory = FakeFactory::new(true);
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry_with(factory.clone()),
    );
    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "claude",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("run starts");
    env.wait_for_running(&conversation.conversation_id, &started.run.run_id)
        .await;
    wait_for_late_usage_prompt(&factory).await;
    let canceled = bridge
        .cancel_conversation_run(
            &env.chat,
            &conversation.conversation_id,
            &started.run.run_id,
        )
        .await
        .expect("run cancels");
    assert_eq!(canceled.run.status, ConversationRunStatus::Canceled);
    assert_eq!(canceled.run.usage, None);
    assert!(canceled.run.usage_unavailable);
    release_late_usage(&factory);
    let usage = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let run = env
                .run(&conversation.conversation_id, &started.run.run_id)
                .await;
            if let Some(usage) = run.usage {
                assert_eq!(run.status, ConversationRunStatus::Canceled);
                assert!(!run.usage_unavailable);
                return usage;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the late reply's usage lands on the stopped run");
    assert_eq!(
        usage.scope,
        ottam_protocol::ConversationRunUsageScope::MainAgent
    );
    assert_eq!(usage.total_tokens, 1_500);
    bridge.shutdown().await;
}

#[tokio::test]
async fn stopped_last_request_run_ignores_the_cancelled_reply_usage() {
    let env = chat_env().await;
    let conversation = env.create_conversation("usage-late prompt").await;
    let factory = FakeFactory::new(true);
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry_with(factory.clone()),
    );
    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "codex",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("run starts");
    env.wait_for_running(&conversation.conversation_id, &started.run.run_id)
        .await;
    wait_for_late_usage_prompt(&factory).await;
    bridge
        .cancel_conversation_run(
            &env.chat,
            &conversation.conversation_id,
            &started.run.run_id,
        )
        .await
        .expect("run cancels");
    release_late_usage(&factory);
    // Past the fake's late reply.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let run = env
        .run(&conversation.conversation_id, &started.run.run_id)
        .await;
    assert_eq!(
        run.usage, None,
        "codex's last request may be the previous turn's"
    );
    assert!(run.usage_unavailable);
    bridge.shutdown().await;
}

#[tokio::test]
async fn native_steer_delivers_to_the_active_run_idempotently() {
    let env = chat_env().await;
    let conversation = env.create_conversation("hold native prompt").await;
    let factory = FakeFactory::with_steering(true, SteeringCapability::Native);
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry_with(factory.clone()),
    );
    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "codex",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("run starts");
    env.wait_for_running(&conversation.conversation_id, &started.run.run_id)
        .await;
    let params = ConversationSteerRunParams {
        workspace_id: env.workspace_id.clone(),
        conversation_id: conversation.conversation_id.clone(),
        run_id: started.run.run_id.clone(),
        message: "native direction".to_string(),
        idempotency_key: "native-steer-key".to_string(),
        attachment_ids: Vec::new(),
    };
    let steered = bridge
        .steer_conversation_run(
            params.clone(),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("native steer succeeds");
    assert_eq!(steered.run.run_id, started.run.run_id);
    assert_eq!(steered.run.status, ConversationRunStatus::Running);
    assert_eq!(steered.input.status, RunInputStatus::Delivered);
    let replay = bridge
        .steer_conversation_run(params, env.chat.clone(), conversation.root.clone(), None)
        .await
        .expect("idempotent steer replays");
    assert_eq!(replay.input.run_input_id, steered.input.run_input_id);
    let second = bridge
        .steer_conversation_run(
            ConversationSteerRunParams {
                workspace_id: env.workspace_id.clone(),
                conversation_id: conversation.conversation_id.clone(),
                run_id: started.run.run_id.clone(),
                message: "second native direction".to_string(),
                idempotency_key: "native-steer-key-2".to_string(),
                attachment_ids: Vec::new(),
            },
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("second native steer succeeds");
    assert_ne!(second.input.run_input_id, steered.input.run_input_id);
    let steers = factory.adapters()[0].recorded_steers();
    assert_eq!(steers.len(), 2);
    assert!(
        steers[1].contains("second native direction"),
        "steers reach the adapter in order: {steers:?}"
    );
    bridge
        .cancel_conversation_run(
            &env.chat,
            &conversation.conversation_id,
            &started.run.run_id,
        )
        .await
        .expect("run cancels");
    bridge.shutdown().await;
}

#[tokio::test]
async fn unattributed_update_from_a_detached_steer_turn_never_reaches_the_run() {
    let env = chat_env().await;
    let conversation = env.create_conversation("hold native prompt").await;
    let factory = FakeFactory::with_steering(true, SteeringCapability::Native);
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry_with(factory.clone()),
    );
    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "codex",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("run starts");
    let running = env
        .wait_for_running(&conversation.conversation_id, &started.run.run_id)
        .await;
    let session = running.agent_session_id.expect("the run bound a session");

    // The detached turn streams before the steer's answer arrives.
    let error = bridge
        .steer_conversation_run(
            ConversationSteerRunParams {
                workspace_id: env.workspace_id.clone(),
                conversation_id: conversation.conversation_id.clone(),
                run_id: started.run.run_id.clone(),
                message: "detached-steer direction".to_string(),
                idempotency_key: "detached-steer-key".to_string(),
                attachment_ids: Vec::new(),
            },
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect_err("a detached turn leaves the steer unresolved");
    assert_eq!(error.kind, "promptExecution");
    bridge
        .cancel_conversation_run(
            &env.chat,
            &conversation.conversation_id,
            &started.run.run_id,
        )
        .await
        .expect("run cancels");
    env.wait_for_terminal(&conversation.conversation_id, &started.run.run_id)
        .await;

    let attribution = bridge.conversation.attribution();
    assert_eq!(attribution.dropped("codex", DropReason::DetachedSteer), 1);
    let generation = factory.adapters()[0].status().borrow().generation;
    assert_eq!(
        resolution_label(attribution.resolve(&session_key(&session.0), generation)),
        "detached_steer"
    );
    let items = env.transcript(&conversation.conversation_id).await;
    assert!(items
        .iter()
        .all(|item| !item.body_preview.contains("DETACHED OUTPUT")));
    bridge.shutdown().await;
}

#[tokio::test]
async fn interrupt_then_prompt_creates_followup_and_preserves_session() {
    let env = chat_env().await;
    let conversation = env.create_conversation("hold interrupt prompt").await;
    let factory = FakeFactory::new(true);
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    // One regular slot proves the replacement launch cannot overtake the
    // interrupted worker's permit release.
    let bridge = AgentRuntimeBridge::new_with_registry_and_regular_limit(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry_with(factory),
        1,
    );
    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "codex",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("run starts");
    let original = env
        .wait_for_running(&conversation.conversation_id, &started.run.run_id)
        .await;
    let initial_ownership = bridge
        .conversation
        .ownership_snapshot(&started.run.run_id)
        .await
        .expect("initial run is coordinator-owned");
    assert!(initial_ownership.worker_registered);
    assert!(initial_ownership.attribution_registered);
    let steered = bridge
        .steer_conversation_run(
            ConversationSteerRunParams {
                workspace_id: env.workspace_id.clone(),
                conversation_id: conversation.conversation_id.clone(),
                run_id: started.run.run_id.clone(),
                message: "stuck followup direction".to_string(),
                idempotency_key: "interrupt-steer-key".to_string(),
                attachment_ids: Vec::new(),
            },
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("interrupt steer succeeds");
    assert_ne!(steered.run.run_id, started.run.run_id);
    let interrupted = env
        .run(&conversation.conversation_id, &started.run.run_id)
        .await;
    assert_eq!(interrupted.status, ConversationRunStatus::Interrupted);
    let followup = env
        .wait_for_running(&conversation.conversation_id, &steered.run.run_id)
        .await;
    assert_eq!(followup.agent_session_id, original.agent_session_id);
    let followup_ownership = bridge
        .conversation
        .ownership_snapshot(&steered.run.run_id)
        .await
        .expect("follow-up run is coordinator-owned");
    assert_eq!(followup_ownership, initial_ownership);
    bridge
        .cancel_conversation_run(
            &env.chat,
            &conversation.conversation_id,
            &steered.run.run_id,
        )
        .await
        .expect("follow-up cancels");
    assert_conversation_run_released(&bridge, &steered.run.run_id).await;
    bridge.shutdown().await;
}

#[tokio::test]
async fn native_steer_rejection_transport_ambiguity_and_resolution_are_typed() {
    let env = chat_env().await;
    let conversation = env.create_conversation("hold native failures").await;
    let factory = FakeFactory::with_steering(true, SteeringCapability::Native);
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry_with(factory),
    );
    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "codex",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("run starts");
    env.wait_for_running(&conversation.conversation_id, &started.run.run_id)
        .await;
    let steer = |message: &str, key: &str| ConversationSteerRunParams {
        workspace_id: env.workspace_id.clone(),
        conversation_id: conversation.conversation_id.clone(),
        run_id: started.run.run_id.clone(),
        message: message.to_string(),
        idempotency_key: key.to_string(),
        attachment_ids: Vec::new(),
    };
    let rejected = bridge
        .steer_conversation_run(
            steer("reject-steer", "reject-key"),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect_err("definitive rejection is returned");
    assert_eq!(rejected.kind, "internal");
    let page = env
        .chat
        .list_run_inputs(&conversation.conversation_id, Some(100), None)
        .await
        .expect("inputs after rejection");
    assert!(page
        .run_inputs
        .iter()
        .any(|input| input.status == RunInputStatus::Failed));

    let transport = bridge
        .steer_conversation_run(
            steer("transport-error", "transport-key"),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect_err("transport ambiguity is returned");
    assert_eq!(transport.kind, "adapterExited");
    let page = env
        .chat
        .list_run_inputs(&conversation.conversation_id, Some(100), None)
        .await
        .expect("inputs after ambiguity");
    let ambiguous = page
        .run_inputs
        .iter()
        .find(|input| input.status == RunInputStatus::DeliveryUnknown)
        .expect("deliveryUnknown input")
        .clone();
    let blocked = bridge
        .steer_conversation_run(
            steer("blocked steer", "blocked-key"),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect_err("unresolved delivery blocks new work");
    assert_eq!(blocked.kind, "deliveryUnresolved");
    let (resolved, retry) = env
        .chat
        .resolve_run_input_delivery(
            &conversation.conversation_id,
            &ambiguous.run_input_id,
            ottam_protocol::RunInputDeliveryResolution::MarkDelivered,
        )
        .await
        .expect("mark delivered resolves ambiguity");
    assert_eq!(resolved.status, RunInputStatus::Delivered);
    assert_eq!(
        resolved.delivery_evidence,
        Some(ottam_protocol::RunInputDeliveryEvidence::UserMarkedDelivered)
    );
    assert!(retry.is_none());
    bridge
        .cancel_conversation_run(
            &env.chat,
            &conversation.conversation_id,
            &started.run.run_id,
        )
        .await
        .expect("run cancels");
    bridge.shutdown().await;
}

#[tokio::test]
async fn reissued_external_session_id_retires_the_stale_holder() {
    let env = chat_env().await;
    let first = env.create_conversation("first prompt").await;
    let second = env.create_conversation("second prompt").await;
    let engine = EngineID("codex".to_string());
    let external = ottam_protocol::AgentSessionID("reissued-session-1".to_string());

    let first_run = env
        .chat
        .begin_run(
            &first.conversation_id,
            &engine,
            None,
            None,
            None,
            &first.input_id,
        )
        .await
        .expect("begins the first run");
    env.chat
        .mark_run_running(
            &first.conversation_id,
            &first_run.run.run_id,
            &external,
            ottam_protocol::SessionOrigin::New,
        )
        .await
        .expect("binds the first conversation to the external session");

    // A restarted adapter reissues the same external id to a different
    // Conversation; the stale holder is retired instead of colliding.
    let second_run = env
        .chat
        .begin_run(
            &second.conversation_id,
            &engine,
            None,
            None,
            None,
            &second.input_id,
        )
        .await
        .expect("begins the second run");
    env.chat
        .mark_run_running(
            &second.conversation_id,
            &second_run.run.run_id,
            &external,
            ottam_protocol::SessionOrigin::New,
        )
        .await
        .expect("rebinding the reissued external id succeeds");

    let stale = env
        .chat
        .list_agent_sessions(&first.conversation_id, Some(10), None)
        .await
        .expect("lists the first conversation's sessions")
        .sessions;
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].state, ottam_protocol::AgentSessionState::Lost);
    assert_eq!(
        stale[0].end_reason.as_deref(),
        Some("replacedByNewExternalSession")
    );

    let live = env
        .chat
        .list_agent_sessions(&second.conversation_id, Some(10), None)
        .await
        .expect("lists the second conversation's sessions")
        .sessions;
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].state, ottam_protocol::AgentSessionState::Active);
}

#[tokio::test]
async fn unsupported_steer_never_degrades_into_stop() {
    let env = chat_env().await;
    let conversation = env.create_conversation("hold unsupported steer").await;
    let factory = FakeFactory::with_steering(true, SteeringCapability::Unsupported);
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry_with(factory),
    );
    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "codex",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("run starts");
    env.wait_for_running(&conversation.conversation_id, &started.run.run_id)
        .await;
    let error = bridge
        .steer_conversation_run(
            ConversationSteerRunParams {
                workspace_id: env.workspace_id.clone(),
                conversation_id: conversation.conversation_id.clone(),
                run_id: started.run.run_id.clone(),
                message: "must not stop".to_string(),
                idempotency_key: "unsupported-key".to_string(),
                attachment_ids: Vec::new(),
            },
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect_err("unsupported steer is rejected");
    assert_eq!(error.kind, "steeringUnsupported");
    assert_eq!(
        env.run(&conversation.conversation_id, &started.run.run_id)
            .await
            .status,
        ConversationRunStatus::Running
    );
    let detail = env
        .chat
        .conversation_detail(&conversation.conversation_id)
        .await
        .expect("conversation detail");
    assert_eq!(detail.latest_message_page.messages.len(), 1);
    bridge
        .cancel_conversation_run(
            &env.chat,
            &conversation.conversation_id,
            &started.run.run_id,
        )
        .await
        .expect("run cancels");
    bridge.shutdown().await;
}

#[tokio::test]
async fn cancellation_finalizes_a_run_after_the_worker_panics() {
    let env = chat_env().await;
    let conversation = env.create_conversation("panic during this prompt").await;
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry(),
    );
    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "codex",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("run starts");

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let finished = bridge.conversation.run_finished(&started.run.run_id).await;
            if finished {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("panicked worker signals completion");

    let canceled = tokio::time::timeout(
        Duration::from_secs(2),
        bridge.cancel_conversation_run(
            &env.chat,
            &conversation.conversation_id,
            &started.run.run_id,
        ),
    )
    .await
    .expect("cancellation does not hang")
    .expect("orphaned run is finalized");
    assert_eq!(canceled.run.status, ConversationRunStatus::Canceled);
    assert_conversation_run_released(&bridge, &started.run.run_id).await;
    bridge.shutdown().await;
}

#[tokio::test]
async fn cancellation_aborts_and_finalizes_an_unresponsive_worker() {
    let env = chat_env().await;
    let conversation = env.create_conversation("stuck prompt").await;
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry(),
    );
    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "codex",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root.clone(),
            None,
        )
        .await
        .expect("run starts");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if env
                .run(&conversation.conversation_id, &started.run.run_id)
                .await
                .status
                == ConversationRunStatus::Running
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("run reaches running");

    let canceled = tokio::time::timeout(
        Duration::from_secs(1),
        bridge.cancel_conversation_run(
            &env.chat,
            &conversation.conversation_id,
            &started.run.run_id,
        ),
    )
    .await
    .expect("bounded cancellation")
    .expect("stuck run is finalized");
    assert_eq!(canceled.run.status, ConversationRunStatus::Canceled);
    assert_conversation_run_released(&bridge, &started.run.run_id).await;
    bridge.shutdown().await;
}

#[tokio::test]
async fn shutdown_finalizes_multiple_active_runs_and_clears_runtime_bookkeeping() {
    let env = chat_env().await;
    let first = env.create_conversation("first stuck prompt").await;
    let second = env.create_conversation("second stuck prompt").await;
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry(),
    );

    let first_started = bridge
        .start_conversation_run(
            env.start_params(&first.conversation_id, "codex", None, &first.input_id),
            env.chat.clone(),
            first.root,
            None,
        )
        .await
        .expect("first run starts");
    let second_started = bridge
        .start_conversation_run(
            env.start_params(&second.conversation_id, "codex", None, &second.input_id),
            env.chat.clone(),
            second.root,
            None,
        )
        .await
        .expect("second run starts");
    env.wait_for_running(&first.conversation_id, &first_started.run.run_id)
        .await;
    env.wait_for_running(&second.conversation_id, &second_started.run.run_id)
        .await;

    tokio::time::timeout(Duration::from_secs(3), bridge.shutdown())
        .await
        .expect("shutdown remains bounded");

    for (conversation_id, run_id) in [
        (&first.conversation_id, &first_started.run.run_id),
        (&second.conversation_id, &second_started.run.run_id),
    ] {
        assert_eq!(
            env.run(conversation_id, run_id).await.status,
            ConversationRunStatus::Canceled
        );
        assert_conversation_run_released(&bridge, run_id).await;
    }
    assert!(
        !bridge.conversation.has_attributions(),
        "shutdown leaves session attribution behind"
    );
    assert!(
        bridge.conversation.workers_empty(),
        "shutdown leaves conversation worker handles behind"
    );
}

#[tokio::test]
async fn shutdown_fences_final_partial_update_before_canceled_status() {
    let env = chat_env().await;
    let conversation = env.create_conversation("shutdown-partial prompt").await;
    let (outbound_tx, mut outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry(),
    );

    let started = bridge
        .start_conversation_run(
            env.start_params(
                &conversation.conversation_id,
                "codex",
                None,
                &conversation.input_id,
            ),
            env.chat.clone(),
            conversation.root,
            None,
        )
        .await
        .expect("run starts");
    env.wait_for_running(&conversation.conversation_id, &started.run.run_id)
        .await;

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let frame = outbound_rx.recv().await.expect("outbound update").frame;
            let appended = frame["method"]
                == ottam_protocol::NOTIFICATION_CONVERSATION_TRANSCRIPT_DELTA
                && frame["params"]["bodyAppends"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|append| append["text"] == "partial before shutdown");
            if appended {
                break;
            }
        }
    })
    .await
    .expect("final partial update is forwarded before shutdown");

    tokio::time::timeout(Duration::from_secs(3), bridge.shutdown())
        .await
        .expect("shutdown remains bounded");

    let run = env
        .run(&conversation.conversation_id, &started.run.run_id)
        .await;
    assert_eq!(run.status, ConversationRunStatus::Canceled);
    let items = env.transcript(&conversation.conversation_id).await;
    let assistant = items
        .iter()
        .find(|item| item.kind == ottam_protocol::TranscriptItemKind::AssistantText)
        .expect("partial item persisted before canceled status");
    assert_eq!(assistant.body_preview, "partial before shutdown");
    assert_eq!(
        assistant.completion,
        ottam_protocol::TranscriptCompletion::Partial
    );
    assert!(
        !bridge.conversation.contains_run(&started.run.run_id).await,
        "shutdown leaves an active run behind"
    );
    assert!(
        !bridge.conversation.has_attributions(),
        "shutdown leaves session attribution behind"
    );
    assert!(
        bridge.conversation.workers_empty(),
        "shutdown leaves conversation worker handles behind"
    );
}

#[tokio::test]
async fn completion_fence_is_not_starved_by_another_streaming_session() {
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_registry(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        fake_registry(),
    );
    let adapter = Arc::new(FakeConversationAdapter::new(
        fake_definition("codex"),
        1,
        true,
        SteeringCapability::InterruptThenPrompt,
    ));
    adapter.start().await.expect("adapter starts");
    bridge.ensure_forwarder(adapter.clone()).await;

    let updates = adapter.updates_tx.clone();
    let spam = tokio::spawn(async move {
        let mut sequence = 1;
        loop {
            let _ = updates.send(HarnessUpdate {
                session_id: crate::agent::HarnessSessionId("other-session".to_string()),
                sequence,
                update: HarnessUpdateKind::AgentMessageChunk("x".to_string(), None),
            });
            sequence = sequence.saturating_add(1);
            tokio::task::yield_now().await;
        }
    });

    tokio::time::timeout(
        Duration::from_millis(250),
        bridge.flush_forwarder(&AgentId("codex".to_string())),
    )
    .await
    .expect("completion fence is serviced while another session streams");
    spam.abort();
    let _ = spam.await;
    bridge.shutdown().await;
}

#[tokio::test]
async fn recorder_publishes_capture_only_delta_after_rejected_tool_mutation() {
    let env = chat_env().await;
    let conversation = env.create_conversation("capture loss").await;
    let started = env
        .chat
        .begin_run(
            &conversation.conversation_id,
            &EngineID("codex".into()),
            None,
            None,
            None,
            &conversation.input_id,
        )
        .await
        .unwrap();
    let (outbound, mut receiver) = crate::server::outbound::test_sink();
    let (recorder, owner) = ConversationRecorder::spawn(
        env.chat.clone(),
        conversation.conversation_id.clone(),
        started.run.run_id.clone(),
        None,
        outbound,
    );
    recorder.queue_update(AgentUpdate::ToolCall {
        payload: json!({"toolCallId":"bounded","rawInput":{"command":"ls"},"status":"in_progress"}),
    }).await.unwrap();
    let initial = receiver.try_recv().unwrap().frame;
    let initial_sequence = initial["params"]["sequence"].as_u64().unwrap();
    let position = initial["params"]["items"][0]["position"].as_u64().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let items = env.transcript(&conversation.conversation_id).await;
            if items.iter().any(|item| {
                item.position == position && item.committed_revision == Some(item.revision)
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("accepted tool checkpoint");
    while receiver.try_recv().is_ok() {}

    recorder.queue_update(AgentUpdate::ToolCallUpdate {
        payload: json!({"toolCallId":"bounded","rawInput":{"command":"x".repeat(8 * 1024 * 1024)}}),
    }).await.unwrap();
    // No further update, finalization, reload, or checkpoint is needed to tell
    // the client that this accepted tool's replacement could not be retained.
    let notification = receiver
        .try_recv()
        .expect("capture-only notification")
        .frame;
    assert_eq!(
        notification["method"],
        ottam_protocol::NOTIFICATION_CONVERSATION_TRANSCRIPT_DELTA
    );
    let delta: ottam_protocol::ConversationTranscriptDeltaParams =
        serde_json::from_value(notification["params"].clone()).unwrap();
    assert!(delta.items.is_empty());
    assert!(delta.body_appends.is_empty());
    assert!(delta.sequence > initial_sequence);
    let capture = delta
        .capture
        .iter()
        .find(|capture| capture.run_id == started.run.run_id)
        .unwrap();
    assert_eq!(capture.state.as_deref(), Some("partial"));
    assert_eq!(capture.gaps[0].start_position, position);
    owner.close(&recorder).await.unwrap();
}

/// The tool row's revision in each transcript delta, in publish order.
fn tool_revisions(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<crate::server::outbound::OutboundFrame>,
    kind: &str,
) -> Vec<u64> {
    let mut revisions = Vec::new();
    while let Ok(frame) = receiver.try_recv() {
        for item in frame.frame["params"]["items"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if item["kind"] == kind {
                revisions.push(item["revision"].as_u64().unwrap());
            }
        }
    }
    revisions
}

#[tokio::test]
async fn recorder_coalesces_rapid_progress_on_one_tool_call() {
    let env = chat_env().await;
    let conversation = env.create_conversation("progress").await;
    let started = env
        .chat
        .begin_run(
            &conversation.conversation_id,
            &EngineID("codex".into()),
            None,
            None,
            None,
            &conversation.input_id,
        )
        .await
        .unwrap();
    let (outbound, mut receiver) = crate::server::outbound::test_sink();
    let (recorder, owner) = ConversationRecorder::spawn(
        env.chat.clone(),
        conversation.conversation_id.clone(),
        started.run.run_id.clone(),
        None,
        outbound,
    );
    recorder.queue_update(AgentUpdate::ToolCall {
        payload: json!({"toolCallId":"batch","title":"Mapping","kind":"other","status":"in_progress","rawInput":{}}),
    }).await.unwrap();
    assert_eq!(tool_revisions(&mut receiver, "tool"), vec![1]);

    for step in 0..10 {
        recorder.queue_update(AgentUpdate::ToolCallUpdate {
            payload: json!({"toolCallId":"batch","status":"in_progress","rawOutput":format!("step {step}")}),
        }).await.unwrap();
    }
    assert!(
        tool_revisions(&mut receiver, "tool").is_empty(),
        "rapid progress is held"
    );

    // Ten updates land as one, once the interval has passed.
    tokio::time::sleep(TOOL_PROGRESS_INTERVAL + Duration::from_millis(200)).await;
    assert_eq!(tool_revisions(&mut receiver, "tool"), vec![2]);

    // Completion is never held, and carries what was held before it.
    recorder
        .queue_update(AgentUpdate::ToolCallUpdate {
            payload: json!({"toolCallId":"batch","status":"in_progress","rawOutput":"step 10"}),
        })
        .await
        .unwrap();
    assert!(
        tool_revisions(&mut receiver, "tool").is_empty(),
        "progress after a flush is held too"
    );
    recorder
        .queue_update(AgentUpdate::ToolCallUpdate {
            payload: json!({"toolCallId":"batch","status":"completed"}),
        })
        .await
        .unwrap();
    let completed = tool_revisions(&mut receiver, "tool");
    assert_eq!(
        completed.len(),
        1,
        "held progress and completion publish together"
    );
    owner.close(&recorder).await.unwrap();
    let items = env.transcript(&conversation.conversation_id).await;
    let tool = items
        .iter()
        .find(|item| item.kind == ottam_protocol::TranscriptItemKind::Tool)
        .unwrap();
    // A tool payload saved off the write path attaches as a later revision,
    // which may land after the completion.
    assert!(tool.revision >= completed[0]);
    assert_eq!(
        tool.completion,
        ottam_protocol::TranscriptCompletion::Complete
    );
}

#[tokio::test]
async fn held_progress_is_recorded_before_a_new_tool_call() {
    let env = chat_env().await;
    let conversation = env.create_conversation("progress order").await;
    let started = env
        .chat
        .begin_run(
            &conversation.conversation_id,
            &EngineID("codex".into()),
            None,
            None,
            None,
            &conversation.input_id,
        )
        .await
        .unwrap();
    let (outbound, mut receiver) = crate::server::outbound::test_sink();
    let (recorder, owner) = ConversationRecorder::spawn(
        env.chat.clone(),
        conversation.conversation_id.clone(),
        started.run.run_id.clone(),
        None,
        outbound,
    );
    recorder.queue_update(AgentUpdate::ToolCall {
        payload: json!({"toolCallId":"first","title":"Batch","kind":"other","status":"in_progress","rawInput":{}}),
    }).await.unwrap();
    recorder
        .queue_update(AgentUpdate::ToolCallUpdate {
            payload: json!({"toolCallId":"first","rawOutput":"progress"}),
        })
        .await
        .unwrap();
    // Text passes held progress without releasing it.
    recorder
        .queue_update(AgentUpdate::AgentThoughtChunk {
            text: "thinking".into(),
            message_id: None,
        })
        .await
        .unwrap();
    assert_eq!(tool_revisions(&mut receiver, "tool"), vec![1]);

    // A new call releases it first, so nothing reads a stale state.
    recorder.queue_update(AgentUpdate::ToolCall {
        payload: json!({"toolCallId":"second","title":"Read","kind":"read","status":"in_progress","rawInput":{}}),
    }).await.unwrap();
    assert_eq!(tool_revisions(&mut receiver, "tool"), vec![2, 1]);
    owner.close(&recorder).await.unwrap();
}

#[test]
fn merged_progress_keeps_earlier_extensions() {
    let mut held = None;
    merge_progress(
        &mut held,
        json!({"toolCallId":"a","rawOutput":"one","_meta":{"parent":"p"}}),
    );
    merge_progress(
        &mut held,
        json!({"toolCallId":"a","rawOutput":"two","_meta":{"phase":"x"}}),
    );
    assert_eq!(
        held.unwrap(),
        json!({"toolCallId":"a","rawOutput":"two","_meta":{"parent":"p","phase":"x"}})
    );
}

/// Output updates carry delivered chunks: a held window keeps all of them.
#[test]
fn merged_progress_appends_each_output_chunk() {
    let mut held = None;
    for (kind, data) in [
        ("terminal_output", "first "),
        ("terminal_output", "second "),
        ("terminal_output_delta", "a"),
        ("terminal_output_delta", "b"),
    ] {
        merge_progress(
            &mut held,
            json!({"toolCallId":"a","_meta":{kind:{"terminal_id":"t","data":data}}}),
        );
    }
    let held = held.unwrap();
    assert_eq!(held["_meta"]["terminal_output"]["data"], "first second ");
    assert_eq!(held["_meta"]["terminal_output_delta"]["data"], "ab");
    // Another terminal's chunk starts its own output.
    let mut held = Some(held);
    merge_progress(
        &mut held,
        json!({"toolCallId":"a","_meta":{"terminal_output":{"terminal_id":"u","data":"new"}}}),
    );
    assert_eq!(held.unwrap()["_meta"]["terminal_output"]["data"], "new");
}

#[tokio::test]
async fn recorder_flush_reports_invalid_run_without_publishing_output() {
    let env = chat_env().await;
    let conversation = env.create_conversation("persist this").await;
    let missing_run_id = ConversationRunID(uuid::Uuid::now_v7().to_string());

    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let (recorder, owner) = ConversationRecorder::spawn(
        env.chat.clone(),
        conversation.conversation_id,
        missing_run_id,
        None,
        outbound_tx,
    );
    recorder
        .queue_update(AgentUpdate::AgentMessageChunk {
            text: "lost".into(),
            message_id: None,
        })
        .await
        .expect_err("live assembly rejects a nonexistent run before publishing");
    assert!(recorder.flush().await.is_err());
    assert!(owner.close(&recorder).await.is_err());
}

#[tokio::test]
async fn deferred_agent_requests_are_bounded() {
    let (outbound_tx, mut outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_limits(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        1,
        Arc::new(ModelCatalogService::load(None)),
    );

    bridge.spawn_response(
        ConnectionId::Primary,
        RequestID::Number(1),
        std::future::pending(),
    );
    bridge.spawn_response(ConnectionId::Primary, RequestID::Number(2), async {
        success_response(RequestID::Number(2), AgentEmptyResult {})
    });

    let response = outbound_rx.recv().await.expect("deferred response").frame;
    assert_eq!(response["id"], json!(2));
    assert_eq!(response["error"]["data"]["kind"], "internal");

    bridge.shutdown().await;
    assert!(
        bridge.regular_lane.try_acquire().is_ok(),
        "shutdown releases the pending regular response permit"
    );
}

/// The catalog's change signal reaches the wire as `engine.changed`, naming
/// the engine by the agent id `engine.list` keys its summaries on.
#[tokio::test]
async fn a_catalog_change_is_broadcast_as_engine_changed() {
    let (outbound_tx, mut outbound_rx) = crate::server::outbound::test_sink();
    let catalog = Arc::new(ModelCatalogService::load(None));
    let bridge = AgentRuntimeBridge::new_with_limits(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        1,
        Arc::clone(&catalog),
    );

    catalog.record(
        &AgentId::new("claude"),
        vec![crate::agent::AgentModelOption {
            model_id: "fable".to_string(),
            display_name: "Fable".to_string(),
        }],
        Some("fable".to_string()),
        Some(true),
        None,
        None,
    );

    let frame = outbound_rx.recv().await.expect("engine.changed").frame;
    assert_eq!(frame["method"], "engine.changed");
    assert_eq!(frame["params"]["engineID"], "claude");
    bridge.shutdown().await;
}

#[tokio::test]
async fn rejected_followup_launch_closes_the_pending_steer_input() {
    let env = chat_env().await;
    let conversation = env.create_conversation("pending follow-up").await;
    let (outbound_tx, _outbound_rx) = crate::server::outbound::test_sink();
    let bridge = AgentRuntimeBridge::new_with_limits(
        outbound_tx.clone(),
        ClientLink::new(outbound_tx),
        1,
        Arc::new(ModelCatalogService::load(None)),
    );
    let held_permit = bridge
        .regular_lane
        .try_acquire()
        .expect("holds the only regular slot");

    let error = bridge
        .reserve_followup_launch(
            &env.chat,
            &conversation.conversation_id,
            &conversation.input_id,
            &EngineID("codex".to_string()),
        )
        .await
        .expect_err("saturated follow-up is rejected");
    assert_eq!(error.kind, "agentRequestQueueSaturated");
    let detail = env
        .chat
        .conversation_detail(&conversation.conversation_id)
        .await
        .expect("loads rejected input");
    let input = detail
        .latest_run_input_page
        .run_inputs
        .iter()
        .find(|input| input.run_input_id == conversation.input_id)
        .expect("pending input remains in history");
    assert_eq!(input.status, RunInputStatus::Failed);

    drop(held_permit);
    bridge.shutdown().await;
}

#[test]
fn source_sequence_gaps_are_emitted_only_for_the_affected_session() {
    let mut sequences = HashMap::new();
    let mut forwarded = Vec::new();
    for (session_id, sequence, text) in [
        ("session-a", 1, "a1"),
        ("session-b", 1, "b1"),
        ("session-a", 4, "a4"),
    ] {
        let updates = forward_harness_update(
            &mut sequences,
            HarnessUpdate {
                session_id: crate::agent::HarnessSessionId(session_id.to_string()),
                sequence,
                update: HarnessUpdateKind::AgentMessageChunk(text.to_string(), None),
            },
        );
        forwarded.extend(updates.into_iter().map(|update| (session_id, update)));
    }

    assert_eq!(forwarded.len(), 4);
    let gaps = forwarded
        .iter()
        .filter(|(_, update)| matches!(update, AgentUpdate::StreamGap { .. }))
        .collect::<Vec<_>>();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].0, "session-a");
    assert!(matches!(gaps[0].1, AgentUpdate::StreamGap { dropped: 2 }));
    // The gap precedes the update that revealed it.
    assert!(matches!(
        forwarded[2],
        ("session-a", AgentUpdate::StreamGap { .. })
    ));
}

/// The permissions flag is global last-write-wins; only the primary's
/// initialize may touch it. An attached client initializing with empty
/// capabilities must not disable the app's permission prompts.
#[tokio::test]
async fn attached_initialize_leaves_permission_support_untouched() {
    let home = TempDir::new().expect("temporary home");
    let database = Database::open_test_home(home.path().to_path_buf()).expect("opens database");
    let mut server = crate::server::AgentServer::new(database, Vec::new());
    let (sink, mut outbound_rx) = crate::server::outbound::test_sink();
    let client_link = ClientLink::new(sink.clone());
    server.client_link = Some(client_link.clone());
    server.agent_bridge = Some(AgentRuntimeBridge::new(sink, client_link));

    let initialize = |methods: Vec<&str>| {
        ProtocolRequest::new(
            RequestID::Number(1),
            "initialize",
            json!({
                "clientInfo": {"name": "test", "version": "0.0.0"},
                "capabilities": {"experimentalMethods": methods}
            }),
        )
    };
    let broker = Arc::clone(
        &server
            .agent_bridge
            .as_ref()
            .expect("bridge installed")
            .permission_broker,
    );
    let ask = |broker: Arc<ServerPermissionBroker>| async move {
        broker.request_permission(permission_request()).await
    };

    // Before any initialize the broker refuses immediately, silently.
    assert_eq!(
        ask(Arc::clone(&broker)).await,
        HarnessPermissionOutcome::Cancelled
    );
    assert!(outbound_rx.try_recv().is_err());

    // The primary advertises permission support: prompts start flowing.
    server.handle_initialize(
        ConnectionId::Primary,
        initialize(vec![METHOD_AGENT_REQUEST_PERMISSION]),
    );
    let asked = tokio::spawn(ask(Arc::clone(&broker)));
    next_frame(&mut outbound_rx, METHOD_AGENT_REQUEST_PERMISSION).await;
    // Prompts wait without a timeout; a Stop releases this one. It is
    // pending once it was sent, so the Stop cannot land before it.
    broker.cancel_session(&AgentId("codex".to_string()), "session-1");
    let _ = asked.await;
    next_frame(&mut outbound_rx, NOTIFICATION_CANCEL_REQUEST).await;

    // An attached client with empty capabilities must not revoke that.
    server.handle_initialize(ConnectionId::Attached(1), initialize(Vec::new()));
    let asked = tokio::spawn(ask(broker));
    next_frame(&mut outbound_rx, METHOD_AGENT_REQUEST_PERMISSION).await;
    asked.abort();
}

/// The next frame calling `method`, past what the server sends of its own
/// meanwhile, such as `agent.installEvent` for adapters this machine has.
async fn next_frame(
    outbound_rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::server::outbound::OutboundFrame>,
    method: &str,
) -> serde_json::Value {
    loop {
        let frame = outbound_rx
            .recv()
            .await
            .unwrap_or_else(|| panic!("a {method} frame"))
            .frame;
        if frame["method"] == method {
            return frame;
        }
    }
}

fn resolution_label(resolution: Resolution) -> String {
    match resolution {
        Resolution::Run(context) => format!("run:{}", context.run_id.0),
        Resolution::NotConversation => "not conversation".to_string(),
        Resolution::Drop(reason) => reason.as_str().to_string(),
    }
}

/// Run contexts for driving `SessionAttribution` directly.
struct AttributionFixture {
    env: ChatEnv,
    conversation_id: ConversationID,
    attribution: SessionAttribution,
    recorders: Vec<ConversationRecorder>,
}

impl AttributionFixture {
    async fn new() -> Self {
        let env = chat_env().await;
        let conversation = env.create_conversation("attribution").await;
        Self {
            env,
            conversation_id: conversation.conversation_id,
            attribution: SessionAttribution::default(),
            recorders: Vec::new(),
        }
    }

    fn context(&mut self, run_id: &str) -> ConversationRunContext {
        let (outbound, _) = crate::server::outbound::test_sink();
        let (context, recorder) = ConversationRunContext::for_attribution_test(
            self.env.chat.clone(),
            self.env.workspace_id.clone(),
            self.conversation_id.clone(),
            run_id,
            outbound,
        );
        self.recorders.push(recorder);
        context
    }
}

fn session_key(session: &str) -> (String, String) {
    ("codex".to_string(), session.to_string())
}

#[tokio::test]
async fn unattributed_update_state_follows_the_holding_run() {
    let mut fixture = AttributionFixture::new().await;
    let first = fixture.context("run-a");
    let second = fixture.context("run-b");
    let attribution = fixture.attribution.clone();
    let key = session_key("session-1");

    attribution.apply(AttributionOp::Claim {
        key: key.clone(),
        context: first.clone(),
        generation: 1,
    });
    // Only the holder can release.
    attribution.apply(AttributionOp::Release {
        key: key.clone(),
        run_id: second.run_id.clone(),
        ended: RunEnd::Completed,
    });
    assert_eq!(resolution_label(attribution.resolve(&key, 1)), "run:run-a");
    attribution.apply(AttributionOp::Release {
        key: key.clone(),
        run_id: first.run_id.clone(),
        ended: RunEnd::Cancelled,
    });
    assert_eq!(
        resolution_label(attribution.resolve(&key, 1)),
        "after_cancel"
    );
    // Forget clears a run's own claim, never a tombstone.
    attribution.apply(AttributionOp::Forget {
        key: key.clone(),
        run_id: first.run_id.clone(),
    });
    assert!(attribution.has_tombstone(&key));
    // A later generation means the adapter restarted under the tombstone.
    assert_eq!(
        resolution_label(attribution.resolve(&key, 2)),
        "unattributed"
    );
    assert!(!attribution.has_tombstone(&key));

    // A claim in a new generation sweeps the agent's older tombstones.
    let stale = session_key("session-2");
    attribution.apply(AttributionOp::Claim {
        key: stale.clone(),
        context: first.clone(),
        generation: 1,
    });
    attribution.apply(AttributionOp::Release {
        key: stale.clone(),
        run_id: first.run_id.clone(),
        ended: RunEnd::Completed,
    });
    assert!(attribution.has_tombstone(&stale));
    attribution.apply(AttributionOp::Claim {
        key: session_key("session-3"),
        context: second,
        generation: 2,
    });
    assert!(!attribution.has_tombstone(&stale));
}

#[tokio::test]
async fn unattributed_update_after_a_detached_steer_is_reported_as_such() {
    let mut fixture = AttributionFixture::new().await;
    let first = fixture.context("run-a");
    let second = fixture.context("run-b");
    let attribution = fixture.attribution.clone();
    let key = session_key("session-1");

    attribution.apply(AttributionOp::Claim {
        key: key.clone(),
        context: first.clone(),
        generation: 1,
    });
    attribution.apply(AttributionOp::MarkDetached {
        key: key.clone(),
        run_id: first.run_id.clone(),
    });
    assert_eq!(
        resolution_label(attribution.resolve(&key, 1)),
        "detached_steer"
    );
    attribution.apply(AttributionOp::Release {
        key: key.clone(),
        run_id: first.run_id.clone(),
        ended: RunEnd::Completed,
    });
    assert_eq!(
        resolution_label(attribution.resolve(&key, 1)),
        "detached_steer"
    );

    // A late mark from an earlier run cannot detach the next holder.
    attribution.apply(AttributionOp::Claim {
        key: key.clone(),
        context: second,
        generation: 1,
    });
    attribution.apply(AttributionOp::MarkDetached {
        key: key.clone(),
        run_id: first.run_id,
    });
    assert_eq!(resolution_label(attribution.resolve(&key, 1)), "run:run-b");
}

#[tokio::test]
async fn unattributed_update_after_a_settled_turn_with_a_steer_in_flight_is_detached() {
    let mut fixture = AttributionFixture::new().await;
    let first = fixture.context("run-a");
    let second = fixture.context("run-b");
    let attribution = fixture.attribution.clone();
    let key = session_key("session-1");
    let steer = |step: &str, run: &ConversationRunContext| {
        let (key, run_id) = (key.clone(), run.run_id.clone());
        attribution.apply(match step {
            "pending" => AttributionOp::SteerPending { key, run_id },
            "settled" => AttributionOp::SteerSettled { key, run_id },
            "detached" => AttributionOp::MarkDetached { key, run_id },
            _ => unreachable!("unknown steer step {step}"),
        });
    };

    attribution.apply(AttributionOp::Claim {
        key: key.clone(),
        context: first.clone(),
        generation: 1,
    });
    // A steer into a running turn is injected; its output is the run's.
    steer("pending", &first);
    assert_eq!(resolution_label(attribution.resolve(&key, 1)), "run:run-a");
    // Once the turn's answer is in, a pending steer starts a detached turn.
    attribution.apply(AttributionOp::PromptSettled { key: key.clone() });
    assert_eq!(
        resolution_label(attribution.resolve(&key, 1)),
        "detached_steer"
    );
    // A settled turn's late metadata is still the run's.
    steer("settled", &first);
    assert_eq!(resolution_label(attribution.resolve(&key, 1)), "run:run-a");
    steer("pending", &first);
    steer("detached", &first);
    steer("settled", &first);
    assert_eq!(
        resolution_label(attribution.resolve(&key, 1)),
        "detached_steer"
    );
    attribution.apply(AttributionOp::Release {
        key: key.clone(),
        run_id: first.run_id.clone(),
        ended: RunEnd::Cancelled,
    });
    assert_eq!(
        resolution_label(attribution.resolve(&key, 1)),
        "detached_steer"
    );

    // An earlier run's steer cannot mark the next holder.
    attribution.apply(AttributionOp::Claim {
        key: key.clone(),
        context: second.clone(),
        generation: 1,
    });
    attribution.apply(AttributionOp::PromptSettled { key: key.clone() });
    steer("pending", &first);
    assert_eq!(resolution_label(attribution.resolve(&key, 1)), "run:run-b");
    // The run can let go while its steer is still out.
    steer("pending", &second);
    attribution.apply(AttributionOp::Release {
        key: key.clone(),
        run_id: second.run_id.clone(),
        ended: RunEnd::Completed,
    });
    assert_eq!(
        resolution_label(attribution.resolve(&key, 1)),
        "detached_steer"
    );
    steer("settled", &second);
    assert_eq!(resolution_label(attribution.resolve(&key, 1)), "after_end");
    steer("pending", &second);
    steer("detached", &second);
    steer("settled", &second);
    assert_eq!(
        resolution_label(attribution.resolve(&key, 1)),
        "detached_steer"
    );
}

fn spawn_label(spawn: super::attribution::ChildSpawn) -> String {
    use super::attribution::ChildSpawn;
    match spawn {
        ChildSpawn::Record {
            thread_id,
            anchor_call,
        } if anchor_call == thread_id => thread_id,
        ChildSpawn::Record {
            thread_id,
            anchor_call,
        } => format!("{thread_id}@{anchor_call}"),
        ChildSpawn::Drop(reason) => reason.as_str().to_string(),
    }
}

fn route_label(route: super::attribution::ChildRoute) -> String {
    use super::attribution::ChildRoute;
    match route {
        ChildRoute::Own => "own".to_string(),
        ChildRoute::Root { key, thread_id } => format!("{}>{thread_id}", key.1),
        ChildRoute::Drop(reason) => reason.as_str().to_string(),
    }
}

/// Rule 9: a native subagent's session routes to its root's run under its
/// own thread, nested ones too, and replay's children are history.
#[tokio::test]
async fn native_subagent_keys_route_to_their_root_run() {
    let mut fixture = AttributionFixture::new().await;
    let first = fixture.context("run-a");
    let second = fixture.context("run-b");
    let attribution = fixture.attribution.clone();
    let root = session_key("root");
    let child = session_key("child");
    let nested = session_key("nested");

    // Nothing holds the root: the child is nobody's.
    assert_eq!(
        spawn_label(attribution.spawn_child(&root, 1, "child", None)),
        "unattributed"
    );
    attribution.apply(AttributionOp::Claim {
        key: root.clone(),
        context: first.clone(),
        generation: 1,
    });
    assert_eq!(
        spawn_label(attribution.spawn_child(&root, 1, "child", Some("Reviewer".into()))),
        "subagent:child"
    );
    // A nested child resolves through its parent to the same root.
    assert_eq!(
        spawn_label(attribution.spawn_child(&child, 1, "nested", None)),
        "subagent:nested"
    );
    assert_eq!(
        route_label(attribution.route_child(&child)),
        "root>subagent:child"
    );
    assert_eq!(
        route_label(attribution.route_child(&nested)),
        "root>subagent:nested"
    );
    assert_eq!(route_label(attribution.route_child(&root)), "own");
    assert_eq!(
        attribution.child_label(&child),
        Some(("subagent:child".to_string(), Some("Reviewer".to_string())))
    );
    assert_eq!(
        attribution
            .running_context(&nested)
            .map(|context| context.run_id),
        Some(first.run_id.clone())
    );
    assert_eq!(
        attribution.end_child(&nested).map(|end| end.anchor_call),
        Some("subagent:nested".to_string())
    );

    // The parent messages a live subagent by its first run's id, from any
    // session of the tree; an ended one has no thread to message.
    assert_eq!(
        attribution.live_thread(&root, "child"),
        Some("subagent:child".to_string())
    );
    assert_eq!(
        attribution.live_thread(&child, "child"),
        Some("subagent:child".to_string())
    );
    assert_eq!(attribution.live_thread(&root, "nested"), None);
    assert_eq!(
        attribution.live_thread(&session_key("other"), "child"),
        None
    );
    // A resumed run joins its subagent's thread under an anchor of its own.
    assert_eq!(
        spawn_label(attribution.spawn_child(&root, 1, "nested:generation:2", None)),
        "subagent:nested@subagent:nested:generation:2"
    );
    assert_eq!(
        route_label(attribution.route_child(&session_key("nested:generation:2"))),
        "root>subagent:nested"
    );
    assert_eq!(
        attribution.live_thread(&root, "nested"),
        Some("subagent:nested".to_string())
    );
    assert_eq!(
        attribution
            .end_child(&session_key("nested:generation:2"))
            .map(|end| end.anchor_call),
        Some("subagent:nested:generation:2".to_string())
    );

    // Once Stop releases the run, a later run's root is not the child's.
    attribution.apply(AttributionOp::Release {
        key: root.clone(),
        run_id: first.run_id.clone(),
        ended: RunEnd::Cancelled,
    });
    attribution.apply(AttributionOp::Claim {
        key: root.clone(),
        context: second.clone(),
        generation: 1,
    });
    assert_eq!(route_label(attribution.route_child(&child)), "after_end");
    assert!(attribution.running_context(&child).is_none());
    // The adapter restarting ends every subagent it ran.
    attribution.take_lost_tasks(|agent, _, generation| agent == root.0 && generation < 2);
    assert_eq!(attribution.live_thread(&root, "child"), None);

    // Replay announces children that are history, and so is all they send.
    let replayed = session_key("replayed-root");
    attribution.apply(AttributionOp::MarkReplay {
        key: replayed.clone(),
        context: second,
        generation: 1,
    });
    assert_eq!(
        spawn_label(attribution.spawn_child(&replayed, 1, "old", None)),
        "replay"
    );
    let old = session_key("old");
    assert_eq!(route_label(attribution.route_child(&old)), "replay");
    assert!(attribution.end_child(&old).is_none());
    assert_eq!(attribution.child_label(&old), None);
    assert!(attribution.running_context(&old).is_none());
}

#[tokio::test]
async fn unattributed_update_during_replay_and_while_opening_is_not_lost_work() {
    let mut fixture = AttributionFixture::new().await;
    let first = fixture.context("run-a");
    let attribution = fixture.attribution.clone();
    let key = session_key("session-1");

    attribution.apply(AttributionOp::MarkReplay {
        key: key.clone(),
        context: first.clone(),
        generation: 1,
    });
    assert_eq!(resolution_label(attribution.resolve(&key, 1)), "replay");
    attribution.apply(AttributionOp::AbandonReplay {
        key: key.clone(),
        run_id: first.run_id.clone(),
    });
    // Stray replay after an abandoned load stays quiet.
    assert_eq!(resolution_label(attribution.resolve(&key, 1)), "replay");

    // A session being opened has no claim yet.
    let opened = session_key("opened-session");
    let opening = attribution.opening(&AgentId("codex".to_string()));
    let other = attribution.opening(&AgentId("claude".to_string()));
    assert_eq!(resolution_label(attribution.resolve(&opened, 1)), "not conversation");
    drop(opening);
    assert_eq!(
        resolution_label(attribution.resolve(&opened, 1)),
        "unattributed"
    );
    drop(other);
}

#[tokio::test]
async fn unattributed_update_tombstones_go_with_their_conversation() {
    let mut fixture = AttributionFixture::new().await;
    let first = fixture.context("run-a");
    let attribution = fixture.attribution.clone();
    let key = session_key("session-1");

    attribution.apply(AttributionOp::Claim {
        key: key.clone(),
        context: first.clone(),
        generation: 1,
    });
    attribution.apply(AttributionOp::Release {
        key: key.clone(),
        run_id: first.run_id.clone(),
        ended: RunEnd::Completed,
    });
    attribution.forget_conversation(&ConversationID("another-conversation".to_string()));
    assert_eq!(resolution_label(attribution.resolve(&key, 1)), "after_end");
    attribution.forget_conversation(&first.conversation_id);
    assert_eq!(
        resolution_label(attribution.resolve(&key, 1)),
        "unattributed"
    );
}

/// Stands in for the coordinator: resolves each update when the forwarder
/// hands it over, and holds the forwarder there until the gate opens.
struct GatedAttributionSink {
    attribution: SessionAttribution,
    gate: tokio::sync::Semaphore,
    entered: StdMutex<Option<tokio::sync::oneshot::Sender<()>>>,
    seen: StdMutex<Vec<(String, String)>>,
}

impl ConversationUpdateSink for GatedAttributionSink {
    fn forward_conversation_update<'a>(
        &'a self,
        agent_id: &'a AgentId,
        session_id: &'a HarnessSessionId,
        generation: u64,
        update: AgentUpdate,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if let Some(entered) = self
                .entered
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                let _ = entered.send(());
            }
            self.gate.acquire().await.expect("gate stays open").forget();
            let text = match update {
                AgentUpdate::AgentMessageChunk { text, .. }
                | AgentUpdate::RichMessageChunk { text, .. } => text,
                _ => "other".to_string(),
            };
            let resolution = resolution_label(
                self.attribution
                    .resolve(&(agent_id.0.clone(), session_id.0.clone()), generation),
            );
            self.seen
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((text, resolution));
        })
    }
}

#[tokio::test]
async fn unattributed_update_queued_before_a_claim_never_reaches_the_claiming_run() {
    let mut fixture = AttributionFixture::new().await;
    let first = fixture.context("run-a");
    let second = fixture.context("run-b");
    let attribution = fixture.attribution.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let sink = Arc::new(GatedAttributionSink {
        attribution: attribution.clone(),
        gate: tokio::sync::Semaphore::new(0),
        entered: StdMutex::new(Some(entered_tx)),
        seen: StdMutex::new(Vec::new()),
    });
    let erased: Arc<dyn ConversationUpdateSink> = sink.clone();
    let (outbound, _outbound_rx) = crate::server::outbound::test_sink();
    let forwarders = Arc::new(AgentUpdateForwarders::new(
        outbound,
        Arc::downgrade(&erased),
        attribution.clone(),
    ));
    let adapter = Arc::new(FakeConversationAdapter::new(
        fake_definition("codex"),
        1,
        true,
        SteeringCapability::InterruptThenPrompt,
    ));
    adapter.start().await.expect("adapter starts");
    forwarders.ensure(adapter.clone()).await;
    let agent = AgentId("codex".to_string());
    let key = session_key("session-1");
    let send = |sequence: u64, text: &str| {
        let _ = adapter.updates_tx.send(HarnessUpdate {
            session_id: HarnessSessionId("session-1".to_string()),
            sequence,
            update: HarnessUpdateKind::AgentMessageChunk(text.to_string(), None),
        });
    };

    // Run A held the session and ended; the agent is still finishing.
    forwarders
        .fence(
            &agent,
            Some(AttributionOp::Claim {
                key: key.clone(),
                context: first.clone(),
                generation: 1,
            }),
        )
        .await;
    forwarders
        .fence(
            &agent,
            Some(AttributionOp::Release {
                key: key.clone(),
                run_id: first.run_id.clone(),
                ended: RunEnd::Completed,
            }),
        )
        .await;
    send(1, "late-1");
    entered_rx.await.expect("forwarder reaches the sink");
    send(2, "late-2");
    // Run B claims while A's tail is still queued behind the gate.
    let claim = {
        let forwarders = Arc::clone(&forwarders);
        let agent = agent.clone();
        let key = key.clone();
        tokio::spawn(async move {
            forwarders
                .fence(
                    &agent,
                    Some(AttributionOp::Claim {
                        key,
                        context: second,
                        generation: 1,
                    }),
                )
                .await;
        })
    };
    tokio::task::yield_now().await;
    sink.gate.add_permits(16);
    claim.await.expect("claim completes");
    send(3, "after-claim");
    forwarders.flush(&agent).await;

    let seen = sink
        .seen
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let expected = [
        ("late-1", "after_end"),
        ("late-2", "after_end"),
        ("after-claim", "run:run-b"),
    ]
    .map(|(text, resolution)| (text.to_string(), resolution.to_string()));
    assert_eq!(seen, expected);
    forwarders.shutdown().await;
}

#[tokio::test]
async fn unattributed_update_after_a_lost_prompt_answer_is_still_detached() {
    let mut fixture = AttributionFixture::new().await;
    let run = fixture.context("run-a");
    let attribution = fixture.attribution.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let sink = Arc::new(GatedAttributionSink {
        attribution: attribution.clone(),
        gate: tokio::sync::Semaphore::new(0),
        entered: StdMutex::new(Some(entered_tx)),
        seen: StdMutex::new(Vec::new()),
    });
    let erased: Arc<dyn ConversationUpdateSink> = sink.clone();
    let (outbound, _outbound_rx) = crate::server::outbound::test_sink();
    let forwarders = Arc::new(AgentUpdateForwarders::new(
        outbound,
        Arc::downgrade(&erased),
        attribution.clone(),
    ));
    let adapter = Arc::new(FakeConversationAdapter::new(
        fake_definition("codex"),
        1,
        true,
        SteeringCapability::Native,
    ));
    adapter.start().await.expect("adapter starts");
    let erased_adapter: Arc<dyn HarnessAdapter> = adapter.clone();
    forwarders.ensure(erased_adapter).await;
    let agent = AgentId("codex".to_string());
    let key = session_key("session-1");
    let send = |sequence: u64, update: HarnessUpdateKind| {
        let _ = adapter.updates_tx.send(HarnessUpdate {
            session_id: HarnessSessionId("session-1".to_string()),
            sequence,
            update,
        });
    };
    let chunk = |text: &str| HarnessUpdateKind::AgentMessageChunk(text.to_string(), None);

    forwarders
        .fence(
            &agent,
            Some(AttributionOp::Claim {
                key: key.clone(),
                context: run.clone(),
                generation: 1,
            }),
        )
        .await;
    forwarders
        .fence(
            &agent,
            Some(AttributionOp::SteerPending {
                key: key.clone(),
                run_id: run.run_id.clone(),
            }),
        )
        .await;
    send(1, chunk("turn"));
    entered_rx.await.expect("forwarder reaches the sink");
    // The turn is answered at sequence 2, which never reaches the forwarder.
    adapter
        .prompt_answers
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert("session-1".to_string(), 2);
    for sequence in 3..=40 {
        send(sequence, chunk("detached"));
    }
    sink.gate.add_permits(64);
    forwarders.flush(&agent).await;

    let seen = sink
        .seen
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    assert_eq!(seen[0], ("turn".to_string(), "run:run-a".to_string()));
    // The gap for the missing answer, then the 38 detached chunks.
    assert_eq!(seen.len(), 40, "{seen:?}");
    assert!(seen[1..]
        .iter()
        .all(|(_, resolution)| resolution == "detached_steer"));
    forwarders.shutdown().await;
}

#[tokio::test]
async fn a_burst_past_the_old_broadcast_capacity_reaches_the_sink_without_a_gap() {
    let fixture = AttributionFixture::new().await;
    let attribution = fixture.attribution.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let sink = Arc::new(GatedAttributionSink {
        attribution: attribution.clone(),
        gate: tokio::sync::Semaphore::new(0),
        entered: StdMutex::new(Some(entered_tx)),
        seen: StdMutex::new(Vec::new()),
    });
    let erased: Arc<dyn ConversationUpdateSink> = sink.clone();
    let (outbound, _outbound_rx) = crate::server::outbound::test_sink();
    let forwarders = Arc::new(AgentUpdateForwarders::new(
        outbound,
        Arc::downgrade(&erased),
        attribution,
    ));
    let adapter = Arc::new(FakeConversationAdapter::new(
        fake_definition("codex"),
        1,
        true,
        SteeringCapability::InterruptThenPrompt,
    ));
    adapter.start().await.expect("adapter starts");
    forwarders.ensure(adapter.clone()).await;
    let agent = AgentId("codex".to_string());
    let send = |sequence: u64| {
        let _ = adapter.updates_tx.send(HarnessUpdate {
            session_id: HarnessSessionId("session-1".to_string()),
            sequence,
            update: HarnessUpdateKind::AgentMessageChunk(format!("chunk-{sequence}"), None),
        });
    };

    // The sink holds the first update while the agent replays a long history
    // back to back, as `session/load` does.
    const BURST: u64 = 642;
    send(1);
    entered_rx.await.expect("forwarder reaches the sink");
    for sequence in 2..=BURST {
        send(sequence);
    }
    sink.gate.add_permits(BURST as usize);
    forwarders.flush(&agent).await;

    let seen: Vec<String> = sink
        .seen
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .map(|(text, _)| text.clone())
        .collect();
    let expected: Vec<String> = (1..=BURST)
        .map(|sequence| format!("chunk-{sequence}"))
        .collect();
    assert_eq!(seen, expected, "no update is lost or replaced by a gap");
    forwarders.shutdown().await;
}

#[tokio::test]
async fn a_replaced_forwarder_handles_its_queued_updates_first() {
    let fixture = AttributionFixture::new().await;
    let attribution = fixture.attribution.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let sink = Arc::new(GatedAttributionSink {
        attribution: attribution.clone(),
        gate: tokio::sync::Semaphore::new(0),
        entered: StdMutex::new(Some(entered_tx)),
        seen: StdMutex::new(Vec::new()),
    });
    let erased: Arc<dyn ConversationUpdateSink> = sink.clone();
    let (outbound, _outbound_rx) = crate::server::outbound::test_sink();
    let forwarders = Arc::new(AgentUpdateForwarders::new(
        outbound,
        Arc::downgrade(&erased),
        attribution,
    ));
    let adapter = Arc::new(FakeConversationAdapter::new(
        fake_definition("codex"),
        1,
        true,
        SteeringCapability::InterruptThenPrompt,
    ));
    adapter.start().await.expect("adapter starts");
    forwarders.ensure(adapter.clone()).await;
    for sequence in 1..=300 {
        let _ = adapter.updates_tx.send(HarnessUpdate {
            session_id: HarnessSessionId("session-1".to_string()),
            sequence,
            update: HarnessUpdateKind::AgentMessageChunk(format!("chunk-{sequence}"), None),
        });
    }
    entered_rx.await.expect("forwarder reaches the sink");

    // The adapter restarts while the old process's frames are still queued.
    adapter
        .status_tx
        .send_modify(|status| status.generation += 1);
    let replacing = {
        let forwarders = Arc::clone(&forwarders);
        let adapter: Arc<dyn HarnessAdapter> = adapter.clone();
        tokio::spawn(async move { forwarders.ensure(adapter).await })
    };
    tokio::task::yield_now().await;
    sink.gate.add_permits(300);
    replacing.await.expect("the forwarder is replaced");

    let seen = sink
        .seen
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len();
    assert_eq!(seen, 300, "the replaced forwarder dropped queued updates");
    forwarders.shutdown().await;
}

#[test]
fn update_sender_gives_each_live_subscriber_every_update_and_forgets_dropped_ones() {
    let sender = HarnessUpdateSender::default();
    let mut kept = sender.subscribe();
    let dropped = sender.subscribe();
    let update = |sequence| HarnessUpdate {
        session_id: HarnessSessionId("session-1".to_string()),
        sequence,
        update: HarnessUpdateKind::PromptSettled,
    };
    assert_eq!(sender.send(update(1)), 2);
    drop(dropped);
    assert_eq!(sender.send(update(2)), 1);
    let mut also = sender.subscribe();
    assert_eq!(sender.send(update(3)), 2);

    let sequences = |receiver: &mut HarnessUpdateReceiver| {
        std::iter::from_fn(|| receiver.try_recv().ok())
            .map(|update| update.sequence)
            .collect::<Vec<_>>()
    };
    assert_eq!(sequences(&mut kept), [1, 2, 3]);
    assert_eq!(sequences(&mut also), [3]);
}

#[test]
fn a_stalled_update_subscriber_loses_the_newest_updates_without_blocking_others() {
    let sender = HarnessUpdateSender::with_capacity(4);
    let mut stalled = sender.subscribe();
    let mut reading = sender.subscribe();
    let update = |sequence| HarnessUpdate {
        session_id: HarnessSessionId("session-1".to_string()),
        sequence,
        update: HarnessUpdateKind::PromptSettled,
    };
    let mut read = Vec::new();
    for sequence in 1..=6 {
        assert_eq!(sender.send(update(sequence)), 2);
        read.extend(std::iter::from_fn(|| reading.try_recv().ok()).map(|u| u.sequence));
    }
    let drain = |receiver: &mut HarnessUpdateReceiver| {
        std::iter::from_fn(|| receiver.try_recv().ok())
            .map(|update| update.sequence)
            .collect::<Vec<_>>()
    };
    // The stall shows as a sequence gap once the subscriber catches up.
    assert_eq!(drain(&mut stalled), [1, 2, 3, 4]);
    sender.send(update(7));
    assert_eq!(drain(&mut stalled), [7]);
    read.extend(drain(&mut reading));
    assert_eq!(read, [1, 2, 3, 4, 5, 6, 7]);
}

#[tokio::test]
async fn unattributed_update_after_run_end_is_counted_not_recorded() {
    let harness = continuity_harness("late_update", FakeFactory::new(true)).await;
    let run = harness.run_to_terminal("codex").await;
    let session = run.agent_session_id.expect("the run bound a session");
    let adapter = harness.adapter();
    let _ = adapter.updates_tx.send(HarnessUpdate {
        session_id: HarnessSessionId(session.0.clone()),
        sequence: 3,
        update: HarnessUpdateKind::AgentMessageChunk("LATE OUTPUT".to_string(), None),
    });
    let _ = adapter.updates_tx.send(HarnessUpdate {
        session_id: HarnessSessionId("never-claimed".to_string()),
        sequence: 1,
        update: HarnessUpdateKind::AgentMessageChunk("STRAY OUTPUT".to_string(), None),
    });
    harness
        .bridge
        .flush_forwarder(&AgentId("codex".to_string()))
        .await;

    let attribution = harness.bridge.conversation.attribution();
    assert_eq!(attribution.dropped("codex", DropReason::AfterEnd), 1);
    assert_eq!(attribution.dropped("codex", DropReason::Unattributed), 1);
    assert!(attribution.has_tombstone(&session_key(&session.0)));
    let items = harness.env.transcript(&harness.conversation_id).await;
    assert!(items.iter().all(|item| {
        !item.body_preview.contains("LATE OUTPUT") && !item.body_preview.contains("STRAY OUTPUT")
    }));
    harness.bridge.shutdown().await;
}

#[tokio::test]
async fn unattributed_update_from_a_session_being_opened_is_not_counted() {
    let harness = continuity_harness("opening_session", FakeFactory::new(true)).await;
    let agent = AgentId("codex".to_string());
    let adapter = harness
        .bridge
        .registry()
        .get_or_start(&agent)
        .await
        .expect("adapter starts");
    harness
        .adapter()
        .announce_new_sessions
        .store(true, Ordering::Relaxed);
    harness.bridge.ensure_forwarder(Arc::clone(&adapter)).await;

    // A conversation's new session announces itself before the run claims it.
    harness.run_to_terminal("codex").await;
    harness.bridge.flush_forwarder(&agent).await;

    let attribution = harness.bridge.conversation.attribution();
    assert_eq!(attribution.dropped("codex", DropReason::Unattributed), 0);
    harness.bridge.shutdown().await;
}

/// Commands an agent announces while `session/new` is in flight reach no run,
/// so the run that claims the session records them and they persist on it.
#[tokio::test]
async fn commands_announced_while_a_session_opens_persist_on_it() {
    let harness = continuity_harness("opening_commands", FakeFactory::new(true)).await;
    let adapter = harness
        .bridge
        .registry()
        .get_or_start(&AgentId("codex".to_string()))
        .await
        .expect("adapter starts");
    harness
        .adapter()
        .announce_new_sessions
        .store(true, Ordering::Relaxed);
    harness.bridge.ensure_forwarder(Arc::clone(&adapter)).await;

    harness.run_to_terminal("codex").await;

    let commands = harness
        .env
        .chat
        .conversation_detail(&harness.conversation_id)
        .await
        .expect("detail")
        .latest_agent_session_page
        .sessions
        .first()
        .and_then(|session| session.commands.clone())
        .expect("the session keeps its commands");
    assert_eq!(
        commands,
        vec![
            ConversationSessionCommand {
                name: "review".into(),
                description: "Review changes".into(),
                input_hint: Some("what to review".into()),
            },
            ConversationSessionCommand {
                name: "compact".into(),
                description: "Summarize the conversation".into(),
                input_hint: None,
            },
        ]
    );

    // A later turn on the same session doesn't record the unchanged list again.
    harness.run_to_terminal("codex").await;
    let recorded = harness
        .env
        .events(&harness.conversation_id)
        .await
        .into_iter()
        .filter(|event| {
            event.event_kind == ottam_protocol::ConversationEventKind::SessionMetadata
                && event.payload.get("commands").is_some()
        })
        .count();
    assert_eq!(recorded, 1);
    harness.bridge.shutdown().await;
}

impl ContinuityHarness {
    /// Autonomous runs of the conversation, oldest first.
    async fn follow_ups(&self) -> Vec<ConversationRunSummary> {
        let mut runs: Vec<_> = self
            .env
            .chat
            .list_runs(&self.conversation_id, Some(50), None)
            .await
            .expect("lists runs")
            .runs
            .into_iter()
            .filter(|run| run.origin == RunOrigin::Autonomous)
            .collect();
        runs.sort_by(|a, b| a.started_at.cmp(&b.started_at));
        runs
    }

    /// Waits for the `nth` follow-up (from 1) to reach a terminal state.
    async fn wait_for_follow_up(&self, nth: usize) -> ConversationRunSummary {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(run) = self.follow_ups().await.into_iter().nth(nth - 1) {
                    if run.status != ConversationRunStatus::Running {
                        return run;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("follow-up {nth} never ended"))
    }

    /// A user run whose turn backgrounds `toolu_bg` as task `task_bg`, as
    /// Claude does: spawn, then the stamp, both in the turn.
    async fn run_with_background_task(&self) -> ConversationRunSummary {
        self.bridge
            .registry()
            .get_or_start(&AgentId("codex".to_string()))
            .await
            .expect("adapter starts");
        self.adapter().script_next_turn([
            background_tool_call("toolu_bg"),
            HarnessUpdateKind::AsyncTaskSpawned(async_task("task_bg", Some("toolu_bg"))),
            backgrounded_stamp("toolu_bg", json!({"version": 1})),
        ]);
        self.run_to_terminal("codex").await
    }
}

fn task_completed(task: &str, call: &str) -> HarnessUpdateKind {
    HarnessUpdateKind::AsyncTaskStateUpdate(
        ottam_protocol::AsyncTaskState::Completed,
        async_task(task, Some(call)),
    )
}

/// Claude's usage report for a cycle it ran on its own.
fn autonomous_result() -> HarnessUpdateKind {
    HarnessUpdateKind::Other {
        kind: "usage_update".to_string(),
        payload: json!({
            "used": 10,
            "size": 100,
            "_meta": {"_claude/origin": {"kind": "task-notification"}},
        }),
    }
}

/// Rules 5 and 6: once a turn's background task ends, what the agent sends
/// on its own opens a follow-up run that records it, and Claude's result
/// for that cycle closes the run after the settle window.
#[tokio::test]
async fn autonomous_follow_up_opens_on_output_after_a_task_ends() {
    let harness = continuity_harness("follow_up_opens", FakeFactory::new(true)).await;
    let first = harness.run_with_background_task().await;
    let session = first.agent_session_id.clone().expect("bound a session");
    harness
        .send_after_turn(
            &session,
            3,
            [
                task_completed("task_bg", "toolu_bg"),
                HarnessUpdateKind::AgentMessageChunk(
                    "The server is ".to_string(),
                    Some("m1".into()),
                ),
                HarnessUpdateKind::AgentMessageChunk("ready.".to_string(), Some("m1".into())),
                autonomous_result(),
            ],
        )
        .await;
    let follow_up = harness.wait_for_follow_up(1).await;
    assert_eq!(follow_up.status, ConversationRunStatus::Completed);
    assert_eq!(follow_up.agent_session_id, Some(session));
    let items = harness.env.transcript(&harness.conversation_id).await;
    let reply = items
        .iter()
        .find(|item| item.body_preview == "The server is ready.")
        .expect("the follow-up's reply");
    assert_eq!(reply.run_id.as_ref(), Some(&follow_up.run_id));
    // The task's end still revises the run that started it.
    assert_eq!(
        transcript_tool(&items, "toolu_bg")
            .1
            .background_task
            .as_ref()
            .map(|task| task.state),
        Some(ottam_protocol::AsyncTaskState::Completed)
    );
    assert_eq!(harness.run_count().await, 2);
    harness.bridge.shutdown().await;
}

/// Without a background task the session is not armed: output after the
/// turn still drops as `after_end`, and nothing opens.
#[tokio::test]
async fn autonomous_follow_up_needs_a_background_task() {
    let harness = continuity_harness("follow_up_unarmed", FakeFactory::new(true)).await;
    let run = harness.run_to_terminal("codex").await;
    let session = run.agent_session_id.clone().expect("bound a session");
    harness
        .send_after_turn(
            &session,
            1,
            [HarnessUpdateKind::AgentMessageChunk(
                "stray".to_string(),
                None,
            )],
        )
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(harness.run_count().await, 1);
    assert_eq!(
        harness
            .bridge
            .conversation
            .attribution()
            .dropped("codex", DropReason::AfterEnd),
        1
    );
    harness.bridge.shutdown().await;
}

/// With no result to settle it, a follow-up closes after the idle gap. What
/// the agent sends later still opens another, since a close re-arms the key
/// for the grace window.
#[tokio::test]
async fn autonomous_follow_up_closes_when_idle_and_reopens_for_more_output() {
    let harness = continuity_harness("follow_up_idle", FakeFactory::new(true)).await;
    let first = harness.run_with_background_task().await;
    let session = first.agent_session_id.clone().expect("bound a session");
    harness
        .send_after_turn(
            &session,
            3,
            [
                task_completed("task_bg", "toolu_bg"),
                HarnessUpdateKind::AgentMessageChunk("first".to_string(), None),
            ],
        )
        .await;
    let follow_up = harness.wait_for_follow_up(1).await;
    assert_eq!(follow_up.status, ConversationRunStatus::Completed);
    harness
        .send_after_turn(
            &session,
            5,
            [
                HarnessUpdateKind::AgentMessageChunk("second".to_string(), None),
                autonomous_result(),
            ],
        )
        .await;
    let second = harness.wait_for_follow_up(2).await;
    assert_ne!(second.run_id, follow_up.run_id);
    let items = harness.env.transcript(&harness.conversation_id).await;
    let owner = |body: &str| {
        items
            .iter()
            .find(|item| item.body_preview == body)
            .and_then(|item| item.run_id.clone())
    };
    assert_eq!(owner("first"), Some(follow_up.run_id));
    assert_eq!(owner("second"), Some(second.run_id));
    harness.bridge.shutdown().await;
}

/// An open tool call keeps a follow-up open past the idle gap; its result
/// lets it close.
#[tokio::test]
async fn autonomous_follow_up_idle_close_waits_for_open_tools() {
    let harness = continuity_harness("follow_up_tools", FakeFactory::new(true)).await;
    let first = harness.run_with_background_task().await;
    let session = first.agent_session_id.clone().expect("bound a session");
    harness
        .send_after_turn(
            &session,
            3,
            [
                task_completed("task_bg", "toolu_bg"),
                HarnessUpdateKind::ToolCall(json!({
                    "toolCallId": "toolu_read",
                    "title": "Read output",
                    "kind": "read",
                    "status": "in_progress",
                })),
            ],
        )
        .await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let open = harness.follow_ups().await;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].status, ConversationRunStatus::Running);
    harness
        .send_after_turn(
            &session,
            5,
            [HarnessUpdateKind::ToolCallUpdate(json!({
                "toolCallId": "toolu_read",
                "status": "completed",
            }))],
        )
        .await;
    let closed = harness.wait_for_follow_up(1).await;
    assert_eq!(closed.status, ConversationRunStatus::Completed);
    harness.bridge.shutdown().await;
}

/// Cancelling a follow-up asks the agent to stop its cycle and disarms the
/// session: what it sends next opens nothing, until the user's next run.
#[tokio::test]
async fn autonomous_follow_up_cancel_stops_the_agent_and_disarms() {
    let harness = continuity_harness("follow_up_cancel", FakeFactory::new(true)).await;
    let first = harness.run_with_background_task().await;
    let session = first.agent_session_id.clone().expect("bound a session");
    harness
        .send_after_turn(
            &session,
            3,
            [
                task_completed("task_bg", "toolu_bg"),
                HarnessUpdateKind::AgentMessageChunk("working".to_string(), None),
            ],
        )
        .await;
    let follow_up = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(run) = harness.follow_ups().await.into_iter().next() {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a follow-up opens");
    let canceled = harness
        .bridge
        .cancel_conversation_run(
            &harness.env.chat,
            &harness.conversation_id,
            &follow_up.run_id,
        )
        .await
        .expect("cancels")
        .run;
    assert_eq!(canceled.status, ConversationRunStatus::Canceled);
    assert_eq!(
        harness.adapter().recorded_out_of_turn_cancels(),
        vec![session.0.clone()]
    );
    harness
        .send_after_turn(
            &session,
            5,
            [HarnessUpdateKind::AgentMessageChunk(
                "more".to_string(),
                None,
            )],
        )
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(harness.run_count().await, 2);
    harness.bridge.shutdown().await;
}

/// Rule 8: a permission request on an armed session opens the follow-up and
/// belongs to it; on an unarmed one it stays unattributed.
#[tokio::test]
async fn autonomous_follow_up_takes_a_permission_request() {
    let harness = continuity_harness("follow_up_permission", FakeFactory::new(true)).await;
    let first = harness.run_with_background_task().await;
    let session = first.agent_session_id.clone().expect("bound a session");
    harness
        .send_after_turn(&session, 3, [task_completed("task_bg", "toolu_bg")])
        .await;
    let key = ("codex".to_string(), session.0.clone());
    let follow_ups: &dyn FollowUpAttribution = harness.bridge.conversation.as_ref();
    let context = follow_ups
        .attribute(key.clone())
        .await
        .expect("the request joins a follow-up");
    assert_eq!(context.origin, RunOrigin::Autonomous);
    assert_ne!(context.run_id, first.run_id);
    // Asked again while it is open, the same run takes it.
    let again = follow_ups.attribute(key).await.expect("still attributed");
    assert_eq!(again.run_id, context.run_id);

    let other = continuity_harness("follow_up_permission_unarmed", FakeFactory::new(true)).await;
    let run = other.run_to_terminal("codex").await;
    let session = run.agent_session_id.clone().expect("bound a session");
    let follow_ups: &dyn FollowUpAttribution = other.bridge.conversation.as_ref();
    assert!(follow_ups
        .attribute(("codex".to_string(), session.0.clone()))
        .await
        .is_none());
    assert_eq!(other.run_count().await, 1);
    harness.bridge.shutdown().await;
    other.bridge.shutdown().await;
}

/// The run status a client sees carries who started the run.
#[tokio::test]
async fn autonomous_follow_up_run_status_carries_its_origin() {
    let harness = continuity_harness("follow_up_status", FakeFactory::new(true)).await;
    let (_, wake_up) = harness.open_wake_up().await;
    // The recorder writes the event behind the run's open.
    let status = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let events = harness.env.events(&harness.conversation_id).await;
            if let Some(status) = events.into_iter().find(|event| {
                event.run_id == wake_up.run_id
                    && event.event_kind == ottam_protocol::ConversationEventKind::RunStatus
            }) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the follow-up's status event");
    assert_eq!(status.payload["origin"], "autonomous");
    harness.bridge.shutdown().await;
}

/// Rule 6's bounds: a follow-up held behind another run keeps a bounded
/// buffer, merging one message's chunks, and drops the rest as
/// `conversation_busy`.
#[tokio::test]
async fn autonomous_follow_up_buffer_overflow_counts_conversation_busy() {
    use super::attribution::Resolved;
    let mut fixture = AttributionFixture::new().await;
    let run = fixture.context("run-a");
    let attribution = fixture.attribution.clone();
    let key = session_key("session-1");
    attribution.apply(AttributionOp::Claim {
        key: key.clone(),
        context: run.clone(),
        generation: 1,
    });
    let spawn = AgentUpdate::AsyncTaskSpawned {
        async_task_id: "task".into(),
        provider_tool_call_id: Some("call".into()),
        name: None,
        task_type: None,
        description: None,
        output_file_path: None,
        show_in_transcript: None,
        can_stop: None,
    };
    let _ = attribution.route_lifecycle(&key, 1, spawn);
    attribution.apply(AttributionOp::Release {
        key: key.clone(),
        run_id: run.run_id.clone(),
        ended: RunEnd::Completed,
    });
    let chunk = |text: &str| AgentUpdate::AgentMessageChunk {
        text: text.into(),
        message_id: Some("m".into()),
    };
    assert!(matches!(
        attribution.resolve_update(&key, 1, chunk("a")),
        Resolved::Open(_)
    ));
    // Chunks of one message merge into the frame they continue.
    for _ in 0..20 {
        assert!(matches!(
            attribution.resolve_update(&key, 1, chunk("b")),
            Resolved::Held
        ));
    }
    let tool = |id: usize| AgentUpdate::ToolCall {
        payload: json!({"toolCallId": format!("call-{id}")}),
    };
    let mut held = 0;
    let mut busy = 0;
    for id in 0..10 {
        match attribution.resolve_update(&key, 1, tool(id)) {
            Resolved::Held => held += 1,
            Resolved::Drop(DropReason::ConversationBusy, _) => busy += 1,
            _ => panic!("unexpected resolution"),
        }
    }
    assert_eq!((held, busy), (7, 3));
}

#[tokio::test]
async fn autonomous_follow_up_buffer_stays_full_after_an_oversized_frame() {
    use super::attribution::Resolved;
    let mut fixture = AttributionFixture::new().await;
    let run = fixture.context("run-a");
    let attribution = fixture.attribution.clone();
    let key = session_key("session-1");
    attribution.apply(AttributionOp::Claim {
        key: key.clone(),
        context: run.clone(),
        generation: 1,
    });
    let spawn = AgentUpdate::AsyncTaskSpawned {
        async_task_id: "task".into(),
        provider_tool_call_id: Some("call".into()),
        name: None,
        task_type: None,
        description: None,
        output_file_path: None,
        show_in_transcript: None,
        can_stop: None,
    };
    let _ = attribution.route_lifecycle(&key, 1, spawn);
    attribution.apply(AttributionOp::Release {
        key: key.clone(),
        run_id: run.run_id.clone(),
        ended: RunEnd::Completed,
    });
    let chunk = |text: String| AgentUpdate::AgentMessageChunk {
        text,
        message_id: Some("m".into()),
    };
    assert!(matches!(
        attribution.resolve_update(&key, 1, chunk("a".into())),
        Resolved::Open(_)
    ));
    let oversized = AgentUpdate::ToolCall {
        payload: json!({"toolCallId": "big", "rawOutput": "x".repeat(5 * 1024 * 1024)}),
    };
    assert!(matches!(
        attribution.resolve_update(&key, 1, oversized),
        Resolved::Drop(DropReason::ConversationBusy, _)
    ));
    // A small frame after it would fit, but holding it would record output
    // with the dropped frame missing from its middle.
    assert!(matches!(
        attribution.resolve_update(&key, 1, chunk("b".into())),
        Resolved::Drop(DropReason::ConversationBusy, _)
    ));
}

/// An oversized first frame drops as `conversation_busy` instead of opening
/// a follow-up that holds nothing.
#[tokio::test]
async fn autonomous_follow_up_does_not_open_on_an_oversized_first_frame() {
    use super::attribution::Resolved;
    let mut fixture = AttributionFixture::new().await;
    let run = fixture.context("run-a");
    let attribution = fixture.attribution.clone();
    let key = session_key("session-1");
    attribution.apply(AttributionOp::Claim {
        key: key.clone(),
        context: run.clone(),
        generation: 1,
    });
    let spawn = AgentUpdate::AsyncTaskSpawned {
        async_task_id: "task".into(),
        provider_tool_call_id: Some("call".into()),
        name: None,
        task_type: None,
        description: None,
        output_file_path: None,
        show_in_transcript: None,
        can_stop: None,
    };
    let _ = attribution.route_lifecycle(&key, 1, spawn);
    attribution.apply(AttributionOp::Release {
        key: key.clone(),
        run_id: run.run_id.clone(),
        ended: RunEnd::Completed,
    });
    let oversized = AgentUpdate::ToolCall {
        payload: json!({"toolCallId": "big", "rawOutput": "x".repeat(5 * 1024 * 1024)}),
    };
    assert!(matches!(
        attribution.resolve_update(&key, 1, oversized),
        Resolved::Drop(DropReason::ConversationBusy, _)
    ));
}

/// A subagent still running after its run completed is the agent's live
/// work, which a restore or a profile change waits for, until the adapter
/// restarts without it.
#[tokio::test]
async fn native_subagents_are_live_work_until_their_adapter_restarts() {
    let mut fixture = AttributionFixture::new().await;
    let run = fixture.context("run-a");
    let attribution = fixture.attribution.clone();
    let root = session_key("root");
    attribution.apply(AttributionOp::Claim {
        key: root.clone(),
        context: run.clone(),
        generation: 1,
    });
    attribution.spawn_child(&root, 1, "child", None);
    attribution.apply(AttributionOp::Release {
        key: root.clone(),
        run_id: run.run_id,
        ended: RunEnd::Completed,
    });
    assert!(attribution.agent_has_live_work(&root.0));
    assert!(!attribution.agent_has_live_work("other-agent"));
    let lose_before = |before: u64| {
        attribution.take_lost_tasks(|agent, _, generation| agent == root.0 && generation < before)
    };
    assert!(lose_before(1).is_empty());
    assert!(attribution.agent_has_live_work(&root.0));
    // The subagent's thread is marked lost on the run holding its anchor.
    let lost = lose_before(2);
    assert_eq!(lost.len(), 1);
    assert_eq!(lost[0].tool_call_id.as_deref(), Some("subagent:child"));
    assert_eq!(lost[0].owner.run_id.0, "run-a");
    assert!(!attribution.agent_has_live_work(&root.0));
    assert!(lose_before(2).is_empty());
}

/// A nested subagent belongs to its parent subagent's run: once a user run
/// holds the root, the background subagent and what it launches both drop.
#[tokio::test]
async fn nested_native_subagents_keep_their_parents_run() {
    let mut fixture = AttributionFixture::new().await;
    let first = fixture.context("run-a");
    let second = fixture.context("run-b");
    let attribution = fixture.attribution.clone();
    let root = session_key("root");
    attribution.apply(AttributionOp::Claim {
        key: root.clone(),
        context: first.clone(),
        generation: 1,
    });
    attribution.spawn_child(&root, 1, "child", None);
    attribution.apply(AttributionOp::Release {
        key: root.clone(),
        run_id: first.run_id.clone(),
        ended: RunEnd::Completed,
    });
    attribution.apply(AttributionOp::Claim {
        key: root.clone(),
        context: second,
        generation: 1,
    });
    let child = session_key("child");
    let nested = session_key("nested");
    assert_eq!(
        spawn_label(attribution.spawn_child(&child, 1, "nested", None)),
        "subagent:nested"
    );
    assert!(attribution.running_context(&nested).is_none());
    assert_eq!(
        route_label(attribution.route_child(&nested)),
        route_label(attribution.route_child(&child))
    );
}

/// A subagent's end is task activity on its root, and revises the anchor in
/// its own run even once that run closed. A forgotten root's subagents are
/// no one's live work.
#[tokio::test]
async fn native_subagent_ends_arm_and_forgotten_roots_end_them() {
    use super::attribution::Resolved;
    let mut fixture = AttributionFixture::new().await;
    let run = fixture.context("run-a");
    let attribution = fixture.attribution.clone();
    let root = session_key("root");
    attribution.apply(AttributionOp::Claim {
        key: root.clone(),
        context: run.clone(),
        generation: 1,
    });
    attribution.spawn_child(&root, 1, "child", None);
    attribution.apply(AttributionOp::Release {
        key: root.clone(),
        run_id: run.run_id.clone(),
        ended: RunEnd::Completed,
    });
    let end = attribution
        .end_child(&session_key("child"))
        .expect("a live subagent's end has an anchor");
    assert_eq!(end.anchor_call, "subagent:child");
    assert_eq!(
        end.owner.map(|owner| owner.run_id.clone()),
        Some(run.run_id.clone()),
        "the closed run holding the anchor takes the end"
    );
    assert!(
        attribution.end_child(&session_key("child")).is_none(),
        "an end is reported once"
    );
    assert!(
        matches!(
            attribution.resolve_update(
                &root,
                1,
                AgentUpdate::AgentMessageChunk {
                    text: "The subagent reported back.".into(),
                    message_id: None,
                }
            ),
            Resolved::Open(_)
        ),
        "what follows the end opens a follow-up"
    );

    let other = session_key("other-root");
    let held = fixture.context("run-b");
    attribution.apply(AttributionOp::Claim {
        key: other.clone(),
        context: held.clone(),
        generation: 1,
    });
    attribution.spawn_child(&other, 1, "orphan", None);
    attribution.apply(AttributionOp::Forget {
        key: other,
        run_id: held.run_id,
    });
    assert!(!attribution.agent_has_live_work(&root.0));
}

/// A subagent's image becomes its stored call and its plan its own; both
/// carry its thread.
#[test]
fn native_subagent_images_and_plans_carry_their_thread() {
    let image = super::native_subagents::stamp(
        AgentUpdate::AgentImageChunk {
            content: serde_json::json!({"type": "image"}),
            message_id: None,
        },
        "subagent:child",
    );
    let AgentUpdate::ToolCall { payload } = image else {
        panic!("an image is stored as a call");
    };
    assert_eq!(payload["_ottamImage"], true);
    assert_eq!(
        payload[crate::server::chat::NATIVE_THREAD_STAMP],
        "subagent:child"
    );
    let plan = super::native_subagents::stamp(
        AgentUpdate::Plan {
            payload: serde_json::json!({"entries": []}),
        },
        "subagent:child",
    );
    let AgentUpdate::Plan { payload } = plan else {
        panic!("a plan stays a plan");
    };
    assert_eq!(
        payload[crate::server::chat::NATIVE_THREAD_STAMP],
        "subagent:child"
    );
}

/// The calls a definition's `SubagentMessageTools` names yield their message
/// once the input arrives, in the call or a later update, and only once.
#[test]
fn subagent_message_calls_yield_their_input_once() {
    use super::native_subagents::{MessageCall, MessageCalls};
    use crate::agent::{SubagentFollowUpTool, SubagentHandbackTool, SubagentMessageTools};
    let tools = || SubagentMessageTools {
        handback: Some(SubagentHandbackTool {
            tool: "SubagentHandback",
            message: "message",
        }),
        follow_up: Some(SubagentFollowUpTool {
            tool: "SendMessage",
            target: "to",
            message: "message",
        }),
    };
    let calls = MessageCalls::default();
    let call = |payload| AgentUpdate::ToolCall { payload };
    let update = |payload| AgentUpdate::ToolCallUpdate { payload };

    // Claude sends the input after the call.
    assert_eq!(
        calls.observe(
            "claude",
            &call(json!({"toolCallId": "send", "name": "SendMessage", "status": "pending"})),
            tools
        ),
        None
    );
    assert_eq!(
        calls.observe(
            "claude",
            &update(json!({"toolCallId": "send", "rawInput": {"to": "a1", "message": "Also run hostname."}})),
            || panic!("a watched call needs no definition")
        ),
        Some(MessageCall::FollowUp {
            target: "a1".into(),
            message: "Also run hostname.".into()
        })
    );
    assert_eq!(
        calls.observe(
            "claude",
            &update(json!({"toolCallId": "send", "status": "completed"})),
            tools
        ),
        None
    );

    // Input in the call itself.
    assert_eq!(
        calls.observe(
            "claude",
            &call(json!({"toolCallId": "hand", "name": "SubagentHandback", "rawInput": {"message": "Done: 11 words."}})),
            tools
        ),
        Some(MessageCall::Handback {
            call_id: "hand".into(),
            message: "Done: 11 words.".into()
        })
    );

    // Other calls, other agents' call ids, and agents without the setting.
    assert_eq!(
        calls.observe(
            "claude",
            &call(json!({"toolCallId": "bash", "name": "Bash", "rawInput": {"message": "x"}})),
            tools
        ),
        None
    );
    assert_eq!(
        calls.observe(
            "codex",
            &call(json!({"toolCallId": "send2", "name": "SendMessage"})),
            SubagentMessageTools::default
        ),
        None
    );
    assert_eq!(
        calls.observe(
            "codex",
            &update(json!({"toolCallId": "send2", "rawInput": {"to": "a1", "message": "hi"}})),
            tools
        ),
        None
    );

    // A watched call id is its agent's: another agent's update with the same
    // id is not its input, and the watch stays for the real one.
    calls.observe(
        "claude",
        &call(json!({"toolCallId": "shared", "name": "SendMessage"})),
        tools,
    );
    assert_eq!(
        calls.observe(
            "claude.work",
            &update(
                json!({"toolCallId": "shared", "rawInput": {"to": "a1", "message": "not mine"}})
            ),
            SubagentMessageTools::default
        ),
        None
    );
    assert_eq!(
        calls.observe(
            "claude",
            &update(json!({"toolCallId": "shared", "rawInput": {"to": "a1", "message": "mine"}})),
            tools
        ),
        Some(MessageCall::FollowUp {
            target: "a1".into(),
            message: "mine".into()
        })
    );

    // A call that ends with no input stops being watched.
    calls.observe(
        "claude",
        &call(json!({"toolCallId": "empty", "name": "SubagentHandback"})),
        tools,
    );
    calls.observe(
        "claude",
        &update(json!({"toolCallId": "empty", "status": "failed"})),
        tools,
    );
    assert_eq!(
        calls.observe(
            "claude",
            &update(json!({"toolCallId": "empty", "rawInput": {"message": "late"}})),
            SubagentMessageTools::default
        ),
        None
    );

    // Past the cap, the oldest watched call is forgotten.
    for id in 0..=256 {
        calls.observe(
            "claude",
            &call(json!({"toolCallId": format!("w{id}"), "name": "SubagentHandback"})),
            tools,
        );
    }
    let input = |id: &str| update(json!({"toolCallId": id, "rawInput": {"message": "m"}}));
    assert_eq!(
        calls.observe("claude", &input("w0"), SubagentMessageTools::default),
        None
    );
    assert!(calls
        .observe("claude", &input("w256"), SubagentMessageTools::default)
        .is_some());
}

/// A subagent's background task routes by its root's key but stops on the
/// subagent's own session, where the adapter knows the task.
#[tokio::test]
async fn native_subagent_tasks_stop_on_their_own_session() {
    let mut fixture = AttributionFixture::new().await;
    let run = fixture.context("run-a");
    let attribution = fixture.attribution.clone();
    let root = session_key("root");
    attribution.apply(AttributionOp::Claim {
        key: root.clone(),
        context: run.clone(),
        generation: 1,
    });
    attribution.index_tool_call(
        &root,
        1,
        &run.run_id,
        &json!({"toolCallId": "bg", "ottamSessionID": "codex:child"}),
    );
    let _ = attribution.route_lifecycle(
        &root,
        1,
        AgentUpdate::AsyncTaskSpawned {
            async_task_id: "task".into(),
            provider_tool_call_id: Some("bg".into()),
            name: None,
            task_type: None,
            description: None,
            output_file_path: None,
            show_in_transcript: None,
            can_stop: None,
        },
    );
    assert_eq!(
        attribution.live_task_session(&run.workspace_id, &run.conversation_id, &run.run_id, "task"),
        Some(("codex".to_string(), "child".to_string(), 1))
    );
}

/// A placeholder is no task: Codex's `Delegated task for <name>` and the
/// `<task-notification>` Claude resumes a subagent with stay off the anchor.
#[test]
fn native_subagent_anchor_drops_placeholder_tasks() {
    let task = |task: &str| {
        let AgentUpdate::ToolCall { payload } = super::native_subagents::anchor(
            "subagent:a",
            "subagent:a",
            Some("A".into()),
            Some(task.into()),
            None,
        ) else {
            panic!("an anchor is a call");
        };
        payload[crate::server::chat::NATIVE_ANCHOR_KEY]["task"].clone()
    };
    assert_eq!(task("Delegated task for A"), Value::Null);
    assert_eq!(task("Delegated task"), Value::Null);
    assert_eq!(
        task("<task-notification>\n<task-id>1</task-id>"),
        Value::Null
    );
    assert_eq!(task("Read the README"), json!("Read the README"));
}

// Delegation (docs/architecture/backend/mcp.md § Tools And Roles).

/// A harness whose conversation has run once and can delegate to
/// `review-bot`, a named agent on the fake Codex.
async fn delegation_harness() -> ContinuityHarness {
    let harness = continuity_harness("delegation", FakeFactory::new(true)).await;
    let chat = harness.env.chat.clone();
    harness.bridge.install_delegation(DelegationHost {
        chats: Arc::new(move |_| chat.clone()),
        database: harness.env.database.clone(),
    });
    harness
        .env
        .database
        .run("test.namedAgent", |connection| {
            crate::storage::repositories::NamedAgentRepository::insert(
                connection,
                &crate::storage::repositories::NamedAgentRecord {
                    id: uuid::Uuid::now_v7().to_string(),
                    name: "review-bot".into(),
                    engine_id: "codex".into(),
                    model_id: None,
                    mode_id: None,
                    instructions: "Review carefully.".into(),
                    created_at: 0,
                    updated_at: 0,
                    avatar_seed: Some(7),
                },
            )
        })
        .await
        .expect("creates the named agent");
    harness.run_to_terminal("codex").await;
    harness
}

impl ContinuityHarness {
    fn caller(&self) -> crate::mcp::McpCaller {
        crate::mcp::McpCaller {
            workspace_id: self.env.workspace_id.clone(),
            conversation_id: self.conversation_id.clone(),
            agent_id: AgentId("codex".into()),
            generation: 1,
            session_id: Some(HarnessSessionId::new("parent-session")),
            shared: false,
            sessions: Vec::new(),
            servers: Vec::new(),
        }
    }

    async fn delegate(&self, prompt: &str) -> Result<String, String> {
        self.delegate_titled(None, prompt).await
    }

    async fn delegate_titled(&self, title: Option<&str>, prompt: &str) -> Result<String, String> {
        self.bridge
            .ottam_tools()
            .delegate(
                self.caller(),
                crate::mcp::DelegateRequest {
                    agent: Some("Review-Bot".into()),
                    title: title.map(str::to_owned),
                    prompt: prompt.into(),
                    ..Default::default()
                },
            )
            .await
    }

    async fn delegate_with(&self, request: crate::mcp::DelegateRequest) -> Result<String, String> {
        self.bridge
            .ottam_tools()
            .delegate(self.caller(), request)
            .await
    }

    async fn child_ids(&self) -> Vec<ConversationID> {
        self.env
            .chat
            .children(&self.conversation_id)
            .await
            .expect("lists children")
            .into_iter()
            .map(|child| child.conversation_id)
            .collect()
    }

    async fn runs_of(&self, conversation_id: &ConversationID) -> Vec<ConversationRunSummary> {
        self.env
            .chat
            .conversation_detail(conversation_id)
            .await
            .expect("reads the conversation")
            .latest_run_page
            .runs
    }

    /// Waits until `conversation_id` has `count` runs, all ended.
    async fn wait_for_ended_runs(&self, conversation_id: &ConversationID, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let runs = self.runs_of(conversation_id).await;
                if runs.len() == count
                    && runs.iter().all(|run| {
                        !matches!(
                            run.status,
                            ConversationRunStatus::Queued | ConversationRunStatus::Running
                        )
                    })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{} never had {count} ended runs", conversation_id.0));
    }

    /// Waits until no note waits in `conversation_id`. A delivery marks its
    /// notes after the run that carries them starts, so that run can end
    /// first.
    async fn wait_for_no_waiting_notes(&self, conversation_id: &ConversationID) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !self
                .env
                .chat
                .waiting_notes(conversation_id)
                .await
                .expect("reads")
                .is_empty()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("notes still wait in {}", conversation_id.0));
    }

    /// The parent's notes, oldest first.
    async fn notes(&self) -> Vec<ottam_protocol::DelegationNote> {
        self.env
            .transcript(&self.conversation_id)
            .await
            .into_iter()
            .rev()
            .filter_map(|item| item.message.and_then(|message| message.note))
            .collect()
    }

    async fn wait_for_notes(&self, count: usize) -> Vec<ottam_protocol::DelegationNote> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let notes = self.notes().await;
                if notes.len() >= count {
                    break notes;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("never had {count} notes"))
    }

    /// Starts a run of `conversation_id` the way the user does, with a new
    /// message.
    async fn user_run(
        &self,
        conversation_id: &ConversationID,
        body: &str,
    ) -> ConversationRunSummary {
        let input_id = self.env.post_message(conversation_id, body).await;
        let started = self
            .bridge
            .start_conversation_run(
                self.env
                    .start_params(conversation_id, "codex", None, &input_id),
                self.env.chat.clone(),
                self.root.clone(),
                None,
            )
            .await
            .expect("run starts");
        self.env
            .wait_for_terminal(conversation_id, &started.run.run_id)
            .await
    }
}

/// A parent's Always allow reaches its child even when the parent's agent
/// has a bypass mode it is not in, unless the child runs in its agent's
/// read-only mode, where it asks before it acts.
#[tokio::test]
async fn a_child_copies_its_parents_always_allow_outside_bypass_mode() {
    let harness = delegation_harness().await;
    harness
        .env
        .chat
        .set_always_allow_tool_actions(&harness.conversation_id, true)
        .await
        .expect("turns on Always allow");
    harness
        .delegate("Review app/Package.swift")
        .await
        .expect("delegates");
    harness
        .env
        .database
        .run("test.namedAgentMode", |connection| {
            use crate::storage::repositories::NamedAgentRepository;
            let named =
                NamedAgentRepository::by_name(connection, "review-bot")?.expect("the named agent");
            NamedAgentRepository::update(
                connection,
                &crate::storage::repositories::NamedAgentRecord {
                    mode_id: Some("auto".into()),
                    ..named
                },
            )
        })
        .await
        .expect("changes the named agent's mode");
    harness
        .delegate("Fix app/Package.swift")
        .await
        .expect("delegates");
    let mut allows = Vec::new();
    for child in harness.child_ids().await {
        allows.push(
            harness
                .env
                .chat
                .conversation_detail(&child)
                .await
                .expect("reads the child")
                .conversation
                .always_allow_tool_actions,
        );
    }
    assert_eq!(allows, [false, true]);
    harness.bridge.shutdown().await;
}

/// Without a named agent, an installed agent runs the task ad hoc: the
/// child is named by the agent's display name and its task, starts in the
/// agent's read-only mode, and carries no instructions.
#[tokio::test]
async fn an_installed_agent_takes_a_task_ad_hoc() {
    let harness = delegation_harness().await;
    harness
        .delegate_with(crate::mcp::DelegateRequest {
            engine: Some("CODEX".into()),
            title: Some("Review README".into()),
            prompt: "Review README.md".into(),
            ..Default::default()
        })
        .await
        .expect("delegates");
    let child = harness.child_ids().await.pop().expect("a child");
    let detail = harness
        .env
        .chat
        .conversation_detail(&child)
        .await
        .expect("reads the child");
    // The test registry calls every agent "Test Agent".
    assert_eq!(detail.conversation.title, "Test Agent: Review README");
    assert_eq!(
        detail.conversation.delegated_agent_name.as_deref(),
        Some("Test Agent")
    );
    let run = &detail.latest_run_page.runs[0];
    assert_eq!(run.engine_id.0, "codex");
    assert_eq!(run.mode_id.as_deref(), Some("read-only"));
    assert!(!detail.conversation.always_allow_tool_actions);
    // An ad-hoc child gets a tile of its own.
    assert!(detail.conversation.delegated_avatar_seed.is_some());
    harness.bridge.shutdown().await;
}

/// A named agent's children draw the agent's current tile seed, so a
/// shuffle reaches children started before it.
#[tokio::test]
async fn named_children_follow_their_agents_seed() {
    let harness = delegation_harness().await;
    harness.delegate("Review it").await.expect("delegates");
    harness
        .env
        .database
        .run("test.namedAgentSeed", |connection| {
            use crate::storage::repositories::NamedAgentRepository;
            let named =
                NamedAgentRepository::by_name(connection, "review-bot")?.expect("the named agent");
            NamedAgentRepository::update(
                connection,
                &crate::storage::repositories::NamedAgentRecord {
                    avatar_seed: Some(11),
                    ..named
                },
            )
        })
        .await
        .expect("shuffles the named agent");
    harness
        .delegate("Review it again")
        .await
        .expect("delegates");
    let mut seeds = Vec::new();
    for child in harness.child_ids().await {
        seeds.push(
            harness
                .env
                .chat
                .conversation_detail(&child)
                .await
                .expect("reads the child")
                .conversation
                .delegated_avatar_seed,
        );
    }
    assert_eq!(seeds, [Some(11), Some(11)]);
    let listed: Vec<_> = harness
        .env
        .chat
        .list_conversations()
        .await
        .expect("lists")
        .into_iter()
        .filter(|summary| summary.delegated_agent_name.is_some())
        .map(|summary| summary.delegated_avatar_seed)
        .collect();
    assert_eq!(listed, [Some(11), Some(11)]);
    // Neither child stored a seed of its own.
    let stored: Vec<_> = harness
        .env
        .database
        .run("test.childSeeds", |connection| {
            use crate::storage::schema::conversations;
            use diesel::prelude::*;
            Ok(conversations::table
                .filter(conversations::delegated_agent_name.is_not_null())
                .select(conversations::delegated_avatar_seed)
                .load::<Option<i64>>(connection)?)
        })
        .await
        .expect("reads the stored seeds");
    assert_eq!(stored, [None, None]);
    // Renamed, the agent keeps its children: they find it by id.
    harness
        .env
        .database
        .run("test.namedAgentRename", |connection| {
            use crate::storage::repositories::NamedAgentRepository;
            let named =
                NamedAgentRepository::by_name(connection, "review-bot")?.expect("the named agent");
            NamedAgentRepository::update(
                connection,
                &crate::storage::repositories::NamedAgentRecord {
                    name: "linter".into(),
                    avatar_seed: Some(12),
                    ..named
                },
            )
        })
        .await
        .expect("renames and shuffles the named agent");
    let listed: Vec<_> = harness
        .env
        .chat
        .list_conversations()
        .await
        .expect("lists")
        .into_iter()
        .filter(|summary| summary.delegated_agent_name.is_some())
        .map(|summary| summary.delegated_avatar_seed)
        .collect();
    assert_eq!(listed, [Some(12), Some(12)]);
    let child = harness.child_ids().await.pop().expect("a child");
    let detail = harness
        .env
        .chat
        .conversation_detail(&child)
        .await
        .expect("reads the child");
    assert_eq!(detail.conversation.delegated_avatar_seed, Some(12));
    harness.bridge.shutdown().await;
}

/// An ad-hoc child may not take its agent's bypass mode while its parent
/// asks before acting; with Always allow on, it may.
#[tokio::test]
async fn an_ad_hoc_bypass_mode_needs_a_parent_that_approves() {
    let harness = delegation_harness().await;
    let request = || crate::mcp::DelegateRequest {
        engine: Some("codex".into()),
        mode: Some("agent-full-access".into()),
        prompt: "Fix it".into(),
        ..Default::default()
    };
    let refused = harness.delegate_with(request()).await.expect_err("refused");
    assert!(
        refused.starts_with("This conversation asks before acting"),
        "{refused}"
    );
    assert!(harness.child_ids().await.is_empty());
    harness
        .env
        .chat
        .set_always_allow_tool_actions(&harness.conversation_id, true)
        .await
        .expect("turns on Always allow");
    harness.delegate_with(request()).await.expect("delegates");
    harness.bridge.shutdown().await;
}

/// A parent that asks hands an ad-hoc child only a mode that asks too:
/// Claude's Accept edits is refused, its Manual mode is not.
#[tokio::test]
async fn an_asking_parent_hands_its_child_only_a_mode_that_asks() {
    let harness = delegation_harness().await;
    let request = |mode: &str| crate::mcp::DelegateRequest {
        engine: Some("claude".into()),
        mode: Some(mode.into()),
        prompt: format!("Fix it in {mode}"),
        ..Default::default()
    };
    let refused = harness
        .delegate_with(request("acceptEdits"))
        .await
        .expect_err("refused");
    assert!(
        refused.starts_with("This conversation asks before acting")
            && refused.ends_with("pick one that asks: default, plan."),
        "{refused}"
    );
    assert!(harness.child_ids().await.is_empty());
    harness
        .delegate_with(request("default"))
        .await
        .expect("delegates");
    let child = harness.child_ids().await.pop().expect("a child");
    assert_eq!(
        harness.runs_of(&child).await[0].mode_id.as_deref(),
        Some("default")
    );
    harness.bridge.shutdown().await;
}

/// Oh-My-Pi's plan mode makes every turn end with a question or a plan,
/// so its ad-hoc child starts in `default` when the parent approves, and
/// falls back to `plan` when the parent asks before acting.
#[tokio::test]
async fn an_omp_child_skips_plan_mode_unless_its_parent_asks() {
    let harness = delegation_harness().await;
    let request = |prompt: &str| crate::mcp::DelegateRequest {
        engine: Some("omp".into()),
        prompt: prompt.into(),
        ..Default::default()
    };
    harness
        .delegate_with(request("List the specs"))
        .await
        .expect("delegates");
    let asking = harness.child_ids().await.pop().expect("a child");
    assert_eq!(
        harness.runs_of(&asking).await[0].mode_id.as_deref(),
        Some("plan")
    );
    harness
        .env
        .chat
        .set_always_allow_tool_actions(&harness.conversation_id, true)
        .await
        .expect("turns on Always allow");
    harness
        .delegate_with(request("Count the specs"))
        .await
        .expect("delegates");
    let approving = harness
        .child_ids()
        .await
        .into_iter()
        .find(|child| *child != asking)
        .expect("a second child");
    assert_eq!(
        harness.runs_of(&approving).await[0].mode_id.as_deref(),
        Some("default")
    );

    // A named agent on Oh-My-Pi with no mode of its own starts the same way.
    harness
        .env
        .database
        .run("test.namedAgentEngine", |connection| {
            use crate::storage::repositories::NamedAgentRepository;
            let named =
                NamedAgentRepository::by_name(connection, "review-bot")?.expect("the named agent");
            NamedAgentRepository::update(
                connection,
                &crate::storage::repositories::NamedAgentRecord {
                    engine_id: "omp".into(),
                    ..named
                },
            )
        })
        .await
        .expect("moves the named agent to Oh-My-Pi");
    harness
        .delegate("Check the specs")
        .await
        .expect("delegates");
    let named = harness
        .child_ids()
        .await
        .into_iter()
        .find(|child| *child != asking && *child != approving)
        .expect("a third child");
    assert_eq!(
        harness.runs_of(&named).await[0].mode_id.as_deref(),
        Some("default")
    );
    harness.bridge.shutdown().await;
}

/// With neither a named agent nor an engine, the caller's own agent runs
/// the task on the caller's model.
#[tokio::test]
async fn a_task_without_an_agent_runs_on_the_callers_own() {
    let harness = delegation_harness().await;
    harness
        .run_to_terminal_with_model("codex", Some("gpt-test"))
        .await;
    harness
        .delegate_with(crate::mcp::DelegateRequest {
            prompt: "Summarize the changes".into(),
            ..Default::default()
        })
        .await
        .expect("delegates");
    let child = harness.child_ids().await.pop().expect("a child");
    let detail = harness
        .env
        .chat
        .conversation_detail(&child)
        .await
        .expect("reads the child");
    let run = &detail.latest_run_page.runs[0];
    assert_eq!(run.engine_id.0, "codex");
    assert_eq!(run.model_id.as_deref(), Some("gpt-test"));
    harness.bridge.shutdown().await;
}

/// An engine that isn't installed starts no child, and the error names
/// the installed agents.
#[tokio::test]
async fn an_unknown_engine_names_the_installed_agents() {
    let harness = delegation_harness().await;
    let refused = harness
        .delegate_with(crate::mcp::DelegateRequest {
            engine: Some("nonesuch".into()),
            prompt: "Review it".into(),
            ..Default::default()
        })
        .await
        .expect_err("refused");
    assert!(
        refused.starts_with("No installed agent is called nonesuch. The installed agents are:")
            && refused.contains("Test Agent (codex)"),
        "{refused}"
    );
    assert!(harness.child_ids().await.is_empty());
    // Both test agents share a display name, so it names neither.
    let ambiguous = harness
        .delegate_with(crate::mcp::DelegateRequest {
            engine: Some("test agent".into()),
            prompt: "Review it".into(),
            ..Default::default()
        })
        .await
        .expect_err("refused");
    assert!(
        ambiguous.starts_with("More than one installed agent is called test agent"),
        "{ambiguous}"
    );
    harness.bridge.shutdown().await;
}

/// A run that fails to start carries no notes: they wait for the next one.
#[tokio::test]
async fn notes_wait_when_the_run_carrying_them_fails_to_start() {
    let harness = delegation_harness().await;
    harness.delegate("hold").await.expect("delegates");
    harness
        .bridge
        .conversation
        .stop_children(&harness.env.chat, &harness.conversation_id)
        .await;
    assert!(harness.wait_for_notes(1).await[0].waits_for_user);
    let missing = ottam_protocol::ConversationRunInputID(uuid::Uuid::now_v7().to_string());
    harness
        .bridge
        .start_conversation_run(
            harness
                .env
                .start_params(&harness.conversation_id, "codex", None, &missing),
            harness.env.chat.clone(),
            harness.root.clone(),
            None,
        )
        .await
        .expect_err("no such input");
    let waiting = |harness: &ContinuityHarness| {
        let (chat, parent) = (harness.env.chat.clone(), harness.conversation_id.clone());
        async move {
            chat.waiting_notes(&parent)
                .await
                .expect("reads notes")
                .len()
        }
    };
    assert_eq!(waiting(&harness).await, 1);
    harness.user_run(&harness.conversation_id, "Go on").await;
    assert_eq!(waiting(&harness).await, 0);
    harness.bridge.shutdown().await;
}

/// A delegated child starts in its agent's read-only mode with the named
/// agent's instructions, and when it ends the parent gets a note and
/// continues with the child's report.
#[tokio::test]
async fn a_delegated_child_reports_back_and_its_parent_continues() {
    let harness = delegation_harness().await;
    let reply = harness
        .delegate("Review app/Package.swift")
        .await
        .expect("delegates");
    assert!(reply.starts_with("Delegated to review-bot"), "{reply}");
    let children = harness
        .env
        .chat
        .children(&harness.conversation_id)
        .await
        .expect("lists children");
    assert_eq!(children.len(), 1);
    let child = children[0].conversation_id.clone();
    harness
        .wait_for_ended_runs(&harness.conversation_id, 2)
        .await;

    let summary = harness
        .env
        .chat
        .conversation_detail(&child)
        .await
        .expect("reads the child")
        .conversation;
    assert_eq!(
        summary.parent_conversation_id.as_ref(),
        Some(&harness.conversation_id)
    );
    assert_eq!(summary.delegated_agent_name.as_deref(), Some("review-bot"));
    assert_eq!(
        summary.delegation_key.as_deref(),
        Some(super::conversation_runner::delegation_key("Review app/Package.swift").as_str())
    );
    assert_eq!(
        harness.runs_of(&child).await[0].mode_id.as_deref(),
        Some("read-only")
    );
    let first = harness
        .env
        .transcript(&child)
        .await
        .into_iter()
        .find_map(|item| item.message)
        .expect("the child's first message");
    assert_eq!(first.source, ottam_protocol::MessageSource::Delegation);
    assert_eq!(first.body, "Review carefully.\n\nReview app/Package.swift");
    assert_eq!(
        first.task,
        Some(ottam_protocol::DelegationTask {
            title: "Review app/Package.swift".into(),
            prompt: "Review app/Package.swift".into(),
            instructions: "Review carefully.".into(),
            started_by_engine_id: Some(EngineID("codex".into())),
        })
    );
    assert_eq!(first.note, None);
    assert_eq!(summary.title, "review-bot: Review app/Package.swift");
    let listed = harness
        .env
        .chat
        .list_conversations()
        .await
        .expect("lists conversations");
    let activity = listed
        .iter()
        .find(|summary| summary.conversation_id == child)
        .and_then(|summary| summary.delegation_activity.clone())
        .expect("the child's activity");
    assert_eq!(
        activity.latest_run_status,
        Some(ottam_protocol::ConversationRunStatus::Completed)
    );
    assert!(listed
        .iter()
        .find(|summary| summary.conversation_id == harness.conversation_id)
        .is_some_and(|summary| summary.delegation_activity.is_none()));

    let notes = harness.notes().await;
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].child_conversation_id, child);
    assert_eq!(notes[0].child_name, "review-bot");
    assert_eq!(
        notes[0].state,
        ottam_protocol::DelegationNoteState::Completed
    );
    assert_eq!(notes[0].excerpt, "Hello from fake");
    assert!(!notes[0].waits_for_user);
    assert!(harness
        .env
        .transcript(&harness.conversation_id)
        .await
        .into_iter()
        .filter_map(|item| item.message)
        .any(|message| message.source == ottam_protocol::MessageSource::NoteDelivery));
    let prompt = harness
        .adapter()
        .recorded_prompts()
        .last()
        .cloned()
        .expect("the follow-up's prompt");
    assert!(
        prompt.contains("review-bot finished") && prompt.contains("Hello from fake"),
        "{prompt}"
    );
    harness.bridge.shutdown().await;
}

/// A `delegate` call's title names the child, and the agent's own session
/// title doesn't replace it.
#[tokio::test]
async fn a_childs_title_is_the_tasks() {
    let harness = delegation_harness().await;
    harness
        .delegate_titled(
            Some("  Package review "),
            "retitle app/Package.swift\nCarefully.",
        )
        .await
        .expect("delegates");
    let child = harness.child_ids().await.remove(0);
    harness
        .wait_for_ended_runs(&harness.conversation_id, 2)
        .await;
    let detail = harness
        .env
        .chat
        .conversation_detail(&child)
        .await
        .expect("reads the child");
    // The agent's own title update leaves the task's title in place.
    assert_eq!(detail.conversation.title, "review-bot: Package review");
    assert_eq!(
        detail.conversation.delegated_task_title.as_deref(),
        Some("Package review")
    );
    harness.bridge.shutdown().await;
}

/// A child keeps the parent's turn that delegated it, so a client can show
/// its card there when the agent's transcript does not name the call.
#[tokio::test]
async fn a_child_keeps_the_turn_that_delegated_it() {
    let harness = delegation_harness().await;
    // The fixture's first turn has ended: no turn delegated this child.
    harness.delegate("review alone").await.expect("delegates");
    let idle = harness.child_ids().await.remove(0);
    let input_id = harness
        .env
        .post_message(&harness.conversation_id, "hold")
        .await;
    let started = harness
        .bridge
        .start_conversation_run(
            harness
                .env
                .start_params(&harness.conversation_id, "codex", None, &input_id),
            harness.env.chat.clone(),
            harness.root.clone(),
            None,
        )
        .await
        .expect("run starts");
    harness
        .delegate("review mid-turn")
        .await
        .expect("delegates");
    let summaries = harness
        .env
        .chat
        .list_conversations()
        .await
        .expect("lists conversations");
    let from_run = |id: &ConversationID| {
        summaries
            .iter()
            .find(|summary| &summary.conversation_id == id)
            .expect("lists the child")
            .delegated_from_run_id
            .clone()
    };
    let working = harness
        .child_ids()
        .await
        .into_iter()
        .find(|id| id != &idle)
        .expect("starts the second child");
    assert_eq!(from_run(&idle), None);
    assert_eq!(from_run(&working), Some(started.run.run_id));
    harness.bridge.shutdown().await;
}

/// A child's card counts its latest run's tool calls.
#[tokio::test]
async fn a_childs_activity_counts_its_tool_calls() {
    let harness = delegation_harness().await;
    harness.delegate("boundary").await.expect("delegates");
    let child = harness.child_ids().await.remove(0);
    harness
        .wait_for_ended_runs(&harness.conversation_id, 2)
        .await;
    let activity = harness
        .env
        .chat
        .delegation_activity(&child)
        .await
        .expect("reads the activity");
    assert_eq!(
        activity,
        ottam_protocol::DelegationActivity {
            latest_run_status: Some(ottam_protocol::ConversationRunStatus::Completed),
            other: 1,
            ..Default::default()
        }
    );
    harness.bridge.shutdown().await;
}

/// A stopped child's note says who stopped it. Each case gets its own
/// harness: the fake agent ends every held prompt on any cancel.
#[tokio::test]
async fn a_stopped_childs_note_says_why() {
    for by_parent in [false, true] {
        let harness = delegation_harness().await;
        harness.delegate("hold").await.expect("delegates");
        let child = harness.child_ids().await.remove(0);
        if by_parent {
            harness
                .bridge
                .conversation
                .stop_children(&harness.env.chat, &harness.conversation_id)
                .await;
        } else {
            let run = harness.runs_of(&child).await[0].run_id.clone();
            harness
                .bridge
                .cancel_conversation_run(&harness.env.chat, &child, &run)
                .await
                .expect("stops the child");
        }
        let note = harness.wait_for_notes(1).await.remove(0);
        assert_eq!(note.child_conversation_id, child);
        assert_eq!(
            note.stop_cause,
            Some(if by_parent {
                ottam_protocol::DelegationStopCause::ParentStopped
            } else {
                ottam_protocol::DelegationStopCause::User
            })
        );
        harness.bridge.shutdown().await;
    }
}

/// Quitting stops a working child; its note waits for the user and starts
/// no follow-up while core shuts down.
#[tokio::test]
async fn a_child_stopped_by_quitting_waits_for_the_user() {
    let harness = delegation_harness().await;
    harness.delegate("hold").await.expect("delegates");
    let child = harness.child_ids().await.remove(0);
    harness.bridge.shutdown().await;
    let note = harness.wait_for_notes(1).await.remove(0);
    assert_eq!(note.child_conversation_id, child);
    assert_eq!(
        note.stop_cause,
        Some(ottam_protocol::DelegationStopCause::Quit)
    );
    assert!(note.waits_for_user);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(harness.runs_of(&harness.conversation_id).await.len(), 1);
}

/// The app prepares shutdown first, which stops adapters, so a child's run
/// can end before core tears down. Its note still counts as quitting.
#[tokio::test]
async fn a_child_ended_by_shutdown_preparation_waits_for_the_user() {
    let harness = delegation_harness().await;
    let prompts = harness.adapter().recorded_prompts().len();
    harness.delegate("hold").await.expect("delegates");
    let child = harness.child_ids().await.remove(0);
    // A child still starting when shutdown is prepared fails to start
    // instead of stopping; this one is working.
    tokio::time::timeout(Duration::from_secs(2), async {
        while !harness.adapter().recorded_prompts()[prompts..]
            .iter()
            .any(|prompt| prompt.contains("hold"))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the child holds its prompt");
    harness
        .bridge
        .registry()
        .prepare_shutdown("quit")
        .await
        .expect("prepares shutdown");
    let run = harness.runs_of(&child).await[0].run_id.clone();
    harness
        .bridge
        .cancel_conversation_run(&harness.env.chat, &child, &run)
        .await
        .expect("ends the child");
    let note = harness.wait_for_notes(1).await.remove(0);
    assert_eq!(
        note.stop_cause,
        Some(ottam_protocol::DelegationStopCause::Quit)
    );
    assert!(note.waits_for_user);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(harness.runs_of(&harness.conversation_id).await.len(), 1);
    harness.bridge.shutdown().await;
}

/// Four children may work at once; the fifth call names them, and so does
/// a message that would wake an idle fifth.
#[tokio::test]
async fn a_fifth_working_child_is_refused_naming_the_four() {
    let harness = delegation_harness().await;
    harness.delegate("boundary").await.expect("delegates");
    let idle = harness.child_ids().await.remove(0);
    harness.wait_for_ended_runs(&idle, 1).await;
    for index in 0..4 {
        harness
            .delegate(&format!("hold {index}"))
            .await
            .expect("delegates");
    }
    let refused = harness.delegate("hold 4").await.expect_err("capped");
    assert!(
        refused.starts_with("4 delegated agents are already working")
            && refused.matches("review-bot (").count() == 4,
        "{refused}"
    );
    let refused = harness
        .bridge
        .ottam_tools()
        .message_child(harness.caller(), idle.0.clone(), "One more thing".into())
        .await
        .expect_err("capped");
    assert!(
        refused.starts_with("4 delegated agents are already working")
            && refused.contains("which is idle"),
        "{refused}"
    );
    assert_eq!(
        harness.runs_of(&idle).await.len(),
        1,
        "the idle child stays idle"
    );
    harness.bridge.shutdown().await;
}

/// One child causes at most five follow-ups in a row; its next note waits.
/// A user message then carries the waiting note and starts the count again.
#[tokio::test]
async fn a_child_wakes_its_parent_five_times_until_the_user_writes() {
    let harness = delegation_harness().await;
    harness.delegate("Review it").await.expect("delegates");
    let child = harness
        .env
        .chat
        .children(&harness.conversation_id)
        .await
        .expect("lists children")[0]
        .conversation_id
        .clone();
    harness
        .wait_for_ended_runs(&harness.conversation_id, 2)
        .await;
    let tools = harness.bridge.ottam_tools();
    let ask = |message: &'static str| {
        let (tools, caller, child) = (tools.clone(), harness.caller(), child.0.clone());
        async move {
            tools
                .message_child(caller, child, message.into())
                .await
                .expect("sends");
        }
    };
    for wake_up in 2..=5 {
        ask("Again").await;
        harness
            .wait_for_ended_runs(&harness.conversation_id, wake_up + 1)
            .await;
    }

    ask("Once more").await;
    let notes = harness.wait_for_notes(6).await;
    assert!(notes[5].waits_for_user, "{notes:#?}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(harness.runs_of(&harness.conversation_id).await.len(), 6);

    // A turn the user drives wakes no one, so the cap does not hold it.
    harness.user_run(&child, "A side question").await;
    let notes = harness.wait_for_notes(7).await;
    assert!(
        notes[6].deferred && !notes[6].waits_for_user,
        "{notes:#?}"
    );
    assert!(harness.report_waiting(&child).await);

    harness.user_run(&harness.conversation_id, "Thanks").await;
    let prompt = harness
        .adapter()
        .recorded_prompts()
        .last()
        .cloned()
        .expect("the user's prompt");
    assert!(
        prompt.contains("since your last turn")
            && prompt.contains("review-bot finished")
            && prompt.ends_with("Thanks"),
        "{prompt}"
    );
    assert!(harness
        .env
        .chat
        .waiting_notes(&harness.conversation_id)
        .await
        .expect("reads notes")
        .is_empty());

    ask("After the user wrote").await;
    harness
        .wait_for_ended_runs(&harness.conversation_id, 8)
        .await;
    harness.bridge.shutdown().await;
}

// Quieter push-back (docs/architecture/backend/mcp.md § Deferred Reports).

impl ContinuityHarness {
    async fn report_waiting(&self, child: &ConversationID) -> bool {
        self.env
            .chat
            .list_conversations()
            .await
            .expect("lists conversations")
            .into_iter()
            .find(|summary| &summary.conversation_id == child)
            .is_some_and(|summary| summary.deferred_report_run_id.is_some())
    }
}

/// A turn the user drives in a child wakes no one: its report waits in the
/// parent, the child offers Send to Parent, and the parent's next run
/// carries it.
#[tokio::test]
async fn a_turn_the_user_drives_in_a_child_waits_for_the_parents_next_run() {
    let harness = delegation_harness().await;
    harness.delegate("Review it").await.expect("delegates");
    let child = harness.child_ids().await.remove(0);
    harness
        .wait_for_ended_runs(&harness.conversation_id, 2)
        .await;
    assert!(!harness.report_waiting(&child).await);

    let side = harness.user_run(&child, "A side question").await;
    let notes = harness.wait_for_notes(2).await;
    assert!(notes[1].deferred && !notes[1].waits_for_user, "{notes:#?}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(harness.runs_of(&harness.conversation_id).await.len(), 2);
    // The child offers Send to Parent on the turn the report is of.
    let summary = harness
        .env
        .chat
        .list_conversations()
        .await
        .expect("lists conversations")
        .into_iter()
        .find(|summary| summary.conversation_id == child)
        .expect("lists the child");
    assert_eq!(summary.deferred_report_run_id, Some(side.run_id.clone()));
    let detail = harness
        .env
        .chat
        .conversation_detail(&child)
        .await
        .expect("gets the child");
    assert_eq!(detail.conversation.deferred_report_run_id, Some(side.run_id));

    harness.user_run(&harness.conversation_id, "Thanks").await;
    let prompt = harness
        .adapter()
        .recorded_prompts()
        .last()
        .cloned()
        .expect("the user's prompt");
    assert!(
        prompt.contains("review-bot finished") && prompt.ends_with("Thanks"),
        "{prompt}"
    );
    assert!(!harness.report_waiting(&child).await);
    harness.bridge.shutdown().await;
}

/// A deferred report rides on the next wake another report causes, and
/// counts no wake-up of its own.
#[tokio::test]
async fn a_deferred_report_rides_on_the_next_wake() {
    let harness = delegation_harness().await;
    harness.delegate("Review it").await.expect("delegates");
    let child = harness.child_ids().await.remove(0);
    harness
        .wait_for_ended_runs(&harness.conversation_id, 2)
        .await;
    harness.user_run(&child, "A side question").await;
    harness.wait_for_notes(2).await;
    harness
        .bridge
        .ottam_tools()
        .message_child(harness.caller(), child.0.clone(), "One more look".into())
        .await
        .expect("sends");
    harness
        .wait_for_ended_runs(&harness.conversation_id, 3)
        .await;
    let notes = harness.notes_in(&harness.conversation_id).await;
    assert_eq!(notes.len(), 3, "{notes:#?}");
    assert!(notes.iter().all(|(_, delivered)| *delivered), "{notes:#?}");
    let prompt = harness
        .adapter()
        .recorded_prompts()
        .last()
        .cloned()
        .expect("the wake's prompt");
    assert_eq!(prompt.matches("review-bot finished").count(), 2, "{prompt}");
    assert!(!harness.report_waiting(&child).await);
    harness.bridge.shutdown().await;
}

/// A turn the user starts in a child that carries its parent's waiting
/// message answers the parent, so its report wakes it.
#[tokio::test]
async fn a_user_turn_carrying_a_parents_message_wakes_the_parent() {
    let harness = delegation_harness().await;
    harness.delegate("hold").await.expect("delegates");
    let child = harness.child_ids().await.remove(0);
    harness
        .bridge
        .ottam_tools()
        .message_child(harness.caller(), child.0.clone(), "Also check tests".into())
        .await
        .expect("queues");
    let run = harness.runs_of(&child).await[0].run_id.clone();
    harness
        .bridge
        .cancel_conversation_run(&harness.env.chat, &child, &run)
        .await
        .expect("stops the child");
    // The stop's note wakes the parent first.
    harness
        .wait_for_ended_runs(&harness.conversation_id, 2)
        .await;

    harness.user_run(&child, "Go on").await;
    let notes = harness.wait_for_notes(2).await;
    assert!(!notes[1].deferred, "{notes:#?}");
    harness
        .wait_for_ended_runs(&harness.conversation_id, 3)
        .await;
    harness.bridge.shutdown().await;
}

/// Send to Parent wakes the parent with the child's deferred report, once.
/// A working parent refuses, and the report stays deferred.
#[tokio::test]
async fn send_to_parent_wakes_the_parent_with_a_deferred_report() {
    let harness = delegation_harness().await;
    harness.delegate("Review it").await.expect("delegates");
    let child = harness.child_ids().await.remove(0);
    harness
        .wait_for_ended_runs(&harness.conversation_id, 2)
        .await;
    let input_id = harness
        .env
        .post_message(&harness.conversation_id, "hold")
        .await;
    let working = harness
        .bridge
        .start_conversation_run(
            harness
                .env
                .start_params(&harness.conversation_id, "codex", None, &input_id),
            harness.env.chat.clone(),
            harness.root.clone(),
            None,
        )
        .await
        .expect("the parent starts")
        .run;
    harness.user_run(&child, "A side question").await;
    harness.wait_for_notes(2).await;

    let busy = harness
        .bridge
        .conversation
        .send_to_parent(&harness.env.chat, &harness.env.workspace_id, &child)
        .await
        .expect_err("the parent is working");
    assert_eq!(busy.kind, "conversationBusy");
    assert!(harness.report_waiting(&child).await);
    harness
        .bridge
        .cancel_conversation_run(&harness.env.chat, &harness.conversation_id, &working.run_id)
        .await
        .expect("stops the parent");
    harness
        .wait_for_ended_runs(&harness.conversation_id, 3)
        .await;
    assert!(harness.report_waiting(&child).await);

    let sent = harness
        .bridge
        .conversation
        .send_to_parent(&harness.env.chat, &harness.env.workspace_id, &child)
        .await
        .expect("sends");
    assert!(sent.run.is_some());
    harness
        .wait_for_ended_runs(&harness.conversation_id, 4)
        .await;
    assert!(!harness.report_waiting(&child).await);
    let notes = harness.notes_in(&harness.conversation_id).await;
    assert!(notes.iter().all(|(note, delivered)| *delivered && !note.deferred));
    let again = harness
        .bridge
        .conversation
        .send_to_parent(&harness.env.chat, &harness.env.workspace_id, &child)
        .await
        .expect("nothing to send");
    assert!(again.run.is_none());
    let refused = harness
        .bridge
        .conversation
        .send_to_parent(
            &harness.env.chat,
            &harness.env.workspace_id,
            &harness.conversation_id,
        )
        .await
        .expect_err("a top-level conversation has no parent");
    assert_eq!(refused.kind, "notDelegated");
    harness.bridge.shutdown().await;
}

/// How a run started is kept per run: a run that ends as the next one
/// starts is judged by its own start, an autonomous run by the prompted run
/// before it, and a deleted conversation's runs are forgotten.
#[tokio::test]
async fn a_run_is_judged_by_how_it_started() {
    let harness = delegation_harness().await;
    let conversation = harness.conversation_id.clone();
    let template = harness.user_run(&conversation, "Hello").await;
    let coordinator = &harness.bridge.conversation;
    let id = |id: &str| ConversationRunID(id.to_owned());
    let run = |id: &str, origin| ConversationRunSummary {
        run_id: ConversationRunID(id.to_owned()),
        origin,
        ..template.clone()
    };
    coordinator.record_prompted_run(&conversation, &id("run-1"), false);
    coordinator.record_prompted_run(&conversation, &id("run-3"), true);
    assert!(!coordinator.user_drove(&conversation, &run("run-1", RunOrigin::User)));
    assert!(coordinator.user_drove(&conversation, &run("run-3", RunOrigin::User)));
    assert!(!coordinator.user_drove(&conversation, &run("run-2", RunOrigin::Autonomous)));
    assert!(coordinator.user_drove(&conversation, &run("run-4", RunOrigin::Autonomous)));
    coordinator.forget_prompted_runs(&conversation);
    assert!(!coordinator.user_drove(&conversation, &run("run-3", RunOrigin::User)));
    harness.bridge.shutdown().await;
}

/// A woken parent is told how to acknowledge, and an acknowledgement
/// stays out of its history.
#[tokio::test]
async fn a_parent_acknowledges_notes_without_a_reply() {
    let harness = delegation_harness().await;
    harness.delegate("hold").await.expect("delegates");
    let child = harness.child_ids().await.remove(0);
    harness
        .bridge
        .ottam_tools()
        .message_parent(harness.child_caller(&child), "nothing to add".into())
        .await
        .expect("sends");
    harness
        .wait_for_ended_runs(&harness.conversation_id, 2)
        .await;
    let prompt = harness
        .prompt_containing("nothing to add")
        .expect("the wake's prompt");
    assert!(
        prompt.contains(ottam_protocol::DELEGATION_ACK_DIRECTIVE),
        "{prompt}"
    );
    let history = harness
        .bridge
        .ottam_tools()
        .session_messages(
            harness.child_caller(&child),
            Some(harness.conversation_id.0.clone()),
            None,
            None,
        )
        .await
        .expect("reads the parent");
    assert!(
        history.contains("nothing to add")
            && !history.contains(ottam_protocol::DELEGATION_ACK_DIRECTIVE),
        "{history}"
    );
    harness.bridge.shutdown().await;
}

// Talking to children and supervising them
// (docs/architecture/backend/mcp.md § Push-Back And Notes).

impl ContinuityHarness {
    /// A call from inside delegated child `child`.
    fn child_caller(&self, child: &ConversationID) -> crate::mcp::McpCaller {
        crate::mcp::McpCaller {
            conversation_id: child.clone(),
            session_id: Some(HarnessSessionId::new("child-session")),
            ..self.caller()
        }
    }

    /// `conversation_id`'s notes, oldest first, with whether a prompt has
    /// carried each.
    async fn notes_in(
        &self,
        conversation_id: &ConversationID,
    ) -> Vec<(ottam_protocol::DelegationNote, bool)> {
        self.env
            .transcript(conversation_id)
            .await
            .into_iter()
            .rev()
            .filter_map(|item| item.message)
            .filter_map(|message| {
                let delivered = message.note_delivered_at.is_some();
                message.note.map(|note| (note, delivered))
            })
            .collect()
    }

    fn prompt_containing(&self, text: &str) -> Option<String> {
        self.adapter()
            .recorded_prompts()
            .into_iter()
            .find(|prompt| prompt.contains(text))
    }
}

/// `message_child` to an idle child runs it with the message at once, and
/// the child's reply comes back to the parent as a report.
#[tokio::test]
async fn a_message_to_an_idle_child_runs_it_and_its_reply_comes_back() {
    let harness = delegation_harness().await;
    harness
        .delegate_titled(Some("Package review"), "Review app/Package.swift")
        .await
        .expect("delegates");
    let child = harness.child_ids().await.remove(0);
    harness
        .wait_for_ended_runs(&harness.conversation_id, 2)
        .await;
    let tools = harness.bridge.ottam_tools();
    let refused = tools
        .message_child(harness.caller(), "lint-bot".into(), "Hi".into())
        .await
        .expect_err("no such child");
    assert!(
        refused.contains("review-bot \"Package review\""),
        "{refused}"
    );
    // By task title, as the agent may name it.
    let reply = tools
        .message_child(
            harness.caller(),
            "package review".into(),
            "Which check is likeliest to break?".into(),
        )
        .await
        .expect("sends");
    assert!(reply.contains("is reading your message now"), "{reply}");
    harness.wait_for_ended_runs(&child, 2).await;
    let in_child = harness.notes_in(&child).await;
    assert_eq!(in_child.len(), 1);
    let (note, delivered) = &in_child[0];
    assert_eq!(
        note.kind,
        ottam_protocol::DelegationNoteKind::ParentMessage
    );
    assert_eq!(note.child_conversation_id, child);
    assert!(delivered);
    let prompt = harness
        .prompt_containing("Which check is likeliest to break?")
        .expect("the child's prompt");
    assert!(
        prompt.contains("Messages from the agent that delegated this task to you"),
        "{prompt}"
    );
    harness
        .wait_for_ended_runs(&harness.conversation_id, 3)
        .await;
    let reports = harness.wait_for_notes(2).await;
    assert!(reports
        .iter()
        .all(|note| note.kind == ottam_protocol::DelegationNoteKind::Report));
    harness.bridge.shutdown().await;
}

/// `message_child` to a working child waits for the end of its turn, then
/// runs; after a stop it keeps waiting for the user.
#[tokio::test]
async fn a_message_to_a_working_child_waits_for_its_turn() {
    let harness = delegation_harness().await;
    harness
        .delegate("pause, then review")
        .await
        .expect("delegates");
    let child = harness.child_ids().await.remove(0);
    let reply = harness
        .bridge
        .ottam_tools()
        .message_child(harness.caller(), child.0.clone(), "Also check tests".into())
        .await
        .expect("queues");
    assert!(reply.contains("waits for the end of its turn"), "{reply}");
    assert_eq!(
        harness
            .env
            .chat
            .waiting_notes(&child)
            .await
            .expect("reads")
            .len(),
        1
    );
    release_paused_prompts(&harness.factory);
    harness.wait_for_ended_runs(&child, 2).await;
    harness.wait_for_no_waiting_notes(&child).await;
    assert_eq!(
        harness
            .adapter()
            .recorded_prompts()
            .iter()
            .filter(|prompt| prompt.contains("Also check tests"))
            .count(),
        1,
        "the message is delivered once"
    );
    harness.bridge.shutdown().await;

    // A child stopped mid-turn keeps the message, as after a relaunch.
    let harness = delegation_harness().await;
    let prompts = harness.adapter().recorded_prompts().len();
    harness.delegate("hold").await.expect("delegates");
    let child = harness.child_ids().await.remove(0);
    // Mid-turn means the agent has the prompt: a stop before it goes out
    // sends the task again with the next run, and "hold" would hold that too.
    tokio::time::timeout(Duration::from_secs(5), async {
        while !harness.adapter().recorded_prompts()[prompts..]
            .iter()
            .any(|prompt| prompt.contains("hold"))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the child's prompt arrives");
    harness
        .bridge
        .ottam_tools()
        .message_child(harness.caller(), child.0.clone(), "Also check tests".into())
        .await
        .expect("queues");
    let run = harness.runs_of(&child).await[0].run_id.clone();
    harness
        .bridge
        .cancel_conversation_run(&harness.env.chat, &child, &run)
        .await
        .expect("stops the child");
    harness.wait_for_notes(1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(harness.runs_of(&child).await.len(), 1);
    assert_eq!(
        harness
            .env
            .chat
            .waiting_notes(&child)
            .await
            .expect("reads")
            .len(),
        1
    );
    harness
        .bridge
        .conversation
        .continue_delegation(&harness.env.chat, &child)
        .await
        .expect("continues");
    harness.wait_for_ended_runs(&child, 2).await;
    assert!(harness.prompt_containing("Also check tests").is_some());
    harness.bridge.shutdown().await;
}

/// `message_parent` wakes an idle parent at once, while another child
/// still works, and carries the reports that waited too.
#[tokio::test]
async fn a_childs_message_wakes_its_parent_without_waiting_for_the_others() {
    let harness = delegation_harness().await;
    harness.delegate("hold").await.expect("delegates");
    let working = harness.child_ids().await.remove(0);
    harness
        .delegate("Review app/Package.swift")
        .await
        .expect("delegates");
    harness.wait_for_notes(1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(harness.runs_of(&harness.conversation_id).await.len(), 1);
    let reply = harness
        .bridge
        .ottam_tools()
        .message_parent(
            harness.child_caller(&working),
            "Blocked on the lockfile".into(),
        )
        .await
        .expect("sends");
    assert!(reply.contains("reading your message now"), "{reply}");
    harness
        .wait_for_ended_runs(&harness.conversation_id, 2)
        .await;
    let prompt = harness
        .prompt_containing("Blocked on the lockfile")
        .expect("the parent's prompt");
    assert!(
        prompt.contains("sent a message while it works")
            && prompt.contains("review-bot finished")
            && prompt.contains("working"),
        "{prompt}"
    );
    let refused = harness
        .bridge
        .ottam_tools()
        .message_parent(harness.caller(), "Hi".into())
        .await
        .expect_err("a top-level conversation has no parent");
    assert!(refused.contains("no parent"), "{refused}");
    harness.bridge.shutdown().await;
}

/// `stop_child` stops the child, and its note is a record that wakes no
/// one: the parent asked for it.
#[tokio::test]
async fn a_child_its_parent_stops_leaves_a_record_only() {
    let harness = delegation_harness().await;
    harness.delegate("hold").await.expect("delegates");
    let child = harness.child_ids().await.remove(0);
    let reply = harness
        .bridge
        .ottam_tools()
        .stop_child(harness.caller(), "review-bot".into())
        .await
        .expect("stops");
    assert!(reply.starts_with("Stopped review-bot"), "{reply}");
    let (note, delivered) = loop {
        if let Some(note) = harness.notes_in(&harness.conversation_id).await.pop() {
            break note;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(note.child_conversation_id, child);
    assert_eq!(
        note.stop_cause,
        Some(ottam_protocol::DelegationStopCause::ParentAgent)
    );
    assert!(delivered);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(harness.runs_of(&harness.conversation_id).await.len(), 1);
    let again = harness
        .bridge
        .ottam_tools()
        .stop_child(harness.caller(), child.0.clone())
        .await
        .expect("answers");
    assert!(again.contains("is not working; it is stopped"), "{again}");
    let listed = harness
        .bridge
        .ottam_tools()
        .list_children(harness.caller())
        .await
        .expect("lists");
    assert!(listed.contains(": stopped"), "{listed}");
    harness.bridge.shutdown().await;
}

/// The history tools read a child's transcript in pages and a tool call
/// by its id, and a child reads its parent's by default.
#[tokio::test]
async fn the_history_tools_read_a_childs_and_a_parents_history() {
    let harness = delegation_harness().await;
    harness.delegate("boundary").await.expect("delegates");
    let child = harness.child_ids().await.remove(0);
    harness
        .wait_for_ended_runs(&harness.conversation_id, 2)
        .await;
    let tools = harness.bridge.ottam_tools();
    let page = tools
        .session_messages(harness.caller(), Some(child.0.clone()), None, None)
        .await
        .expect("reads the child");
    // The fake's call has no ACP call id, so the history names its item.
    let call_id = page
        .split("tool call ")
        .nth(1)
        .and_then(|rest| rest.split(':').next())
        .expect("a tool call entry")
        .to_owned();
    assert!(
        page.contains("task from the delegating agent")
            && page.contains("## Heading continues")
            && page.contains("This is the start of the conversation."),
        "{page}"
    );
    // The task comes first, the latest message last.
    assert!(page.find("task from").unwrap() < page.find("tool call ").unwrap());
    let first = tools
        .session_messages(harness.caller(), Some(child.0.clone()), None, Some(1))
        .await
        .expect("reads one entry");
    let before = first
        .split("before=")
        .nth(1)
        .and_then(|rest| rest.split('.').next())
        .and_then(|position| position.trim().parse::<u64>().ok())
        .unwrap_or_else(|| panic!("names an older page: {first}"));
    assert!(
        first.contains("## Heading continues") && !first.contains(&call_id),
        "{first}"
    );
    let older = tools
        .session_messages(
            harness.caller(),
            Some(child.0.clone()),
            Some(before),
            Some(1),
        )
        .await
        .expect("reads further back");
    assert!(older.contains(&format!("tool call {call_id}")), "{older}");
    let output = tools
        .toolcall_output(
            harness.caller(),
            Some(child.0.clone()),
            call_id.clone(),
            None,
        )
        .await
        .expect("reads the call");
    assert!(
        output.starts_with(&format!("Tool call {call_id}")),
        "{output}"
    );
    let missing = tools
        .toolcall_output(harness.caller(), Some(child.0.clone()), "nope".into(), None)
        .await
        .expect_err("no such call");
    assert!(missing.contains("No tool call nope"), "{missing}");
    let parent = tools
        .session_messages(harness.child_caller(&child), None, None, None)
        .await
        .expect("a child reads its parent");
    assert!(parent.contains("report from review-bot"), "{parent}");
    // A conversation that was not delegated reads its own by default.
    let own = tools
        .session_messages(harness.caller(), None, None, None)
        .await
        .expect("a parent reads its own");
    assert!(own.contains("report from review-bot"), "{own}");
    let info = tools
        .conversation_info(harness.caller())
        .await
        .expect("describes the conversation");
    assert!(
        info.starts_with(&format!("Conversation: {}", harness.conversation_id.0))
            && info.contains("Delegated agents: 1 (list_children lists them)"),
        "{info}"
    );
    let child_info = tools
        .conversation_info(harness.child_caller(&child))
        .await
        .expect("describes the child");
    assert!(
        child_info.contains(&format!(
            "Delegated by: conversation {}",
            harness.conversation_id.0
        )),
        "{child_info}"
    );
    harness.bridge.shutdown().await;
}

/// A message longer than a listed message's preview reads from its stored
/// body, so its cut counts what is really left.
#[tokio::test]
async fn the_history_reads_a_long_message_past_its_preview() {
    let harness = delegation_harness().await;
    harness.delegate("boundary").await.expect("delegates");
    let child = harness.child_ids().await.remove(0);
    harness.wait_for_ended_runs(&child, 1).await;
    let tools = harness.bridge.ottam_tools();
    tools
        .message_child(harness.caller(), child.0.clone(), "m".repeat(6_000))
        .await
        .expect("sends");
    harness.wait_for_ended_runs(&child, 2).await;
    let page = tools
        .session_messages(harness.caller(), Some(child.0.clone()), None, None)
        .await
        .expect("reads the child");
    assert!(page.contains("(4000 more characters)"), "{page}");
    harness.bridge.shutdown().await;
}

/// A child's edit reads back with the text it saved, not just the input
/// left after core moved that text out.
#[tokio::test]
async fn the_history_reads_a_file_changes_saved_contents() {
    let harness = delegation_harness().await;
    harness.delegate("edit a file").await.expect("delegates");
    let child = harness.child_ids().await.remove(0);
    harness.wait_for_ended_runs(&child, 1).await;
    let tools = harness.bridge.ottam_tools();
    let page = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let page = tools
                .toolcall_output(harness.caller(), Some(child.0.clone()), "edit-1".into(), None)
                .await
                .expect("reads the edit");
            if page.contains("the new line") {
                return page;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the change is saved");
    assert!(
        page.contains("Changes and output") && page.contains("for notes.md ---"),
        "{page}"
    );
    harness.bridge.shutdown().await;
}

/// Deleting a parent deletes its children too, or keeps them stopped as
/// top-level conversations.
#[tokio::test]
async fn deleting_a_parent_deletes_or_keeps_its_children() {
    for children in [
        ottam_protocol::ConversationDeleteChildren::Keep,
        ottam_protocol::ConversationDeleteChildren::Delete,
    ] {
        let harness = delegation_harness().await;
        harness.delegate("hold").await.expect("delegates");
        let child = harness.child_ids().await.remove(0);
        harness
            .bridge
            .delete_conversation_with_children(
                &harness.env.chat,
                &harness.env.workspace_id,
                &harness.conversation_id,
                children,
            )
            .await
            .expect("deletes the parent");
        let listed = harness
            .env
            .chat
            .list_conversations()
            .await
            .expect("lists conversations");
        let kept = listed
            .iter()
            .find(|summary| summary.conversation_id == child);
        match children {
            ottam_protocol::ConversationDeleteChildren::Keep => {
                let kept = kept.expect("the child stays");
                assert_eq!(kept.parent_conversation_id, None);
                harness.wait_for_ended_runs(&child, 1).await;
                assert_eq!(
                    harness.runs_of(&child).await[0].status,
                    ConversationRunStatus::Canceled
                );
            }
            ottam_protocol::ConversationDeleteChildren::Delete => {
                assert!(kept.is_none(), "the child is deleted too");
            }
        }
        harness.bridge.shutdown().await;
    }
}
