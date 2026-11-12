use super::attribution::{
    AttributionOp, ChildEnd, ChildRoute, ChildSpawn, OpeningSession, Resolved, RunEnd,
    SessionAttribution, SessionKey,
};
use super::conversation_recorder::{ConversationRecorder, ConversationRecorderHandle};
use super::events::{
    send_conversation_notification, AgentUpdateForwarders, ConversationUpdateSink,
};
use super::native_subagents::{self, MessageCall, MessageCalls};
use super::task_index::{record_lost, Route, TaskOwner};
use super::wire::{
    agent_error_data, core_error_from_harness, harness_error_kind, map_stop_reason,
    sanitize_transport_message,
};
use crate::agent::{
    AgentId, AgentRegistry, HarnessAdapter, HarnessError, HarnessErrorKind, HarnessPrompt,
    HarnessPromptImage, HarnessSessionId, HarnessSessionState, HarnessStopReason, ModelSwitch,
    PlanUsageService, SessionConfigRequest, SteeringCapability,
};
use crate::core_error::CoreError;
use crate::diagnostics::{record_unhandled, shape};
use crate::mcp::{McpGrant, McpService, SessionGrant};
use crate::server::chat::{BeginRunRecord, ChatService};
use crate::server::deferred::DeferredLane;
use crate::server::outbound::OutboundSink;
use ottam_protocol::{
    AgentSessionID, AgentStopReason, AgentUpdate, ConversationCancelRunResult,
    ConversationDeleteResult, ConversationEventKind, ConversationHistoryReplay, ConversationID,
    ConversationMessageID, ConversationRunID, ConversationRunInputID, ConversationRunStatus,
    ConversationSetModeOutcome, ConversationSetModeParams, ConversationSetModeResult,
    ConversationStartRunParams, ConversationStartRunResult, ConversationSteerRunParams,
    ConversationSteerRunResult, ConversationStopBackgroundTaskResult, EngineID,
    HarnessSessionCleanup, HarnessSessionCleanupStatus, ProjectID, RunInputStatus, RunOrigin,
    SessionOrigin, WorkspaceID,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, MutexGuard, OwnedMutexGuard, OwnedSemaphorePermit};
use tokio::task::{AbortHandle, JoinSet};
use tokio_util::sync::CancellationToken;

mod autonomous;
mod delegation;

pub(super) use autonomous::{FollowUpActivity, FollowUpAttribution};
#[cfg(test)]
pub(in crate::server) use delegation::delegation_key;
pub(in crate::server) use delegation::DelegationHost;

/// How long a background-task stop waits for the agent's answer. The agent
/// ends the shell or subagent first, which is quick when it works at all.
const STOP_BACKGROUND_TASK_WAIT: Duration = Duration::from_secs(15);

#[cfg(not(test))]
const CONVERSATION_CANCEL_GRACE_PERIOD: Duration = Duration::from_secs(5);
#[cfg(test)]
const CONVERSATION_CANCEL_GRACE_PERIOD: Duration = Duration::from_millis(100);

/// How long a run waits for its session to finish someone else's prompt before
/// prompting anyway. Generous, because the thing being waited on is an agent
/// winding a cancelled turn down — which can mean finishing a tool call that
/// cannot be interrupted. Nothing is blocked on this wait: the request that
/// created the run has already returned, and the run is cancellable throughout.
#[cfg(not(test))]
const CONVERSATION_SESSION_BUSY_WAIT: Duration = Duration::from_secs(120);
#[cfg(test)]
const CONVERSATION_SESSION_BUSY_WAIT: Duration = Duration::from_millis(500);

/// How often the busy session is re-checked. There is no change notification
/// for session state, and `sessions()` is a lock and a small clone.
const CONVERSATION_SESSION_BUSY_POLL: Duration = Duration::from_millis(25);

/// How long a prompt the agent rejected as busy keeps being resent, and the
/// first and longest pause between tries. oh-my-pi settles within a second
/// of answering a prompt (it drains finished jobs for at most 750 ms).
const CONVERSATION_REMOTE_BUSY_RETRY: Duration = Duration::from_secs(10);
const CONVERSATION_REMOTE_BUSY_BACKOFF: Duration = Duration::from_millis(50);
const CONVERSATION_REMOTE_BUSY_BACKOFF_MAX: Duration = Duration::from_secs(1);

const NEW_CONVERSATION_PREAMBLE: &str = "You are running inside Ottam Chat. Respond to the user's request using the opened project as context. You may answer questions, plan work, or complete requested ad-hoc tasks. Do not modify files unless the user explicitly requested a mutation. Preserve uncertainty and identify missing evidence.";
const CONTINUED_CONVERSATION_PREAMBLE: &str = "Continue the same Ottam Chat session.";

pub(super) trait ConversationPermissionControl: Send + Sync {
    fn cancel_session(&self, agent_id: &AgentId, session_id: &str);
    /// Whether the session, or a subagent it runs, waits on the user.
    fn has_pending(&self, agent_id: &AgentId, session_id: &str) -> bool;
}

pub(super) struct ConversationRunCoordinator {
    registry: Arc<AgentRegistry>,
    outbound: OutboundSink,
    regular_lane: DeferredLane,
    forwarders: Arc<AgentUpdateForwarders>,
    active_runs: Mutex<HashMap<ConversationRunID, ActiveConversationRun>>,
    steering: Mutex<()>,
    /// A conversation's lock while someone holds or waits on it; see
    /// [`Self::lock_launch`].
    launches: ConversationLocks,
    attribution: SessionAttribution,
    message_calls: MessageCalls,
    permissions: Arc<dyn ConversationPermissionControl>,
    workers: StdMutex<JoinSet<()>>,
    /// Lets the forwarder, which only borrows the coordinator, start an
    /// autonomous open on a task of its own.
    this: Weak<Self>,
    /// Set once shutdown begins: nothing opens after it.
    closing: AtomicBool,
    recorded_commands: RecordedCommands,
    /// The MCP proxy sessions are given entries on, once the server starts
    /// it. Unset, as in tests that start none, sessions get no MCP servers.
    mcp: OnceLock<McpService>,
    delegation: delegation::Delegation,
    /// Subscription usage windows; a turn's `_claude/rateLimit` feeds it.
    plan_usage: Arc<PlanUsageService>,
}

/// A lock per conversation, created on first use: launches, and
/// delegation's per-parent locks.
type ConversationLocks = StdMutex<HashMap<ConversationID, Arc<Mutex<()>>>>;

/// A held conversation lock. Dropping it removes the conversation's lock
/// once nobody else holds or waits on it.
pub(super) struct ConversationLockGuard<'a> {
    launches: &'a ConversationLocks,
    conversation_id: ConversationID,
    guard: Option<OwnedMutexGuard<()>>,
}

/// Holds `conversation_id`'s lock in `locks`.
async fn lock_conversation<'a>(
    locks: &'a ConversationLocks,
    conversation_id: &ConversationID,
) -> ConversationLockGuard<'a> {
    let lock = Arc::clone(
        locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(conversation_id.clone())
            .or_default(),
    );
    ConversationLockGuard {
        launches: locks,
        conversation_id: conversation_id.clone(),
        guard: Some(lock.lock_owned().await),
    }
}

impl Drop for ConversationLockGuard<'_> {
    fn drop(&mut self) {
        self.guard.take();
        let mut launches = self
            .launches
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if launches
            .get(&self.conversation_id)
            .is_some_and(|lock| Arc::strong_count(lock) == 1)
        {
            launches.remove(&self.conversation_id);
        }
    }
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(super) struct RunOwnershipSnapshot {
    pub(super) session_id: Option<String>,
    pub(super) worker_registered: bool,
    pub(super) attribution_registered: bool,
}

impl ConversationRunCoordinator {
    pub(super) fn agent_has_live_work(&self, agent: &str) -> bool {
        self.attribution.agent_has_live_work(agent)
    }

    pub(super) fn new(
        registry: Arc<AgentRegistry>,
        outbound: OutboundSink,
        regular_lane: DeferredLane,
        forwarders: Arc<AgentUpdateForwarders>,
        attribution: SessionAttribution,
        permissions: Arc<dyn ConversationPermissionControl>,
        this: Weak<Self>,
    ) -> Self {
        let plan_usage = super::services::plan_usage_service(Arc::clone(&registry), &outbound);
        Self {
            registry,
            outbound,
            regular_lane,
            forwarders,
            active_runs: Mutex::new(HashMap::new()),
            steering: Mutex::new(()),
            launches: StdMutex::new(HashMap::new()),
            attribution,
            message_calls: MessageCalls::default(),
            permissions,
            workers: StdMutex::new(JoinSet::new()),
            this,
            closing: AtomicBool::new(false),
            recorded_commands: StdMutex::new(HashMap::new()),
            mcp: OnceLock::new(),
            delegation: delegation::Delegation::default(),
            plan_usage,
        }
    }

    pub(super) fn plan_usage(&self) -> &Arc<PlanUsageService> {
        &self.plan_usage
    }

    pub(super) fn install_mcp(&self, service: McpService) {
        let _ = self.mcp.set(service);
    }

    /// Mint a token for the session a run is about to open or restore in
    /// `project_root`, and put the entries carrying it into its config. A
    /// run continuing `prior` while the adapter holds it live sends no
    /// session request, so it keeps the entries and token it was opened
    /// with and is granted nothing.
    async fn grant_mcp(
        &self,
        adapter: &Arc<dyn HarnessAdapter>,
        agent_id: &AgentId,
        context: &ConversationRunContext,
        prior: Option<&AgentSessionID>,
        project_root: &Path,
        config: &mut SessionConfigRequest,
    ) -> Option<McpGrant> {
        let continues = prior.is_some_and(|prior| {
            adapter.sessions().iter().any(|session| {
                session.session_id.0 == prior.0 && session.state != HarnessSessionState::Dead
            })
        });
        if continues {
            return None;
        }
        let (grant, entries) = self
            .mcp
            .get()?
            .grant(SessionGrant {
                adapter,
                workspace_id: context.workspace_id.clone(),
                conversation_id: context.conversation_id.clone(),
                agent_id: agent_id.clone(),
                cwd: project_root,
            })
            .await?;
        config.mcp_servers = entries;
        Some(grant)
    }

    pub(super) fn forwarders(&self) -> Arc<AgentUpdateForwarders> {
        Arc::clone(&self.forwarders)
    }

    async fn ensure_forwarder(&self, adapter: Arc<dyn HarnessAdapter>) {
        self.forwarders.ensure(adapter).await;
    }

    async fn flush_forwarder(&self, agent_id: &AgentId) {
        self.forwarders.flush(agent_id).await;
    }

    fn cancel_permissions(&self, agent_id: &AgentId, session_id: &str) {
        self.permissions.cancel_session(agent_id, session_id);
    }

