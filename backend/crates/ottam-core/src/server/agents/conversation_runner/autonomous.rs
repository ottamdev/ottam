//! Autonomous follow-up runs: the agent continuing on its own after a turn
//! ended, such as when a background task's completion wakes it. See "Follow-Up
//! Runs" in `docs/architecture/backend/acp-attribution.md`.
//!
//! The run takes the session of the run it continues as soon as it opens, and
//! never prompts. Its worker only holds the session key, so the forwarder
//! records what the session sends against it, until a user start or steer
//! closes the run, the run is cancelled, the adapter exits, or the agent's
//! cycle looks finished (see [`FollowUpActivity`]).
//!
//! The forwarder opens a run when output arrives on an armed key (rules 5 and
//! 6 of "Attribution And Lifetime"), and the permission broker joins or opens
//! one for a request on such a key (rule 8). Neither opens inline: an open
//! waits on the launch lock and on a claim only the forwarder applies.

use super::{
    wait_for_conversation_worker, ActiveConversationRun, ConversationRunCompletionGuard,
    ConversationRunContext, ConversationRunCoordinator,
};
use crate::agent::{
    AdapterLifecycleState, AdapterStatus, AgentId, HarnessAdapter, HarnessError, HarnessErrorKind,
    HarnessSessionId,
};
use crate::core_error::CoreError;
use crate::server::agents::attribution::{
    AttributionOp, DropReason, FollowUpJoin, OpenRequest, RunEnd, SessionKey,
};
use crate::server::agents::conversation_recorder::ConversationRecorder;
use crate::server::agents::task_index::TaskOwner;
use ottam_protocol::{
    AgentStopReason, AgentUpdate, ConversationEventKind, ConversationID, ConversationRunStatus,
    ConversationRunSummary, RunOrigin,
};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::{watch, Notify, OwnedSemaphorePermit};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// After Claude reports an autonomous cycle's result, how long the run waits
/// for anything more before it closes. Batched notifications report a
/// placeholder result first, so an early close is possible; it costs a
/// second follow-up run, not output.
#[cfg(not(test))]
const SETTLE_WINDOW: Duration = Duration::from_secs(5);
#[cfg(test)]
const SETTLE_WINDOW: Duration = Duration::from_millis(150);

/// How long a run with no open tool call or permission waits for output
/// before it closes. Covers agents that report no result, such as a task
/// stop's acknowledgment.
#[cfg(not(test))]
const IDLE_GAP: Duration = Duration::from_secs(30);
#[cfg(test)]
const IDLE_GAP: Duration = Duration::from_secs(2);

/// How long a permission request waits for its follow-up to open. The ACP
/// SDK handles the request inside the agent's dispatch loop, so nothing else
/// from that agent arrives meanwhile; past this the request goes out
/// unattributed.
#[cfg(not(test))]
const PERMISSION_OPEN_WAIT: Duration = Duration::from_secs(30);
#[cfg(test)]
const PERMISSION_OPEN_WAIT: Duration = Duration::from_secs(2);

/// The open waits for another run of the conversation to leave.
const FOLLOW_UP_BLOCKED: &str = "followUpBlocked";

/// How long a cancel waits to tell the agent to stop its cycle.
const OUT_OF_TURN_CANCEL_WAIT: Duration = Duration::from_secs(1);

/// What an autonomous run's close rules watch, fed by the forwarder and the
/// permission broker.
pub(in crate::server::agents) struct FollowUpActivity {
    state: StdMutex<ActivityState>,
    changed: Notify,
}

struct ActivityState {
    last_output: Instant,
    /// When the agent reported its cycle's result.
    settled_at: Option<Instant>,
    open_tools: HashSet<String>,
    permissions: usize,
}

