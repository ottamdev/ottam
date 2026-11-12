//! Which conversation run owns each agent session's updates, and why an update
//! with no owner was dropped. Implements rules 1, 2, and 6 of "Attribution And
//! Lifetime" in `docs/architecture/backend/acp-attribution.md`, and holds
//! the background-task index of rule 3 (see `task_index.rs`).
//!
//! A session key is `Running` while a run holds it, `Replaying` while
//! `session/load` replays its history, and `Released` after its run lets go;
//! the tombstone remembers that run and how it ended. A running key also
//! tracks whether the agent has answered the run's prompt and whether a
//! native steer is in flight: together they mark where a detached steer turn
//! begins. Run transitions are [`AttributionOp`]s that the agent's update
//! forwarder applies in order with the updates around them (see `events.rs`).
//! Sessions being opened and the cleanup when a conversation or project goes
//! away apply directly: no update depends on their order.

use super::conversation_runner::ConversationRunContext;
use super::task_index::{is_lifecycle, KeyView, LostTask, Route, TaskIndex, TaskOwner};
use crate::agent::{AgentId, HarnessSessionId};
use crate::diagnostics::{is_log_point, BoundedCounts};
use ottam_protocol::{
    AgentUpdate, ConversationID, ConversationRunID, ProjectID, RunOrigin, WorkspaceID,
};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

pub(super) type SessionKey = (String, String);

/// How long a session stays armed after its last background-task event, so
/// the agent's answer to a finished task still opens a follow-up (rule 5).
#[cfg(not(test))]
const ARMED_GRACE: Duration = Duration::from_secs(60);
#[cfg(test)]
const ARMED_GRACE: Duration = Duration::from_secs(5);

/// Bounds on what a follow-up holds while another run of its conversation is
/// active (rule 6). Consecutive chunks of one message merge before counting.
#[cfg(not(test))]
const FOLLOW_UP_FRAMES: usize = 512;
#[cfg(test)]
const FOLLOW_UP_FRAMES: usize = 8;
const FOLLOW_UP_BYTES: usize = 4 * 1024 * 1024;
const FOLLOW_UP_AGE: Duration = Duration::from_secs(10 * 60);

/// How a run let go of its session. Decides how a late update is reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunEnd {
    Completed,
    Cancelled,
    Failed,
    /// A native steer found the turn settled and the agent started a turn of
    /// its own for it, which core then cancelled.
    DetachedSteer,
    /// The run was cancelled, or fell back to a new session, while
    /// `session/load` was replaying this one.
    ReplayAbandoned,
}

impl RunEnd {
    fn drop_reason(self) -> DropReason {
        match self {
            Self::Completed | Self::Failed => DropReason::AfterEnd,
            Self::Cancelled => DropReason::AfterCancel,
            Self::DetachedSteer => DropReason::DetachedSteer,
            Self::ReplayAbandoned => DropReason::Replay,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DropReason {
    /// History replayed by `session/load`. Expected, so never logged.
    Replay,
    AfterEnd,
    AfterCancel,
    DetachedSteer,
    /// No run has held this session in the current adapter generation.
    Unattributed,
    /// A follow-up could not hold or open: its buffer overflowed or aged out
    /// behind another run, or core had no room to open it.
    ConversationBusy,
}

impl DropReason {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Replay => "replay",
            Self::AfterEnd => "after_end",
            Self::AfterCancel => "after_cancel",
            Self::DetachedSteer => "detached_steer",
            Self::Unattributed => "unattributed",
            Self::ConversationBusy => "conversation_busy",
        }
    }

    /// Output that may be lost work, rather than the expected tail of
    /// something core stopped on purpose.
    fn warns(self) -> bool {
        matches!(
            self,
            Self::AfterEnd | Self::Unattributed | Self::ConversationBusy
        )
    }
}

/// Where an update goes.
pub(super) enum Resolution {
    Run(Box<ConversationRunContext>),
    /// Not conversation traffic yet: a session still being opened, which no
    /// run has claimed. Neither recorded nor counted.
    NotConversation,
    Drop(DropReason),
}

/// Where an update goes once follow-ups are considered (rules 5 and 6).
pub(super) enum Resolved {
    Run(Box<ConversationRunContext>, AgentUpdate),
    NotConversation,
    Drop(DropReason, AgentUpdate),
    /// Held for a follow-up run that is opening or waiting on another run.
    Held,
    /// Held as the first frame of a follow-up nobody is opening yet: the
    /// caller starts the open.
    Open(OpenRequest),
}

/// A follow-up run to open: an autonomous continuation of `continues` on
/// `key`, under the adapter generation whose output it holds.
pub(super) struct OpenRequest {
    pub(super) key: SessionKey,
    pub(super) generation: u64,
    pub(super) continues: Arc<TaskOwner>,
}

/// What a permission request on a session no run holds should do (rule 8).
pub(super) enum FollowUpJoin {
    Running(Box<ConversationRunContext>),
    /// A follow-up is opening; wait for it, then read the key again. The
    /// sender is dropped once it opens or fails.
    Wait(watch::Receiver<()>),
    /// As `Wait`, and the caller starts the open.
    Open(OpenRequest, watch::Receiver<()>),
    Unattributed(DropReason),
}

/// Frames an autonomous claim took from its key's follow-up buffer, which the
/// forwarder records next, ahead of anything still queued.
pub(super) struct Drained {
    pub(super) key: SessionKey,
    pub(super) frames: Vec<AgentUpdate>,
}

#[derive(Clone)]
pub(super) enum AttributionOp {
    /// Sent before `session/load`: what the load replays is history.
    MarkReplay {
        key: SessionKey,
        context: ConversationRunContext,
        generation: u64,
    },
    /// The load was cancelled or abandoned; stray replay stays quiet.
    AbandonReplay {
        key: SessionKey,
        run_id: ConversationRunID,
    },
    Claim {
        key: SessionKey,
        context: ConversationRunContext,
        generation: u64,
    },
    /// The agent answered the holder's prompt. Applied by the forwarder when
    /// it reaches the adapter's in-band marker, so it falls exactly between
    /// the turn's updates and whatever the agent sends next.
    PromptSettled { key: SessionKey },
    /// Sent, and acknowledged, before a native steer goes out: if the turn
    /// has already settled, the agent answers the steer with a detached turn
    /// whose output can arrive before the answer does.
    SteerPending {
        key: SessionKey,
        run_id: ConversationRunID,
    },
    /// The steer was injected into the turn, or delivered nothing.
    SteerSettled {
        key: SessionKey,
        run_id: ConversationRunID,
    },
    /// The steer was answered with a detached turn, which core cancelled.
    MarkDetached {
        key: SessionKey,
        run_id: ConversationRunID,
    },
    /// Only the run that holds the key can release it.
    Release {
        key: SessionKey,
        run_id: ConversationRunID,
        ended: RunEnd,
    },
    /// Release with no tombstone, for a session that is gone or never ran.
    Forget {
        key: SessionKey,
        run_id: ConversationRunID,
    },
}

struct Tombstone {
    owner: Arc<TaskOwner>,
    ended: RunEnd,
    /// The run's native steer was still in flight when it let go. A run that
    /// ends usually releases before the agent starts the detached turn.
    steering: bool,
    /// The agent had answered the run's prompt. A cancelled run releases
    /// before its answer, so what arrives until then is the turn's tail.
    settled: bool,
}

impl Tombstone {
    fn new(owner: Arc<TaskOwner>, ended: RunEnd) -> Self {
        Self {
            owner,
            ended,
            steering: false,
            settled: false,
        }
    }