    fn spawn_worker<F>(&self, future: F) -> AbortHandle
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.reap_finished_workers();
        self.workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .spawn(future)
    }

    fn reap_finished_workers(&self) {
        let mut workers = self
            .workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while let Some(result) = workers.try_join_next() {
            if let Err(error) = result {
                if !error.is_cancelled() {
                    tracing::warn!(error = %error, "conversation worker task failed");
                }
            }
        }
    }

    async fn shutdown_workers(&self) {
        let mut workers = {
            let mut workers = self
                .workers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *workers)
        };
        workers.abort_all();
        while let Some(result) = workers.join_next().await {
            if let Err(error) = result {
                if !error.is_cancelled() {
                    tracing::warn!(error = %error, "conversation worker task failed during shutdown");
                }
            }
        }
    }

    async fn active_runs(
        &self,
    ) -> MutexGuard<'_, HashMap<ConversationRunID, ActiveConversationRun>> {
        self.active_runs.lock().await
    }

    async fn lock_steering(&self) -> MutexGuard<'_, ()> {
        self.steering.lock().await
    }

    /// Held from deciding which run takes a conversation next until that run
    /// is in `active_runs`: by a user start, a steer that interrupts or
    /// preempts a run, an autonomous open, and conversation delete. A user
    /// start or steer closes a live autonomous run under it, so the agent's
    /// own continuation cannot reopen in between.
    ///
    /// The forwarder never takes it. Closing a run waits on that run's
    /// Release fence, which only the forwarder applies.
    pub(super) async fn lock_launch(
        &self,
        conversation_id: &ConversationID,
    ) -> ConversationLockGuard<'_> {
        lock_conversation(&self.launches, conversation_id).await
    }

    #[cfg(test)]
    pub(super) fn launch_locks(&self) -> usize {
        self.launches
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    /// How many hold or wait on the conversation's launch lock.
    #[cfg(test)]
    pub(super) fn launch_lock_users(&self, conversation_id: &ConversationID) -> usize {
        self.launches
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(conversation_id)
            .map_or(0, |lock| Arc::strong_count(lock) - 1)
    }

    /// Applies `op` after the agent's queued updates; see
    /// [`AgentUpdateForwarders::fence`].
    async fn fence_attribution(&self, agent_id: &AgentId, op: AttributionOp) {
        self.forwarders.fence(agent_id, Some(op)).await;
    }

    /// Claims the session for the run once every update already queued for
    /// the agent is handled, so neither a previous turn's tail nor a restored
    /// session's replay can be recorded as this run's output.
    async fn claim_session(
        &self,
        adapter: &Arc<dyn HarnessAdapter>,
        agent_id: &AgentId,
        session_key: SessionKey,
        context: &ConversationRunContext,
    ) {
        let generation = adapter.status().borrow().generation;
        self.fence_attribution(
            agent_id,
            AttributionOp::Claim {
                key: session_key,
                context: context.clone(),
                generation,
            },
        )
        .await;
    }

    pub(super) async fn forward_conversation_update(
        &self,
        agent_id: &AgentId,
        session_id: &HarnessSessionId,
        generation: u64,
        update: AgentUpdate,
    ) {
        let mut next = Some(update);
        while let Some(update) = next.take() {
            next = self
                .forward_one(agent_id, session_id, generation, update)
                .await;
        }
    }

    /// Forwards one update, and returns the subagent report it carried
    /// (`SubagentMessageTools`). The report takes the same route right after
    /// it: into the run, or held with it for a follow-up.
    async fn forward_one(
        &self,
        agent_id: &AgentId,
        session_id: &HarnessSessionId,
        generation: u64,
        update: AgentUpdate,
    ) -> Option<AgentUpdate> {
        let key = (agent_id.0.clone(), session_id.0.clone());
        // Native subagents (rule 9): a spawn or state on the parent's session
        // becomes the thread's anchor call; a child's own update resolves on
        // its root's key, stamped with its thread.
        let update = match update {
            AgentUpdate::SubagentSpawned {
                subagent_session_id,
                name,
                task,
                prompt,
            } => match self.attribution.spawn_child(
                &key,
                generation,
                &subagent_session_id,
                name.clone(),
            ) {
                ChildSpawn::Record {
                    thread_id,
                    anchor_call,
                } => native_subagents::anchor(&anchor_call, &thread_id, name, task, prompt),
                ChildSpawn::Drop(reason) => {
                    let update = AgentUpdate::SubagentSpawned {
                        subagent_session_id,
                        name,
                        task,
                        prompt,
                    };
                    self.attribution
                        .record_drop(agent_id, session_id, reason, &update);
                    return None;
                }
            },
            AgentUpdate::SubagentStateUpdate {
                subagent_session_id,
                state,
            } => match self
                .attribution
                .end_child(&(agent_id.0.clone(), subagent_session_id))
            {
                Some(ChildEnd {
                    anchor_call,
                    owner: None,
                }) => native_subagents::state(&anchor_call, state),
                // The run holding the anchor closed or detached: the state
                // revises that anchor, never one in whichever run is current.
                Some(ChildEnd {
                    anchor_call,
                    owner: Some(owner),
                }) => {
                    if let Err(error) = owner
                        .chat
                        .record_task_lifecycle(
                            &owner.conversation_id,
                            &owner.run_id,
                            native_subagents::state(&anchor_call, state),
                        )
                        .await
                    {
                        tracing::warn!(
                            run_id = %owner.run_id.0,
                            kind = %error.kind,
                            "Could not record a subagent's end"
                        );
                    }
                    return None;
                }
                None => return None,
            },
            update => update,
        };
        let (key, thread) = match self.attribution.route_child(&key) {
            ChildRoute::Own => (key, None),
            ChildRoute::Root { key, thread_id } => (key, Some(thread_id)),
            ChildRoute::Drop(reason) => {
                self.attribution
                    .record_drop(agent_id, session_id, reason, &update);
                return None;
            }
        };
        // A subagent's session metadata (title, usage, status, commands) is
        // not the conversation's.
        if thread.is_some() && !native_subagents::is_content(&update) {
            tracing::debug!(agent_id = %agent_id.0, "dropped a subagent's session metadata");
            return None;
        }
        let mut update = match &thread {
            Some(thread) => native_subagents::stamp(update, thread),
            None => update,
        };
        // A subagent's report or the parent's message to it that the adapter
        // forwards only as a call's input (`SubagentMessageTools`).
        let mut handback = None;
        match self.message_calls.observe(&agent_id.0, &update, || {
            self.registry
                .definition(agent_id)
                .map(|definition| definition.subagent_messages)
                .unwrap_or_default()
        }) {
            Some(MessageCall::Handback { call_id, message }) => {
                handback = thread
                    .as_deref()
                    .map(|thread| native_subagents::handback_text(&call_id, message, thread));
            }
            Some(MessageCall::FollowUp { target, message }) => {
                if let Some(target) = self.attribution.live_thread(&key, &target) {
                    native_subagents::stamp_follow_up(&mut update, &target, &message);
                }
            }
            None => {}
        }
        let update = match self.attribution.route_lifecycle(&key, generation, update) {
            Route::Unowned(update) => *update,
            Route::Held => return handback,
            Route::Owner(owner, updates) => {
                // The owner's recorder may be closed, and its errors are its
                // own: a task's late frame must not fail whichever run now
                // holds the session.
                for mut update in updates {
                    if let AgentUpdate::ToolCallUpdate { payload } = &mut update {
                        tag_session(payload, agent_id, session_id);
                    }
                    if let Err(error) = owner
                        .chat
                        .record_task_lifecycle(&owner.conversation_id, &owner.run_id, update)
                        .await
                    {
                        tracing::warn!(
                            run_id = %owner.run_id.0,
                            kind = %error.kind,
                            "Could not record a background task update"
                        );
                    }
                }
                return handback;
            }
        };
        let (context, mut update) = match self.attribution.resolve_update(&key, generation, update)
        {
            Resolved::Run(context, update) => (*context, update),
            Resolved::NotConversation | Resolved::Held => return handback,
            Resolved::Open(request) => {
                self.request_follow_up(request);
                return handback;
            }
            Resolved::Drop(reason, update) => {
                self.attribution
                    .record_drop(agent_id, session_id, reason, &update);
                return handback;
            }
        };
        if let Some(activity) = &context.activity {
            activity.observe(&update);
        }
        if let AgentUpdate::ToolCall { payload } = &update {
            self.attribution
                .index_tool_call(&key, generation, &context.run_id, payload);
            if context.is_delegated_child().await {
                self.child_activity_changed(&context.workspace_id);
            }
        }
        if let AgentUpdate::SessionSignal { mut signal } = update {
            signal.session_id = format!("{}:{}", agent_id.0, session_id.0);
            if context.recorder.queue_signal(signal).await.is_err() {
                tracing::warn!("Session signal recorder closed");
            }
            return handback;
        }
        match &mut update {
            AgentUpdate::RichMessageChunk { metadata, .. } => {
                metadata.session_id = Some(format!("{}:{}", agent_id.0, session_id.0))
            }
            AgentUpdate::AgentMessageChunk { text, message_id }
            | AgentUpdate::AgentThoughtChunk { text, message_id } => {
                let text = text.clone();
                let message_id = message_id.clone();
                let reasoning = matches!(update, AgentUpdate::AgentThoughtChunk { .. });
                update = AgentUpdate::RichMessageChunk {
                    text,
                    message_id,
                    reasoning,
                    metadata: ottam_protocol::TranscriptAdapterMetadata {
                        session_id: Some(format!("{}:{}", agent_id.0, session_id.0)),
                        ..Default::default()
                    },
                };
            }
            AgentUpdate::ToolCall { payload } | AgentUpdate::ToolCallUpdate { payload } => {
                tag_session(payload, agent_id, session_id)
            }
            _ => {}
        }
        // ACP `session_info_update` carries the harness's session title. A real
        // thread name should replace the first-message placeholder, but codex-acp
        // also emits the whole first prompt as a fallback title when no name exists.
        // That prompt begins with Ottam's private wrapper, so adopting it leaks
        // the wrapper into both the sidebar and window title. Strip only that
        // recognizable fallback; a later explicit title still flows normally.
        if let AgentUpdate::Other {
            update_kind,
            payload,
        } = &mut update
        {
            if crate::agent::metadata::has_unknown_extensions(payload) {
                record_unhandled(&agent_id.0, "extension", || shape(&payload["_meta"]));
            }
            self.plan_usage.record_session_update(agent_id, payload);
            if let Some(mut signal) = crate::agent::metadata::session_signal(payload) {
                crate::agent::reword_refresh_lock_failure(&mut signal);
                signal.session_id = format!("{}:{}", agent_id.0, session_id.0);
                if context.recorder.queue_signal(signal).await.is_err() {
                    tracing::warn!("Session signal recorder closed");
                }
            }
            if update_kind == "session_info_update" {
                let title = payload
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if title
                    .as_deref()
                    .is_some_and(is_ottam_prompt_fallback_title)
                {
                    if let Some(object) = payload.as_object_mut() {
                        object.remove("title");
                    }
                } else if let Some(title) = title {
                    // A delegated child keeps the name its parent gave it.
                    let changed = !context.is_delegated_child().await && {
                        let mut last = context
                            .last_adopted_title
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        if last.as_deref() == Some(title.as_str()) {
                            false
                        } else {
                            *last = Some(title.clone());
                            true
                        }
                    };
                    if changed {
                        // Serialize title adoption with the event recorder. A detached
                        // writer can race the recorder's transaction upgrade and make
                        // SQLite return BUSY even with a configured busy timeout.
                        if context.recorder.adopt_title(title).await.is_err() {
                            tracing::warn!(
                                conversation_id = %context.conversation_id.0,
                                "Chat recorder closed before the harness title was adopted"
                            );
                        }
                    }
                }
            }
            if update_kind != "session_info_update" {
                if let Some(metadata) = normalized_session_metadata(update_kind, payload) {
                    if context
                        .recorder
                        .queue_event(
                            ConversationEventKind::SessionMetadata,
                            serde_json::to_value(metadata).expect("typed session metadata"),
                        )
                        .await
                        .is_err()
                    {
                        tracing::warn!("Could not queue session metadata");
                    }
                } else {
                    record_unhandled(&agent_id.0, &format!("unmodeled:{update_kind}"), || {
                        shape(payload)
                    });
                }
            }
            return handback;
        }
        if context.recorder.queue_update(update).await.is_err() {
            tracing::warn!(run_id = %context.run_id.0, "Transcript revision was not persisted");
        }
        handback
    }

    #[cfg(test)]
    pub(super) async fn contains_run(&self, run_id: &ConversationRunID) -> bool {
        self.active_runs.lock().await.contains_key(run_id)
    }

    #[cfg(test)]
    pub(super) async fn run_finished(&self, run_id: &ConversationRunID) -> bool {
        self.active_runs
            .lock()
            .await
            .get(run_id)
            .map(|run| run.finished.is_cancelled())
            .unwrap_or(false)
    }

    #[cfg(test)]
    pub(super) async fn ownership_snapshot(
        &self,
        run_id: &ConversationRunID,
    ) -> Option<RunOwnershipSnapshot> {
        let (session_key, session_id, worker_registered) = {
            let runs = self.active_runs.lock().await;
            let run = runs.get(run_id)?;
            let session_id = run.session_id.as_ref().map(|session| session.0.clone());
            let session_key = session_id
                .as_ref()
                .map(|session| (run.agent_id.0.clone(), session.clone()));
            (session_key, session_id, run.worker_abort.is_some())
        };
        let attribution_registered = session_key
            .as_ref()
            .and_then(|key| self.attribution.running_context(key))
            .is_some_and(|context| context.run_id == *run_id);
        Some(RunOwnershipSnapshot {
            session_id,
            worker_registered,
            attribution_registered,
        })
    }

    #[cfg(test)]
    pub(super) fn attributed_sessions_for_run(
        &self,
        run_id: &ConversationRunID,
    ) -> Vec<SessionKey> {
        self.attribution
            .running_keys()
            .into_iter()
            .filter(|(_, holder)| holder == run_id)
            .map(|(key, _)| key)
            .collect()
    }

    #[cfg(test)]
    pub(super) fn has_attributions(&self) -> bool {
        !self.attribution.running_keys().is_empty()
    }

    #[cfg(test)]
    pub(super) fn attributed_session_keys(&self) -> Vec<SessionKey> {
        self.attribution
            .running_keys()
            .into_iter()
            .map(|(key, _)| key)
            .collect()
    }

    /// After the forwarders stop: see [`SessionAttribution::clear`].
    pub(super) fn forget_sessions(&self) {
        self.attribution.clear();
    }

    #[cfg(test)]
    pub(super) fn attribution(&self) -> &SessionAttribution {
        &self.attribution
    }

    #[cfg(test)]
    pub(super) fn workers_empty(&self) -> bool {
        self.workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    }
}

fn normalized_session_metadata(
    kind: &str,
    payload: &Value,
) -> Option<ottam_protocol::ConversationSessionMetadata> {
    use ottam_protocol::*;
    let mut result = ConversationSessionMetadata::default();
    match kind {
        "usage_update" => result.usage = Some(serde_json::from_value(payload.clone()).ok()?),
        "current_mode_update" => {
            result.current_mode_id = Some(payload.get("currentModeId")?.as_str()?.into())
        }
        "available_commands_update" => {
            result.commands = Some(
                payload
                    .get("availableCommands")?
                    .as_array()?
                    .iter()
                    .map(|command| {
                        Some(ConversationSessionCommand {
                            name: command.get("name")?.as_str()?.into(),
                            description: command.get("description")?.as_str()?.into(),
                            input_hint: command
                                .get("input")
                                .and_then(|input| input.get("hint"))
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                        })
                    })
                    .collect::<Option<Vec<_>>>()?,
            )
        }
        "config_option_update" => {
            result.configuration = Some(
                payload
                    .get("configOptions")?
                    .as_array()?
                    .iter()
                    .map(|option| {
                        let (current_value, value_kind) = match option.get("currentValue")? {
                            Value::String(value) => {
                                (value.clone(), SessionConfigurationValueKind::Text)
                            }
                            Value::Bool(value) => {
                                (value.to_string(), SessionConfigurationValueKind::Boolean)
                            }
                            Value::Number(value) => {
                                (value.to_string(), SessionConfigurationValueKind::Number)
                            }
                            _ => return None,
                        };
                        Some(ConversationSessionConfiguration {
                            id: option.get("id")?.as_str()?.into(),
                            name: option.get("name")?.as_str()?.into(),
                            category: option
                                .get("category")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            current_value,
                            value_kind,
                        })
                    })
                    .collect::<Option<Vec<_>>>()?,
            );
            // Agents that report modes as a config option (OpenCode, and Claude
            // alongside current_mode_update) announce the mode here.
            result.current_mode_id = result
                .configuration
                .iter()
                .flatten()
                .find(|option| option.category.as_deref() == Some("mode"))
                .map(|option| option.current_value.clone())
                .filter(|value| !value.is_empty());
        }
        _ => return None,
    }
    Some(result)
}

#[cfg(test)]
mod prompt_tests {
    use super::{
        compile_conversation_prompt_text, earlier_turns_block, is_ottam_prompt_fallback_title,
        is_slash_command, ConversationPromptInputs, HarnessPromptImage, SessionOrigin,
    };
    use ottam_protocol::ConversationID;

    fn inputs(message: &str, images: Vec<HarnessPromptImage>) -> ConversationPromptInputs {
        ConversationPromptInputs {
            message: message.to_string(),
            message_id: None,
            images,
            model_id: None,
            mode_id: None,
            thought_level_id: None,
        }
    }

    fn conversation() -> ConversationID {
        ConversationID("01a19a6a-8f8f-70c2-874e-49d800e31afe".to_string())
    }

    fn compile(inputs: &ConversationPromptInputs, origin: SessionOrigin) -> String {
        compile_conversation_prompt_text(inputs, origin, &conversation(), None)
    }

    #[test]
    fn slash_commands_skip_the_preamble() {
        for origin in [SessionOrigin::New, SessionOrigin::Continued] {
            assert_eq!(compile(&inputs("/plan", vec![]), origin), "/plan");
            assert_eq!(
                compile(&inputs("/review the diff", vec![]), origin),
                "/review the diff"
            );
        }
    }

    #[test]
    fn paths_and_images_keep_the_preamble() {
        let path = compile(
            &inputs("/Users/me/notes.txt is wrong", vec![]),
            SessionOrigin::Continued,
        );
        assert!(path.starts_with("Continue the same Ottam Chat session."));

        let image = HarnessPromptImage {
            path: "/tmp/shot.png".into(),
            mime_type: "image/png".to_string(),
            uri: None,
        };
        let with_image = compile(&inputs("/plan", vec![image]), SessionOrigin::New);
        assert!(with_image.starts_with("You are running inside Ottam Chat."));
    }

    #[test]
    fn every_wrapped_prompt_names_the_conversation() {
        for origin in [
            SessionOrigin::New,
            SessionOrigin::Continued,
            SessionOrigin::Restored,
        ] {
            let prompt = compile(&inputs("Fix the bug", vec![]), origin);
            assert!(
                prompt.contains("\nOttam conversation: 01a19a6a-8f8f-70c2-874e-49d800e31afe\n"),
                "{prompt}"
            );
            assert!(prompt.ends_with("User request:\nFix the bug"), "{prompt}");
        }
    }