impl FollowUpActivity {
    fn new() -> Self {
        Self {
            state: StdMutex::new(ActivityState {
                last_output: Instant::now(),
                settled_at: None,
                open_tools: HashSet::new(),
                permissions: 0,
            }),
            changed: Notify::new(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, ActivityState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// An update the forwarder routed to the run.
    pub(in crate::server::agents) fn observe(&self, update: &AgentUpdate) {
        {
            let mut state = self.state();
            match update {
                AgentUpdate::Other {
                    update_kind,
                    payload,
                } => {
                    if update_kind == "usage_update"
                        && crate::agent::metadata::autonomous_origin(payload)
                    {
                        state.settled_at = Some(Instant::now());
                    } else {
                        return;
                    }
                }
                AgentUpdate::SessionSignal { .. } | AgentUpdate::StreamGap { .. } => return,
                AgentUpdate::ToolCall { payload } | AgentUpdate::ToolCallUpdate { payload } => {
                    state.last_output = Instant::now();
                    if let Some(id) = payload.get("toolCallId").and_then(Value::as_str) {
                        match payload.get("status").and_then(Value::as_str) {
                            Some("completed" | "failed") => {
                                state.open_tools.remove(id);
                            }
                            _ if matches!(update, AgentUpdate::ToolCall { .. }) => {
                                state.open_tools.insert(id.to_owned());
                            }
                            _ => {}
                        }
                    }
                }
                _ => state.last_output = Instant::now(),
            }
        }
        self.changed.notify_one();
    }

    /// Held while a permission request of the run waits on the user.
    pub(in crate::server::agents) fn permission(self: &Arc<Self>) -> PermissionGuard {
        self.state().permissions += 1;
        self.changed.notify_one();
        PermissionGuard(Arc::clone(self))
    }

    /// When the run may close if nothing changes, or `None` while a
    /// permission or tool call is open and no result has settled it.
    fn close_at(&self) -> Option<Instant> {
        let state = self.state();
        if state.permissions > 0 {
            return None;
        }
        let settle = state
            .settled_at
            .map(|settled| settled.max(state.last_output) + SETTLE_WINDOW);
        let idle = state
            .open_tools
            .is_empty()
            .then(|| state.last_output + IDLE_GAP);
        match (settle, idle) {
            (Some(settle), Some(idle)) => Some(settle.min(idle)),
            (settle, idle) => settle.or(idle),
        }
    }
}

pub(in crate::server::agents) struct PermissionGuard(Arc<FollowUpActivity>);

impl Drop for PermissionGuard {
    fn drop(&mut self) {
        {
            let mut state = self.0.state();
            state.permissions = state.permissions.saturating_sub(1);
            // The user's answer is the agent's cue to go on.
            state.last_output = Instant::now();
        }
        self.0.changed.notify_one();
    }
}

/// The permission broker's way to the coordinator, bound after both exist.
pub(in crate::server::agents) trait FollowUpAttribution:
    Send + Sync
{
    /// The run a permission request on `key` belongs to, opening or joining
    /// a follow-up when the key is armed (rule 8).
    fn attribute<'a>(
        &'a self,
        key: SessionKey,
    ) -> Pin<Box<dyn Future<Output = Option<ConversationRunContext>> + Send + 'a>>;

    /// Returns once every update `agent_id` sent before now is recorded.
    fn record_sent_updates<'a>(
        &'a self,
        agent_id: &'a AgentId,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

impl FollowUpAttribution for ConversationRunCoordinator {
    fn attribute<'a>(
        &'a self,
        key: SessionKey,
    ) -> Pin<Box<dyn Future<Output = Option<ConversationRunContext>> + Send + 'a>> {
        Box::pin(self.attribute_permission(key))
    }

    fn record_sent_updates<'a>(
        &'a self,
        agent_id: &'a AgentId,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(self.flush_forwarder(agent_id))
    }
}

struct AutonomousRunWorker {
    permit: OwnedSemaphorePermit,
    context: ConversationRunContext,
    activity: Arc<FollowUpActivity>,
    adapter: Arc<dyn HarnessAdapter>,
    session_key: SessionKey,
    status: watch::Receiver<AdapterStatus>,
    generation: u64,
    cancel: CancellationToken,
    close: CancellationToken,
    finished: CancellationToken,
}

enum AutonomousRunEnd {
    Closed,
    Cancelled,
    AdapterExited,
}

impl ConversationRunCoordinator {
    /// Starts opening the follow-up `request` names, on a task of its own.
    pub(in crate::server::agents) fn request_follow_up(&self, request: OpenRequest) {
        if self.closing.load(Ordering::Acquire) {
            return;
        }
        let Some(this) = self.this.upgrade() else {
            return;
        };
        self.spawn_worker(async move { this.open_follow_up(request).await });
    }

