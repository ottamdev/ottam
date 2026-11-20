//! Which conversations want the user, and when a client should alert them.
//!
//! Core owns this decision so every client, including one that cannot hold a
//! connection, gets the same answer; clients only present it
//! (`docs/architecture/app/macos-ui.md` § Attention). A conversation needs
//! input while one of its asks waits on a client, and is unseen when a run
//! ended while no connection focused it. Focus is also the seen signal.
//!
//! Every change is sent as `attention.changed` with a revision bumped under
//! the same lock that sends it, so revisions reach clients in order.

use super::outbound::{ConnectionId, OutboundSink};
use super::text::excerpt;
use crate::storage::models::{ConversationId, ConversationRunId, RunStatus, WorkspaceId};
use crate::storage::repositories::ConversationRepository;
use crate::storage::Database;
use ottam_protocol::{
    AttentionChangedParams, AttentionEntry, AttentionListResult, AttentionReason,
    AttentionState, ConversationID, ConversationRunID, ConversationRunStatus, ProtocolNotification,
    WorkspaceID, METHOD_ATTENTION_CHANGED,
};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::Duration;
use tokio::task::JoinSet;

/// How long an ask waits before a client is told to alert: one answered at
/// once, or auto-answered, never alerts.
const ASK_NOTIFY_DELAY: Duration = Duration::from_millis(400);
/// How long an ended run waits before it counts as unseen, so a follow-up
/// that starts right away, or the user glancing over, cancels it.
const SETTLE_DELAY: Duration = Duration::from_secs(2);
/// Longest detail sent, in characters, the stored message preview's size.
/// It bounds the payload only: the cut a user sees is the client's, made
/// after it renders the markdown.
const DETAIL_CHARS: usize = 4096;

pub(crate) type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// What the settle check found when its delay ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Settle {
    /// The conversation and everything it delegated are idle.
    Idle,
    /// A child still works; its end settles the conversation again.
    Waiting,
    /// The conversation works again, or Ottam is quitting: the run is not
    /// unseen.
    Drop,
}

/// A run that ended and may become unseen once its tree is idle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ended {
    pub(crate) workspace_id: WorkspaceID,
    pub(crate) run_id: ConversationRunID,
    pub(crate) reason: AttentionReason,
    pub(crate) title: String,
    pub(crate) detail: Option<String>,
}

impl Ended {
    /// The ended run's attention, when its status can be unseen.
    pub(crate) fn from_status(
        workspace_id: WorkspaceID,
        run_id: ConversationRunID,
        status: &ConversationRunStatus,
        error: &serde_json::Value,
        title: String,
    ) -> Option<Self> {
        let reason = match status {
            ConversationRunStatus::Completed => AttentionReason::Completed,
            ConversationRunStatus::Failed => AttentionReason::Failed,
            _ => return None,
        };
        let detail = (reason == AttentionReason::Failed)
            .then(|| error.get("message").and_then(serde_json::Value::as_str))
            .flatten()
            .and_then(clip);
        Some(Self {
            workspace_id,
            run_id,
            reason,
            title,
            detail,
        })
    }

    /// For a completed run: it shows the start of its last message, as
    /// markdown, since presenting it as text is the client's job.
    pub(crate) fn with_message(mut self, message: &str) -> Self {
        self.detail = clip(message);
        self
    }
}

/// What an ask needs to be shown. The title is read while the ask waits, so
/// it never delays the request.
pub(crate) struct AskInfo {
    pub(crate) workspace_id: WorkspaceID,
    pub(crate) conversation_id: ConversationID,
    pub(crate) run_id: ConversationRunID,
    pub(crate) reason: AttentionReason,
    pub(crate) detail: Option<String>,
    pub(crate) title: BoxFuture<String>,
}

struct Ask {
    id: u64,
    workspace_id: WorkspaceID,
    run_id: ConversationRunID,
    reason: AttentionReason,
    detail: Option<String>,
    /// Set once the title is read and the ask was sent.
    title: Option<String>,
    /// Its alert went to a newer ask; if that one ends first, this alerts.
    deferred: bool,
}

struct Candidate {
    ended: Ended,
    generation: u64,
}

#[derive(Default)]
struct State {
    revision: u64,
    next_id: u64,
    asks: HashMap<ConversationID, Vec<Ask>>,
    unseen: HashMap<ConversationID, Ended>,
    candidates: HashMap<ConversationID, Candidate>,
    focus: HashMap<ConnectionId, ConversationID>,
    /// Attached connections still open: a `setFocus` still queued from one
    /// that closed must not bring its focus back.
    open: HashSet<ConnectionId>,
}