    #[test]
    fn earlier_turns_follow_the_request() {
        let block = earlier_turns_block(
            &conversation(),
            &["opencode (opencode-go/deepseek-v4.1-flash)".to_string()],
            "[t] user:\nwhy the prefix?",
        );
        let prompt = compile_conversation_prompt_text(
            &inputs("any other recommendations?", vec![]),
            SessionOrigin::New,
            &conversation(),
            Some(&block),
        );
        let request = prompt.find("User request:\nany other").unwrap();
        let earlier = prompt.find("Earlier in this conversation").unwrap();
        assert!(request < earlier, "{prompt}");
        assert!(prompt.contains("They ran on opencode (opencode-go/deepseek-v4.1-flash)."));
        assert!(prompt.contains(
            "get_session_toolcall_output with conversation 01a19a6a-8f8f-70c2-874e-49d800e31afe"
        ));
        assert!(prompt.ends_with("[t] user:\nwhy the prefix?"));
        // A replayed first prompt is still a wrapper Codex must not adopt as
        // the session title, whole or cut.
        assert!(is_ottam_prompt_fallback_title(&prompt));
        let normalized = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
        let cut: String = normalized.chars().take(255).collect();
        assert!(is_ottam_prompt_fallback_title(&format!(
            "{}…",
            cut.trim_end()
        )));
    }

    #[test]
    fn prompt_wrapper_titles_are_fallbacks_whole_or_cut() {
        let whole = compile(&inputs("Fix the bug", vec![]), SessionOrigin::New);
        assert!(is_ottam_prompt_fallback_title(&whole));
        // codex-acp 2.2's `normalizeSessionTitle`: 255 characters, trimmed, plus `…`.
        let normalized = whole.split_whitespace().collect::<Vec<_>>().join(" ");
        let cut: String = normalized.chars().take(255).collect();
        let cut = format!("{}…", cut.trim_end());
        assert!(is_ottam_prompt_fallback_title(&cut));
        let short_cut: String = normalized.chars().take(80).collect();
        assert!(is_ottam_prompt_fallback_title(&short_cut));
        let continued: String = "Continue the same Ottam Chat session. User re"
            .chars()
            .collect();
        assert!(is_ottam_prompt_fallback_title(&continued));
        let continued = compile(&inputs("Fix the bug", vec![]), SessionOrigin::Continued)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for chars in [50, 70, 90] {
            let cut: String = continued.chars().take(chars).collect();
            assert!(is_ottam_prompt_fallback_title(&cut), "{cut}");
        }
        for title in ["Fix the bug", "You are", "Add CONTRIBUTING.md", ""] {
            assert!(!is_ottam_prompt_fallback_title(title), "{title}");
        }
    }

    #[test]
    fn slash_command_names() {
        for command in ["/plan", "  /compact", "/plugin:cmd arg", "/my-cmd_2"] {
            assert!(is_slash_command(command), "{command}");
        }
        for text in [
            "plan",
            "/",
            "/ plan",
            "/tmp/x",
            "/Users/me/a b",
            "/1st",
            "a /plan",
        ] {
            assert!(!is_slash_command(text), "{text}");
        }
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::normalized_session_metadata;
    use serde_json::json;

    #[test]
    fn known_metadata_keeps_only_owned_fields_and_explicit_empty_values() {
        let commands = normalized_session_metadata("available_commands_update", &json!({
            "availableCommands": [{"name":"help", "description":"Help", "_meta":{"secret":"ignored"}}],
            "rawOutput":"not retained"
        })).unwrap();
        let saved = serde_json::to_string(&commands).unwrap();
        assert!(!saved.contains("secret"));
        assert!(!saved.contains("rawOutput"));
        assert_eq!(commands.commands.unwrap()[0].name, "help");
        let with_input = normalized_session_metadata(
            "available_commands_update",
            &json!({
                "availableCommands": [
                    {"name":"review", "description":"Review", "input":{"hint":"what to review"}},
                    {"name":"plan", "description":"Plan", "input":null}
                ]
            }),
        )
        .unwrap()
        .commands
        .unwrap();
        assert_eq!(with_input[0].input_hint.as_deref(), Some("what to review"));
        assert_eq!(with_input[1].input_hint, None);
        assert!(normalized_session_metadata(
            "available_commands_update",
            &json!({"availableCommands":[]})
        )
        .unwrap()
        .commands
        .unwrap()
        .is_empty());
        assert_eq!(
            normalized_session_metadata("current_mode_update", &json!({"currentModeId":"plan"}))
                .unwrap()
                .current_mode_id
                .as_deref(),
            Some("plan")
        );
        let config = normalized_session_metadata(
            "config_option_update",
            &json!({"configOptions":[
            {"id":"mode", "name":"Mode", "currentValue":"", "options":[{"opaque":"discard"}]}]}),
        )
        .unwrap();
        assert_eq!(config.configuration.unwrap()[0].current_value, "");
        assert_eq!(config.current_mode_id, None);
        let announced = normalized_session_metadata(
            "config_option_update",
            &json!({"configOptions":[
            {"id":"model", "name":"Model", "category":"model", "currentValue":"opus"},
            {"id":"mode", "name":"Mode", "category":"mode", "currentValue":"default"}]}),
        )
        .unwrap();
        assert_eq!(announced.current_mode_id.as_deref(), Some("default"));
        let usage = normalized_session_metadata(
            "usage_update",
            &json!({"used": 30, "size": 100,
            "cost":{"amount":0.2,"currency":"USD","_meta":{"secret":"ignored"}}}),
        )
        .unwrap();
        assert_eq!(usage.usage.as_ref().unwrap().used, 30);
        assert!(!serde_json::to_string(&usage).unwrap().contains("secret"));
        assert!(normalized_session_metadata("unknown", &json!({"body":"not retained"})).is_none());
    }
}

impl ConversationUpdateSink for ConversationRunCoordinator {
    fn forward_conversation_update<'a>(
        &'a self,
        agent_id: &'a AgentId,
        session_id: &'a HarnessSessionId,
        generation: u64,
        update: AgentUpdate,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(ConversationRunCoordinator::forward_conversation_update(
            self, agent_id, session_id, generation, update,
        ))
    }
}

fn tag_session(payload: &mut Value, agent_id: &AgentId, session_id: &HarnessSessionId) {
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "ottamSessionID".into(),
            Value::String(format!("{}:{}", agent_id.0, session_id.0)),
        );
    }
}

#[derive(Clone)]
pub(super) struct ConversationRunContext {
    pub(super) workspace_id: WorkspaceID,
    pub(super) project_id: Option<ProjectID>,
    pub(super) conversation_id: ConversationID,
    pub(super) run_id: ConversationRunID,
    /// Who started the run.
    pub(super) origin: RunOrigin,
    /// The input the run delivers. An autonomous run has none: the agent
    /// started it, so there is nothing to send and no delivery to resolve.
    run_input_id: Option<ConversationRunInputID>,
    is_steer_input: bool,
    chat: ChatService,
    pub(super) recorder: ConversationRecorderHandle,
    last_adopted_title: Arc<StdMutex<Option<String>>>,
    /// Whether the conversation was delegated, read once per run.
    delegated: Arc<tokio::sync::OnceCell<bool>>,
    /// What an autonomous run's close rules watch. `None` for a user run,
    /// which ends with its prompt.
    pub(super) activity: Option<Arc<FollowUpActivity>>,
}

impl ConversationRunContext {
    /// Read per request, so turning it off mid-turn brings the next sheet
    /// back. A read that fails leaves the decision to the user.
    pub(super) async fn always_allow_tool_actions(&self) -> bool {
        match self
            .chat
            .always_allow_tool_actions(&self.conversation_id)
            .await
        {
            Ok(enabled) => enabled,
            Err(error) => {
                tracing::warn!(%error, "could not read the conversation's always-allow setting");
                false
            }
        }
    }

    /// Whether the run's conversation was delegated. A child's "Always allow
    /// tool actions" covers every agent, since it was inherited from a
    /// parent that either allowed everything or ran in a bypass mode.
    pub(super) async fn is_delegated_child(&self) -> bool {
        // A read that fails counts as no and is tried again next time.
        self.delegated
            .get_or_try_init(|| async {
                self.chat
                    .delegation_of(&self.conversation_id)
                    .await
                    .map(|delegation| delegation.is_some())
            })
            .await
            .is_ok_and(|delegated| *delegated)
    }

    /// Records a question on the tool call that asked it. A record that
    /// can't be written leaves the panel working; the run's end settles it.
    pub(super) async fn record_question(
        &self,
        provider_tool_call_id: String,
        question: ottam_protocol::TranscriptQuestion,
    ) {
        if let Err(error) = self
            .chat
            .record_question(
                &self.conversation_id,
                &self.run_id,
                provider_tool_call_id,
                question,
            )
            .await
        {
            tracing::debug!(error = %error.message, "question record not written");
        }
    }

    pub(super) fn task_owner(&self) -> TaskOwner {
        TaskOwner {
            chat: self.chat.clone(),
            workspace_id: self.workspace_id.clone(),
            project_id: self.project_id.clone(),
            conversation_id: self.conversation_id.clone(),
            run_id: self.run_id.clone(),
        }
    }
}

#[cfg(test)]
impl ConversationRunContext {
    /// A context for attribution tests, which compare run ids and never
    /// record through it. The returned recorder owns its task.
    pub(super) fn for_attribution_test(
        chat: ChatService,
        workspace_id: WorkspaceID,
        conversation_id: ConversationID,
        run_id: &str,
        outbound: OutboundSink,
    ) -> (Self, ConversationRecorder) {
        let run_id = ConversationRunID(run_id.to_string());
        let (recorder, owner) = ConversationRecorder::spawn(
            chat.clone(),
            conversation_id.clone(),
            run_id.clone(),
            None,
            outbound,
        );
        let context = Self {
            workspace_id,
            project_id: None,
            conversation_id,
            run_id,
            origin: RunOrigin::User,
            run_input_id: Some(ConversationRunInputID("attribution-test-input".to_string())),
            is_steer_input: false,
            chat,
            recorder,
            last_adopted_title: Arc::new(StdMutex::new(None)),
            delegated: Default::default(),
            activity: None,
        };
        (context, owner)
    }
}

struct ActiveConversationRun {
    context: ConversationRunContext,
    agent_id: AgentId,
    session_id: Option<crate::agent::HarnessSessionId>,
    adapter: Option<Arc<dyn HarnessAdapter>>,
    cancel: CancellationToken,
    /// Ends an autonomous run as completed, so the user's next run can take
    /// its session. A prompted run ends with its prompt and never watches it.
    close: CancellationToken,
    finished: CancellationToken,
    recorder: ConversationRecorder,
    worker_abort: Option<tokio::task::AbortHandle>,
}

/// Everything the in-memory runtime needs after the caller has completed its
/// durable initial-run or follow-up preparation.
struct ConversationRunLaunch {
    permit: OwnedSemaphorePermit,
    agent_id: AgentId,
    engine_id: EngineID,
    workspace_id: WorkspaceID,
    project_id: Option<ProjectID>,
    conversation_id: ConversationID,
    record: BeginRunRecord,
    chat: ChatService,
    working_root: PathBuf,
    session_config: SessionConfigRequest,
    kind: ConversationRunLaunchKind,
}

#[derive(Clone, Copy)]
enum ConversationRunLaunchKind {
    Initial,
    Followup,
}

enum ConversationRunCompletion {
    CancelledBeforeBinding,
    HarnessFailureBeforeBinding(HarnessError),
    StorageFailureAfterBinding {
        error: CoreError,
        session_key: (String, String),
    },
    PromptFinished(Result<HarnessStopReason, HarnessError>),
}

struct ConversationRunWorker {
    permit: OwnedSemaphorePermit,
    context: ConversationRunContext,
    agent_id: AgentId,
    working_root: PathBuf,
    inputs: ConversationPromptInputs,
    prior_session: Option<AgentSessionID>,
    /// The mode the agent last confirmed for `prior_session`.
    prior_mode_id: Option<String>,
    cancel: CancellationToken,
    finished: CancellationToken,
}

struct ConversationRunCompletionGuard(CancellationToken);

impl Drop for ConversationRunCompletionGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl ConversationRunCoordinator {
    /// A run the user started, which also delivers the conversation's
    /// waiting delegation notes.
    pub(super) async fn start_conversation_run(
        self: &Arc<Self>,
        params: ConversationStartRunParams,
        chat: ChatService,
        working_root: PathBuf,
        project_id: Option<ProjectID>,
    ) -> Result<ConversationStartRunResult, CoreError> {
        self.start_run(params, chat, working_root, project_id, true)
            .await
    }

    /// Starts a run of a pending input. `prefix` goes ahead of the input's
    /// text in the prompt, not in the stored message. `user_driven` says the
    /// user started it with their own words, carrying no parent's message.
    async fn start_conversation_run_with(
        self: &Arc<Self>,
        params: ConversationStartRunParams,
        chat: ChatService,
        working_root: PathBuf,
        project_id: Option<ProjectID>,
        prefix: Option<String>,
        user_driven: bool,
    ) -> Result<ConversationStartRunResult, CoreError> {
        let _launch = self.lock_launch(&params.conversation_id).await;
        let agent_id = AgentId(params.engine_id.0.clone());
        self.registry
            .definition(&agent_id)
            .map_err(core_error_from_harness)?;
        // The user's message preempts whatever the agent is doing on its own,
        // unless `begin_run` would refuse it anyway. That run frees its permit
        // before it ends.
        if self.has_autonomous_run(&params.conversation_id).await {
            chat.ensure_delivery_resolved(&params.conversation_id)
                .await?;
            self.close_autonomous_run(&params.conversation_id).await;
        }
        let permit = self.regular_lane.try_acquire().map_err(|_| {
            CoreError::new(
                "agentRequestQueueSaturated",
                "agent request queue is saturated",
            )
        })?;
        // Rejection-on-ambiguity, working-root snapshot, run creation naming the
        // pending input, and the durable `sending` transition all happen inside
        // one persisted operation before any adapter I/O.
        let mut record = chat
            .begin_run(
                &params.conversation_id,
                &params.engine_id,
                params.model_id.clone(),
                params.mode_id.clone(),
                params.thought_level_id.clone(),
                &params.run_input_id,
            )
            .await?;
        if let Some(prefix) = prefix {
            record.message_body.insert_str(0, &prefix);
        }
        self.record_prompted_run(&params.conversation_id, &record.run.run_id, user_driven);
        let result = ConversationStartRunResult {
            run: record.run.clone(),
            input: record.input.clone(),
        };
        self.bootstrap_conversation_run(ConversationRunLaunch {
            permit,
            agent_id,
            engine_id: params.engine_id,
            workspace_id: params.workspace_id,
            project_id,
            conversation_id: params.conversation_id,
            record,
            chat,
            working_root,
            session_config: SessionConfigRequest {
                model_id: params.model_id,
                mode_id: params.mode_id,
                thought_level_id: params.thought_level_id,
                mcp_servers: Vec::new(),
            },
            kind: ConversationRunLaunchKind::Initial,
        })
        .await;
        Ok(result)
    }

    async fn launch_followup_run(self: &Arc<Self>, launch: ConversationRunLaunch) {
        self.bootstrap_conversation_run(launch).await;
    }