    async fn open_follow_up(self: Arc<Self>, request: OpenRequest) {
        let OpenRequest {
            key,
            generation,
            continues,
        } = request;
        let error = match self.open_autonomous_run(&continues, &key, generation).await {
            Ok(_) => return,
            Err(error) => error,
        };
        match error.kind {
            // Held until the run in the way is removed.
            FOLLOW_UP_BLOCKED => {}
            "sessionNotFound" => self
                .attribution
                .fail_follow_up(&key, DropReason::AfterEnd, true),
            _ => {
                tracing::warn!(
                    agent_id = %key.0,
                    kind = %error.kind,
                    "Could not open a follow-up run"
                );
                self.attribution
                    .fail_follow_up(&key, DropReason::ConversationBusy, false);
            }
        }
    }

    /// See [`FollowUpAttribution::attribute`].
    async fn attribute_permission(&self, key: SessionKey) -> Option<ConversationRunContext> {
        if let Some(context) = self.attribution.running_context(&key) {
            return Some(context);
        }
        let agent_id = AgentId(key.0.clone());
        // What the agent sent before asking may be what arms the key.
        self.flush_forwarder(&agent_id).await;
        let generation = self
            .registry
            .running_adapter(&agent_id)
            .await
            .ok()?
            .status()
            .borrow()
            .generation;
        let mut waiter = match self.attribution.join_follow_up(&key, generation) {
            FollowUpJoin::Running(context) => return Some(*context),
            FollowUpJoin::Unattributed(reason) => {
                self.attribution.record_permission_drop(&key, reason);
                return None;
            }
            FollowUpJoin::Wait(waiter) => waiter,
            FollowUpJoin::Open(request, waiter) => {
                self.request_follow_up(request);
                waiter
            }
        };
        let joined = tokio::time::timeout(PERMISSION_OPEN_WAIT, async {
            loop {
                let ended = waiter.changed().await.is_err();
                if let Some(context) = self.attribution.running_context(&key) {
                    return Some(context);
                }
                if ended {
                    return None;
                }
            }
        })
        .await
        .ok()
        .flatten();
        if joined.is_none() {
            self.attribution
                .record_permission_drop(&key, DropReason::ConversationBusy);
        }
        joined
    }