    /// Output after the release is still the run's own, so a task it spawns
    /// belongs to the run.
    fn adopts(&self) -> bool {
        !self.steering
            && matches!(
                self.ended,
                RunEnd::Completed | RunEnd::Cancelled | RunEnd::Failed
            )
    }

    fn drop_reason(&self) -> DropReason {
        if self.steering {
            DropReason::DetachedSteer
        } else {
            self.ended.drop_reason()
        }
    }
}

/// A run holding a key, and what core knows about its turn.
struct Holder {
    context: ConversationRunContext,
    owner: Arc<TaskOwner>,
    /// The agent answered the run's prompt; what follows is outside its turn.
    settled: bool,
    /// A native steer is in flight. Once the turn has settled, the agent
    /// answers it by starting a detached turn.
    steering: bool,
    /// The steer was answered with a detached turn.
    detached: bool,
}

impl Holder {
    fn new(context: ConversationRunContext) -> Self {
        Self {
            owner: Arc::new(context.task_owner()),
            context,
            settled: false,
            steering: false,
            detached: false,
        }
    }

    /// Output now comes from a detached steer turn, not the run's own.
    fn detached_output(&self) -> bool {
        self.detached || (self.settled && self.steering)
    }

    fn release(&self, ended: RunEnd) -> Tombstone {
        let ended = if self.detached {
            RunEnd::DetachedSteer
        } else {
            ended
        };
        Tombstone {
            steering: self.steering,
            settled: self.settled,
            ..Tombstone::new(self.owner.clone(), ended)
        }
    }
}

enum SessionState {
    Running(Holder),
    Replaying { context: ConversationRunContext },
    Released(Tombstone),
}

struct Entry {
    generation: u64,
    state: SessionState,
}

/// A session's background-task activity, which arms its key (rule 5).
struct Arming {
    generation: u64,
    /// The latest task frame, or the close of a follow-up run, on the key.
    last_task_event: Option<Instant>,
    /// A follow-up the user cancelled, or one that could not open because
    /// the session is gone. Lasts until the next user run claims the key.
    disarmed: bool,
}

/// Content for a follow-up run that has not claimed its key yet (rule 6).
struct PendingFollowUp {
    generation: u64,
    continues: Arc<TaskOwner>,
    since: Instant,
    frames: VecDeque<AgentUpdate>,
    bytes: usize,
    /// A frame was dropped for room. Everything after it is dropped too, so
    /// the run never records output with a hole in it.
    overflowed: bool,
    /// Another run of the conversation is active; its removal reopens this.
    blocked: bool,
    /// Permission requests waiting for the run. Dropped when it claims the
    /// key or fails; signalled when a user run claims the key first.
    waiters: watch::Sender<()>,
}

impl PendingFollowUp {
    fn new(generation: u64, continues: Arc<TaskOwner>) -> Self {
        Self {
            generation,
            continues,
            since: Instant::now(),
            frames: VecDeque::new(),
            bytes: 0,
            overflowed: false,
            blocked: false,
            waiters: watch::channel(()).0,
        }
    }

    fn expired(&self) -> bool {
        self.since.elapsed() >= FOLLOW_UP_AGE
    }