    async fn bootstrap_conversation_run(self: &Arc<Self>, launch: ConversationRunLaunch) {
        let ConversationRunLaunch {
            permit,
            agent_id,
            engine_id,
            workspace_id,
            project_id,
            conversation_id,
            record,
            chat,
            working_root,
            session_config,
            kind,
        } = launch;
        let run = record.run;
        let input = record.input;
        let is_steer_input = matches!(kind, ConversationRunLaunchKind::Followup)
            && input.kind == ottam_protocol::RunInputKind::Steer;
        let (recorder, recorder_owner) = ConversationRecorder::spawn(
            chat.clone(),
            conversation_id.clone(),
            run.run_id.clone(),
            project_id.clone(),
            self.outbound.clone(),
        );
        let context = ConversationRunContext {
            workspace_id,
            project_id,
            conversation_id,
            run_id: run.run_id.clone(),
            origin: RunOrigin::User,
            run_input_id: Some(input.run_input_id),
            is_steer_input,
            chat: chat.clone(),
            recorder,
            last_adopted_title: Arc::new(StdMutex::new(None)),
            delegated: Default::default(),
            activity: None,
        };
        let cancel = CancellationToken::new();
        let finished = CancellationToken::new();
        self.active_runs().await.insert(
            run.run_id.clone(),
            ActiveConversationRun {
                context: context.clone(),
                agent_id: agent_id.clone(),
                session_id: None,
                adapter: None,
                cancel: cancel.clone(),
                close: CancellationToken::new(),
                finished: finished.clone(),
                recorder: recorder_owner,
                worker_abort: None,
            },
        );
        if context.is_steer_input {
            context
                .queue_event(
                    ConversationEventKind::SteerRequested,
                    json!({"runInputID": context.run_input_id}),
                )
                .await;
        }
        // Continue the harness session for this conversation, engine, and source
        // scope; a different engine or scope always starts fresh.
        let (prior_session, prior_mode_id) = match chat
            .resumable_session(&context.conversation_id, &engine_id)
            .await
        {
            Ok(Some((session, mode_id))) => (Some(session), mode_id),
            Ok(None) => (None, None),
            Err(error) => {
                if matches!(kind, ConversationRunLaunchKind::Initial) {
                    tracing::warn!(
                        conversation_id = %context.conversation_id.0,
                        error = %error,
                        "failed to resolve a resumable session; starting fresh"
                    );
                }
                (None, None)
            }
        };
        let inputs = ConversationPromptInputs {
            message: record.message_body,
            message_id: Some(input.message_id),
            images: record.images,
            model_id: session_config.model_id,
            mode_id: session_config.mode_id,
            thought_level_id: session_config.thought_level_id,
        };
        self.spawn_conversation_worker(ConversationRunWorker {
            permit,
            context,
            agent_id,
            working_root,
            inputs,
            prior_session,
            prior_mode_id,
            cancel,
            finished,
        })
        .await;
    }

    async fn spawn_conversation_worker(self: &Arc<Self>, worker: ConversationRunWorker) {
        let ConversationRunWorker {
            permit,
            context,
            agent_id,
            working_root,
            inputs,
            prior_session,
            prior_mode_id,
            cancel,
            finished,
        } = worker;
        let run_id = context.run_id.clone();
        let coordinator = Arc::clone(self);
        let worker_abort = self.spawn_worker(async move {
            // Preserve the existing drop order: the completion guard is
            // declared after the permit, so it signals `finished` before
            // the permit is released. Changing that ordering is behavioral
            // work and belongs in a separately characterized fix.
            let _permit = permit;
            let _completion_guard = ConversationRunCompletionGuard(finished.clone());
            coordinator
                .run_conversation_prompt(
                    context,
                    agent_id,
                    working_root,
                    inputs,
                    prior_session,
                    prior_mode_id,
                    cancel,
                    finished,
                )
                .await;
        });
        if let Some(active) = self.active_runs().await.get_mut(&run_id) {
            active.worker_abort = Some(worker_abort);
        }
    }

    async fn fail_pending_followup_input(
        &self,
        chat: &ChatService,
        conversation_id: &ConversationID,
        input_id: &ConversationRunInputID,
        reason: &str,
    ) {
        if let Err(error) = chat
            .mark_input_failed(conversation_id, input_id, reason.to_string())
            .await
        {
            tracing::warn!(
                conversation_id = %conversation_id.0,
                run_input_id = %input_id.0,
                error = %error,
                "failed to close a pending steering input after follow-up launch rejection"
            );
        }
    }

    pub(super) async fn reserve_followup_launch(
        &self,
        chat: &ChatService,
        conversation_id: &ConversationID,
        input_id: &ConversationRunInputID,
        engine_id: &EngineID,
    ) -> Result<(OwnedSemaphorePermit, AgentId), CoreError> {
        let permit = match self.regular_lane.try_acquire() {
            Ok(permit) => permit,
            Err(_) => {
                let error = CoreError::new(
                    "agentRequestQueueSaturated",
                    "agent request queue is saturated",
                );
                self.fail_pending_followup_input(chat, conversation_id, input_id, error.kind)
                    .await;
                return Err(error);
            }
        };
        let agent_id = AgentId(engine_id.0.clone());
        if let Err(error) = self.registry.definition(&agent_id) {
            let error = core_error_from_harness(error);
            self.fail_pending_followup_input(chat, conversation_id, input_id, error.kind)
                .await;
            return Err(error);
        }
        Ok((permit, agent_id))
    }

    pub(super) async fn steer_conversation_run(
        self: &Arc<Self>,
        params: ConversationSteerRunParams,
        chat: ChatService,
        working_root: PathBuf,
        project_id: Option<ProjectID>,
    ) -> Result<ConversationSteerRunResult, CoreError> {
        self.user_wrote(&params.conversation_id);
        let _steer_guard = self.lock_steering().await;
        let active = {
            let runs = self.active_runs().await;
            runs.get(&params.run_id).map(|run| {
                (
                    run.context.clone(),
                    run.agent_id.clone(),
                    run.session_id.clone(),
                    run.adapter.clone(),
                )
            })
        };
        // The agent continuing on its own is not steered, whatever the engine
        // supports: the message preempts that run and starts one of its own.
        let mut preempts = active
            .as_ref()
            .is_some_and(|(context, ..)| context.origin == RunOrigin::Autonomous);
        // Native steering needs the two things a run binds late: a session to
        // steer and an adapter to steer through. Reading the route off that
        // bound tuple, rather than off what the engine advertises, is what
        // keeps the decision and the means in step — an active run registers
        // before it binds either (see `session_id: None` at the launch site),
        // so a steer can arrive while the capability is already `Native` and
        // there is still nothing to send it through.
        let bound = match active {
            Some((context, _, Some(session_id), Some(adapter))) => {
                Some((context, session_id, adapter))
            }
            _ => None,
        };
        let capability = match bound.as_ref() {
            Some((_, _, adapter)) => adapter.status().borrow().capabilities.steering,
            None => {
                let run = chat
                    .conversation_run(&params.conversation_id, &params.run_id)
                    .await?;
                // An autonomous target that is no longer live still decides:
                // a replay of the steer that preempted it must reach
                // `prepare_steer`, which answers it, not the refusal below.
                preempts |= run.origin == RunOrigin::Autonomous;
                let status = self
                    .registry
                    .cached_status(&AgentId(run.engine_id.0.clone()))
                    .await
                    .map_err(core_error_from_harness)?;
                // An engine that cannot steer at all is still refused. Anything
                // else takes interrupt-and-continue, the road every engine
                // without native steering already travels — the message is
                // delivered rather than parked in a status nothing resumes.
                match status.capabilities.steering {
                    SteeringCapability::Unsupported => SteeringCapability::Unsupported,
                    _ => SteeringCapability::InterruptThenPrompt,
                }
            }
        };
        if capability == SteeringCapability::Unsupported && !preempts {
            return Err(CoreError::new(
                "steeringUnsupported",
                "the selected engine does not support steering",
            )
            .recoverable(true));
        }

        let prepared = chat
            .prepare_steer(
                &params.conversation_id,
                &params.run_id,
                params.message,
                params.attachment_ids,
                params.idempotency_key,
            )
            .await?;
        if prepared.input.status != RunInputStatus::Pending {
            return Ok(ConversationSteerRunResult {
                input: prepared.input,
                run: prepared.run,
            });
        }

        // Some only when the capability was read from this very adapter, so the
        // native block cannot be entered without the session it needs.
        if let Some((context, session_id, adapter)) =
            bound.filter(|_| !preempts && capability == SteeringCapability::Native)
        {
            let input = chat
                .mark_input_sending(
                    &params.conversation_id,
                    &prepared.input.run_input_id,
                    &params.run_id,
                    crate::storage::models::DeliveryMode::NativeSteer,
                )
                .await?;
            context
                .queue_event(
                    ConversationEventKind::SteerRequested,
                    json!({"runInputID": input.run_input_id}),
                )
                .await;
            let steer_prompt = HarnessPrompt {
                text: prepared.message_body,
                images: prepared.images,
            };
            let steering = SteerGuard::begin(
                self.forwarders(),
                &adapter.definition().agent_id,
                &session_id,
                &params.run_id,
            )
            .await;
            let steered = adapter.steer(session_id.clone(), steer_prompt).await;
            if steered.as_ref().is_err_and(|error| error.detached_turn) {
                steering.detached();
            }
            drop(steering);
            match steered {
                Ok(()) => {
                    let input = chat
                        .mark_input_delivered(&params.conversation_id, &input.run_input_id)
                        .await?;
                    context
                        .queue_event(
                            ConversationEventKind::SteerDelivered,
                            json!({"runInputID": input.run_input_id}),
                        )
                        .await;
                    let run = chat
                        .conversation_run(&params.conversation_id, &params.run_id)
                        .await?;
                    return Ok(ConversationSteerRunResult { input, run });
                }
                // Two answers that both mean the message was never taken, so
                // neither is a reason to lose it. `NoActivePrompt`: the turn
                // settled between the local check and the request landing.
                // `SteeringUnsupported`: the agent advertised the extension and
                // does not implement it, and the adapter has now corrected its
                // own capability — a demotion that could equally have landed
                // between this run's capability read and the request.
                //
                // Either way the input goes back and travels the road an engine
                // without native steering takes. The paths below already handle
                // both a run that completed under us and one still winding down.
                Err(error)
                    if matches!(
                        error.kind,
                        HarnessErrorKind::NoActivePrompt | HarnessErrorKind::SteeringUnsupported
                    ) =>
                {
                    tracing::debug!(
                        run_id = %params.run_id.0,
                        error = %error.message,
                        "native steer delivered nothing; falling back to interrupt-and-continue"
                    );
                    chat.mark_input_undelivered(&params.conversation_id, &input.run_input_id)
                        .await?;
                }
                Err(error) => {
                    if matches!(
                        error.kind,
                        HarnessErrorKind::AdapterDead
                            | HarnessErrorKind::AdapterExited
                            | HarnessErrorKind::PromptExecution
                    ) {
                        chat.mark_input_delivery_unknown(
                            &params.conversation_id,
                            &input.run_input_id,
                        )
                        .await?;
                    } else {
                        chat.mark_input_failed(
                            &params.conversation_id,
                            &input.run_input_id,
                            harness_error_kind(error.kind).to_string(),
                        )
                        .await?;
                    }
                    return Err(core_error_from_harness(error));
                }
            }
        }

        // No other run takes the conversation until the follow-up is active.
        // The agent's own continuation gives way first: the run this steer
        // preempts, or one that opened after the targeted run ended.
        let _launch = self.lock_launch(&params.conversation_id).await;
        self.close_autonomous_run(&params.conversation_id).await;
        let ended = if preempts {
            chat.conversation_run(&params.conversation_id, &params.run_id)
                .await
        } else {
            self.cancel_run_under_launch(&chat, &params.conversation_id, &params.run_id)
                .await
                .map(|canceled| canceled.run)
        };
        let ended = match ended {
            Ok(ended) => ended,
            Err(error) => {
                self.fail_pending_followup_input(
                    &chat,
                    &params.conversation_id,
                    &prepared.input.run_input_id,
                    error.kind,
                )
                .await;
                return Err(error);
            }
        };
        // The canceled worker has released its regular permit. Reserve the
        // replacement slot and validate the engine before any follow-up run or
        // `sending` transition is committed.
        let (followup_permit, followup_agent_id) = self
            .reserve_followup_launch(
                &chat,
                &params.conversation_id,
                &prepared.input.run_input_id,
                &prepared.run.engine_id,
            )
            .await?;
        if preempts || ended.status != ConversationRunStatus::Canceled {
            // Completion won the race after intent was persisted, or the run
            // was the agent's own and gave way. Route the still-pending input
            // through the ordinary follow-up path without interrupting the
            // run.
            let followup = match chat
                .begin_run(
                    &params.conversation_id,
                    &prepared.run.engine_id,
                    prepared.run.model_id.clone(),
                    prepared.run.mode_id.clone(),
                    prepared.run.thought_level_id.clone(),
                    &prepared.input.run_input_id,
                )
                .await
            {
                Ok(followup) => followup,
                Err(error) => {
                    self.fail_pending_followup_input(
                        &chat,
                        &params.conversation_id,
                        &prepared.input.run_input_id,
                        error.kind,
                    )
                    .await;
                    return Err(error);
                }
            };
            let result = ConversationSteerRunResult {
                input: followup.input.clone(),
                run: followup.run.clone(),
            };
            self.launch_followup_run(ConversationRunLaunch {
                permit: followup_permit,
                agent_id: followup_agent_id,
                engine_id: prepared.run.engine_id,
                workspace_id: params.workspace_id,
                project_id,
                conversation_id: params.conversation_id,
                record: followup,
                chat,
                working_root,
                // Follow-ups continue (or restore) the prior session, which
                // keeps its mode and effort. Only the model rides along for a
                // degraded fresh-session path, matching prior behavior.
                session_config: SessionConfigRequest {
                    model_id: prepared.run.model_id,
                    mode_id: None,
                    thought_level_id: None,
                    mcp_servers: Vec::new(),
                },
                kind: ConversationRunLaunchKind::Followup,
            })
            .await;
            return Ok(result);
        }

        let interrupted = match chat
            .interrupt_after_cancel(
                &params.conversation_id,
                &params.run_id,
                &prepared.input.run_input_id,
            )
            .await
        {
            Ok(interrupted) => interrupted,
            Err(error) => {
                self.fail_pending_followup_input(
                    &chat,
                    &params.conversation_id,
                    &prepared.input.run_input_id,
                    error.kind,
                )
                .await;
                return Err(error);
            }
        };
        let result = ConversationSteerRunResult {
            input: interrupted.followup.input.clone(),
            run: interrupted.followup.run.clone(),
        };
        self.launch_followup_run(ConversationRunLaunch {
            permit: followup_permit,
            agent_id: followup_agent_id,
            engine_id: interrupted.followup.run.engine_id.clone(),
            workspace_id: params.workspace_id,
            project_id: project_id.clone(),
            conversation_id: params.conversation_id.clone(),
            session_config: SessionConfigRequest {
                model_id: interrupted.followup.run.model_id.clone(),
                // Interrupt follow-ups continue (or restore) the interrupted
                // session, which keeps its mode and effort. Only the model rides
                // along for the degraded fresh-session path, matching prior behavior.
                mode_id: None,
                thought_level_id: None,
                mcp_servers: Vec::new(),
            },
            record: interrupted.followup,
            chat: chat.clone(),
            working_root,
            kind: ConversationRunLaunchKind::Followup,
        })
        .await;
        let events = chat
            .record_run_events(
                &params.conversation_id,
                &params.run_id,
                project_id.clone(),
                vec![(
                    ConversationEventKind::RunInterrupted,
                    json!({
                        "status": interrupted.interrupted_run.status,
                        "runInputID": prepared.input.run_input_id
                    }),
                )],
            )
            .await?;
        for event in events {
            send_conversation_notification(&self.outbound, event);
        }
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_conversation_prompt(
        self: &Arc<Self>,
        context: ConversationRunContext,
        agent_id: AgentId,
        project_root: PathBuf,
        inputs: ConversationPromptInputs,
        prior_session: Option<AgentSessionID>,
        prior_mode_id: Option<String>,
        cancel: CancellationToken,
        finished: CancellationToken,
    ) {
        let (adapter, _session_admission) = tokio::select! {
            _ = cancel.cancelled() => {
                self.complete_conversation_run(
                    &context,
                    ConversationRunCompletion::CancelledBeforeBinding,
                    &finished,
                ).await;
                return;
            }
            result = self.registry.get_or_start_for_conversation(&agent_id) => match result {
                Ok(adapter) => adapter,
                Err(error) => {
                    let resolution = match &context.run_input_id {
                        Some(run_input_id) => context
                            .chat
                            .resolve_input_after_launch_failure(
                                &context.conversation_id,
                                run_input_id,
                                harness_error_kind(error.kind).to_string(),
                            )
                            .await,
                        // Only a prompted run launches the adapter.
                        None => Ok(false),
                    };
                    match resolution {
                        Ok(_) => self.finalize_conversation_error(&context, &error).await,
                        Err(resolution_error) => {
                            tracing::error!(
                                run_id = %context.run_id.0,
                                error = %resolution_error,
                                adapter_error = %error,
                                "failed to resolve durable input after adapter launch failure"
                            );
                            self.finalize_conversation_run(
                                &context,
                                ConversationRunStatus::Failed,
                                None,
                                json!({
                                    "kind": resolution_error.kind,
                                    "message": sanitize_transport_message(
                                        &resolution_error.message
                                    ),
                                    "adapterLaunchError": {
                                        "kind": agent_error_data(&error)["kind"],
                                        "message": sanitize_transport_message(&error.message),
                                    }
                                }),
                            )
                            .await;
                        }
                    }
                    self.remove_conversation_run(&context, &finished).await;
                    return;
                }
            }
        };
        self.ensure_forwarder(Arc::clone(&adapter)).await;
        let mut session_config = inputs.session_config();
        let mcp_grant = tokio::select! {
            _ = cancel.cancelled() => {
                self.complete_conversation_run(
                    &context,
                    ConversationRunCompletion::CancelledBeforeBinding,
                    &finished,
                ).await;
                return;
            }
            grant = self.grant_mcp(&adapter, &agent_id, &context, prior_session.as_ref(), &project_root, &mut session_config) => grant,
        };
        let mut resolution =
            ResolutionGuard::new(self.forwarders(), &self.attribution, &agent_id, &context);
        let (session_id, origin) = tokio::select! {
            _ = cancel.cancelled() => {
                self.complete_conversation_run(
                    &context,
                    ConversationRunCompletion::CancelledBeforeBinding,
                    &finished,
                ).await;
                return;
            }
            result = self.resolve_session(&adapter, &agent_id, prior_session.as_ref(), prior_mode_id.as_deref(), &project_root, &session_config, &mut resolution) => match result {
                Ok(resolved) => resolved,
                Err(error) => {
                    self.complete_conversation_run(
                        &context,
                        ConversationRunCompletion::HarnessFailureBeforeBinding(error),
                        &finished,
                    ).await;
                    return;
                }
            }
        };
        bind_mcp_grant(mcp_grant, &session_id, origin);
        self.await_session_availability(&adapter, &session_id, &cancel)
            .await;
        if cancel.is_cancelled() {
            self.complete_conversation_run(
                &context,
                ConversationRunCompletion::CancelledBeforeBinding,
                &finished,
            )
            .await;
            return;
        }
        {
            let mut runs = self.active_runs().await;
            if let Some(run) = runs.get_mut(&context.run_id) {
                run.session_id = Some(session_id.clone());
                run.adapter = Some(Arc::clone(&adapter));
            }
        }
        let session_key = (agent_id.0.clone(), session_id.0.clone());
        self.claim_session(&adapter, &agent_id, session_key.clone(), &context)
            .await;
        resolution.disarm();
        // Read before the run shows as running, so a stop sent the moment it
        // does still finds the prompt on its way.
        let (prompt, seed) = self
            .seeded_prompt(&context, &inputs, origin, &session_id)
            .await;
        if let Err(error) = context
            .chat
            .mark_run_running(
                &context.conversation_id,
                &context.run_id,
                &AgentSessionID(session_id.0.clone()),
                origin,
            )
            .await
        {
            self.complete_conversation_run(
                &context,
                ConversationRunCompletion::StorageFailureAfterBinding { error, session_key },
                &finished,
            )
            .await;
            return;
        }
        let seeded = record_history_seed(&context, seed).await;
        record_confirmed_session(&adapter, &context, &session_id, &self.recorded_commands).await;
        context
            .queue_event(
                ConversationEventKind::RunStatus,
                json!({"status": "running", "origin": context.origin}),
            )
            .await;

        let (result, sent) = self
            .prompt_or_cancel(&adapter, &agent_id, &session_id, prompt, &cancel)
            .await;
        if seeded && !prompt_reached_agent(&result, sent) {
            unseed_history(&context).await;
        }
        // A continued session can race an adapter restart between resolution and
        // prompt. Recover once onto a restored or fresh session, then re-prompt;
        // the retry seeds the session it lands on.
        let (result, _session_id, session_key) = match result {
            Err(error)
                if error.kind == HarnessErrorKind::SessionNotFound
                    && origin == SessionOrigin::Continued
                    && !cancel.is_cancelled() =>
            {
                tracing::warn!(
                    run_id = %context.run_id.0,
                    session_id = %session_id.0,
                    "conversation session vanished before prompt; recovering onto a new session"
                );
                self.retry_conversation_prompt(
                    &context,
                    &agent_id,
                    &adapter,
                    &project_root,
                    &inputs,
                    session_id,
                    session_key,
                    &cancel,
                )
                .await
            }
            result => (result, session_id, session_key),
        };
        // ACP adapters send session updates before resolving the prompt, so the
        // release fence folds in everything this run produced before it lets go
        // of the key. It lets go before the terminal status is published: a
        // client is free to start the next run the instant it sees this one end.
        let ended = match &result {
            Ok(HarnessStopReason::Cancelled) => RunEnd::Cancelled,
            Ok(_) => RunEnd::Completed,
            Err(_) => RunEnd::Failed,
        };
        self.release_conversation_session(&agent_id, session_key, &context.run_id, ended)
            .await;
        self.complete_conversation_run(
            &context,
            ConversationRunCompletion::PromptFinished(result),
            &finished,
        )
        .await;
    }

    /// Adapter-confirmed acceptance: the prompt request was accepted into a
    /// turn. Transition errors are expected when the input already left
    /// `sending` (e.g. an earlier mark) and are ignored.
    ///
    /// This and the other input transitions do nothing for a run without an
    /// input.
    async fn mark_context_input_delivered(&self, context: &ConversationRunContext) {
        let Some(run_input_id) = &context.run_input_id else {
            return;
        };
        match context
            .chat
            .mark_input_delivered(&context.conversation_id, run_input_id)
            .await
        {
            Ok(input) if context.is_steer_input => {
                context
                    .queue_event(
                        ConversationEventKind::SteerDelivered,
                        json!({"runInputID": input.run_input_id}),
                    )
                    .await;
            }
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(
                    run_id = %context.run_id.0,
                    error = %error,
                    "run input was not in sending when marked delivered"
                );
            }
        }
    }