    /// Opens an autonomous follow-up of `continues`, the run that last held
    /// `key`, after the agent's adapter of `generation` sent output there with
    /// no turn open. The run copies the continued run's scope and is bound to
    /// its session from the start.
    ///
    /// Refused with `sessionNotFound`, writing nothing, unless `continues` ran
    /// on the session and that adapter is still running it once the
    /// conversation's launch lock is held. An adapter that exits after that
    /// check ends the run `failed`.
    pub(in crate::server::agents) async fn open_autonomous_run(
        self: &Arc<Self>,
        continues: &TaskOwner,
        key: &SessionKey,
        generation: u64,
    ) -> Result<ConversationRunSummary, CoreError> {
        // Held until the worker is registered, so a user start or steer that
        // follows finds the run and closes it. Everything checked below is
        // checked under it: an opener can wait here while the adapter
        // restarts.
        let _launch = self.lock_launch(&continues.conversation_id).await;
        {
            // Under the run lock, so the run in the way cannot leave between
            // this check and the mark its removal looks for (rule 6).
            let runs = self.active_runs().await;
            if runs
                .values()
                .any(|run| run.context.conversation_id == continues.conversation_id)
            {
                self.attribution.block_follow_up(key);
                return Err(CoreError::new(
                    FOLLOW_UP_BLOCKED,
                    "another run of the conversation is active",
                )
                .recoverable(true));
            }
        }
        let chat = continues.chat.clone();
        let continued = chat
            .conversation_run(&continues.conversation_id, &continues.run_id)
            .await?;
        let ran_on_session = continued.engine_id.0 == key.0
            && continued
                .agent_session_id
                .as_ref()
                .is_some_and(|session| session.0 == key.1);
        if !ran_on_session {
            return Err(session_lost());
        }
        let agent_id = AgentId(key.0.clone());
        let session_id = HarnessSessionId(key.1.clone());
        let adapter = self
            .registry
            .running_adapter(&agent_id)
            .await
            .ok()
            .filter(|adapter| {
                adapter.status().borrow().generation == generation
                    && adapter
                        .sessions()
                        .iter()
                        .any(|session| session.session_id == session_id)
            })
            .ok_or_else(session_lost)?;
        let permit = self.regular_lane.try_acquire().map_err(|_| {
            CoreError::new(
                "agentRequestQueueSaturated",
                "agent request queue is saturated",
            )
        })?;
        let run = chat
            .begin_autonomous_run(&continues.conversation_id, &continues.run_id)
            .await?;

        let (recorder, recorder_owner) = ConversationRecorder::spawn(
            chat.clone(),
            continues.conversation_id.clone(),
            run.run_id.clone(),
            continues.project_id.clone(),
            self.outbound.clone(),
        );
        let context = ConversationRunContext {
            workspace_id: continues.workspace_id.clone(),
            project_id: continues.project_id.clone(),
            conversation_id: continues.conversation_id.clone(),
            run_id: run.run_id.clone(),
            origin: RunOrigin::Autonomous,
            run_input_id: None,
            is_steer_input: false,
            chat,
            recorder,
            last_adopted_title: Arc::new(StdMutex::new(None)),
            delegated: Default::default(),
            activity: Some(Arc::new(FollowUpActivity::new())),
        };
        let activity = context.activity.clone().expect("autonomous activity");
        let cancel = CancellationToken::new();
        let close = CancellationToken::new();
        let finished = CancellationToken::new();
        self.active_runs().await.insert(
            run.run_id.clone(),
            ActiveConversationRun {
                context: context.clone(),
                agent_id: agent_id.clone(),
                session_id: Some(session_id),
                adapter: Some(Arc::clone(&adapter)),
                cancel: cancel.clone(),
                close: close.clone(),
                finished: finished.clone(),
                recorder: recorder_owner,
                worker_abort: None,
            },
        );
        self.ensure_forwarder(Arc::clone(&adapter)).await;
        // Ahead of the claim, which records what the follow-up held.
        context
            .queue_event(
                ConversationEventKind::RunStatus,
                json!({"status": "running", "origin": RunOrigin::Autonomous}),
            )
            .await;
        // At the generation checked above, the one the worker watches, not
        // the adapter's current one: if it restarted since, the worker fails
        // the run at once and releases this claim.
        self.fence_attribution(
            &agent_id,
            AttributionOp::Claim {
                key: key.clone(),
                context: context.clone(),
                generation,
            },
        )
        .await;

        let run_id = run.run_id.clone();
        let coordinator = Arc::clone(self);
        let worker = AutonomousRunWorker {
            permit,
            context,
            activity,
            adapter: Arc::clone(&adapter),
            session_key: key.clone(),
            status: adapter.status(),
            generation,
            cancel,
            close,
            finished: finished.clone(),
        };
        let worker_abort = self.spawn_worker(async move {
            // Dropped last, so an aborted worker frees its permit before
            // `finished` fires.
            let _completion_guard = ConversationRunCompletionGuard(finished);
            coordinator.watch_autonomous_run(worker).await;
        });
        if let Some(active) = self.active_runs().await.get_mut(&run_id) {
            active.worker_abort = Some(worker_abort);
        }
        Ok(run)
    }