    /// Holds `update`, merging a chunk into the previous chunk of the same
    /// message. Hands the update back once the buffer has been full.
    fn push(&mut self, update: AgentUpdate) -> Result<(), Box<AgentUpdate>> {
        let size = update_size(&update);
        if self.overflowed || self.bytes + size > FOLLOW_UP_BYTES {
            self.overflowed = true;
            return Err(Box::new(update));
        }
        let update = match (self.frames.back_mut(), update) {
            (
                Some(AgentUpdate::AgentMessageChunk {
                    text: held,
                    message_id: held_id,
                }),
                AgentUpdate::AgentMessageChunk { text, message_id },
            )
            | (
                Some(AgentUpdate::AgentThoughtChunk {
                    text: held,
                    message_id: held_id,
                }),
                AgentUpdate::AgentThoughtChunk { text, message_id },
            ) if *held_id == message_id => {
                held.push_str(&text);
                self.bytes += size;
                return Ok(());
            }
            (_, update) => update,
        };
        if self.frames.len() >= FOLLOW_UP_FRAMES {
            self.overflowed = true;
            return Err(Box::new(update));
        }
        self.bytes += size;
        self.frames.push_back(update);
        Ok(())
    }
}

fn update_size(update: &AgentUpdate) -> usize {
    match update {
        AgentUpdate::AgentMessageChunk { text, .. }
        | AgentUpdate::AgentThoughtChunk { text, .. } => text.len(),
        update => serde_json::to_vec(update).map_or(0, |bytes| bytes.len()),
    }
}

/// Turn output that can open a follow-up. Lifecycle frames never do (rule
/// 3), and session state alone is not the agent working.
fn opens_follow_up(update: &AgentUpdate) -> bool {
    matches!(
        update,
        AgentUpdate::RichMessageChunk { .. }
            | AgentUpdate::AgentMessageChunk { .. }
            | AgentUpdate::AgentThoughtChunk { .. }
            | AgentUpdate::AgentImageChunk { .. }
            | AgentUpdate::ToolCall { .. }
            | AgentUpdate::ToolCallUpdate { .. }
            | AgentUpdate::Plan { .. }
    )
}

/// A task frame, or the stamp that marks a tool call backgrounded.
fn task_event(update: &AgentUpdate) -> bool {
    match update {
        AgentUpdate::AsyncTaskSpawned { .. }
        | AgentUpdate::AsyncTaskProgress { .. }
        | AgentUpdate::AsyncTaskStateUpdate { .. } => true,
        AgentUpdate::ToolCallUpdate { payload } => {
            payload
                .pointer("/_meta/jetbrains/air/asyncTasks/backgrounded")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        }
        _ => false,
    }
}

/// Most native subagent sessions an agent keeps, the oldest ended going first.
const CHILDREN_PER_AGENT: usize = 1024;

/// A native subagent session (rule 9). Its updates resolve through the
/// conversation session at the top of its tree, stamped with its thread.
#[derive(Clone)]
struct Child {
    generation: u64,
    /// The conversation's own session, where the tree starts.
    root: SessionKey,
    /// One thread per subagent: a resumed run shares its first run's thread.
    thread_id: String,
    /// This run's anchor call, `subagent:<child session>`.
    anchor_call: String,
    name: Option<String>,
    /// The run it spawned in, or the follow-up that took it over. A later
    /// user run on the root never adopts it. `None` for history.
    bound: Option<Arc<TaskOwner>>,
    /// The run that holds this run's anchor call, which takes its state
    /// even after it closes. Taken once the state is reported or lost.
    anchor: Option<Arc<TaskOwner>>,
    live: bool,
    /// Announced while its root replayed history: all it sends is history.
    replay: bool,
    order: u64,
}

/// Where a native subagent's reported end goes.
pub(super) struct ChildEnd {
    pub(super) anchor_call: String,
    /// The closed or detached run holding the anchor; `None` when the run
    /// holding the root holds it too.
    pub(super) owner: Option<Arc<TaskOwner>>,
}

fn bound_run(child: &Child) -> Option<&ConversationRunID> {
    child.bound.as_deref().map(|owner| &owner.run_id)
}

/// What a `subagent_spawned` does (rule 9).
pub(super) enum ChildSpawn {
    /// Record the thread's anchor on the parent's session.
    Record {
        thread_id: String,
        anchor_call: String,
    },
    Drop(DropReason),
}

/// Where an update from a session goes before its key resolves.
pub(super) enum ChildRoute {
    /// Not a native subagent session.
    Own,
    /// Resolve on the root's key, stamped with the child's thread.
    Root {
        key: SessionKey,
        thread_id: String,
    },
    Drop(DropReason),
}

#[derive(Default)]
struct Sessions {
    entries: HashMap<SessionKey, Entry>,
    /// Native subagent sessions, by `(agent, child session)`.
    children: HashMap<SessionKey, Child>,
    child_order: u64,
    /// Sessions each agent is opening, which no run covers yet.
    opening: HashMap<String, usize>,
    tasks: TaskIndex,
    arming: HashMap<SessionKey, Arming>,
    follow_ups: HashMap<SessionKey, PendingFollowUp>,
}

impl Sessions {
    /// Whether output on `key`'s released tombstone opens a follow-up: the
    /// output is still the released run's own, and the session has a live
    /// task or had task activity within the grace window (rule 5).
    fn armed(&self, key: &SessionKey, generation: u64, tombstone: &Tombstone) -> bool {
        if !tombstone.adopts() || (tombstone.ended == RunEnd::Cancelled && !tombstone.settled) {
            return false;
        }
        let arming = self
            .arming
            .get(key)
            .filter(|arming| arming.generation == generation);
        if arming.is_some_and(|arming| arming.disarmed) {
            return false;
        }
        arming
            .and_then(|arming| arming.last_task_event)
            .is_some_and(|at| at.elapsed() < ARMED_GRACE)
            || self.tasks.has_live(&key.0, &key.1, generation)
            || self.has_live_child(key, generation)
    }

    /// A live subagent counts as a live task for its root (rules 5 and 9).
    fn has_live_child(&self, root: &SessionKey, generation: u64) -> bool {
        self.children.values().any(|child| {
            child.live && !child.replay && child.root == *root && child.generation == generation
        })
    }

    /// A child outlives its root's key only as long as the key's state.
    fn prune_children(&mut self) {
        let Sessions {
            entries, children, ..
        } = self;
        children.retain(|_, child| entries.contains_key(&child.root));
    }

    /// Keeps each agent's children bounded, dropping its oldest ended ones.
    fn bound_children(&mut self, agent: &str) {
        let count = self.children.keys().filter(|(a, _)| a == agent).count();
        if count <= CHILDREN_PER_AGENT {
            return;
        }
        let mut ended: Vec<(u64, SessionKey)> = self
            .children
            .iter()
            .filter(|((a, _), child)| a == agent && !child.live)
            .map(|(key, child)| (child.order, key.clone()))
            .collect();
        ended.sort();
        for (_, key) in ended.into_iter().take(count - CHILDREN_PER_AGENT) {
            self.children.remove(&key);
        }
    }

    fn arming(&mut self, key: &SessionKey, generation: u64) -> &mut Arming {
        let arming = self.arming.entry(key.clone()).or_insert(Arming {
            generation,
            last_task_event: None,
            disarmed: false,
        });
        if arming.generation != generation {
            *arming = Arming {
                generation,
                last_task_event: None,
                disarmed: false,
            };
        }
        arming
    }