    async fn mark_context_input_failed(&self, context: &ConversationRunContext, reason: &str) {
        let Some(run_input_id) = &context.run_input_id else {
            return;
        };
        if let Err(error) = context
            .chat
            .mark_input_failed(&context.conversation_id, run_input_id, reason.to_string())
            .await
        {
            tracing::debug!(
                run_id = %context.run_id.0,
                error = %error,
                "run input was not in a failable state"
            );
        }
    }

    /// Per the acceptance contract: a definitive rejection before acceptance
    /// fails the input; a transport loss after send without a durable negative
    /// ack leaves delivery unknown for the user to resolve.
    async fn mark_context_input_undelivered(
        &self,
        context: &ConversationRunContext,
        error: &HarnessError,
    ) {
        let Some(run_input_id) = &context.run_input_id else {
            return;
        };
        if delivery_unknown(error.kind) {
            if let Err(mark_error) = context
                .chat
                .mark_input_delivery_unknown(&context.conversation_id, run_input_id)
                .await
            {
                tracing::debug!(
                    run_id = %context.run_id.0,
                    error = %mark_error,
                    "run input was not in sending when delivery became unknown"
                );
            }
        } else {
            self.mark_context_input_failed(context, harness_error_kind(error.kind))
                .await;
        }
    }