    async fn watch_autonomous_run(&self, worker: AutonomousRunWorker) {
        let AutonomousRunWorker {
            permit,
            context,
            activity,
            adapter,
            session_key,
            status,
            generation,
            cancel,
            close,
            finished,
        } = worker;
        let exited = adapter_exited(status, generation);
        tokio::pin!(exited);
        let end = loop {
            let close_at = activity.close_at();
            tokio::select! {
                _ = close.cancelled() => break AutonomousRunEnd::Closed,
                _ = cancel.cancelled() => break AutonomousRunEnd::Cancelled,
                _ = &mut exited => break AutonomousRunEnd::AdapterExited,
                _ = activity.changed.notified() => {}
                _ = sleep_until(close_at) => {
                    // Re-read: output may have landed since the deadline was
                    // taken.
                    if activity.close_at().is_some_and(|at| at <= Instant::now()) {
                        break AutonomousRunEnd::Closed;
                    }
                }
            }
        };
        let agent_id = AgentId(session_key.0.clone());
        if matches!(end, AutonomousRunEnd::Cancelled) {
            self.cancel_permissions(&agent_id, &session_key.1);
            // Best effort: the agent may stop its cycle. Never on shutdown,
            // which cancels every run.
            if !self.closing.load(Ordering::Acquire) {
                let session_id = HarnessSessionId(session_key.1.clone());
                if tokio::time::timeout(
                    OUT_OF_TURN_CANCEL_WAIT,
                    adapter.cancel_out_of_turn(&session_id),
                )
                .await
                .is_err()
                {
                    tracing::debug!(agent_id = %agent_id, "out-of-turn cancel timed out");
                }
            }
        }
        let ended = match end {
            AutonomousRunEnd::Closed => RunEnd::Completed,
            AutonomousRunEnd::Cancelled => RunEnd::Cancelled,
            AutonomousRunEnd::AdapterExited => RunEnd::Failed,
        };
        // Folds in what the agent already sent before the run reads as ended.
        self.release_conversation_session(&agent_id, session_key, &context.run_id, ended)
            .await;
        match end {
            AutonomousRunEnd::Closed => {
                self.finalize_conversation_run(
                    &context,
                    ConversationRunStatus::Completed,
                    None,
                    json!({}),
                )
                .await;
            }
            AutonomousRunEnd::Cancelled => {
                self.finalize_conversation_run(
                    &context,
                    ConversationRunStatus::Canceled,
                    Some(AgentStopReason::Cancelled),
                    json!({}),
                )
                .await;
            }
            AutonomousRunEnd::AdapterExited => {
                let error = HarnessError::new(
                    HarnessErrorKind::AdapterExited,
                    "the agent exited while it was continuing on its own",
                );
                self.finalize_conversation_error(&context, &error).await;
            }
        }
        // The run that closed this one takes a permit as soon as it sees
        // this one end.
        drop(permit);
        self.remove_conversation_run(&context, &finished).await;
    }

    pub(super) async fn has_autonomous_run(&self, conversation_id: &ConversationID) -> bool {
        self.active_runs().await.values().any(|run| {
            run.context.conversation_id == *conversation_id
                && run.context.origin == RunOrigin::Autonomous
        })
    }

    /// Closes the conversation's live autonomous run, if it has one, and
    /// returns once that run is final. It ends `completed`, keeping what the
    /// agent sent before the close.
    pub(in crate::server::agents) async fn close_autonomous_run(
        &self,
        conversation_id: &ConversationID,
    ) {
        let open = {
            let runs = self.active_runs().await;
            runs.values()
                .find(|run| {
                    run.context.conversation_id == *conversation_id
                        && run.context.origin == RunOrigin::Autonomous
                })
                .map(|run| {
                    (
                        run.context.run_id.clone(),
                        run.close.clone(),
                        run.finished.clone(),
                        run.worker_abort.clone(),
                    )
                })
        };
        let Some((run_id, close, finished, worker_abort)) = open else {
            return;
        };
        close.cancel();
        wait_for_conversation_worker(&finished, worker_abort.as_ref()).await;
        self.finalize_orphaned_conversation_run(
            &run_id,
            ConversationRunStatus::Completed,
            None,
            json!({}),
        )
        .await;
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Resolves once the adapter is no longer `generation`'s ready adapter: it
/// stopped, died, or restarted.
async fn adapter_exited(mut status: watch::Receiver<AdapterStatus>, generation: u64) {
    loop {
        {
            let current = status.borrow_and_update();
            if current.generation != generation
                || !matches!(current.lifecycle, AdapterLifecycleState::Ready)
            {
                return;
            }
        }
        if status.changed().await.is_err() {
            return;
        }
    }
}

fn session_lost() -> CoreError {
    CoreError::new(
        "sessionNotFound",
        "the agent is no longer running the session the run would continue",
    )
    .recoverable(true)
}