    /// Changes `run_id`'s state on `key`, whether it still holds the key or
    /// has let go of it.
    fn update_run(
        &mut self,
        key: &SessionKey,
        run_id: &ConversationRunID,
        running: impl FnOnce(&mut Holder),
        released: impl FnOnce(&mut Tombstone),
    ) {
        match self.entries.get_mut(key).map(|entry| &mut entry.state) {
            Some(SessionState::Running(holder)) if holder.context.run_id == *run_id => {
                running(holder)
            }
            Some(SessionState::Released(tombstone)) if tombstone.owner.run_id == *run_id => {
                released(tombstone)
            }
            _ => {}
        }
    }
}

/// Held while an agent opens a session, until a run claims it. The agent can
/// send the session's first updates before then.
pub(super) struct OpeningSession {
    attribution: SessionAttribution,
    agent_id: String,
}

impl Drop for OpeningSession {
    fn drop(&mut self) {
        let mut sessions = self.attribution.sessions();
        if let Some(count) = sessions.opening.get_mut(&self.agent_id) {
            *count -= 1;
            if *count == 0 {
                sessions.opening.remove(&self.agent_id);
            }
        }
    }
}

/// Shared by the coordinator, the forwarders, and the permission broker.
#[derive(Clone, Default)]
pub(super) struct SessionAttribution {
    sessions: Arc<StdMutex<Sessions>>,
    drops: Arc<BoundedCounts>,
}

impl SessionAttribution {
    fn sessions(&self) -> std::sync::MutexGuard<'_, Sessions> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Applies `op`. An autonomous claim hands back the frames its key's
    /// follow-up held, for the caller to record before anything newer.
    pub(super) fn apply(&self, op: AttributionOp) -> Option<Drained> {
        let mut sessions = self.sessions();
        match op {
            AttributionOp::MarkReplay {
                key,
                context,
                generation,
            } => {
                sessions.entries.insert(
                    key,
                    Entry {
                        generation,
                        state: SessionState::Replaying { context },
                    },
                );
            }
            AttributionOp::AbandonReplay { key, run_id } => {
                if let Some(entry) = sessions.entries.get_mut(&key) {
                    let tombstone = match &entry.state {
                        SessionState::Replaying { context } if context.run_id == run_id => Some(
                            Tombstone::new(Arc::new(context.task_owner()), RunEnd::ReplayAbandoned),
                        ),
                        _ => None,
                    };
                    if let Some(tombstone) = tombstone {
                        entry.state = SessionState::Released(tombstone);
                    }
                }
            }
            AttributionOp::Claim {
                key,
                context,
                generation,
            } => {
                // A restarted adapter cannot send updates for an earlier
                // generation's sessions, so their tombstones go.
                sessions.entries.retain(|(agent, _), entry| {
                    *agent != key.0
                        || entry.generation >= generation
                        || matches!(entry.state, SessionState::Running(_))
                });
                sessions.arming.retain(|(agent, _), arming| {
                    *agent != key.0 || arming.generation >= generation
                });
                sessions.follow_ups.retain(|(agent, _), pending| {
                    *agent != key.0 || pending.generation >= generation
                });
                sessions
                    .children
                    .retain(|(agent, _), child| *agent != key.0 || child.generation >= generation);
                // A follow-up takes over the children of the run it continues
                // (rule 9); a user run never does.
                if context.origin == RunOrigin::Autonomous {
                    for child in sessions.children.values_mut() {
                        if child.root == key && !child.replay {
                            child.bound = Some(Arc::new(context.task_owner()));
                        }
                    }
                }
                if let Some(Entry {
                    state: SessionState::Running(holder),
                    ..
                }) = sessions.entries.get(&key)
                {
                    if holder.context.run_id != context.run_id {
                        tracing::warn!(
                            agent_id = %key.0,
                            session_id = %key.1,
                            holder = %holder.context.run_id.0,
                            claimant = %context.run_id.0,
                            "a run claimed a session another run still holds"
                        );
                    }
                }
                sessions
                    .tasks
                    .claimed(&key, &context.run_id, context.origin, generation);
                let drained = match context.origin {
                    // The user's run ends grace and any disarm; a live task
                    // still arms the key after it. A follow-up held behind
                    // another run stays held, and opens after this one.
                    RunOrigin::User => {
                        sessions.arming.remove(&key);
                        if let Some(pending) = sessions.follow_ups.get(&key) {
                            pending.waiters.send_replace(());
                        }
                        None
                    }
                    RunOrigin::Autonomous => {
                        sessions.follow_ups.remove(&key).map(|pending| Drained {
                            key: key.clone(),
                            frames: pending.frames.into(),
                        })
                    }
                };
                sessions.entries.insert(
                    key,
                    Entry {
                        generation,
                        state: SessionState::Running(Holder::new(context)),
                    },
                );
                return drained;
            }
            AttributionOp::PromptSettled { key } => {
                // Only the holder prompts its session, and a later run claims
                // it only after this prompt's marker has been handled. A
                // cancelled run lets go before its answer arrives.
                match sessions.entries.get_mut(&key).map(|entry| &mut entry.state) {
                    Some(SessionState::Running(holder)) => holder.settled = true,
                    Some(SessionState::Released(tombstone)) => tombstone.settled = true,
                    _ => {}
                }
            }
            AttributionOp::SteerPending { key, run_id } => {
                sessions.update_run(
                    &key,
                    &run_id,
                    |holder| holder.steering = true,
                    |tombstone| tombstone.steering = true,
                );
            }
            AttributionOp::SteerSettled { key, run_id } => {
                sessions.update_run(
                    &key,
                    &run_id,
                    |holder| holder.steering = false,
                    |tombstone| tombstone.steering = false,
                );
            }
            AttributionOp::MarkDetached { key, run_id } => {
                sessions.update_run(
                    &key,
                    &run_id,
                    |holder| {
                        holder.steering = false;
                        holder.detached = true;
                    },
                    |tombstone| {
                        tombstone.steering = false;
                        tombstone.ended = RunEnd::DetachedSteer;
                    },
                );
            }
            AttributionOp::Release { key, run_id, ended } => {
                let entry = sessions.entries.get_mut(&key)?;
                let generation = entry.generation;
                let (tombstone, origin) = match &entry.state {
                    SessionState::Running(holder) if holder.context.run_id == run_id => {
                        (holder.release(ended), holder.context.origin)
                    }
                    _ => return None,
                };
                entry.state = SessionState::Released(tombstone);
                // Cancelling the parent cancels its children; the adapter
                // owns their processes and says so in its own time.
                if ended == RunEnd::Cancelled {
                    for child in sessions.children.values_mut() {
                        if child.root == key && bound_run(child) == Some(&run_id) {
                            child.live = false;
                        }
                    }
                }
                if origin == RunOrigin::Autonomous {
                    let arming = sessions.arming(&key, generation);
                    match ended {
                        // Nobody wants the agent's cycle back.
                        RunEnd::Cancelled => arming.disarmed = true,
                        // Closed early: what the cycle still sends opens the
                        // next follow-up.
                        RunEnd::Completed => arming.last_task_event = Some(Instant::now()),
                        _ => {}
                    }
                }
            }
            AttributionOp::Forget { key, run_id } => {
                let owned = sessions
                    .entries
                    .get(&key)
                    .is_some_and(|entry| match &entry.state {
                        SessionState::Running(Holder { context, .. })
                        | SessionState::Replaying { context } => context.run_id == run_id,
                        SessionState::Released(_) => false,
                    });
                if owned {
                    sessions.entries.remove(&key);
                    // Its subagents went with it: nothing routes them now,
                    // and they are no one's live work.
                    for child in sessions.children.values_mut() {
                        if child.root == key {
                            child.live = false;
                        }
                    }
                }
            }
        }
        None
    }