    /// Prompts, or stops early if the run is cancelled.
    ///
    /// Cancelling drops the prompt future rather than waiting on it. The
    /// session stays busy until the agent resolves the cancelled turn, and the
    /// run that has to respect that is the next one — see
    /// [`Self::await_session_availability`], which is where the invariant is
    /// enforced. Waiting here instead would hold the request that cancelled
    /// this run open for as long as the agent takes to wind down.
    ///
    /// Also returns whether the prompt went out: a stop while waiting out a
    /// busy agent sent nothing.
    async fn prompt_or_cancel(
        &self,
        adapter: &Arc<dyn HarnessAdapter>,
        agent_id: &AgentId,
        session_id: &HarnessSessionId,
        prompt: HarnessPrompt,
        cancel: &CancellationToken,
    ) -> (Result<HarnessStopReason, HarnessError>, bool) {
        let started = Instant::now();
        let mut backoff = CONVERSATION_REMOTE_BUSY_BACKOFF;
        let mut cleared_refresh_lock = false;
        loop {
            let result = tokio::select! {
                // The prompt goes out before a stop is heard, so a stop sent
                // the moment the run shows as running finds it on its way.
                biased;
                result = adapter.prompt(session_id.clone(), prompt.clone()) => result,
                _ = cancel.cancelled() => {
                    self.cancel_permissions(agent_id, &session_id.0);
                    let _ = adapter.cancel(session_id.clone()).await;
                    return (Ok(HarnessStopReason::Cancelled), true);
                }
            };
            // An agent that rejects a prompt while it is still busy, rather
            // than queueing it, never took the message: send it again once
            // the agent settles. Local state can't see that window, because
            // the agent answered the previous prompt before it was idle.
            match result {
                Err(error)
                    if error.kind == HarnessErrorKind::RemoteSessionBusy
                        && started.elapsed() < CONVERSATION_REMOTE_BUSY_RETRY =>
                {
                    tracing::debug!(
                        session_id = %session_id.0,
                        error = %error.message,
                        "agent reported its session busy; retrying the prompt"
                    );
                    tokio::select! {
                        _ = cancel.cancelled() => return (Ok(HarnessStopReason::Cancelled), false),
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(CONVERSATION_REMOTE_BUSY_BACKOFF_MAX);
                }
                // A refresh blocked by a lock whose holder exited never reached
                // the model. Clear the lock and send the turn once more; if it
                // now needs sign-in, that error takes the ordinary sign-in path.
                Err(error)
                    if error.kind == HarnessErrorKind::AuthRefreshBusy
                        && !cleared_refresh_lock
                        && self.registry.clear_abandoned_refresh_lock(agent_id) =>
                {
                    cleared_refresh_lock = true;
                    tracing::info!(
                        session_id = %session_id.0,
                        "cleared an abandoned OAuth refresh lock; resending the prompt"
                    );
                }
                result => return (result, true),
            }
        }
    }

    /// [`resolve_conversation_session`], tried once more after clearing a
    /// refresh lock whose holder exited: `session/new` refreshes the token as
    /// readily as a prompt does.
    #[allow(clippy::too_many_arguments)]
    async fn resolve_session(
        &self,
        adapter: &Arc<dyn HarnessAdapter>,
        agent_id: &AgentId,
        prior_session: Option<&AgentSessionID>,
        prior_mode_id: Option<&str>,
        project_root: &Path,
        config: &SessionConfigRequest,
        resolution: &mut ResolutionGuard,
    ) -> Result<(HarnessSessionId, SessionOrigin), HarnessError> {
        let first = resolve_conversation_session(
            adapter,
            prior_session,
            prior_mode_id,
            project_root,
            config,
            Some(&mut *resolution),
        )
        .await;
        match first {
            Err(error)
                if error.kind == HarnessErrorKind::AuthRefreshBusy
                    && self.registry.clear_abandoned_refresh_lock(agent_id) =>
            {
                tracing::info!(
                    agent_id = %agent_id.0,
                    "cleared an abandoned OAuth refresh lock; opening the session again"
                );
                resolve_conversation_session(
                    adapter,
                    prior_session,
                    prior_mode_id,
                    project_root,
                    config,
                    Some(resolution),
                )
                .await
            }
            result => result,
        }
    }

    /// Waits until this run's session is not already prompting.
    ///
    /// `session/cancel` is a notification, not an acknowledgement: the adapter
    /// deliberately holds a session in `Prompting` until the agent resolves the
    /// prompt, so a follow-up cannot overlap work still running in the remote
    /// session. A message sent mid-run cancels the active run and launches its
    /// follow-up milliseconds later, well inside that window, and the follow-up
    /// was rejected with `sessionBusy` — the reader saw their message marked
    /// Failed while the turn it interrupted was still winding down.
    ///
    /// Waiting here rather than at the cancel keeps the cancelling request
    /// fast, survives an agent that takes a long time to stop, and closes the
    /// window in which the old turn's late updates would be attributed to this
    /// run, since this returns before the session key is claimed.
    ///
    /// Bounded and best effort. On expiry the prompt goes out anyway and fails
    /// the way it did before if the session really is still busy; a session
    /// held that long is stuck, not slow.
    async fn await_session_availability(
        &self,
        adapter: &Arc<dyn HarnessAdapter>,
        session_id: &HarnessSessionId,
        cancel: &CancellationToken,
    ) {
        let busy = |adapter: &Arc<dyn HarnessAdapter>| {
            adapter.sessions().iter().any(|session| {
                session.session_id == *session_id && session.state == HarnessSessionState::Prompting
            })
        };
        if !busy(adapter) {
            return;
        }
        let started = Instant::now();
        let waited = tokio::select! {
            _ = cancel.cancelled() => return,
            waited = async {
                while busy(adapter) {
                    if started.elapsed() >= CONVERSATION_SESSION_BUSY_WAIT {
                        return None;
                    }
                    tokio::time::sleep(CONVERSATION_SESSION_BUSY_POLL).await;
                }
                Some(started.elapsed())
            } => waited,
        };
        match waited {
            Some(waited) => tracing::debug!(
                session_id = %session_id.0,
                waited_ms = waited.as_millis() as u64,
                "waited for the session to finish its previous prompt"
            ),
            None => tracing::warn!(
                session_id = %session_id.0,
                "session still prompting after the busy wait elapsed; prompting anyway"
            ),
        }
    }

    /// One-shot recovery for a Continued session that vanished between resolution and
    /// prompt (the adapter restarted in that window). Re-resolves onto a restored or
    /// fresh session, re-keys the run's context, re-persists the session id, and
    /// prompts again. A second failure propagates normally.
    #[allow(clippy::too_many_arguments)]
    async fn retry_conversation_prompt(
        self: &Arc<Self>,
        context: &ConversationRunContext,
        agent_id: &AgentId,
        adapter: &Arc<dyn HarnessAdapter>,
        project_root: &Path,
        inputs: &ConversationPromptInputs,
        stale_session: HarnessSessionId,
        stale_key: (String, String),
        cancel: &CancellationToken,
    ) -> (
        Result<HarnessStopReason, HarnessError>,
        HarnessSessionId,
        (String, String),
    ) {
        let prior = AgentSessionID(stale_session.0.clone());
        // Fold in what the failed prompt produced, then drop the stale key with
        // no tombstone: the adapter no longer has that session.
        self.fence_attribution(
            agent_id,
            AttributionOp::Forget {
                key: stale_key.clone(),
                run_id: context.run_id.clone(),
            },
        )
        .await;
        // Nothing can end that session's tasks now (rule 7).
        record_lost(
            self.attribution.take_lost_tasks(|agent, session, _| {
                (agent, session) == (&stale_key.0, &stale_key.1)
            }),
        )
        .await;
        let mut session_config = inputs.session_config();
        let mcp_grant = tokio::select! {
            _ = cancel.cancelled() => {
                return (Ok(HarnessStopReason::Cancelled), stale_session, stale_key);
            }
            grant = self.grant_mcp(adapter, agent_id, context, Some(&prior), project_root, &mut session_config) => grant,
        };
        let mut resolution =
            ResolutionGuard::new(self.forwarders(), &self.attribution, agent_id, context);
        let resolved = tokio::select! {
            _ = cancel.cancelled() => {
                return (Ok(HarnessStopReason::Cancelled), stale_session, stale_key);
            }
            // The adapter still tracks the stale session's mode for the restore.
            result = resolve_conversation_session(adapter, Some(&prior), None, project_root, &session_config, Some(&mut resolution)) => result,
        };
        let (session_id, origin) = match resolved {
            Ok(resolved) => resolved,
            Err(error) => return (Err(error), stale_session, stale_key),
        };
        bind_mcp_grant(mcp_grant, &session_id, origin);
        let session_key = (agent_id.0.clone(), session_id.0.clone());
        self.claim_session(adapter, agent_id, session_key.clone(), context)
            .await;
        resolution.disarm();
        {
            let mut runs = self.active_runs().await;
            if let Some(run) = runs.get_mut(&context.run_id) {
                run.session_id = Some(session_id.clone());
            }
        }
        let (prompt, seed) = self
            .seeded_prompt(context, inputs, origin, &session_id)
            .await;
        if let Err(error) = context
            .chat
            .update_run_session(
                &context.conversation_id,
                &context.run_id,
                &AgentSessionID(session_id.0.clone()),
                origin,
            )
            .await
        {
            return (
                Err(HarnessError::new(HarnessErrorKind::Internal, error.message)),
                session_id,
                session_key,
            );
        }
        let seeded = record_history_seed(context, seed).await;
        record_confirmed_session(adapter, context, &session_id, &self.recorded_commands).await;
        let (result, sent) = self
            .prompt_or_cancel(adapter, agent_id, &session_id, prompt, cancel)
            .await;
        if seeded && !prompt_reached_agent(&result, sent) {
            unseed_history(context).await;
        }
        (result, session_id, session_key)
    }

    /// The prompt for a run about to bind `session_id`. When that session
    /// has yet to see the conversation's earlier turns (it opened after an
    /// agent switch, or its earlier session could not be restored), the
    /// prompt carries them and the seed records it once the run binds.
    async fn seeded_prompt(
        &self,
        context: &ConversationRunContext,
        inputs: &ConversationPromptInputs,
        origin: SessionOrigin,
        session_id: &HarnessSessionId,
    ) -> (HarnessPrompt, Option<HistorySeed>) {
        let mut seed = None;
        let mut earlier_turns = None;
        let mut preamble_origin = origin;
        // A prompt sent as typed has no room for them; the session's first
        // wrapped prompt carries them instead.
        if let (false, Some(message_id)) = (sends_unwrapped(inputs), &inputs.message_id) {
            let chat = &context.chat;
            let conversation_id = &context.conversation_id;
            match chat
                .session_needs_history(
                    conversation_id,
                    &context.run_id,
                    &AgentSessionID(session_id.0.clone()),
                )
                .await
            {
                Ok(true) => {
                    // An unseeded session has had no wrapped prompt yet, so
                    // it gets the full preamble, even after a command.
                    preamble_origin = SessionOrigin::New;
                    match chat.history_replay(conversation_id, message_id).await {
                        Ok(Some(turns)) => {
                            let agents = earlier_agents(context).await;
                            earlier_turns =
                                Some(earlier_turns_block(conversation_id, &agents, &turns.text));
                            seed = Some(HistorySeed {
                                replay: Some(turns.replay),
                            });
                        }
                        Ok(None) => seed = Some(HistorySeed { replay: None }),
                        Err(error) => tracing::warn!(
                            run_id = %context.run_id.0,
                            error = %error,
                            "failed to read earlier turns; the next prompt tries again"
                        ),
                    }
                }
                Ok(false) => {}
                Err(error) => tracing::warn!(
                    run_id = %context.run_id.0,
                    error = %error,
                    "failed to read whether the session has earlier turns"
                ),
            }
        }
        let prompt = compile_conversation_prompt(
            inputs,
            preamble_origin,
            &context.conversation_id,
            earlier_turns.as_deref(),
        );
        (prompt, seed)
    }

    /// Applies the terminal durable transitions for the named run outcome,
    /// then closes recorder/runtime ownership exactly once.
    async fn complete_conversation_run(
        &self,
        context: &ConversationRunContext,
        completion: ConversationRunCompletion,
        finished: &CancellationToken,
    ) {
        match completion {
            ConversationRunCompletion::CancelledBeforeBinding => {
                self.mark_context_input_failed(context, "runCanceled").await;
                self.finalize_conversation_run(
                    context,
                    ConversationRunStatus::Canceled,
                    Some(AgentStopReason::Cancelled),
                    json!({}),
                )
                .await;
            }
            ConversationRunCompletion::HarnessFailureBeforeBinding(error) => {
                self.mark_context_input_failed(context, harness_error_kind(error.kind))
                    .await;
                self.finalize_conversation_error(context, &error).await;
            }
            ConversationRunCompletion::StorageFailureAfterBinding { error, session_key } => {
                self.mark_context_input_failed(context, error.kind).await;
                // Nothing was prompted, so there is no turn for a tombstone to
                // describe.
                self.fence_attribution(
                    &AgentId(session_key.0.clone()),
                    AttributionOp::Forget {
                        key: session_key,
                        run_id: context.run_id.clone(),
                    },
                )
                .await;
                self.finalize_conversation_run(
                    context,
                    ConversationRunStatus::Failed,
                    None,
                    json!({"kind": error.kind, "message": error.message}),
                )
                .await;
            }
            ConversationRunCompletion::PromptFinished(result) => match result {
                Ok(HarnessStopReason::Cancelled) => {
                    self.mark_context_input_delivered(context).await;
                    self.finalize_conversation_run(
                        context,
                        ConversationRunStatus::Canceled,
                        Some(AgentStopReason::Cancelled),
                        json!({}),
                    )
                    .await;
                }
                Ok(reason) => {
                    self.mark_context_input_delivered(context).await;
                    self.finalize_conversation_run(
                        context,
                        ConversationRunStatus::Completed,
                        Some(map_stop_reason(reason)),
                        json!({}),
                    )
                    .await;
                }
                Err(error) => {
                    if error.terminal_signal.is_some() {
                        self.mark_context_input_delivered(context).await;
                    } else {
                        self.mark_context_input_undelivered(context, &error).await;
                    }
                    self.finalize_conversation_error(context, &error).await;
                }
            },
        }
        self.remove_conversation_run(context, finished).await;
    }

    async fn finalize_conversation_error(
        &self,
        context: &ConversationRunContext,
        error: &HarnessError,
    ) {
        if let Some(mut signal) = error.terminal_signal.clone() {
            if let Some(active) = self.active_runs().await.get(&context.run_id) {
                signal.session_id = format!("{}:{}", active.agent_id.0, signal.session_id);
            }
            let _ = context.recorder.queue_signal(*signal).await;
        }
        // Runtime and auth failures persist as `failed`; clients distinguish
        // them by the typed `error_json` kind (e.g. `authRequired`).
        //
        self.finalize_conversation_run(
            context,
            ConversationRunStatus::Failed,
            None,
            json!({
                "kind": agent_error_data(error)["kind"],
                "message": sanitize_transport_message(&error.message)
            }),
        )
        .await;
    }

    async fn finalize_conversation_run(
        &self,
        context: &ConversationRunContext,
        status: ConversationRunStatus,
        stop_reason: Option<AgentStopReason>,
        error: Value,
    ) {
        let (status, stop_reason, error) = match flush_conversation_recorder(context).await {
            Ok(()) => (status, stop_reason, error),
            Err(recorder_error) => (
                ConversationRunStatus::Failed,
                None,
                json!({
                    "kind": recorder_error.kind,
                    "message": sanitize_transport_message(&recorder_error.message),
                }),
            ),
        };
        match context
            .chat
            .finalize_run(
                &context.conversation_id,
                &context.run_id,
                status.clone(),
                stop_reason,
                error.clone(),
            )
            .await
        {
            Ok(_) => {
                match context
                    .chat
                    .record_run_events(
                        &context.conversation_id,
                        &context.run_id,
                        context.project_id.clone(),
                        vec![(
                            ConversationEventKind::RunStatus,
                            json!({
                                "status": status,
                                "origin": context.origin,
                                "stopReason": stop_reason,
                                "error": error,
                            }),
                        )],
                    )
                    .await
                {
                    Ok(deltas) => {
                        for params in deltas {
                            send_conversation_notification(&self.outbound, params);
                        }
                    }
                    Err(event_error) => tracing::warn!(
                        run_id = %context.run_id.0,
                        error = %event_error,
                        "failed to persist terminal Chat run event"
                    ),
                }
            }
            Err(store_error) => tracing::warn!(
                run_id = %context.run_id.0,
                error = %store_error,
                "failed to finalize Chat run"
            ),
        }
    }

    /// Stops attributing the session's updates to this run, after the
    /// forwarder has handled every update already queued for the agent. That
    /// ordering is what makes the cut safe: everything the run produced is
    /// folded in, and whatever arrives next is reported against the tombstone
    /// until another run claims the session.
    async fn release_conversation_session(
        &self,
        agent_id: &AgentId,
        session_key: SessionKey,
        run_id: &ConversationRunID,
        ended: RunEnd,
    ) {
        self.fence_attribution(
            agent_id,
            AttributionOp::Release {
                key: session_key,
                run_id: run_id.clone(),
                ended,
            },
        )
        .await;
    }

    async fn remove_conversation_run(
        &self,
        context: &ConversationRunContext,
        finished: &CancellationToken,
    ) {
        let active = self.active_runs().await.remove(&context.run_id);
        if let Some(active) = active {
            let _ = active.recorder.close(&context.recorder).await;
        }
        finished.cancel();
        self.after_run(context);
        // Rule 6: a follow-up this run held back opens now.
        for request in self
            .attribution
            .unblock_follow_ups(&context.conversation_id)
        {
            self.request_follow_up(request);
        }
    }

    /// Asks the agent to stop a background task `run` started, in the session
    /// it runs in. A task core does not hold live, or whose adapter restarted
    /// since, is not stopped and nothing is sent: its end, or its loss, is
    /// already on the transcript or on its way.
    pub(super) async fn stop_background_task(
        &self,
        workspace_id: &WorkspaceID,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
        async_task_id: &str,
    ) -> Result<ConversationStopBackgroundTaskResult, HarnessError> {
        let not_stopped = Ok(ConversationStopBackgroundTaskResult { stopped: false });
        let Some((agent, session, generation)) = self.attribution.live_task_session(
            workspace_id,
            conversation_id,
            run_id,
            async_task_id,
        ) else {
            return not_stopped;
        };
        let Ok(adapter) = self.registry.running_adapter(&AgentId(agent)).await else {
            return not_stopped;
        };
        if adapter.status().borrow().generation != generation {
            return not_stopped;
        }
        let session_id = HarnessSessionId(session);
        let stopped = tokio::time::timeout(
            STOP_BACKGROUND_TASK_WAIT,
            adapter.stop_background_task(&session_id, async_task_id),
        )
        .await
        .map_err(|_| {
            HarnessError::new(
                HarnessErrorKind::Internal,
                "the agent did not answer the request to stop its background task",
            )
        })??;
        Ok(ConversationStopBackgroundTaskResult { stopped })
    }

    /// Switches a conversation's session to `mode_id` now, idle or mid-turn.
    /// A session no running agent holds is deferred untouched: the next
    /// message opens it with the mode it requests.
    pub(super) async fn set_conversation_mode(
        &self,
        chat: &ChatService,
        params: &ConversationSetModeParams,
    ) -> Result<ConversationSetModeResult, CoreError> {
        let session = chat
            .active_agent_session(&params.conversation_id, params.agent_session_id.as_ref())
            .await?;
        let session = match (session, &params.agent_session_id) {
            (Some(session), _) => session,
            // The client named no session and none is active: the pick
            // rides the next prompt.
            (None, None) => {
                return Ok(ConversationSetModeResult {
                    outcome: ConversationSetModeOutcome::Deferred,
                    current_mode_id: None,
                });
            }
            (None, Some(_)) => {
                return Err(CoreError::new(
                    "staleAgentSession",
                    "This conversation's agent session has changed. Pick the mode again.",
                )
                .recoverable(true));
            }
        };
        let deferred = ConversationSetModeResult {
            outcome: ConversationSetModeOutcome::Deferred,
            current_mode_id: session.current_mode_id.clone(),
        };
        let Some(external) = session.external_session_id else {
            return Ok(deferred);
        };
        let Ok(adapter) = self
            .registry
            .running_adapter(&AgentId(session.engine_id))
            .await
        else {
            return Ok(deferred);
        };
        let session_id = HarnessSessionId(external);
        let Some(live) = adapter
            .sessions()
            .into_iter()
            .find(|live| live.session_id == session_id && live.state != HarnessSessionState::Dead)
        else {
            return Ok(deferred);
        };
        if live.current_mode_id.as_deref() != Some(params.mode_id.as_str()) {
            adapter
                .apply_mode(session_id, params.mode_id.clone())
                .await
                .map_err(core_error_from_harness)?;
        }
        chat.record_session_mode(session.id, &params.mode_id)
            .await?;
        Ok(ConversationSetModeResult {
            outcome: ConversationSetModeOutcome::Applied,
            current_mode_id: Some(params.mode_id.clone()),
        })
    }

    pub(super) async fn cancel_conversation_run(
        &self,
        chat: &ChatService,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
    ) -> Result<ConversationCancelRunResult, CoreError> {
        // An autonomous run is stored before it is in `active_runs`, and a
        // stop can name it in between. Its opener holds the launch lock until
        // the run is registered, so a stop for an open run that isn't active
        // yet waits for that.
        let opening = !self.active_runs().await.contains_key(run_id)
            && matches!(
                chat.conversation_run(conversation_id, run_id).await?.status,
                ConversationRunStatus::Queued | ConversationRunStatus::Running
            );
        let _launch = if opening {
            Some(self.lock_launch(conversation_id).await)
        } else {
            None
        };
        self.cancel_run_under_launch(chat, conversation_id, run_id)
            .await
    }

    /// Cancels a run for a caller that already holds the conversation's
    /// launch lock.
    async fn cancel_run_under_launch(
        &self,
        chat: &ChatService,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
    ) -> Result<ConversationCancelRunResult, CoreError> {
        let active = {
            let runs = self.active_runs().await;
            runs.get(run_id).map(|run| {
                (
                    run.context.conversation_id.clone(),
                    run.agent_id.clone(),
                    run.session_id.clone(),
                    run.cancel.clone(),
                    run.finished.clone(),
                    run.worker_abort.clone(),
                )
            })
        };
        if let Some((
            active_conversation_id,
            agent_id,
            session_id,
            cancel,
            finished,
            worker_abort,
        )) = active
        {
            if active_conversation_id != *conversation_id {
                return Err(CoreError::new(
                    "unknownConversationRunID",
                    format!(
                        "run {} does not belong to conversation {}",
                        run_id.0, conversation_id.0
                    ),
                ));
            }
            if let Some(session_id) = session_id.as_ref() {
                self.cancel_permissions(&agent_id, &session_id.0);
            }
            cancel.cancel();
            wait_for_conversation_worker(&finished, worker_abort.as_ref()).await;
            self.finalize_orphaned_conversation_run(
                run_id,
                ConversationRunStatus::Canceled,
                Some(AgentStopReason::Cancelled),
                json!({}),
            )
            .await;
        }
        let run = chat.conversation_run(conversation_id, run_id).await?;
        Ok(ConversationCancelRunResult { run })
    }

    pub(super) async fn cancel_project_runs(
        &self,
        workspace_id: &WorkspaceID,
        project_id: &ProjectID,
    ) {
        let runs = {
            let active = self.active_runs().await;
            active
                .values()
                .filter(|run| {
                    run.context.workspace_id == *workspace_id
                        && run.context.project_id.as_ref() == Some(project_id)
                })
                .map(|run| {
                    (
                        run.context.chat.clone(),
                        run.context.conversation_id.clone(),
                        run.context.run_id.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        for (chat, conversation_id, run_id) in runs {
            if let Err(error) = self
                .cancel_conversation_run(&chat, &conversation_id, &run_id)
                .await
            {
                tracing::warn!(
                    error = %error,
                    workspace_id = %workspace_id.0,
                    project_id = %project_id.0,
                    run_id = %run_id.0,
                    "failed to cancel a run for removed Project"
                );
            }
        }
        self.attribution.forget_project(workspace_id, project_id);
    }

    pub(super) async fn delete_conversation(
        &self,
        chat: &ChatService,
        conversation_id: &ConversationID,
    ) -> Result<ConversationDeleteResult, CoreError> {
        // No run opens while the conversation goes.
        let _launch = self.lock_launch(conversation_id).await;
        if let Some(mcp) = self.mcp.get() {
            mcp.revoke_conversation(conversation_id);
        }
        let active_run_ids = {
            let runs = self.active_runs().await;
            runs.values()
                .filter(|run| run.context.conversation_id == *conversation_id)
                .map(|run| run.context.run_id.clone())
                .collect::<Vec<_>>()
        };
        for run_id in active_run_ids {
            self.cancel_run_under_launch(chat, conversation_id, &run_id)
                .await?;
        }

        let harness_sessions = chat.harness_sessions(conversation_id).await?;
        let mut harness_session_cleanup = Vec::with_capacity(harness_sessions.len());
        for (engine_id, agent_session_id) in harness_sessions {
            let agent_id = AgentId(engine_id.0.clone());
            let cleanup = match self.registry.get_or_start(&agent_id).await {
                Ok(adapter) => {
                    let supports_delete = adapter.status().borrow().capabilities.delete_session;
                    if !supports_delete {
                        HarnessSessionCleanup {
                            engine_id,
                            agent_session_id,
                            status: HarnessSessionCleanupStatus::Unsupported,
                            message: Some(
                                "The harness does not advertise ACP session deletion.".to_string(),
                            ),
                        }
                    } else {
                        let session_id = HarnessSessionId(agent_session_id.0.clone());
                        match adapter.delete_session(session_id).await {
                            Ok(()) => HarnessSessionCleanup {
                                engine_id,
                                agent_session_id,
                                status: HarnessSessionCleanupStatus::Deleted,
                                message: None,
                            },
                            Err(error) => HarnessSessionCleanup {
                                engine_id,
                                agent_session_id,
                                status: HarnessSessionCleanupStatus::Failed,
                                message: Some(sanitize_transport_message(&error.message)),
                            },
                        }
                    }
                }
                Err(error) => HarnessSessionCleanup {
                    engine_id,
                    agent_session_id,
                    status: HarnessSessionCleanupStatus::Failed,
                    message: Some(sanitize_transport_message(&error.message)),
                },
            };
            harness_session_cleanup.push(cleanup);
        }

        chat.delete_conversation(conversation_id).await?;
        self.attribution.forget_conversation(conversation_id);
        self.forget_prompted_runs(conversation_id);
        Ok(ConversationDeleteResult {
            conversation_id: conversation_id.clone(),
            harness_session_cleanup,
        })
    }

    /// Ottam is quitting. The app's shutdown preparation stops adapters
    /// before core tears down, so runs can end on that before `closing` is set.
    pub(super) fn quitting(&self) -> bool {
        self.closing.load(Ordering::Acquire) || self.registry.is_shutting_down()
    }

    pub(super) async fn shutdown_runs(&self) {
        self.closing.store(true, Ordering::Release);
        let active_runs = {
            let runs = self.active_runs().await;
            runs.values()
                .map(|run| {
                    (
                        run.cancel.clone(),
                        run.finished.clone(),
                        run.worker_abort.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        for (cancel, _, _) in &active_runs {
            cancel.cancel();
        }
        for (_, finished, worker_abort) in active_runs {
            wait_for_conversation_worker(&finished, worker_abort.as_ref()).await;
        }
        let orphaned_run_ids = {
            let runs = self.active_runs().await;
            runs.keys().cloned().collect::<Vec<_>>()
        };
        for run_id in orphaned_run_ids {
            self.finalize_orphaned_conversation_run(
                &run_id,
                ConversationRunStatus::Canceled,
                Some(AgentStopReason::Cancelled),
                json!({"kind": "coreShutdown", "message": "Ottam core shut down during the run"}),
            )
            .await;
        }
        self.finish_run_ends().await;
        self.shutdown_workers().await;
    }

    async fn finalize_orphaned_conversation_run(
        &self,
        run_id: &ConversationRunID,
        status: ConversationRunStatus,
        stop_reason: Option<AgentStopReason>,
        error: Value,
    ) {
        let (context, agent_id, session_key) = {
            let runs = self.active_runs().await;
            let Some(run) = runs.get(run_id) else { return };
            (
                run.context.clone(),
                run.agent_id.clone(),
                run.session_id
                    .as_ref()
                    .map(|session_id| (run.agent_id.0.clone(), session_id.0.clone())),
            )
        };
        // The aborted worker never reached its own fence, so the output it did
        // produce is still sitting in the forwarder. Release folds it in, before
        // the terminal status goes out: an interrupted run keeps its partial
        // answer, and the run that takes the session next cannot have its
        // updates recorded against this one.
        match session_key {
            Some(session_key) => {
                let ended = match status {
                    ConversationRunStatus::Canceled => RunEnd::Cancelled,
                    ConversationRunStatus::Completed => RunEnd::Completed,
                    _ => RunEnd::Failed,
                };
                self.release_conversation_session(&agent_id, session_key, run_id, ended)
                    .await;
            }
            None => self.flush_forwarder(&agent_id).await,
        }
        // The aborted worker never resolved delivery; a still-sending input
        // fails with the run (a delivered input is untouched).
        self.mark_context_input_failed(&context, "runCanceled")
            .await;
        self.finalize_conversation_run(&context, status, stop_reason, error)
            .await;
        let finished = {
            let runs = self.active_runs().await;
            runs.get(run_id).map(|run| run.finished.clone())
        };
        if let Some(finished) = finished {
            self.remove_conversation_run(&context, &finished).await;
        }
    }
}

async fn wait_for_conversation_worker(
    finished: &CancellationToken,
    worker_abort: Option<&tokio::task::AbortHandle>,
) {
    if tokio::time::timeout(CONVERSATION_CANCEL_GRACE_PERIOD, finished.cancelled())
        .await
        .is_ok()
    {
        return;
    }
    if let Some(worker_abort) = worker_abort {
        worker_abort.abort();
        let _ = tokio::time::timeout(CONVERSATION_CANCEL_GRACE_PERIOD, finished.cancelled()).await;
    }
}

/// Per-run inputs for the conversation prompt, carried unrendered because the prompt
/// variant depends on how the harness session resolves.
#[derive(Debug, Clone)]
struct ConversationPromptInputs {
    message: String,
    /// The Message the run delivers. A replay of earlier turns stops before it.
    message_id: Option<ConversationMessageID>,
    /// Paths, not bytes — the ACP adapter loads and encodes them. Retry clones
    /// this struct, and megabytes of base64 have no business riding along.
    images: Vec<HarnessPromptImage>,
    model_id: Option<String>,
    mode_id: Option<String>,
    thought_level_id: Option<String>,
}

impl ConversationPromptInputs {
    /// Fresh-session selections. Restoration also reapplies the explicit mode;
    /// model and effort remain owned by the restored harness session.
    fn session_config(&self) -> SessionConfigRequest {
        SessionConfigRequest {
            model_id: self.model_id.clone(),
            mode_id: self.mode_id.clone(),
            thought_level_id: self.thought_level_id.clone(),
            mcp_servers: Vec::new(),
        }
    }
}

/// What resolving a run's session leaves in attribution until the run claims
/// it. A restored session is marked `Replaying` while `session/load` runs,
/// so the history it replays is dropped rather than recorded; a guard dropped
/// before the claim abandons that mark. A new session holds an opening mark,
/// so what the agent announces before the claim is not counted as lost.
pub(super) struct ResolutionGuard {
    forwarders: Arc<AgentUpdateForwarders>,
    attribution: SessionAttribution,
    agent_id: AgentId,
    context: ConversationRunContext,
    marked: Option<SessionKey>,
    opening: Option<OpeningSession>,
}

impl ResolutionGuard {
    fn new(
        forwarders: Arc<AgentUpdateForwarders>,
        attribution: &SessionAttribution,
        agent_id: &AgentId,
        context: &ConversationRunContext,
    ) -> Self {
        Self {
            forwarders,
            attribution: attribution.clone(),
            agent_id: agent_id.clone(),
            context: context.clone(),
            marked: None,
            opening: None,
        }
    }

    fn open_session(&mut self) {
        if self.opening.is_none() {
            self.opening = Some(self.attribution.opening(&self.agent_id));
        }
    }

    async fn mark_replay(&mut self, session_id: &HarnessSessionId, generation: u64) {
        let key = (self.agent_id.0.clone(), session_id.0.clone());
        // Recorded before the fence is awaited, so a cancel mid-fence still
        // abandons a mark the forwarder applies later.
        self.marked = Some(key.clone());
        self.forwarders
            .fence(
                &self.agent_id,
                Some(AttributionOp::MarkReplay {
                    key,
                    context: self.context.clone(),
                    generation,
                }),
            )
            .await;
    }

    /// The prior session is gone, even if `session/load` restores its
    /// history: nothing can end the tasks it left running (rule 7). Their
    /// last frames may still be queued, so those land first.
    async fn lose_session_tasks(&self, session_id: &HarnessSessionId) {
        self.forwarders.flush(&self.agent_id).await;
        let agent = &self.agent_id.0;
        record_lost(self.attribution.take_lost_tasks(|task_agent, session, _| {
            task_agent == agent && session == session_id.0
        }))
        .await;
    }

    fn abandon_replay(&mut self) {
        if let Some(key) = self.marked.take() {
            self.forwarders.post(
                &self.agent_id,
                AttributionOp::AbandonReplay {
                    key,
                    run_id: self.context.run_id.clone(),
                },
            );
        }
    }

    /// The run claimed the session, which replaced both marks.
    fn disarm(&mut self) {
        self.marked = None;
        self.opening = None;
    }
}

impl Drop for ResolutionGuard {
    fn drop(&mut self) {
        self.abandon_replay();
    }
}

/// Marks a native steer in flight on the run's session. If the agent has
/// already answered the run's prompt, it answers the steer by starting a
/// detached turn, and that turn's output can arrive before the steer's answer
/// does; while the mark stands, output after the prompt's answer is dropped.
/// Dropping the guard clears the mark.
struct SteerGuard {
    forwarders: Arc<AgentUpdateForwarders>,
    agent_id: AgentId,
    key: SessionKey,
    run_id: ConversationRunID,
}

impl SteerGuard {
    async fn begin(
        forwarders: Arc<AgentUpdateForwarders>,
        agent_id: &AgentId,
        session_id: &HarnessSessionId,
        run_id: &ConversationRunID,
    ) -> Self {
        let guard = Self {
            forwarders,
            agent_id: agent_id.clone(),
            key: (agent_id.0.clone(), session_id.0.clone()),
            run_id: run_id.clone(),
        };
        // Acknowledged before the steer goes out, so the mark is in place
        // before anything the agent sends in answer.
        guard
            .forwarders
            .fence(
                &guard.agent_id,
                Some(AttributionOp::SteerPending {
                    key: guard.key.clone(),
                    run_id: guard.run_id.clone(),
                }),
            )
            .await;
        guard
    }

    /// The agent answered with a detached turn, which core cancels: whatever
    /// the session sends until the run lets go is that turn's.
    fn detached(&self) {
        self.forwarders.post(
            &self.agent_id,
            AttributionOp::MarkDetached {
                key: self.key.clone(),
                run_id: self.run_id.clone(),
            },
        );
    }
}

impl Drop for SteerGuard {
    fn drop(&mut self) {
        self.forwarders.post(
            &self.agent_id,
            AttributionOp::SteerSettled {
                key: self.key.clone(),
                run_id: self.run_id.clone(),
            },
        );
    }
}

/// Pick the harness session for a conversation run. Preference order: prompt the prior
/// session while the adapter still holds it live (Continued), restore it via ACP
/// `session/load` when supported (Restored), else start fresh (New). A failed load
/// degrades to a fresh session; only a failed `new_session` is an error.
pub(super) async fn resolve_conversation_session(
    adapter: &Arc<dyn HarnessAdapter>,
    prior_session: Option<&AgentSessionID>,
    prior_mode_id: Option<&str>,
    project_root: &Path,
    config: &SessionConfigRequest,
    mut resolution: Option<&mut ResolutionGuard>,
) -> Result<(HarnessSessionId, SessionOrigin), HarnessError> {
    // A live or restored session takes the model, effort and mode the run
    // picked wherever they differ from what the agent last confirmed (or the
    // load reported), before the prompt. Restored adapters may reset
    // permissions to their startup default, so restores reapply the mode too.
    let Some(prior) = prior_session else {
        let session_id = open_new_session(adapter, project_root, config, &mut resolution).await?;
        return Ok((session_id, SessionOrigin::New));
    };
    let session_id = HarnessSessionId(prior.0.clone());
    let known = adapter
        .sessions()
        .into_iter()
        .find(|session| session.session_id == session_id);
    let live = known
        .as_ref()
        .filter(|session| session.state != HarnessSessionState::Dead);
    if live.is_some() {
        apply_changed_selection(adapter, &session_id, config, false).await?;
        return Ok((session_id, SessionOrigin::Continued));
    }
    // A run that requests no mode (a steer or interrupt follow-up) keeps the
    // one the agent last confirmed: tracked by the adapter across its own
    // restart, persisted across Core's. Never the agent's startup default.
    let config = &SessionConfigRequest {
        mode_id: config
            .mode_id
            .clone()
            .or_else(|| known.and_then(|session| session.current_mode_id))
            .or_else(|| prior_mode_id.map(str::to_owned)),
        ..config.clone()
    };
    if let Some(resolution) = resolution.as_deref() {
        resolution.lose_session_tasks(&session_id).await;
    }
    if !adapter.status().borrow().capabilities.load_session {
        let session_id = open_new_session(adapter, project_root, config, &mut resolution).await?;
        return Ok((session_id, SessionOrigin::New));
    }
    if let Some(resolution) = resolution.as_mut() {
        let generation = adapter.status().borrow().generation;
        resolution.mark_replay(&session_id, generation).await;
    }
    match adapter
        .load_session_with_config(
            session_id.clone(),
            project_root.to_path_buf(),
            config.mode_id.clone(),
            config.mcp_servers.clone(),
        )
        .await
    {
        Ok(()) => {
            apply_changed_selection(adapter, &session_id, config, true).await?;
            Ok((session_id, SessionOrigin::Restored))
        }
        Err(error) if error.kind == HarnessErrorKind::SelectionUnavailable => Err(error),
        Err(error) => {
            tracing::warn!(
                session_id = %session_id.0,
                error = %error,
                "failed to load prior conversation session; starting fresh"
            );
            if let Some(resolution) = resolution.as_mut() {
                resolution.abandon_replay();
            }
            let session_id =
                open_new_session(adapter, project_root, config, &mut resolution).await?;
            Ok((session_id, SessionOrigin::New))
        }
    }
}

/// Switch a held session to the run's model, effort and mode where they differ
/// from what the agent last confirmed, in `session/new`'s order. The level
/// rides along with a model switch, which can move it (omp applies each
/// model's own default), and the mode is compared after it for the same
/// reason. A refusal fails the run rather than prompting on the old model.
async fn apply_changed_selection(
    adapter: &Arc<dyn HarnessAdapter>,
    session_id: &HarnessSessionId,
    config: &SessionConfigRequest,
    restored: bool,
) -> Result<(), HarnessError> {
    let session = || {
        adapter
            .sessions()
            .into_iter()
            .find(|session| &session.session_id == session_id)
    };
    let Some(current) = session() else {
        return Ok(());
    };
    let model = config
        .model_id
        .clone()
        .filter(|requested| current.current_model_id.as_ref() != Some(requested));
    let level_changed = config
        .thought_level_id
        .as_ref()
        .is_some_and(|requested| current.current_thought_level_id.as_ref() != Some(requested));
    if model.is_some() || level_changed {
        let switch = ModelSwitch {
            model_id: model,
            thought_level_id: config.thought_level_id.clone(),
            restored,
        };
        adapter.apply_model(session_id.clone(), switch).await?;
    }
    let current_mode = session().and_then(|session| session.current_mode_id);
    if let Some(requested) = config
        .mode_id
        .as_ref()
        .filter(|requested| current_mode.as_ref() != Some(*requested))
    {
        adapter
            .apply_mode(session_id.clone(), requested.clone())
            .await?;
    }
    Ok(())
}

/// Publish the mode the agent confirmed while the run's session was resolved
/// (its acknowledgment at new, load, or a live switch), and the commands it
/// last announced — often while `session/new` was still in flight, before
/// any run could record them. Recording them as session metadata also
/// persists them on the bound AgentSession, so the app shows them and later
/// runs start from them.
///
/// Separate events, so a command list too large to record never takes the
/// mode with it. The list is recorded once per announcement, not every turn.
async fn record_confirmed_session(
    adapter: &Arc<dyn HarnessAdapter>,
    context: &ConversationRunContext,
    session_id: &HarnessSessionId,
    recorded_commands: &RecordedCommands,
) {
    let Some(session) = adapter
        .sessions()
        .into_iter()
        .find(|session| &session.session_id == session_id)
    else {
        return;
    };
    if let Some(mode_id) = session.current_mode_id {
        context
            .queue_event(
                ConversationEventKind::SessionMetadata,
                json!({"currentModeId": mode_id}),
            )
            .await;
    }
    let Some(commands) = session
        .announced_commands
        .and_then(|payload| normalized_session_metadata("available_commands_update", &payload))
        .and_then(|metadata| metadata.commands)
    else {
        return;
    };
    let fingerprint = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        json!(commands).to_string().hash(&mut hasher);
        hasher.finish()
    };
    // A restarted adapter can reissue a session id, and its new AgentSession
    // still needs the list: scope to the generation, dropping older ones.
    let agent = adapter.definition().agent_id.0.clone();
    let generation = adapter.status().borrow().generation;
    {
        let mut recorded = recorded_commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        recorded.retain(|(owner, seen, _), _| *owner != agent || *seen >= generation);
        if recorded.insert((agent, generation, session_id.clone()), fingerprint)
            == Some(fingerprint)
        {
            return;
        }
    }
    context
        .queue_event(
            ConversationEventKind::SessionMetadata,
            json!({"commands": commands}),
        )
        .await;
}

/// A session the run opened or restored keeps the token it was sent. One
/// that became live between the grant and its resolution was continued and
/// already holds a token, so the new one is given up.
fn bind_mcp_grant(grant: Option<McpGrant>, session_id: &HarnessSessionId, origin: SessionOrigin) {
    if let Some(grant) = grant {
        if origin != SessionOrigin::Continued {
            grant.bind(session_id.clone());
        }
    }
}

/// The command list each session last recorded, by fingerprint, keyed by
/// agent, adapter generation, and session.
type RecordedCommands = StdMutex<HashMap<(String, u64, HarnessSessionId), u64>>;

async fn open_new_session(
    adapter: &Arc<dyn HarnessAdapter>,
    project_root: &Path,
    config: &SessionConfigRequest,
    resolution: &mut Option<&mut ResolutionGuard>,
) -> Result<HarnessSessionId, HarnessError> {
    if let Some(resolution) = resolution.as_mut() {
        resolution.open_session();
    }
    adapter
        .new_session(project_root.to_path_buf(), config.clone())
        .await
}

// Output target is deliberately absent from the prompt inputs: conversations are
// open-ended (a brainstorm can end up a spec or a single task), so shaping the
// answer up-front was premature. The field remains part of the wire contract.
fn compile_conversation_prompt(
    inputs: &ConversationPromptInputs,
    origin: SessionOrigin,
    conversation_id: &ConversationID,
    earlier_turns: Option<&str>,
) -> HarnessPrompt {
    HarnessPrompt {
        text: compile_conversation_prompt_text(inputs, origin, conversation_id, earlier_turns),
        images: inputs.images.clone(),
    }
}

/// Whether the prompt goes out as typed, with no preamble to carry the
/// conversation's id or earlier turns.
fn sends_unwrapped(inputs: &ConversationPromptInputs) -> bool {
    // Adapters parse slash commands (Codex's `/plan`, Claude's `/compact`)
    // only from a prompt whose first block starts with `/`, so a command goes
    // out as typed. Images lead the prompt, so with images it can't parse.
    inputs.images.is_empty() && is_slash_command(&inputs.message)
}

fn compile_conversation_prompt_text(
    inputs: &ConversationPromptInputs,
    origin: SessionOrigin,
    conversation_id: &ConversationID,
    earlier_turns: Option<&str>,
) -> String {
    let message = &inputs.message;
    if sends_unwrapped(inputs) {
        return message.clone();
    }
    let preamble = match origin {
        SessionOrigin::New => NEW_CONVERSATION_PREAMBLE,
        // The session already carries the full preamble from its first turn.
        SessionOrigin::Continued | SessionOrigin::Restored => CONTINUED_CONVERSATION_PREAMBLE,
    };
    // Earlier turns follow the request, so the prompt still opens with
    // `"{preamble} … User request: "` for `is_ottam_prompt_fallback_title`.
    let mut text = format!(
        "{preamble}\nOttam conversation: {}\n\nUser request:\n{message}",
        conversation_id.0
    );
    if let Some(earlier_turns) = earlier_turns {
        text.push_str("\n\n");
        text.push_str(earlier_turns);
    }
    text
}

/// A session the run's prompt gives the conversation's earlier turns, and
/// what it replayed: nothing when the conversation had no earlier turns.
struct HistorySeed {
    replay: Option<ConversationHistoryReplay>,
}

/// Record the seed as the bound run's prompt goes out, so the run shows its
/// replay while the agent works and keeps it if core stops mid-turn.
/// Returns whether this run seeded the session.
async fn record_history_seed(context: &ConversationRunContext, seed: Option<HistorySeed>) -> bool {
    let Some(seed) = seed else {
        return false;
    };
    match context
        .chat
        .mark_run_history_seeded(
            &context.conversation_id,
            &context.run_id,
            seed.replay.as_ref(),
        )
        .await
    {
        Ok(seeded) => seeded,
        Err(error) => {
            tracing::warn!(
                run_id = %context.run_id.0,
                error = %error,
                "failed to record that the session has the earlier turns"
            );
            false
        }
    }
}

/// Undo the run's seed, so the session's next prompt carries the turns.
async fn unseed_history(context: &ConversationRunContext) {
    if let Err(error) = context
        .chat
        .unmark_run_history_seeded(&context.conversation_id, &context.run_id)
        .await
    {
        tracing::warn!(
            run_id = %context.run_id.0,
            error = %error,
            "failed to record that the session still needs the earlier turns"
        );
    }
}

/// Whether the agent may have the prompt: a turn it ended, a stop after the
/// prompt went out, an error it signalled from inside the turn, or a lost
/// adapter. Any other error is a rejection. This is how input delivery
/// counts it, except that a stop while waiting out a busy agent sent nothing.
fn prompt_reached_agent(result: &Result<HarnessStopReason, HarnessError>, sent: bool) -> bool {
    match result {
        Ok(HarnessStopReason::Cancelled) => sent,
        Ok(_) => true,
        Err(error) => error.terminal_signal.is_some() || delivery_unknown(error.kind),
    }
}

/// A transport loss after send, with no word on whether the agent took it.
fn delivery_unknown(kind: HarnessErrorKind) -> bool {
    matches!(
        kind,
        HarnessErrorKind::AdapterDead | HarnessErrorKind::AdapterExited
    )
}

/// The agents earlier runs used, newest first, as `engine (model)`.
async fn earlier_agents(context: &ConversationRunContext) -> Vec<String> {
    let Ok(page) = context
        .chat
        .list_runs(&context.conversation_id, Some(50), None)
        .await
    else {
        return Vec::new();
    };
    let mut agents: Vec<String> = Vec::new();
    for run in page.runs.iter().filter(|run| run.run_id != context.run_id) {
        let agent = match &run.model_id {
            Some(model) => format!("{} ({model})", run.engine_id.0),
            None => run.engine_id.0.clone(),
        };
        if !agents.contains(&agent) {
            agents.push(agent);
        }
    }
    agents
}

/// Earlier turns as a new session's prompt carries them, under a heading
/// that says where they came from and how to read what they leave out.
fn earlier_turns_block(
    conversation_id: &ConversationID,
    earlier_agents: &[String],
    turns: &str,
) -> String {
    let agents = if earlier_agents.is_empty() {
        String::new()
    } else {
        format!(" They ran on {}.", earlier_agents.join(", "))
    };
    format!(
        "Earlier in this conversation, before this session began (you did not see these \
         turns; oldest first).{agents} Tool calls show no input or output: \
         get_session_toolcall_output with conversation {} and the call's id reads them.\n\n\
         {turns}",
        conversation_id.0
    )
}

/// `/name` or `/name args`, where the name has no further `/`, so a message
/// that opens with a path (`/Users/me/notes.txt is wrong`) stays a request.
fn is_slash_command(message: &str) -> bool {
    let Some(rest) = message.trim_start().strip_prefix('/') else {
        return false;
    };
    let name = rest.split(char::is_whitespace).next().unwrap_or("");
    name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
}

/// codex-acp falls back to the whitespace-normalized first prompt when the
/// underlying thread has no explicit name. Ottam wraps that prompt, so this
/// shape is private transport context rather than a user-facing session title.
fn is_ottam_prompt_fallback_title(title: &str) -> bool {
    let normalized = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let wrapped_first_prompt = normalized.starts_with(NEW_CONVERSATION_PREAMBLE);
    let wrapped_followup = normalized.starts_with(CONTINUED_CONVERSATION_PREAMBLE);
    if (wrapped_first_prompt || wrapped_followup) && normalized.contains(" User request: ") {
        return true;
    }
    // codex-acp 2.2 cuts a fallback past 256 characters to 255 plus `…`, and
    // the new-conversation preamble alone is longer than that, so the cut ends
    // inside it. A cut long enough to be unmistakable still counts.
    const MIN_CUT: usize = 24;
    let cut = normalized
        .strip_suffix('…')
        .unwrap_or(&normalized)
        .trim_end();
    cut.len() >= MIN_CUT
        && [NEW_CONVERSATION_PREAMBLE, CONTINUED_CONVERSATION_PREAMBLE]
            .iter()
            .any(|preamble| {
                let id_line = format!("{preamble} Ottam conversation: ");
                // Prompts from before the id line went straight to the request.
                let request = format!("{preamble} User request: ");
                id_line.starts_with(cut) || cut.starts_with(&id_line) || request.starts_with(cut)
            })
}

async fn flush_conversation_recorder(context: &ConversationRunContext) -> Result<(), CoreError> {
    context.recorder.flush().await
}