impl State {
    fn focused(&self, conversation: &ConversationID) -> bool {
        self.focus.values().any(|focused| focused == conversation)
    }

    /// The conversation's entry: its latest shown ask, else its unseen run.
    fn entry(&self, conversation: &ConversationID) -> Option<AttentionEntry> {
        let ask = self
            .asks
            .get(conversation)
            .and_then(|asks| asks.iter().rev().find(|ask| ask.title.is_some()));
        if let Some(ask) = ask {
            return Some(AttentionEntry {
                workspace_id: ask.workspace_id.clone(),
                conversation_id: conversation.clone(),
                state: AttentionState::NeedsInput,
                reason: ask.reason,
                run_id: Some(ask.run_id.clone()),
                title: ask.title.clone().unwrap_or_default(),
                detail: ask.detail.clone(),
            });
        }
        self.unseen.get(conversation).map(|ended| AttentionEntry {
            workspace_id: ended.workspace_id.clone(),
            conversation_id: conversation.clone(),
            state: AttentionState::Unseen,
            reason: ended.reason,
            run_id: Some(ended.run_id.clone()),
            title: ended.title.clone(),
            detail: ended.detail.clone(),
        })
    }
}

pub(crate) struct Attention {
    outbound: OutboundSink,
    state: StdMutex<State>,
    /// Ask and settle tasks. They hold the service weakly and are aborted at
    /// shutdown, so none keeps the outbound router alive.
    tasks: StdMutex<JoinSet<()>>,
    /// Serializes unseen markers: each write stores what the state wants at
    /// that moment, before the change is sent.
    writes: tokio::sync::Mutex<()>,
    store: OnceLock<Database>,
    this: Weak<Self>,
}