    /// See [`TaskIndex::live_session`].
    pub(super) fn live_task_session(
        &self,
        workspace: &WorkspaceID,
        conversation: &ConversationID,
        run: &ConversationRunID,
        task: &str,
    ) -> Option<(String, String, u64)> {
        self.sessions()
            .tasks
            .live_session(workspace, conversation, run, task)
    }

    /// The run that holds `key`, if one does. The permission broker reads this
    /// directly, without ordering against queued updates.
    /// A native subagent's key reads its root's, while the run it is bound to
    /// still holds the root.
    /// The run whose prompt the agent is answering on `key`: the holder, or a
    /// run that let go before its answer came (a stopped run). None once the
    /// answer is handled, and for a detached steer's turn, which is no run's.
    pub(super) fn answering_run(&self, key: &SessionKey) -> Option<Arc<TaskOwner>> {
        let sessions = self.sessions();
        match &sessions.entries.get(key)?.state {
            SessionState::Running(holder) if !holder.settled && !holder.detached => {
                Some(holder.owner.clone())
            }
            SessionState::Released(tombstone)
                if !tombstone.settled && tombstone.ended != RunEnd::DetachedSteer =>
            {
                Some(tombstone.owner.clone())
            }
            _ => None,
        }
    }

    pub(super) fn running_context(&self, key: &SessionKey) -> Option<ConversationRunContext> {
        let sessions = self.sessions();
        let (key, bound) = match sessions.children.get(key) {
            Some(child) if child.replay => return None,
            Some(child) => (&child.root, bound_run(child)),
            None => (key, None),
        };
        match &sessions.entries.get(key)?.state {
            SessionState::Running(holder)
                if bound.is_none_or(|bound| *bound == holder.context.run_id) =>
            {
                Some(holder.context.clone())
            }
            _ => None,
        }
    }

    /// A `subagent_spawned` on `parent` announced `child` (rule 9). The child
    /// binds to the run its root routes content to now: the holder, or the
    /// released run; a nested child, to its parent subagent's run. One
    /// announced during replay is history, and so is all it sends.
    pub(super) fn spawn_child(
        &self,
        parent: &SessionKey,
        generation: u64,
        child: &str,
        name: Option<String>,
    ) -> ChildSpawn {
        let mut sessions = self.sessions();
        // A nested subagent belongs to its parent subagent's run, which a
        // later user run on the root never adopts.
        let (root, replay, inherited) = match sessions.children.get(parent) {
            Some(parent) => (
                parent.root.clone(),
                parent.replay,
                Some(parent.bound.clone()),
            ),
            None => (parent.clone(), false, None),
        };
        let bound = match sessions.entries.get(&root) {
            _ if replay => None,
            _ if inherited.is_some() => match inherited.flatten() {
                Some(run) => Some(run),
                None => return ChildSpawn::Drop(DropReason::Unattributed),
            },
            Some(Entry {
                state: SessionState::Replaying { .. },
                ..
            }) => None,
            Some(Entry {
                state: SessionState::Running(holder),
                ..
            }) if holder.detached_output() => return ChildSpawn::Drop(DropReason::DetachedSteer),
            Some(Entry {
                state: SessionState::Running(holder),
                ..
            }) => Some(holder.owner.clone()),
            Some(Entry {
                generation: entry_generation,
                state: SessionState::Released(tombstone),
            }) if *entry_generation == generation && tombstone.adopts() => {
                Some(tombstone.owner.clone())
            }
            Some(Entry {
                generation: entry_generation,
                state: SessionState::Released(tombstone),
            }) if *entry_generation == generation => {
                return ChildSpawn::Drop(tombstone.drop_reason())
            }
            _ => return ChildSpawn::Drop(DropReason::Unattributed),
        };
        let thread_id = crate::server::chat::native_thread_id(child);
        let anchor_call = format!("subagent:{child}");
        sessions.child_order += 1;
        let order = sessions.child_order;
        sessions.children.insert(
            (parent.0.clone(), child.to_owned()),
            Child {
                generation,
                root,
                thread_id: thread_id.clone(),
                anchor_call: anchor_call.clone(),
                name,
                anchor: bound.clone(),
                live: bound.is_some(),
                replay: bound.is_none(),
                bound: bound.clone(),
                order,
            },
        );
        sessions.bound_children(&parent.0);
        match bound {
            Some(_) => ChildSpawn::Record {
                thread_id,
                anchor_call,
            },
            None => ChildSpawn::Drop(DropReason::Replay),
        }
    }

    /// A native subagent reported its end: the anchor call of that run, and
    /// where its state goes. `None` when its spawn was history or never
    /// attributed, or its state was already reported or lost.
    pub(super) fn end_child(&self, key: &SessionKey) -> Option<ChildEnd> {
        let mut sessions = self.sessions();
        let child = sessions.children.get_mut(key)?;
        if child.replay {
            return None;
        }
        child.live = false;
        let owner = child.anchor.take()?;
        let (root, generation, anchor_call) = (
            child.root.clone(),
            child.generation,
            child.anchor_call.clone(),
        );
        // Its end is task activity on the root: a run released just before
        // it still takes the anchor's final state within the grace (rule 5).
        if sessions
            .entries
            .get(&root)
            .is_some_and(|entry| entry.generation == generation)
        {
            sessions.arming(&root, generation).last_task_event = Some(Instant::now());
        }
        // The run holding the anchor still holds the root: the state follows
        // the frames queued before it. Otherwise it revises that run's
        // anchor directly, even once the run has closed.
        let holds = matches!(
            sessions.entries.get(&root),
            Some(Entry { state: SessionState::Running(holder), .. })
                if holder.context.run_id == owner.run_id && !holder.detached_output()
        );
        Some(ChildEnd {
            anchor_call,
            owner: (!holds).then_some(owner),
        })
    }

    /// The thread of the live subagent `subagent` in the tree `key` belongs
    /// to, for the parent's message to it. A finished subagent has none: the
    /// adapter resumes it as a new run, whose anchor carries the message.
    pub(super) fn live_thread(&self, key: &SessionKey, subagent: &str) -> Option<String> {
        let sessions = self.sessions();
        let root = sessions.children.get(key).map_or(key, |child| &child.root);
        sessions
            .children
            .iter()
            .find(|((agent, session), child)| {
                *agent == key.0
                    && child.live
                    && !child.replay
                    && child.root == *root
                    && crate::server::chat::native_subagent_id(session) == subagent
            })
            .map(|(_, child)| child.thread_id.clone())
    }

