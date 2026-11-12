//! Which run owns each background task, and each tool call that can start
//! one. Implements rule 3 of "Attribution And Lifetime" in
//! `docs/architecture/backend/acp-attribution.md`.
//!
//! The forwarder indexes each tool call it records on a running run, and a
//! task spawn that names no tool call yet. Lifecycle frames for an indexed
//! call or task go to its run whoever holds the session key by then, so a task
//! that outlives its turn still updates the tool that started it. The index
//! lives inside the attribution lock: a lookup sees the key state the
//! forwarder resolves against.

use crate::server::chat::ChatService;
use ottam_protocol::{
    AgentUpdate, AsyncTaskState, ConversationID, ConversationRunID, ProjectID, RunOrigin,
    WorkspaceID,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Entries one agent may hold. Past this, its oldest ended entry goes; live
/// ones are never evicted.
const AGENT_ENTRIES: usize = 4096;

/// The run a task belongs to, and what writing its transcript after the run
/// closed needs.
pub(super) struct TaskOwner {
    pub(super) chat: ChatService,
    pub(super) workspace_id: WorkspaceID,
    pub(super) project_id: Option<ProjectID>,
    pub(super) conversation_id: ConversationID,
    pub(super) run_id: ConversationRunID,
}

/// A live task whose end core can no longer expect.
pub(super) struct LostTask {
    pub(super) owner: Arc<TaskOwner>,
    pub(super) tool_call_id: Option<String>,
    pub(super) async_task_id: Option<String>,
}

/// What frames for a task said before its tool call was known. The transcript
/// keeps only state and output path, so nothing else is held.
#[derive(Default)]
struct Held {
    state: Option<AsyncTaskState>,
    output_file_path: Option<String>,
}

struct Entry {
    agent: String,
    session: String,
    /// The session the task runs in when it is not `session`: a native
    /// subagent's task routes by its root's key but stops on its own.
    stop_session: Option<String>,
    generation: u64,
    owner: Arc<TaskOwner>,
    tool_call_id: Option<String>,
    task_id: Option<String>,
    /// The tool shows a background task, so a later stamp or spawn leaves
    /// its state alone.
    started: bool,
    /// Stamped or spawned, and not seen to end.
    live: bool,
    held: Option<Held>,
    /// Claims of the session by other runs since this entry's run held it.
    claims: u8,
    order: u64,
}

impl Entry {
    /// Tracks the task as the transcript records it: a stamp, spawn, or
    /// progress starts a task only on a tool that shows none, and a state
    /// sets it. Claude's stamp after a terminal state leaves it ended.
    fn observe(&mut self, update: &AgentUpdate) {
        match update {
            AgentUpdate::ToolCallUpdate { payload } if is_backgrounded(payload) => self.start(),
            AgentUpdate::AsyncTaskSpawned { .. } | AgentUpdate::AsyncTaskProgress { .. } => {
                self.start()
            }
            AgentUpdate::AsyncTaskStateUpdate { state, .. } => {
                self.started = true;
                self.live = state.is_live();
            }
            _ => {}
        }
    }

    fn start(&mut self) {
        if !self.started {
            self.started = true;
            self.live = true;
        }
    }
}

/// Where a lifecycle frame goes.
pub(super) enum Route {
    /// Not the index's to place: the key's resolution takes it.
    Unowned(Box<AgentUpdate>),
    /// To the owning run, in order. Two frames when an anchoring frame
    /// releases what was held for its task.
    Owner(Arc<TaskOwner>, Vec<AgentUpdate>),
    /// Held until a frame names the task's tool call.
    Held,
}

/// The frame's session key, as the attribution lock sees it.
pub(super) enum KeyView<'a> {
    /// A run holds the key and its output is its own.
    Running(&'a Arc<TaskOwner>),
    /// A run holds the key, but output now comes from a detached steer turn.
    Detached,
    /// The key's run let go. Carries its owner when output after the release
    /// is still that run's.
    Released(Option<&'a Arc<TaskOwner>>),
    /// `session/load` is replaying the session's history.
    Replaying,
    /// A session still being opened, which no run has claimed.
    NotConversation,
    /// Core has no state for this session.
    Absent,
}

/// Entry ids by agent and a tool call or task id. ACP ids are unique only
/// within a session, so one id can hold an entry per session.
type Ids = HashMap<(String, String), Vec<u64>>;

#[derive(Default)]
pub(super) struct TaskIndex {
    entries: HashMap<u64, Entry>,
    by_call: Ids,
    by_task: Ids,
    per_agent: HashMap<String, usize>,
    next: u64,
}

fn link(ids: &mut Ids, key: (String, String), id: u64) {
    let bucket = ids.entry(key).or_default();
    if !bucket.contains(&id) {
        bucket.push(id);
    }
}

fn unlink(ids: &mut Ids, key: &(String, String), id: u64) {
    if let Some(bucket) = ids.get_mut(key) {
        bucket.retain(|other| *other != id);
        if bucket.is_empty() {
            ids.remove(key);
        }
    }
}

/// Frames the index may place: `async_task_*`, and tool updates, which can
/// carry the background stamp.
pub(super) fn is_lifecycle(update: &AgentUpdate) -> bool {
    matches!(
        update,
        AgentUpdate::AsyncTaskSpawned { .. }
            | AgentUpdate::AsyncTaskProgress { .. }
            | AgentUpdate::AsyncTaskStateUpdate { .. }
            | AgentUpdate::ToolCallUpdate { .. }
    )
}

fn is_backgrounded(payload: &Value) -> bool {
    payload
        .pointer("/_meta/jetbrains/air/asyncTasks/backgrounded")
        .and_then(Value::as_bool)
        == Some(true)
}

/// The task an `async_task_*` frame names, and its tool call slot.
fn task_frame(update: &mut AgentUpdate) -> Option<(&str, &mut Option<String>)> {
    match update {
        AgentUpdate::AsyncTaskSpawned {
            async_task_id,
            provider_tool_call_id,
            ..
        }
        | AgentUpdate::AsyncTaskProgress {
            async_task_id,
            provider_tool_call_id,
            ..
        }
        | AgentUpdate::AsyncTaskStateUpdate {
            async_task_id,
            provider_tool_call_id,
            ..
        } => Some((async_task_id.as_str(), provider_tool_call_id)),
        _ => None,
    }
}

impl TaskIndex {
    /// Records a tool call the forwarder routed to `owner`, a running run
    /// whose output is its own. A call that arrives stamped is live already.
    pub(super) fn index_tool_call(
        &mut self,
        key: &(String, String),
        generation: u64,
        owner: &Arc<TaskOwner>,
        payload: &Value,
    ) {
        let Some(tool_call_id) = payload.get("toolCallId").and_then(Value::as_str) else {
            return;
        };
        let live = is_backgrounded(payload);
        let call = (key.0.clone(), tool_call_id.to_owned());
        if let Some(id) = self.pick(&self.by_call, &call, &key.1, None, false) {
            let entry = self.entries.get_mut(&id).expect("indexed entry");
            if entry.generation == generation && entry.owner.run_id == owner.run_id {
                if live {
                    entry.start();
                }
                return;
            }
            // An id the session reused in another run or adapter process: the
            // newest call is the one later frames mean.
            self.remove(id);
        }
        let stop_session = payload
            .get("ottamSessionID")
            .and_then(Value::as_str)
            .and_then(|tagged| tagged.strip_prefix(&format!("{}:", key.0)))
            .filter(|session| *session != key.1)
            .map(str::to_owned);
        if let Some(id) = self.insert(Entry {
            agent: key.0.clone(),
            session: key.1.clone(),
            stop_session,
            generation,
            owner: owner.clone(),
            tool_call_id: Some(tool_call_id.to_owned()),
            task_id: None,
            started: live,
            live,
            held: None,
            claims: 0,
            order: 0,
        }) {
            link(&mut self.by_call, call, id);
        }
    }

    /// The entry `ids` holds under `key` in `session`, or with `agent_wide`
    /// the newest in any session, limited to `generation` when given.
    fn pick(
        &self,
        ids: &Ids,
        key: &(String, String),
        session: &str,
        generation: Option<u64>,
        agent_wide: bool,
    ) -> Option<u64> {
        ids.get(key)?
            .iter()
            .filter_map(|id| Some((*id, self.entries.get(id)?)))
            .filter(|(_, entry)| {
                (agent_wide || entry.session == session)
                    && generation.is_none_or(|generation| entry.generation == generation)
            })
            .max_by_key(|(_, entry)| entry.order)
            .map(|(id, _)| id)
    }

    /// Places a lifecycle frame by the run that owns its tool call or task.
    pub(super) fn route(
        &mut self,
        key: &(String, String),
        generation: u64,
        view: KeyView<'_>,
        mut update: AgentUpdate,
    ) -> Route {
        // Rule 10: a replay records nothing. A session still being opened is
        // not conversation traffic yet.
        let (holder, adopter, agent_wide) = match view {
            KeyView::Replaying | KeyView::NotConversation => {
                return Route::Unowned(Box::new(update))
            }
            KeyView::Running(owner) => (Some(owner), Some(owner), false),
            KeyView::Detached => (None, None, false),
            KeyView::Released(owner) => (None, owner, false),
            KeyView::Absent => (None, None, true),
        };
        let Some(id) = self.find(key, generation, &mut update, agent_wide) else {
            return self.unindexed(key, generation, adopter, update);
        };
        let entry = self.entries.get_mut(&id).expect("indexed entry");
        entry.observe(&update);
        let Some((task, call)) = task_frame(&mut update) else {
            return Self::owned(entry, holder, update);
        };
        let task = task.to_owned();
        if call.is_none() {
            call.clone_from(&entry.tool_call_id);
        }
        let Some(call) = call.clone() else {
            // Still no tool call to anchor on: keep what the frame says.
            let held = entry.held.get_or_insert_with(Held::default);
            match &update {
                AgentUpdate::AsyncTaskStateUpdate {
                    state,
                    output_file_path,
                    ..
                } => {
                    held.state = Some(*state);
                    if output_file_path.is_some() {
                        held.output_file_path.clone_from(output_file_path);
                    }
                }
                AgentUpdate::AsyncTaskSpawned {
                    output_file_path, ..
                }
                | AgentUpdate::AsyncTaskProgress {
                    output_file_path, ..
                } if output_file_path.is_some() => {
                    held.output_file_path.clone_from(output_file_path);
                }
                _ => {}
            }
            return Route::Held;
        };
        let Some(held) = entry.held.take() else {
            return Self::owned(entry, holder, update);
        };
        let owner = self.anchor(id, &task, &call);
        // One frame stands for everything held: the spawn, and the last
        // state seen before the tool call was known.
        let replay = match held.state {
            Some(state) => AgentUpdate::AsyncTaskStateUpdate {
                async_task_id: task,
                state,
                provider_tool_call_id: Some(call.clone()),
                summary: None,
                output_file_path: held.output_file_path,
            },
            None => AgentUpdate::AsyncTaskSpawned {
                async_task_id: task,
                provider_tool_call_id: Some(call.clone()),
                name: None,
                task_type: None,
                description: None,
                output_file_path: held.output_file_path,
                show_in_transcript: None,
                can_stop: None,
            },
        };
        Route::Owner(owner, vec![replay, update])
    }

    /// Links held entry `id` to its tool call and returns the run whose tool
    /// it is. When the forwarder indexed that call before the task was known,
    /// the call's entry takes the task and `id` goes, so one entry tracks
    /// each tool.
    fn anchor(&mut self, id: u64, task: &str, call: &str) -> Arc<TaskOwner> {
        let entry = &self.entries[&id];
        let (agent, session, generation, live) = (
            entry.agent.clone(),
            entry.session.clone(),
            entry.generation,
            entry.live,
        );
        let call = (agent.clone(), call.to_owned());
        let indexed = self
            .pick(&self.by_call, &call, &session, None, false)
            .filter(|other| *other != id);
        if let Some(other) = indexed {
            if self.entries[&other].generation != generation {
                self.remove(other);
            } else {
                self.remove(id);
                let tool = self.entries.get_mut(&other).expect("indexed entry");
                if let Some(prior) = tool.task_id.replace(task.to_owned()) {
                    unlink(&mut self.by_task, &(agent.clone(), prior), other);
                }
                // The held frames saw the task; the tool call's stamp did not.
                tool.started = true;
                tool.live = live;
                link(&mut self.by_task, (agent, task.to_owned()), other);
                return tool.owner.clone();
            }
        }
        let entry = self.entries.get_mut(&id).expect("indexed entry");
        entry.tool_call_id = Some(call.1.clone());
        link(&mut self.by_call, call, id);
        entry.owner.clone()
    }

    /// The holder's own frames take the ordinary path, where a storage error
    /// fails its run as any other update's would. The index places only what
    /// the holder would not take.
    fn owned(entry: &Entry, holder: Option<&Arc<TaskOwner>>, update: AgentUpdate) -> Route {
        if holder.is_some_and(|holder| holder.run_id == entry.owner.run_id) {
            Route::Unowned(Box::new(update))
        } else {
            Route::Owner(entry.owner.clone(), vec![update])
        }
    }

    /// The entry a frame names: by task id, else by tool call id, in the
    /// frame's session unless core has no state for it, and in its adapter
    /// generation.
    fn find(
        &mut self,
        key: &(String, String),
        generation: u64,
        update: &mut AgentUpdate,
        agent_wide: bool,
    ) -> Option<u64> {
        let (task, call) = match update {
            AgentUpdate::ToolCallUpdate { payload } => (
                None,
                payload
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            ),
            _ => match task_frame(update) {
                Some((task, call)) => (Some(task.to_owned()), call.clone()),
                None => (None, None),
            },
        };
        let lookup = |ids: &Ids, id: &Option<String>| {
            let id = (key.0.clone(), id.clone()?);
            self.pick(ids, &id, &key.1, Some(generation), agent_wide)
        };
        if let Some(id) = lookup(&self.by_task, &task) {
            return Some(id);
        }
        let id = lookup(&self.by_call, &call)?;
        if let Some(task) = task {
            let entry = self.entries.get_mut(&id).expect("indexed entry");
            if entry.task_id.is_none() {
                entry.task_id = Some(task.clone());
                link(&mut self.by_task, (key.0.clone(), task), id);
            }
        }
        Some(id)
    }

    /// A frame no entry names. A spawn with no tool call is held for the run
    /// whose output it is; anything else is the key's to resolve.
    fn unindexed(
        &mut self,
        key: &(String, String),
        generation: u64,
        adopter: Option<&Arc<TaskOwner>>,
        update: AgentUpdate,
    ) -> Route {
        let (
            AgentUpdate::AsyncTaskSpawned {
                async_task_id,
                provider_tool_call_id: None,
                output_file_path,
                ..
            },
            Some(owner),
        ) = (&update, adopter)
        else {
            return Route::Unowned(Box::new(update));
        };
        let task = (key.0.clone(), async_task_id.clone());
        let Some(id) = self.insert(Entry {
            agent: key.0.clone(),
            session: key.1.clone(),
            stop_session: None,
            generation,
            owner: owner.clone(),
            tool_call_id: None,
            task_id: Some(async_task_id.clone()),
            started: true,
            live: true,
            held: Some(Held {
                state: None,
                output_file_path: output_file_path.clone(),
            }),
            claims: 0,
            order: 0,
        }) else {
            // Full of live tasks: nothing holds it, so the key resolves it
            // and counts the drop.
            return Route::Unowned(Box::new(update));
        };
        link(&mut self.by_task, task, id);
        Route::Held
    }

    fn insert(&mut self, mut entry: Entry) -> Option<u64> {
        if self.len(&entry.agent) >= AGENT_ENTRIES {
            let oldest = self
                .entries
                .iter()
                .filter(|(_, held)| held.agent == entry.agent && !held.live)
                .min_by_key(|(_, held)| held.order)
                .map(|(id, _)| *id);
            let Some(oldest) = oldest else {
                tracing::debug!(agent_id = %entry.agent, "background task index is full of live tasks");
                return None;
            };
            self.remove(oldest);
        }
        let id = self.next;
        self.next += 1;
        entry.order = id;
        *self.per_agent.entry(entry.agent.clone()).or_default() += 1;
        self.entries.insert(id, entry);
        Some(id)
    }

    fn remove(&mut self, id: u64) {
        let Some(entry) = self.entries.remove(&id) else {
            return;
        };
        for (ids, key) in [
            (&mut self.by_call, entry.tool_call_id),
            (&mut self.by_task, entry.task_id),
        ] {
            if let Some(key) = key {
                unlink(ids, &(entry.agent.clone(), key), id);
            }
        }
        if let Some(count) = self.per_agent.get_mut(&entry.agent) {
            *count -= 1;
            if *count == 0 {
                self.per_agent.remove(&entry.agent);
            }
        }
    }

    fn retain(&mut self, mut keep: impl FnMut(&mut Entry) -> bool) {
        let gone: Vec<u64> = self
            .entries
            .iter_mut()
            .filter_map(|(id, entry)| (!keep(entry)).then_some(*id))
            .collect();
        for id in gone {
            self.remove(id);
        }
    }

    /// `run` claimed `key` under `generation`. Other runs' ended entries on
    /// the key survive one user claim, since a follow-up input can claim
    /// before Codex's post-turn frames arrive. An autonomous claim spends none
    /// of that grace: the user's run after it still gets the one claim. An
    /// earlier generation's ended entries go. Live entries stay until their
    /// task ends or is lost.
    pub(super) fn claimed(
        &mut self,
        key: &(String, String),
        run: &ConversationRunID,
        origin: RunOrigin,
        generation: u64,
    ) {
        self.retain(|entry| {
            if entry.agent != key.0 || entry.live {
                return true;
            }
            if entry.generation < generation {
                return false;
            }
            if entry.session != key.1 || entry.owner.run_id == *run {
                return true;
            }
            if origin == RunOrigin::User {
                entry.claims = entry.claims.saturating_add(1);
            }
            entry.claims < 2
        });
    }

    pub(super) fn forget_conversation(&mut self, conversation: &ConversationID) {
        self.retain(|entry| entry.owner.conversation_id != *conversation);
    }

    pub(super) fn has_live_agent(&self, agent: &str) -> bool {
        self.entries
            .values()
            .any(|entry| entry.agent == agent && entry.live)
    }

    /// Ended entries of the project go; a live task still reports its end.
    pub(super) fn forget_project(&mut self, workspace: &WorkspaceID, project: &ProjectID) {
        self.retain(|entry| {
            entry.live
                || entry.owner.workspace_id != *workspace
                || entry.owner.project_id.as_ref() != Some(project)
        });
    }

    /// Takes the live tasks `lost` selects by agent, session, and generation.
    pub(super) fn take_lost(
        &mut self,
        mut lost: impl FnMut(&str, &str, u64) -> bool,
    ) -> Vec<LostTask> {
        let mut tasks = Vec::new();
        self.retain(|entry| {
            if !entry.live || !lost(&entry.agent, &entry.session, entry.generation) {
                return true;
            }
            tasks.push(LostTask {
                owner: entry.owner.clone(),
                tool_call_id: entry.tool_call_id.clone(),
                async_task_id: entry.task_id.clone(),
            });
            false
        });
        tasks
    }

    /// Whether a task started in the session under `generation` is still
    /// live, whichever run owns it (rule 5).
    pub(super) fn has_live(&self, agent: &str, session: &str, generation: u64) -> bool {
        self.entries.values().any(|entry| {
            entry.live
                && entry.agent == agent
                && entry.session == session
                && entry.generation == generation
        })
    }

    /// The session a live task of `run` runs in, as `(agent, session,
    /// generation)`. Task ids are scoped to their session, so the run that
    /// recorded the task names which one; a subagent's task runs in the
    /// subagent's session.
    pub(super) fn live_session(
        &self,
        workspace: &WorkspaceID,
        conversation: &ConversationID,
        run: &ConversationRunID,
        task: &str,
    ) -> Option<(String, String, u64)> {
        self.entries
            .values()
            .find(|entry| {
                entry.live
                    && entry.task_id.as_deref() == Some(task)
                    && entry.owner.run_id == *run
                    && entry.owner.conversation_id == *conversation
                    && entry.owner.workspace_id == *workspace
            })
            .map(|entry| {
                let session = entry.stop_session.as_ref().unwrap_or(&entry.session);
                (entry.agent.clone(), session.clone(), entry.generation)
            })
    }

    pub(super) fn len(&self, agent: &str) -> usize {
        self.per_agent.get(agent).copied().unwrap_or(0)
    }
}

/// Marks each task lost on its run's transcript (rule 7). Errors are logged,
/// never raised: the run may be long finished. The writes run on a task of
/// their own: the index no longer holds these tasks, so a forwarder or run
/// aborted while waiting must not drop the rest.
pub(super) async fn record_lost(tasks: Vec<LostTask>) {
    if tasks.is_empty() {
        return;
    }
    let writes = tokio::spawn(async move {
        for task in tasks {
            if let Err(error) = task
                .owner
                .chat
                .record_task_lost(
                    &task.owner.conversation_id,
                    &task.owner.run_id,
                    task.tool_call_id,
                    task.async_task_id,
                )
                .await
            {
                tracing::warn!(
                    run_id = %task.owner.run_id.0,
                    kind = %error.kind,
                    "Could not mark a background task lost"
                );
            }
        }
    });
    let _ = writes.await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Database;
    use serde_json::json;

    const KEY: (&str, &str) = ("codex", "session");

    fn key() -> (String, String) {
        (KEY.0.to_owned(), KEY.1.to_owned())
    }

    fn owners(runs: &[&str]) -> (tempfile::TempDir, Vec<Arc<TaskOwner>>) {
        let home = tempfile::TempDir::new().unwrap();
        let chat = ChatService::new(
            Database::open_test_home(home.path().to_path_buf()).unwrap(),
            WorkspaceID("workspace".into()),
        );
        let owners = runs
            .iter()
            .map(|run| {
                Arc::new(TaskOwner {
                    chat: chat.clone(),
                    workspace_id: WorkspaceID("workspace".into()),
                    project_id: None,
                    conversation_id: ConversationID("conversation".into()),
                    run_id: ConversationRunID((*run).into()),
                })
            })
            .collect();
        (home, owners)
    }

    fn call(id: &str, stamped: bool) -> Value {
        let mut payload = json!({"toolCallId": id});
        if stamped {
            payload["_meta"] =
                json!({"jetbrains": {"air": {"asyncTasks": {"backgrounded": true}}}});
        }
        payload
    }

    fn stamp(id: &str) -> AgentUpdate {
        AgentUpdate::ToolCallUpdate {
            payload: call(id, true),
        }
    }

    fn spawn(task: &str, call: Option<&str>) -> AgentUpdate {
        AgentUpdate::AsyncTaskSpawned {
            async_task_id: task.into(),
            provider_tool_call_id: call.map(Into::into),
            name: None,
            task_type: None,
            description: None,
            output_file_path: None,
            show_in_transcript: None,
            can_stop: None,
        }
    }

    fn state(task: &str, state: AsyncTaskState, call: Option<&str>) -> AgentUpdate {
        AgentUpdate::AsyncTaskStateUpdate {
            async_task_id: task.into(),
            state,
            provider_tool_call_id: call.map(Into::into),
            summary: None,
            output_file_path: None,
        }
    }

    /// The run a tool update for `id` reaches once its run let go.
    fn owner_of(index: &mut TaskIndex, id: &str, generation: u64) -> Option<String> {
        let update = AgentUpdate::ToolCallUpdate {
            payload: call(id, false),
        };
        match index.route(&key(), generation, KeyView::Released(None), update) {
            Route::Owner(owner, _) => Some(owner.run_id.0.clone()),
            _ => None,
        }
    }

    #[test]
    fn task_index_cap_evicts_the_oldest_ended_entry_never_a_live_one() {
        let (_home, owners) = owners(&["run"]);
        let mut index = TaskIndex::default();
        index.index_tool_call(&key(), 1, &owners[0], &call("live", true));
        for n in 1..AGENT_ENTRIES {
            index.index_tool_call(&key(), 1, &owners[0], &call(&format!("ended-{n}"), false));
        }
        index.index_tool_call(&key(), 1, &owners[0], &call("newest", false));
        assert_eq!(index.len(KEY.0), AGENT_ENTRIES);
        assert_eq!(owner_of(&mut index, "live", 1).as_deref(), Some("run"));
        assert_eq!(owner_of(&mut index, "ended-1", 1), None);
        assert_eq!(owner_of(&mut index, "ended-2", 1).as_deref(), Some("run"));
        assert_eq!(owner_of(&mut index, "newest", 1).as_deref(), Some("run"));
    }

    #[test]
    fn task_index_keeps_ended_entries_one_claim_and_live_ones_until_lost() {
        let (_home, owners) = owners(&["a", "b", "c"]);
        let mut index = TaskIndex::default();
        index.index_tool_call(&key(), 1, &owners[0], &call("ended", false));
        index.index_tool_call(&key(), 1, &owners[0], &call("live", true));
        // Codex's post-turn frames can follow the next run's claim.
        index.claimed(&key(), &owners[1].run_id, RunOrigin::User, 1);
        assert_eq!(owner_of(&mut index, "ended", 1).as_deref(), Some("a"));
        index.claimed(&key(), &owners[2].run_id, RunOrigin::User, 1);
        assert_eq!(owner_of(&mut index, "ended", 1), None);
        assert_eq!(owner_of(&mut index, "live", 1).as_deref(), Some("a"));

        let lost = index.take_lost(|_, session, _| session == KEY.1);
        assert_eq!(lost.len(), 1);
        assert_eq!(lost[0].tool_call_id.as_deref(), Some("live"));
        assert_eq!(lost[0].owner.run_id.0, "a");
        assert_eq!(index.len(KEY.0), 0);
    }

    /// An autonomous run between two user runs leaves the first one's ended
    /// entries their claim of grace.
    #[test]
    fn task_index_autonomous_claim_keeps_the_prior_runs_claim_grace() {
        let (_home, owners) = owners(&["a", "wake-up", "b", "c"]);
        let mut index = TaskIndex::default();
        index.index_tool_call(&key(), 1, &owners[0], &call("ended", false));
        index.claimed(&key(), &owners[1].run_id, RunOrigin::Autonomous, 1);
        index.claimed(&key(), &owners[2].run_id, RunOrigin::User, 1);
        assert_eq!(owner_of(&mut index, "ended", 1).as_deref(), Some("a"));
        index.claimed(&key(), &owners[3].run_id, RunOrigin::User, 1);
        assert_eq!(owner_of(&mut index, "ended", 1), None);
    }

    #[test]
    fn task_index_ignores_replays_and_earlier_generations() {
        let (_home, owners) = owners(&["a", "b"]);
        let mut index = TaskIndex::default();
        index.index_tool_call(&key(), 1, &owners[0], &call("tool", true));
        let update = || AgentUpdate::AsyncTaskStateUpdate {
            async_task_id: "task".into(),
            state: AsyncTaskState::Completed,
            provider_tool_call_id: Some("tool".into()),
            summary: None,
            output_file_path: None,
        };
        assert!(matches!(
            index.route(&key(), 1, KeyView::Replaying, update()),
            Route::Unowned(_)
        ));
        assert!(matches!(
            index.route(&key(), 2, KeyView::Running(&owners[1]), update()),
            Route::Unowned(_)
        ));
        // The holder's own frames take the ordinary path.
        assert!(matches!(
            index.route(&key(), 1, KeyView::Running(&owners[0]), update()),
            Route::Unowned(_)
        ));
        assert!(index.take_lost(|_, _, _| true).is_empty());
    }

    /// Claude's order: the tool call, a spawn naming no call, the stamp, then
    /// a frame linking the two. The call's entry takes the task, so the
    /// task's end ends the one entry the tool has.
    #[test]
    fn task_index_merges_a_held_task_into_its_tool_call() {
        let (_home, owners) = owners(&["run"]);
        let mut index = TaskIndex::default();
        let holder = || KeyView::Running(&owners[0]);
        index.index_tool_call(&key(), 1, &owners[0], &call("tool", false));
        assert!(matches!(
            index.route(&key(), 1, holder(), spawn("task", None)),
            Route::Held
        ));
        assert!(matches!(
            index.route(&key(), 1, holder(), stamp("tool")),
            Route::Unowned(_)
        ));
        let linked = state("task", AsyncTaskState::Running, Some("tool"));
        let Route::Owner(owner, frames) = index.route(&key(), 1, holder(), linked) else {
            panic!("the link releases what was held");
        };
        assert_eq!(owner.run_id.0, "run");
        assert_eq!(frames.len(), 2);
        assert_eq!(index.len(KEY.0), 1);

        let end = state("task", AsyncTaskState::Completed, None);
        let Route::Owner(_, frames) = index.route(&key(), 1, KeyView::Released(None), end) else {
            panic!("the task's run takes its end");
        };
        assert!(matches!(
            &frames[..],
            [AgentUpdate::AsyncTaskStateUpdate { provider_tool_call_id: Some(call), .. }]
                if call == "tool"
        ));
        assert!(index.take_lost(|_, _, _| true).is_empty());
        assert_eq!(owner_of(&mut index, "tool", 1).as_deref(), Some("run"));
    }

    /// Claude can stamp a tool after its task ended; the task stays ended.
    /// A stamp before any task frame starts one.
    #[test]
    fn task_index_keeps_an_ended_task_ended_after_a_late_stamp() {
        let (_home, owners) = owners(&["run"]);
        let mut index = TaskIndex::default();
        let released = || KeyView::Released(None);
        index.index_tool_call(&key(), 1, &owners[0], &call("tool", false));
        index.route(&key(), 1, released(), spawn("task", Some("tool")));
        let end = state("task", AsyncTaskState::Completed, Some("tool"));
        index.route(&key(), 1, released(), end);
        index.route(&key(), 1, released(), stamp("tool"));
        index.index_tool_call(&key(), 1, &owners[0], &call("tool", true));
        assert!(index.take_lost(|_, _, _| true).is_empty());

        index.index_tool_call(&key(), 1, &owners[0], &call("other", false));
        index.route(&key(), 1, released(), stamp("other"));
        assert_eq!(index.take_lost(|_, _, _| true).len(), 1);
    }

    /// A call id the next run reuses is that run's, though the previous
    /// run's entry survives the claim.
    #[test]
    fn task_index_gives_a_reused_call_id_to_the_newest_run() {
        let (_home, owners) = owners(&["a", "b"]);
        let mut index = TaskIndex::default();
        index.index_tool_call(&key(), 1, &owners[0], &call("read-1", false));
        index.claimed(&key(), &owners[1].run_id, RunOrigin::User, 1);
        index.index_tool_call(&key(), 1, &owners[1], &call("read-1", false));
        assert_eq!(owner_of(&mut index, "read-1", 1).as_deref(), Some("b"));
        assert_eq!(index.len(KEY.0), 1);
    }

    /// ACP ids are unique only within a session: two sessions of one agent
    /// can each have a `read-1` and a `task`, and each keeps its own run.
    /// With no state for a frame's session, the newest entry takes it.
    #[test]
    fn task_index_keeps_ids_per_session() {
        fn routed(
            index: &mut TaskIndex,
            key: &(String, String),
            view: KeyView<'_>,
            update: AgentUpdate,
        ) -> Option<String> {
            match index.route(key, 1, view, update) {
                Route::Owner(owner, _) => Some(owner.run_id.0.clone()),
                _ => None,
            }
        }
        let (_home, owners) = owners(&["a", "b"]);
        let mut index = TaskIndex::default();
        let other = (KEY.0.to_owned(), "other".to_owned());
        index.index_tool_call(&key(), 1, &owners[0], &call("read-1", true));
        index.index_tool_call(&other, 1, &owners[1], &call("read-1", true));
        assert_eq!(index.len(KEY.0), 2);

        let released = || KeyView::Released(None);
        let spawned = spawn("task", Some("read-1"));
        assert_eq!(
            routed(&mut index, &key(), released(), spawned.clone()).as_deref(),
            Some("a")
        );
        assert_eq!(
            routed(&mut index, &other, released(), spawned).as_deref(),
            Some("b")
        );
        let end = state("task", AsyncTaskState::Completed, None);
        assert_eq!(
            routed(&mut index, &other, released(), end).as_deref(),
            Some("b")
        );

        let gone = (KEY.0.to_owned(), "gone".to_owned());
        let update = AgentUpdate::ToolCallUpdate {
            payload: call("read-1", false),
        };
        assert_eq!(
            routed(&mut index, &gone, KeyView::Absent, update).as_deref(),
            Some("b")
        );

        let lost = index.take_lost(|_, session, _| session == KEY.1);
        assert_eq!(lost.len(), 1);
        assert_eq!(lost[0].owner.run_id.0, "a");
        assert_eq!(lost[0].async_task_id.as_deref(), Some("task"));
        assert_eq!(index.len(KEY.0), 1);
    }

    /// A spawn the full index cannot hold goes to the key's resolution, which
    /// counts it, rather than vanishing.
    #[test]
    fn task_index_full_of_live_tasks_leaves_a_spawn_to_the_key() {
        let (_home, owners) = owners(&["run"]);
        let mut index = TaskIndex::default();
        for n in 0..AGENT_ENTRIES {
            index.index_tool_call(&key(), 1, &owners[0], &call(&format!("live-{n}"), true));
        }
        let adopter = KeyView::Released(Some(&owners[0]));
        assert!(matches!(
            index.route(&key(), 1, adopter, spawn("task", None)),
            Route::Unowned(_)
        ));
        assert_eq!(index.len(KEY.0), AGENT_ENTRIES);
    }
}
