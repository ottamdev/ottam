use super::attribution::{AttributionOp, Drained, SessionAttribution};
use super::task_index::{record_lost, TaskOwner};
use super::wire::map_update;
use crate::agent::{
    AdapterLifecycleState, AdapterStatus, AgentId, HarnessAdapter, HarnessSessionId,
    HarnessTurnUsage, HarnessUpdate, HarnessUpdateKind, HarnessUpdateReceiver,
};
use crate::server::outbound::OutboundSink;
use ottam_protocol::{
    AgentUpdate, ConversationEventDeltaParams, ConversationRunUsage, ConversationRunUsageScope,
    ProtocolNotification, METHOD_CONVERSATION_EVENT_DELTA,
};
use std::collections::HashMap;
use std::future::Future;
use std::ops::Range;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex, Weak};
use tokio::sync::{mpsc, oneshot, watch, Mutex};
use tokio::task::{AbortHandle, JoinHandle};

/// Queued updates past which a forwarder logs that it is behind, once until
/// it catches up. Above a routine `session/load` replay (about a thousand),
/// well below the lane's capacity, where updates start to drop.
const BACKLOG_WARNING: usize = 4096;
/// How long a replaced forwarder may take to handle its queued updates.
const REPLACED_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub(super) trait ConversationUpdateSink: Send + Sync {
    /// `generation` is the adapter generation current when the forwarder
    /// handled the update.
    fn forward_conversation_update<'a>(
        &'a self,
        agent_id: &'a AgentId,
        session_id: &'a HarnessSessionId,
        generation: u64,
        update: AgentUpdate,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

/// Drains every update queued when it arrives, then applies its operation,
/// then acknowledges. Updates and fences come from different channels, so
/// this drain is what puts an operation after the updates sent before it.
struct Fence {
    op: Option<AttributionOp>,
    ack: Option<oneshot::Sender<()>>,
}

struct Forwarder {
    generation: u64,
    task: JoinHandle<()>,
}

pub(super) struct AgentUpdateForwarders {
    outbound: OutboundSink,
    conversation: Weak<dyn ConversationUpdateSink>,
    attribution: SessionAttribution,
    forwarders: Mutex<HashMap<AgentId, Forwarder>>,
    /// Kept outside the async map so a drop guard can post without awaiting.
    fences: StdMutex<HashMap<AgentId, mpsc::UnboundedSender<Fence>>>,
    drop_aborts: StdMutex<HashMap<(AgentId, u64), AbortHandle>>,
}

impl AgentUpdateForwarders {
    pub(super) fn new(
        outbound: OutboundSink,
        conversation: Weak<dyn ConversationUpdateSink>,
        attribution: SessionAttribution,
    ) -> Self {
        Self {
            outbound,
            conversation,
            attribution,
            forwarders: Mutex::new(HashMap::new()),
            fences: StdMutex::new(HashMap::new()),
            drop_aborts: StdMutex::new(HashMap::new()),
        }
    }

    fn fence_sender(&self, agent_id: &AgentId) -> Option<mpsc::UnboundedSender<Fence>> {
        self.fences
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(agent_id)
            .cloned()
    }

    pub(super) async fn ensure(&self, adapter: Arc<dyn HarnessAdapter>) {
        let agent_id = adapter.definition().agent_id.clone();
        let generation = adapter.status().borrow().generation;
        let mut forwarders = self.forwarders.lock().await;
        if forwarders.get(&agent_id).is_some_and(|forwarder| {
            forwarder.generation == generation && !forwarder.task.is_finished()
        }) {
            return;
        }
        if let Some(previous) = forwarders.remove(&agent_id) {
            // Its lane may still hold the previous process's last frames, and
            // the restart reset their sequences, so a gap would never show.
            // A finished forwarder refuses the fence at once.
            if tokio::time::timeout(REPLACED_DRAIN_TIMEOUT, self.flush(&agent_id))
                .await
                .is_err()
            {
                tracing::warn!(
                    agent_id = %agent_id,
                    generation = previous.generation,
                    "replaced agent update forwarder did not drain in time; its queued updates are dropped"
                );
            }
            previous.task.abort();
            let _ = previous.task.await;
            self.drop_aborts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&(agent_id.clone(), previous.generation));
        }

        let mut updates = adapter.subscribe_updates();
        let mut status = adapter.status();
        // The adapter may have exited before this receiver subscribed, and
        // its tasks end with it: judge the current status once at start.
        status.mark_changed();
        let (fence_tx, mut fence_rx) = mpsc::unbounded_channel::<Fence>();
        let target = ForwardTarget {
            conversation: self.conversation.clone(),
            outbound: self.outbound.clone(),
            attribution: self.attribution.clone(),
            agent_id: agent_id.clone(),
            status: adapter.status(),
            adapter: Arc::downgrade(&adapter),
        };
        let task = tokio::spawn(async move {
            let mut sequences = HashMap::<String, ForwardSequenceState>::new();
            // Rule 7: no earlier adapter process can end its tasks now, and
            // the forwarder that would have seen it exit may have been
            // replaced first.
            target.lose_tasks_before(generation).await;
            let mut seen = generation;
            let mut watching = true;
            let mut behind = false;
            loop {
                tokio::select! {
                    update = updates.recv() => match update {
                        Some(update) => {
                            // The lane drops only once a stall fills it, so a
                            // forwarder falling behind shows here first.
                            let backlog = updates.len();
                            if backlog >= BACKLOG_WARNING && !behind {
                                behind = true;
                                tracing::warn!(
                                    agent_id = %target.agent_id,
                                    generation,
                                    backlog,
                                    "agent update forwarder is behind; updates stay queued"
                                );
                            } else if backlog == 0 {
                                behind = false;
                            }
                            forward_adapter_update(&target, &mut sequences, update).await;
                        }
                        None => break,
                    },
                    fence = fence_rx.recv() => match fence {
                        Some(fence) => {
                            drain_queued(&target, &mut sequences, &mut updates).await;
                            if let Some(op) = fence.op {
                                if let Some(drained) = target.attribution.apply(op) {
                                    target.forward_drained(drained).await;
                                }
                            }
                            if let Some(ack) = fence.ack {
                                let _ = ack.send(());
                            }
                        }
                        None => break,
                    },
                    changed = status.changed(), if watching => {
                        if changed.is_err() {
                            watching = false;
                            continue;
                        }
                        // Other status fields change too; only an exit or a
                        // restart ends tasks.
                        let (lifecycle, current) = {
                            let status = status.borrow_and_update();
                            (status.lifecycle, status.generation)
                        };
                        let exited = matches!(
                            lifecycle,
                            AdapterLifecycleState::Stopped | AdapterLifecycleState::Dead
                        );
                        let before = if exited {
                            Some(current.saturating_add(1))
                        } else {
                            (current > seen).then_some(current)
                        };
                        seen = seen.max(current);
                        if let Some(before) = before {
                            // A task's last frames may still be queued.
                            drain_queued(&target, &mut sequences, &mut updates).await;
                            target.lose_tasks_before(before).await;
                        }
                    }
                }
            }
        });
        self.drop_aborts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert((agent_id.clone(), generation), task.abort_handle());
        self.fences
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(agent_id.clone(), fence_tx);
        forwarders.insert(agent_id, Forwarder { generation, task });
    }

    /// Waits until every update already queued for the agent is handled.
    pub(super) async fn flush(&self, agent_id: &AgentId) {
        self.fence(agent_id, None).await;
    }

    /// Applies `op` after every update already queued for the agent, and
    /// returns once it has. With no live forwarder there is nothing to order
    /// against, so the operation applies at once; the same holds when the
    /// forwarder is replaced before reaching it. Operations are idempotent,
    /// so that fallback is safe even if the forwarder applied it first.
    pub(super) async fn fence(&self, agent_id: &AgentId, op: Option<AttributionOp>) {
        let fallback = op.clone();
        let acknowledged = match self.fence_sender(agent_id) {
            Some(sender) => {
                let (ack_tx, ack_rx) = oneshot::channel();
                sender
                    .send(Fence {
                        op,
                        ack: Some(ack_tx),
                    })
                    .is_ok()
                    && ack_rx.await.is_ok()
            }
            None => false,
        };
        if !acknowledged {
            if let Some(op) = fallback {
                if let Some(drained) = self.attribution.apply(op) {
                    tracing::debug!(
                        agent_id = %agent_id,
                        frames = drained.frames.len(),
                        "a follow-up claimed its session with no forwarder to record what it held"
                    );
                }
            }
        }
    }

    /// [`Self::fence`] without waiting, for drop guards. Unlike a fence, an
    /// operation the forwarder has already queued is lost if the forwarder is
    /// replaced first. That happens only when the adapter restarts or goes
    /// away: the next claim sweeps the earlier generation's states and lookups
    /// discard its tombstones, so a lost cleanup changes at most how a stale
    /// update is labeled.
    pub(super) fn post(&self, agent_id: &AgentId, op: AttributionOp) {
        let unsent = match self.fence_sender(agent_id) {
            Some(sender) => sender
                .send(Fence {
                    op: Some(op),
                    ack: None,
                })
                .err()
                .and_then(|error| error.0.op),
            None => Some(op),
        };
        if let Some(op) = unsent {
            let _ = self.attribution.apply(op);
        }
    }

    pub(super) async fn shutdown(&self) {
        self.fences
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        let forwarders = {
            let mut forwarders = self.forwarders.lock().await;
            forwarders
                .drain()
                .map(|(_, forwarder)| forwarder.task)
                .collect::<Vec<_>>()
        };
        for task in &forwarders {
            task.abort();
        }
        for task in forwarders {
            let _ = task.await;
        }
        self.drop_aborts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
}

impl Drop for AgentUpdateForwarders {
    fn drop(&mut self) {
        for abort in self
            .drop_aborts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
        {
            abort.abort();
        }
    }
}

#[derive(Default)]
pub(super) struct ForwardSequenceState {
    source: u64,
}

/// Where one agent's forwarder task delivers.
struct ForwardTarget {
    conversation: Weak<dyn ConversationUpdateSink>,
    outbound: OutboundSink,
    attribution: SessionAttribution,
    agent_id: AgentId,
    /// A restart can reuse the adapter and its update channel, so the
    /// generation for attribution is read live rather than captured.
    status: watch::Receiver<AdapterStatus>,
    adapter: Weak<dyn HarnessAdapter>,
}

impl ForwardTarget {
    /// Records what a follow-up held before it claimed its key, now that the
    /// key is its own. Runs before the claim is acknowledged, so nothing the
    /// agent sent later can overtake it.
    async fn forward_drained(&self, drained: Drained) {
        let Some(conversation) = self.conversation.upgrade() else {
            return;
        };
        let generation = self.status.borrow().generation;
        let session_id = HarnessSessionId(drained.key.1);
        for update in drained.frames {
            conversation
                .forward_conversation_update(&self.agent_id, &session_id, generation, update)
                .await;
        }
    }

    /// Marks lost every live task and subagent the agent started under a
    /// generation before `before`.
    async fn lose_tasks_before(&self, before: u64) {
        let agent = &self.agent_id.0;
        record_lost(
            self.attribution
                .take_lost_tasks(|task_agent, _, generation| {
                    task_agent == agent && generation < before
                }),
        )
        .await;
    }

    /// Stores a prompt answer's usage on its run, labelled with what this
    /// agent's counts cover and the adapter that reported them.
    async fn record_turn_usage(&self, owner: &TaskOwner, usage: &HarnessTurnUsage) {
        let adapter = self.adapter.upgrade();
        let definition = adapter.as_ref().map(|adapter| adapter.definition());
        let package = definition.and_then(|definition| definition.adapter.as_ref());
        let scope = definition.map_or(ConversationRunUsageScope::Turn, |definition| {
            definition.turn_usage_scope()
        });
        // A cancelled turn's "last request" can be the previous turn's:
        // codex-acp 2.1.1 repeats it when Stop lands before a request
        // completes, and core can't tell the two apart.
        if usage.cancelled && scope == ConversationRunUsageScope::LastRequest {
            return;
        }
        let usage = ConversationRunUsage {
            scope,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: usage.total_tokens,
            thought_tokens: usage.thought_tokens,
            cached_read_tokens: usage.cached_read_tokens,
            cached_write_tokens: usage.cached_write_tokens,
            adapter: package.map(|package| package.name.clone()),
            adapter_version: package.map(|package| package.version.clone()),
        };
        match owner
            .chat
            .record_turn_usage(
                &owner.conversation_id,
                &owner.run_id,
                owner.project_id.clone(),
                usage,
            )
            .await
        {
            Ok(deltas) => {
                for params in deltas {
                    send_conversation_notification(&self.outbound, params);
                }
            }
            Err(error) => tracing::warn!(
                run_id = %owner.run_id.0,
                kind = %error.kind,
                "Could not record a run's token usage"
            ),
        }
    }

    /// Whether the session's latest prompt answer was among the `lost`
    /// updates, sent before this forwarder subscribed or dropped while it
    /// stalled with a full lane.
    fn lost_prompt_answer(&self, session_id: &HarnessSessionId, lost: Range<u64>) -> bool {
        self.adapter
            .upgrade()
            .and_then(|adapter| adapter.prompt_settled_at(session_id))
            .is_some_and(|answered| lost.contains(&answered))
    }
}

/// Forwards every update already queued, so what follows applies after them.
async fn drain_queued(
    target: &ForwardTarget,
    sequences: &mut HashMap<String, ForwardSequenceState>,
    updates: &mut HarnessUpdateReceiver,
) {
    // Bounded by what is queued now, so a busy agent cannot starve the fence.
    for _ in 0..updates.len() {
        let Ok(update) = updates.try_recv() else {
            break;
        };
        forward_adapter_update(target, sequences, update).await;
    }
}

async fn forward_adapter_update(
    target: &ForwardTarget,
    sequences: &mut HashMap<String, ForwardSequenceState>,
    update: HarnessUpdate,
) {
    let session_id = update.session_id.clone();
    let expected = sequences
        .get(&session_id.0)
        .map_or(1, |sequence| sequence.source.saturating_add(1));
    let key = || (target.agent_id.0.clone(), session_id.0.clone());
    // The answer's usage belongs to the run that prompted, still holding the
    // session or stopped before the answer came. A run releases through a
    // fence that drains this first, so a finished run has it before it ends.
    if let HarnessUpdateKind::TurnUsage(usage) = &update.update {
        if let Some(owner) = target.attribution.answering_run(&key()) {
            target.record_turn_usage(&owner, usage).await;
        }
    }
    // The turn boundary: this update is the prompt's answer, or the answer
    // was lost just before it. Either way it applies ahead of what follows.
    if matches!(update.update, HarnessUpdateKind::PromptSettled)
        || (update.sequence > expected
            && target.lost_prompt_answer(&session_id, expected..update.sequence))
    {
        let _ = target
            .attribution
            .apply(AttributionOp::PromptSettled { key: key() });
    }
    let forwarded = forward_harness_update(sequences, update);
    let current_generation = target.status.borrow().generation;
    if let Some(conversation) = target.conversation.upgrade() {
        for agent_update in forwarded {
            conversation
                .forward_conversation_update(
                    &target.agent_id,
                    &session_id,
                    current_generation,
                    agent_update,
                )
                .await;
        }
    }
}

/// Maps one adapter update for the conversation, preceded by a `StreamGap`
/// when the adapter's sequence skipped. The ACP wire log
/// (`OTTAM_ACP_WIRE_LOG`) shows the raw chunks when debugging.
pub(super) fn forward_harness_update(
    sequences: &mut HashMap<String, ForwardSequenceState>,
    update: HarnessUpdate,
) -> Vec<AgentUpdate> {
    let mut forwarded = Vec::with_capacity(2);
    let sequence = sequences.entry(update.session_id.0).or_default();
    let expected_source = sequence.source.saturating_add(1);
    if update.sequence > expected_source {
        forwarded.push(AgentUpdate::StreamGap {
            dropped: update.sequence - expected_source,
        });
    }
    sequence.source = sequence.source.max(update.sequence);
    forwarded.extend(map_update(update.update));
    forwarded
}

pub(super) fn send_conversation_notification(
    outbound: &OutboundSink,
    params: ConversationEventDeltaParams,
) {
    let notification = ProtocolNotification::new(METHOD_CONVERSATION_EVENT_DELTA, params);
    if let Ok(value) = serde_json::to_value(notification) {
        let _ = outbound.notify(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopConversationSink;

    impl ConversationUpdateSink for NoopConversationSink {
        fn forward_conversation_update<'a>(
            &'a self,
            _agent_id: &'a AgentId,
            _session_id: &'a HarnessSessionId,
            _generation: u64,
            _update: AgentUpdate,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
            Box::pin(async {})
        }
    }

    struct DropSignal(Option<oneshot::Sender<()>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    #[tokio::test]
    async fn dropping_forwarders_aborts_tasks_without_retaining_the_conversation_sink() {
        let sink: Arc<dyn ConversationUpdateSink> = Arc::new(NoopConversationSink);
        let weak = Arc::downgrade(&sink);
        let (outbound, _outbound_rx) = crate::server::outbound::test_sink();
        let forwarders =
            AgentUpdateForwarders::new(outbound, weak.clone(), SessionAttribution::default());
        assert_eq!(Arc::strong_count(&sink), 1);
        drop(sink);
        assert!(weak.upgrade().is_none());

        let (ready_tx, ready_rx) = oneshot::channel();
        let (dropped_tx, dropped_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _drop_signal = DropSignal(Some(dropped_tx));
            let _ = ready_tx.send(());
            std::future::pending::<()>().await;
        });
        ready_rx.await.expect("forwarder task starts");

        let agent_id = AgentId::new("drop-test");
        let generation = 7;
        forwarders
            .drop_aborts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert((agent_id.clone(), generation), task.abort_handle());
        forwarders
            .forwarders
            .lock()
            .await
            .insert(agent_id, Forwarder { generation, task });

        drop(forwarders);
        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .expect("forwarder task is aborted on owner drop")
            .expect("drop signal is delivered");
    }
}