    /// Where an update from `key` resolves: its own key, or, for a native
    /// subagent, its root's while the run it is bound to can take it.
    pub(super) fn route_child(&self, key: &SessionKey) -> ChildRoute {
        let sessions = self.sessions();
        let Some(child) = sessions.children.get(key) else {
            return ChildRoute::Own;
        };
        if child.replay {
            return ChildRoute::Drop(DropReason::Replay);
        }
        // A later user run holds the root: the child is not its output.
        if let Some(Entry {
            state: SessionState::Running(holder),
            ..
        }) = sessions.entries.get(&child.root)
        {
            if bound_run(child) != Some(&holder.context.run_id) {
                return ChildRoute::Drop(DropReason::AfterEnd);
            }
        }
        ChildRoute::Root {
            key: child.root.clone(),
            thread_id: child.thread_id.clone(),
        }
    }

    /// The thread and name of the native subagent session `key`, for its
    /// permission requests.
    /// Every subagent session whose tree starts at `root`.
    pub(super) fn children_of(&self, root: &SessionKey) -> Vec<SessionKey> {
        self.sessions()
            .children
            .iter()
            .filter(|(_, child)| &child.root == root)
            .map(|(key, _)| key.clone())
            .collect()
    }

    pub(super) fn child_label(&self, key: &SessionKey) -> Option<(String, Option<String>)> {
        let sessions = self.sessions();
        let child = sessions.children.get(key).filter(|child| !child.replay)?;
        Some((child.thread_id.clone(), child.name.clone()))
    }

    /// Where an update for `key` goes, as seen by the forwarder under adapter
    /// `generation`.
    pub(super) fn resolve(&self, key: &SessionKey, generation: u64) -> Resolution {
        let mut sessions = self.sessions();
        if let Some(entry) = sessions.entries.get(key) {
            match &entry.state {
                // A running run's own lifecycle releases it, even across an
                // adapter restart, so its generation is not checked.
                SessionState::Running(holder) if holder.detached_output() => {
                    return Resolution::Drop(DropReason::DetachedSteer)
                }
                SessionState::Running(holder) => {
                    return Resolution::Run(Box::new(holder.context.clone()))
                }
                SessionState::Replaying { .. } => return Resolution::Drop(DropReason::Replay),
                SessionState::Released(tombstone) if entry.generation == generation => {
                    return Resolution::Drop(tombstone.drop_reason())
                }
                SessionState::Released(_) => {
                    sessions.entries.remove(key);
                }
            }
        }
        // Most likely the session being opened, so not lost output.
        if sessions.opening.contains_key(&key.0) {
            Resolution::NotConversation
        } else {
            Resolution::Drop(DropReason::Unattributed)
        }
    }

    /// [`Self::resolve`] for the forwarder: output on an armed key, and
    /// anything after it until its follow-up claims the key, is held for that
    /// follow-up (rules 5 and 6).
    pub(super) fn resolve_update(
        &self,
        key: &SessionKey,
        generation: u64,
        update: AgentUpdate,
    ) -> Resolved {
        {
            let mut sessions = self.sessions();
            let running = sessions
                .entries
                .get(key)
                .is_some_and(|entry| matches!(entry.state, SessionState::Running(_)));
            if !running {
                if let Some(pending) = sessions
                    .follow_ups
                    .get_mut(key)
                    .filter(|pending| pending.generation == generation)
                {
                    return match pending.push(update) {
                        Ok(()) => Resolved::Held,
                        Err(update) => Resolved::Drop(DropReason::ConversationBusy, *update),
                    };
                }
                let armed = match sessions.entries.get(key) {
                    Some(Entry {
                        generation: entry_generation,
                        state: SessionState::Released(tombstone),
                    }) if *entry_generation == generation
                        && opens_follow_up(&update)
                        && sessions.armed(key, generation, tombstone) =>
                    {
                        Some(tombstone.owner.clone())
                    }
                    _ => None,
                };
                if let Some(continues) = armed {
                    let mut pending = PendingFollowUp::new(generation, continues.clone());
                    // A first frame too large to hold would open a run with
                    // nothing in it; drop it like any frame past the bound.
                    if let Err(update) = pending.push(update) {
                        return Resolved::Drop(DropReason::ConversationBusy, *update);
                    }
                    sessions.follow_ups.insert(key.clone(), pending);
                    return Resolved::Open(OpenRequest {
                        key: key.clone(),
                        generation,
                        continues,
                    });
                }
            }
        }
        match self.resolve(key, generation) {
            Resolution::Run(context) => Resolved::Run(context, update),
            Resolution::NotConversation => Resolved::NotConversation,
            Resolution::Drop(reason) => Resolved::Drop(reason, update),
        }
    }

    /// For a permission request on `key` (rule 8): the run holding the key,
    /// else the follow-up opening or about to open on it.
    pub(super) fn join_follow_up(&self, key: &SessionKey, generation: u64) -> FollowUpJoin {
        // A native subagent asks on its own session; its root's run answers.
        let root = match self.route_child(key) {
            ChildRoute::Own => None,
            ChildRoute::Root { key, .. } => Some(key),
            ChildRoute::Drop(reason) => return FollowUpJoin::Unattributed(reason),
        };
        let key = root.as_ref().unwrap_or(key);
        let mut sessions = self.sessions();
        let reason = match sessions.entries.get(key) {
            Some(Entry {
                state: SessionState::Running(holder),
                ..
            }) => {
                return if holder.detached_output() {
                    FollowUpJoin::Unattributed(DropReason::DetachedSteer)
                } else {
                    FollowUpJoin::Running(Box::new(holder.context.clone()))
                }
            }
            Some(Entry {
                state: SessionState::Replaying { .. },
                ..
            }) => return FollowUpJoin::Unattributed(DropReason::Replay),
            Some(Entry {
                generation: entry_generation,
                state: SessionState::Released(tombstone),
            }) if *entry_generation == generation => {
                if let Some(pending) = sessions
                    .follow_ups
                    .get(key)
                    .filter(|pending| pending.generation == generation)
                {
                    return FollowUpJoin::Wait(pending.waiters.subscribe());
                }
                if sessions.armed(key, generation, tombstone) {
                    let continues = tombstone.owner.clone();
                    let pending = PendingFollowUp::new(generation, continues.clone());
                    let waiter = pending.waiters.subscribe();
                    sessions.follow_ups.insert(key.clone(), pending);
                    return FollowUpJoin::Open(
                        OpenRequest {
                            key: key.clone(),
                            generation,
                            continues,
                        },
                        waiter,
                    );
                }
                tombstone.drop_reason()
            }
            _ => DropReason::Unattributed,
        };
        FollowUpJoin::Unattributed(reason)
    }