impl Attention {
    pub(crate) fn new(outbound: OutboundSink) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            outbound,
            state: StdMutex::new(State::default()),
            tasks: StdMutex::new(JoinSet::new()),
            writes: tokio::sync::Mutex::new(()),
            store: OnceLock::new(),
            this: this.clone(),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        let mut tasks = self
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while tasks.try_join_next().is_some() {}
        tasks.spawn(task);
    }

    /// Stops every pending ask and settle task. Called from the bridge's
    /// shutdown, after the runs stopped.
    pub(crate) fn shutdown(&self) {
        self.tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .abort_all();
    }

    /// Sends `conversation`'s entry, or `none` in the shape of `before` when
    /// it has none. Called with the state locked, so revisions go out in
    /// order.
    fn emit(
        &self,
        state: &mut State,
        conversation: &ConversationID,
        before: Option<AttentionEntry>,
        notify: bool,
    ) {
        let entry = match state.entry(conversation) {
            Some(entry) => entry,
            None => match before {
                Some(before) => AttentionEntry {
                    state: AttentionState::None,
                    ..before
                },
                None => return,
            },
        };
        state.revision += 1;
        tracing::debug!(
            conversation_id = %conversation.0,
            state = ?entry.state,
            revision = state.revision,
            notify,
            "attention changed"
        );
        let notification = ProtocolNotification::new(
            METHOD_ATTENTION_CHANGED,
            AttentionChangedParams {
                revision: state.revision,
                entry,
                notify,
            },
        );
        if let Ok(value) = serde_json::to_value(notification) {
            let _ = self.outbound.notify(value);
        }
    }

    pub(crate) fn list(&self) -> AttentionListResult {
        let state = self.state();
        let conversations: HashSet<&ConversationID> =
            state.asks.keys().chain(state.unseen.keys()).collect();
        let mut entries: Vec<AttentionEntry> = conversations
            .into_iter()
            .filter_map(|conversation| state.entry(conversation))
            .collect();
        entries.sort_by(|a, b| a.conversation_id.0.cmp(&b.conversation_id.0));
        AttentionListResult {
            revision: state.revision,
            entries,
        }
    }

    pub(crate) fn is_focused(&self, conversation: &ConversationID) -> bool {
        self.state().focused(conversation)
    }

    /// What `origin` looks at now. Focusing a conversation is seeing it: an
    /// end still settling drops, and an unseen one clears in the background,
    /// so a focus change never waits on the database.
    pub(crate) fn set_focus(&self, origin: ConnectionId, conversation: Option<ConversationID>) {
        let seen = {
            let mut state = self.state();
            if matches!(origin, ConnectionId::Attached(_)) && !state.open.contains(&origin) {
                return;
            }
            match &conversation {
                Some(conversation) => {
                    state.focus.insert(origin, conversation.clone());
                    state.candidates.remove(conversation);
                    state.unseen.get(conversation).map(|ended| ended.run_id.clone())
                }
                None => {
                    state.focus.remove(&origin);
                    None
                }
            }
        };
        tracing::debug!(
            focused = conversation.is_some(),
            "attention focus changed"
        );
        if let (Some(run), Some(conversation)) = (seen, conversation) {
            let this = self.this.clone();
            self.spawn(async move {
                if let Some(attention) = this.upgrade() {
                    attention.mark_seen(&conversation, &run).await;
                }
            });
        }
    }

    /// An attached connection opened, before any of its requests is read.
    pub(crate) fn connection_opened(&self, origin: ConnectionId) {
        self.state().open.insert(origin);
    }

    /// An attached connection closed: its focus ends, for good.
    pub(crate) fn connection_closed(&self, origin: ConnectionId) {
        let mut state = self.state();
        state.focus.remove(&origin);
        state.open.remove(&origin);
    }

    /// Clears `run`, the unseen run focus saw, unless a newer one replaced it
    /// while this waited on the lock.
    async fn mark_seen(&self, conversation: &ConversationID, run: &ConversationRunID) {
        let _write = self.writes.lock().await;
        let workspace = {
            let state = self.state();
            match state.unseen.get(conversation) {
                Some(ended) if &ended.run_id == run => ended.workspace_id.clone(),
                _ => return,
            }
        };
        self.persist(&workspace, conversation, None).await;
        let mut state = self.state();
        let before = state.entry(conversation);
        if state.unseen.remove(conversation).is_some() {
            self.emit(&mut state, conversation, before, false);
        }
    }

    /// Registers an ask that now waits on a client. It is sent once its
    /// title is read, and alerts after [`ASK_NOTIFY_DELAY`] unless answered
    /// or focused by then. Dropping the guard ends it.
    pub(crate) fn begin_ask(&self, info: AskInfo) -> AskGuard {
        let conversation = info.conversation_id.clone();
        let id = {
            let mut state = self.state();
            state.next_id += 1;
            let id = state.next_id;
            state.asks.entry(conversation.clone()).or_default().push(Ask {
                id,
                workspace_id: info.workspace_id,
                run_id: info.run_id,
                reason: info.reason,
                detail: info.detail.as_deref().and_then(clip),
                title: None,
                deferred: false,
            });
            id
        };
        let this = self.this.clone();
        let task_conversation = conversation.clone();
        let title = info.title;
        self.spawn(async move {
            let title = title.await;
            {
                let Some(attention) = this.upgrade() else { return };
                let mut state = attention.state();
                let before = state.entry(&task_conversation);
                let Some(ask) = state
                    .asks
                    .get_mut(&task_conversation)
                    .and_then(|asks| asks.iter_mut().find(|ask| ask.id == id))
                else {
                    return;
                };
                ask.title = Some(title);
                attention.emit(&mut state, &task_conversation, before, false);
            }
            tokio::time::sleep(ASK_NOTIFY_DELAY).await;
            let Some(attention) = this.upgrade() else { return };
            let mut state = attention.state();
            let Some(asks) = state.asks.get_mut(&task_conversation) else {
                return;
            };
            let latest = asks.last().is_some_and(|ask| ask.id == id);
            if !latest {
                if let Some(ask) = asks.iter_mut().find(|ask| ask.id == id) {
                    ask.deferred = true;
                }
                return;
            }
            if !state.focused(&task_conversation) {
                let before = state.entry(&task_conversation);
                attention.emit(&mut state, &task_conversation, before, true);
            }
        });
        AskGuard {
            attention: self.this.clone(),
            conversation,
            id,
        }
    }

    fn end_ask(&self, conversation: &ConversationID, id: u64) {
        let mut state = self.state();
        let before = state.entry(conversation);
        let Some(asks) = state.asks.get_mut(conversation) else {
            return;
        };
        let Some(index) = asks.iter().position(|ask| ask.id == id) else {
            return;
        };
        let latest = index + 1 == asks.len();
        let shown = asks.remove(index).title.is_some();
        // The newer ask an older one deferred to ended: the older one, still
        // unanswered, alerts in its place.
        let deferred = latest
            && asks
                .last_mut()
                .is_some_and(|ask| std::mem::take(&mut ask.deferred));
        if asks.is_empty() {
            state.asks.remove(conversation);
        }
        let notify = deferred && !state.focused(conversation);
        if shown || notify {
            self.emit(&mut state, conversation, before, notify);
        }
    }

    /// A run of `conversation` ended. `ended` is what it would be unseen as,
    /// or `None` when it can't be: it was stopped, interrupted, or a run the
    /// user did not drive in a delegated child. Settling is separate, once
    /// the tree is idle.
    pub(crate) fn run_ended(&self, conversation: &ConversationID, ended: Option<Ended>) {
        let mut state = self.state();
        match ended {
            Some(ended) => {
                state.next_id += 1;
                let generation = state.next_id;
                state
                    .candidates
                    .insert(conversation.clone(), Candidate { ended, generation });
            }
            None => {
                state.candidates.remove(conversation);
            }
        }
    }

    pub(crate) fn has_candidate(&self, conversation: &ConversationID) -> bool {
        self.state().candidates.contains_key(conversation)
    }

    fn drop_candidate(&self, conversation: &ConversationID, generation: Option<u64>) {
        let mut state = self.state();
        if state
            .candidates
            .get(conversation)
            .is_some_and(|candidate| generation.is_none_or(|g| g == candidate.generation))
        {
            state.candidates.remove(conversation);
        }
    }

    /// `conversation`'s tree is idle now. A focused conversation was seen;
    /// otherwise its ended run becomes unseen after [`SETTLE_DELAY`], if
    /// `check` still finds the tree idle.
    pub(crate) fn settle(
        &self,
        conversation: &ConversationID,
        check: impl FnOnce() -> BoxFuture<Settle> + Send + 'static,
    ) {
        let generation = {
            let mut state = self.state();
            if state.focused(conversation) {
                state.candidates.remove(conversation);
                return;
            }
            match state.candidates.get(conversation) {
                Some(candidate) => candidate.generation,
                None => return,
            }
        };
        let this = self.this.clone();
        let conversation = conversation.clone();
        self.spawn(async move {
            tokio::time::sleep(SETTLE_DELAY).await;
            let verdict = check().await;
            let Some(attention) = this.upgrade() else { return };
            match verdict {
                Settle::Idle => attention.commit(&conversation, generation).await,
                Settle::Waiting => {}
                Settle::Drop => attention.drop_candidate(&conversation, Some(generation)),
            }
        });
    }

    async fn commit(&self, conversation: &ConversationID, generation: u64) {
        let _write = self.writes.lock().await;
        let ended = {
            let mut state = self.state();
            let Some(candidate) = state
                .candidates
                .get(conversation)
                .filter(|candidate| candidate.generation == generation)
            else {
                return;
            };
            let ended = candidate.ended.clone();
            if state.focused(conversation) {
                state.candidates.remove(conversation);
                return;
            }
            ended
        };
        if !self
            .persist(&ended.workspace_id, conversation, Some(&ended.run_id))
            .await
        {
            // The conversation went, or its workspace was archived, while
            // the end settled: nothing of it is shown.
            self.drop_candidate(conversation, Some(generation));
            return;
        }
        let workspace = ended.workspace_id.clone();
        let wanted = {
            let mut state = self.state();
            let current = state
                .candidates
                .get(conversation)
                .is_some_and(|candidate| candidate.generation == generation);
            if current && !state.focused(conversation) {
                state.candidates.remove(conversation);
                let before = state.entry(conversation);
                // A newer run of a conversation already unseen does not
                // alert again; a client drops the banner the older one left.
                let notify = !state.unseen.contains_key(conversation);
                state.unseen.insert(conversation.clone(), ended);
                tracing::info!(conversation_id = %conversation.0, notify, "conversation unseen");
                self.emit(&mut state, conversation, before, notify);
                return;
            }
            // Looked at, or replaced by a newer run, while the marker was
            // written: put the marker back to what the state holds.
            if current {
                state.candidates.remove(conversation);
            }
            state
                .unseen
                .get(conversation)
                .map(|ended| ended.run_id.clone())
        };
        self.persist(&workspace, conversation, wanted.as_ref()).await;
    }

    /// The conversation is gone: nothing of it is shown again.
    pub(crate) fn forget_conversation(&self, conversation: &ConversationID) {
        let mut state = self.state();
        let before = state.entry(conversation);
        state.asks.remove(conversation);
        state.unseen.remove(conversation);
        state.candidates.remove(conversation);
        if before.is_some() {
            self.emit(&mut state, conversation, before, false);
        }
    }

    /// The workspace was archived: its conversations' attention ends and
    /// their markers clear.
    pub(crate) async fn forget_workspace(&self, workspace: &WorkspaceID) {
        let _write = self.writes.lock().await;
        self.persist_workspace_cleared(workspace).await;
        let mut state = self.state();
        let conversations: HashSet<ConversationID> = state
            .asks
            .iter()
            .filter(|(_, asks)| asks.iter().any(|ask| &ask.workspace_id == workspace))
            .map(|(conversation, _)| conversation.clone())
            .chain(
                state
                    .unseen
                    .iter()
                    .filter(|(_, ended)| &ended.workspace_id == workspace)
                    .map(|(conversation, _)| conversation.clone()),
            )
            .chain(
                state
                    .candidates
                    .iter()
                    .filter(|(_, candidate)| &candidate.ended.workspace_id == workspace)
                    .map(|(conversation, _)| conversation.clone()),
            )
            .collect();
        for conversation in conversations {
            let before = state.entry(&conversation);
            state.asks.remove(&conversation);
            state.unseen.remove(&conversation);
            state.candidates.remove(&conversation);
            if before.is_some() {
                self.emit(&mut state, &conversation, before, false);
            }
        }
    }

    /// Gives the service the database its unseen markers live in, and reads
    /// back the ones an earlier session left. Runs before any request is
    /// served, so nothing is sent: clients read them with `attention.list`.
    pub(crate) async fn install_store(&self, database: Database) {
        let records = database
            .run("attention.unseenRuns", ConversationRepository::unseen_runs)
            .await;
        let _ = self.store.set(database);
        let records = match records {
            Ok(records) => records,
            Err(error) => {
                tracing::warn!(kind = error.kind, "failed to read unseen conversations");
                return;
            }
        };
        let mut state = self.state();
        for record in records {
            let status = match record.status {
                RunStatus::Completed => ConversationRunStatus::Completed,
                RunStatus::Failed => ConversationRunStatus::Failed,
                _ => continue,
            };
            let error = record
                .error_json
                .as_deref()
                .and_then(|json| serde_json::from_str(json).ok())
                .unwrap_or_default();
            let ended = Ended::from_status(
                WorkspaceID(record.workspace_id.to_string()),
                ConversationRunID(record.run_id.to_string()),
                &status,
                &error,
                record.title,
            );
            if let Some(ended) = ended {
                state
                    .unseen
                    .insert(ConversationID(record.conversation_id.to_string()), ended);
            }
        }
        tracing::info!(count = state.unseen.len(), "unseen conversations restored");
    }

    /// Stores the conversation's marker. False only when the conversation
    /// cannot hold `run`: it is gone, or its workspace was archived. A failed
    /// write is logged and counts as stored, since the state still holds it.
    async fn persist(
        &self,
        workspace: &WorkspaceID,
        conversation: &ConversationID,
        run: Option<&ConversationRunID>,
    ) -> bool {
        let Some(database) = self.store.get() else {
            return true;
        };
        let ids = (
            WorkspaceId::try_from(workspace.0.as_str()),
            ConversationId::try_from(conversation.0.as_str()),
            run.map(|run| ConversationRunId::try_from(run.0.as_str()))
                .transpose(),
        );
        let (Ok(workspace_id), Ok(conversation_id), Ok(run_id)) = ids else {
            tracing::warn!(conversation_id = %conversation.0, "unseen marker has an invalid id");
            return true;
        };
        let stored = database
            .run("attention.setUnseen", move |connection| {
                ConversationRepository::set_unseen_run(
                    connection,
                    &workspace_id,
                    &conversation_id,
                    run_id.as_ref(),
                )
            })
            .await;
        stored.unwrap_or_else(|error| {
            tracing::warn!(
                conversation_id = %conversation.0,
                kind = error.kind,
                "failed to store the unseen marker"
            );
            true
        })
    }

    async fn persist_workspace_cleared(&self, workspace: &WorkspaceID) {
        let Some(database) = self.store.get() else {
            return;
        };
        let Ok(workspace_id) = WorkspaceId::try_from(workspace.0.as_str()) else {
            return;
        };
        let cleared = database
            .run("attention.clearUnseen", move |connection| {
                ConversationRepository::clear_unseen_runs(connection, &workspace_id)
            })
            .await;
        if let Err(error) = cleared {
            tracing::warn!(
                workspace_id = %workspace.0,
                kind = error.kind,
                "failed to clear unseen markers"
            );
        }
    }
}

/// An ask waiting on a client; dropping it ends the ask.
pub(crate) struct AskGuard {
    attention: Weak<Attention>,
    conversation: ConversationID,
    id: u64,
}

impl Drop for AskGuard {
    fn drop(&mut self) {
        if let Some(attention) = self.attention.upgrade() {
            attention.end_ask(&self.conversation, self.id);
        }
    }
}

/// `text` as an [`excerpt`] of [`DETAIL_CHARS`]; `None` when blank.
fn clip(text: &str) -> Option<String> {
    Some(excerpt(text, DETAIL_CHARS)).filter(|text| !text.is_empty())
}

#[cfg(test)]
#[path = "attention_tests.rs"]
mod tests;