    /// Another run of the conversation is active, so the follow-up on `key`
    /// waits for that run's removal. Called under the runner's run lock.
    pub(super) fn block_follow_up(&self, key: &SessionKey) {
        if let Some(pending) = self.sessions().follow_ups.get_mut(key) {
            pending.blocked = true;
        }
    }

    /// A run of `conversation_id` left: the follow-ups it blocked open now.
    /// Each continues whichever run last released its key. One that aged out
    /// is dropped.
    pub(super) fn unblock_follow_ups(&self, conversation_id: &ConversationID) -> Vec<OpenRequest> {
        let mut sessions = self.sessions();
        let blocked: Vec<SessionKey> = sessions
            .follow_ups
            .iter()
            .filter(|(_, pending)| {
                pending.blocked && pending.continues.conversation_id == *conversation_id
            })
            .map(|(key, _)| key.clone())
            .collect();
        let mut requests = Vec::new();
        for key in blocked {
            if sessions.follow_ups[&key].expired() {
                let pending = sessions.follow_ups.remove(&key).expect("pending follow-up");
                self.count_dropped(&key, DropReason::ConversationBusy, pending.frames.len());
                continue;
            }
            let latest = match sessions.entries.get(&key).map(|entry| &entry.state) {
                Some(SessionState::Released(tombstone)) => Some(tombstone.owner.clone()),
                _ => None,
            };
            let pending = sessions
                .follow_ups
                .get_mut(&key)
                .expect("pending follow-up");
            pending.blocked = false;
            if let Some(latest) = latest {
                pending.continues = latest;
            }
            requests.push(OpenRequest {
                key,
                generation: pending.generation,
                continues: pending.continues.clone(),
            });
        }
        requests
    }

    /// The follow-up on `key` cannot open: its frames are dropped as
    /// `reason`. A session that is gone also disarms the key, so its next
    /// frames do not retry.
    pub(super) fn fail_follow_up(&self, key: &SessionKey, reason: DropReason, disarm: bool) {
        let mut sessions = self.sessions();
        let Some(pending) = sessions.follow_ups.remove(key) else {
            return;
        };
        if disarm {
            sessions.arming(key, pending.generation).disarmed = true;
        }
        drop(sessions);
        self.count_dropped(key, reason, pending.frames.len().max(1));
    }

    /// Counts a permission request no run could take, as an update would be.
    pub(super) fn record_permission_drop(&self, key: &SessionKey, reason: DropReason) {
        self.count_dropped(key, reason, 1);
    }

    fn count_dropped(&self, key: &SessionKey, reason: DropReason, frames: usize) {
        let mut occurrences = 0;
        for _ in 0..frames {
            occurrences = self.drops.record(&key.0, reason.as_str()).1;
        }
        if reason.warns() && frames > 0 {
            tracing::warn!(
                agent_id = %key.0,
                session_id = %key.1,
                frames,
                reason = reason.as_str(),
                occurrences,
                "follow-up output was dropped"
            );
        }
    }

    /// Places a background-task lifecycle frame by its task's owner, before
    /// the key resolves (rule 3). Anything else comes back `Unowned`.
    pub(super) fn route_lifecycle(
        &self,
        key: &SessionKey,
        generation: u64,
        update: AgentUpdate,
    ) -> Route {
        if !is_lifecycle(&update) {
            return Route::Unowned(Box::new(update));
        }
        let mut sessions = self.sessions();
        let stamp = task_event(&update)
            && sessions.entries.get(key).is_some_and(|entry| {
                entry.generation == generation
                    && !matches!(entry.state, SessionState::Replaying { .. })
            });
        if stamp {
            sessions.arming(key, generation).last_task_event = Some(Instant::now());
        }
        let Sessions {
            entries,
            opening,
            tasks,
            ..
        } = &mut *sessions;
        let view = match entries.get(key) {
            Some(entry) => match &entry.state {
                SessionState::Running(holder) if holder.detached_output() => KeyView::Detached,
                SessionState::Running(holder) => KeyView::Running(&holder.owner),
                SessionState::Replaying { .. } => KeyView::Replaying,
                SessionState::Released(tombstone) if entry.generation == generation => {
                    KeyView::Released(tombstone.adopts().then_some(&tombstone.owner))
                }
                SessionState::Released(_) => KeyView::Absent,
            },
            None if opening.contains_key(&key.0) => KeyView::NotConversation,
            None => KeyView::Absent,
        };
        tasks.route(key, generation, view, update)
    }

    /// Indexes a tool call that `resolve` gave `run_id`, so its task's later
    /// frames reach that run.
    pub(super) fn index_tool_call(
        &self,
        key: &SessionKey,
        generation: u64,
        run_id: &ConversationRunID,
        payload: &serde_json::Value,
    ) {
        let mut sessions = self.sessions();
        let Sessions { entries, tasks, .. } = &mut *sessions;
        if let Some(Entry {
            state: SessionState::Running(holder),
            ..
        }) = entries.get(key)
        {
            if holder.context.run_id == *run_id && !holder.detached_output() {
                tasks.index_tool_call(key, generation, &holder.owner, payload);
            }
        }
    }

    /// Takes the live tasks `lost` selects by agent, session, and adapter
    /// generation, for core to mark lost (rule 7), with the subagents whose
    /// root session it selects: their threads read as disconnected.
    pub(super) fn take_lost_tasks(
        &self,
        mut lost: impl FnMut(&str, &str, u64) -> bool,
    ) -> Vec<LostTask> {
        let mut sessions = self.sessions();
        let mut tasks = sessions.tasks.take_lost(&mut lost);
        for ((agent, _), child) in sessions.children.iter_mut() {
            if child.anchor.is_none() || !lost(agent, &child.root.1, child.generation) {
                continue;
            }
            child.live = false;
            tasks.extend(child.anchor.take().map(|owner| LostTask {
                owner,
                tool_call_id: Some(child.anchor_call.clone()),
                async_task_id: None,
            }));
        }
        tasks
    }

    /// Drops every key state and indexed task once the forwarders have
    /// stopped. Tombstones and index entries hold transcript handles, and the
    /// server waits for every outbound handle to drop before it exits. Nothing
    /// is marked lost: the next core reads a stored running task as lost
    /// (rule 7).
    pub(super) fn clear(&self) {
        let mut sessions = self.sessions();
        sessions.entries.clear();
        sessions.children.clear();
        sessions.arming.clear();
        sessions.follow_ups.clear();
        sessions.tasks = TaskIndex::default();
    }

    /// Hold the result until a run claims the new session.
    pub(super) fn opening(&self, agent_id: &AgentId) -> OpeningSession {
        *self
            .sessions()
            .opening
            .entry(agent_id.0.clone())
            .or_default() += 1;
        OpeningSession {
            attribution: self.clone(),
            agent_id: agent_id.0.clone(),
        }
    }

    pub(super) fn forget_conversation(&self, conversation_id: &ConversationID) {
        let mut sessions = self.sessions();
        sessions.entries.retain(|_, entry| {
            !matches!(&entry.state, SessionState::Released(tombstone)
                if tombstone.owner.conversation_id == *conversation_id)
        });
        sessions
            .follow_ups
            .retain(|_, pending| pending.continues.conversation_id != *conversation_id);
        sessions.tasks.forget_conversation(conversation_id);
        sessions.prune_children();
    }

    pub(super) fn forget_project(&self, workspace_id: &WorkspaceID, project_id: &ProjectID) {
        let mut sessions = self.sessions();
        sessions.entries.retain(|_, entry| {
            !matches!(&entry.state, SessionState::Released(tombstone)
                if tombstone.owner.workspace_id == *workspace_id
                    && tombstone.owner.project_id.as_ref() == Some(project_id))
        });
        sessions.follow_ups.retain(|_, pending| {
            pending.continues.workspace_id != *workspace_id
                || pending.continues.project_id.as_ref() != Some(project_id)
        });
        sessions.tasks.forget_project(workspace_id, project_id);
        sessions.prune_children();
    }

    /// Counts the drop and logs it rate-limited. Logs carry labels and ids,
    /// never the update's content.
    pub(super) fn record_drop(
        &self,
        agent_id: &AgentId,
        session_id: &HarnessSessionId,
        reason: DropReason,
        update: &AgentUpdate,
    ) {
        let (_, occurrences) = self.drops.record(&agent_id.0, reason.as_str());
        if reason == DropReason::Replay || !is_log_point(occurrences) {
            return;
        }
        let update_kind = update_kind(update);
        if reason.warns() && !is_metadata(update) {
            tracing::warn!(
                agent_id = %agent_id.0,
                session_id = %session_id.0,
                update_kind,
                reason = reason.as_str(),
                occurrences,
                "agent update arrived with no run to record it"
            );
        } else {
            tracing::debug!(
                agent_id = %agent_id.0,
                session_id = %session_id.0,
                update_kind,
                reason = reason.as_str(),
                occurrences,
                "agent update arrived with no run to record it"
            );
        }
    }

    #[cfg(test)]
    pub(super) fn dropped(&self, agent_id: &str, reason: DropReason) -> u64 {
        self.drops.count(agent_id, reason.as_str())
    }

    #[cfg(test)]
    pub(super) fn running_keys(&self) -> Vec<(SessionKey, ConversationRunID)> {
        self.sessions()
            .entries
            .iter()
            .filter_map(|(key, entry)| match &entry.state {
                SessionState::Running(holder) => Some((key.clone(), holder.context.run_id.clone())),
                _ => None,
            })
            .collect()
    }

    /// A live subagent is live work even after the run it spawned in ends
    /// (rule 9): a second adapter process would contend with it.
    pub(super) fn agent_has_live_work(&self, agent_id: &str) -> bool {
        let sessions = self.sessions();
        sessions.tasks.has_live_agent(agent_id)
            || sessions.entries.iter().any(|(key, entry)| {
                key.0 == agent_id && matches!(entry.state, SessionState::Running(_))
            })
            || sessions
                .children
                .iter()
                .any(|((agent, _), child)| agent == agent_id && child.live && !child.replay)
    }

    #[cfg(test)]
    pub(super) fn indexed_tasks(&self, agent_id: &str) -> usize {
        self.sessions().tasks.len(agent_id)
    }

    #[cfg(test)]
    pub(super) fn has_tombstone(&self, key: &SessionKey) -> bool {
        self.sessions()
            .entries
            .get(key)
            .is_some_and(|entry| matches!(entry.state, SessionState::Released(_)))
    }
}

fn update_kind(update: &AgentUpdate) -> &str {
    match update {
        AgentUpdate::SessionSignal { .. } => "session_signal",
        AgentUpdate::RichMessageChunk { .. } => "rich_message_chunk",
        AgentUpdate::UserMessageChunk { .. } => "user_message_chunk",
        AgentUpdate::AgentMessageChunk { .. } => "agent_message_chunk",
        AgentUpdate::AgentThoughtChunk { .. } => "agent_thought_chunk",
        AgentUpdate::AgentImageChunk { .. } => "agent_image_chunk",
        AgentUpdate::ToolCall { .. } => "tool_call",
        AgentUpdate::ToolCallUpdate { .. } => "tool_call_update",
        AgentUpdate::Plan { .. } => "plan",
        AgentUpdate::StreamGap { .. } => "stream_gap",
        AgentUpdate::AsyncTaskSpawned { .. } => "async_task_spawned",
        AgentUpdate::AsyncTaskProgress { .. } => "async_task_progress",
        AgentUpdate::AsyncTaskStateUpdate { .. } => "async_task_state_update",
        AgentUpdate::SubagentSpawned { .. } => "subagent_spawned",
        AgentUpdate::SubagentStateUpdate { .. } => "subagent_state_update",
        AgentUpdate::Other { update_kind, .. } => {
            let end = update_kind
                .char_indices()
                .nth(64)
                .map_or(update_kind.len(), |(index, _)| index);
            &update_kind[..end]
        }
    }
}

/// Session state rather than turn output: titles, usage, modes, commands,
/// config, signals, and gap markers. Never worth a warning on its own.
fn is_metadata(update: &AgentUpdate) -> bool {
    matches!(
        update,
        AgentUpdate::SessionSignal { .. }
            | AgentUpdate::StreamGap { .. }
            | AgentUpdate::Other { .. }
    )
}
