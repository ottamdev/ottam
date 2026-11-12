//! Memory-owned transcript revisions. No coordinator lock crosses a database await.
use super::{assembly::*, partial_capture_problem, to_wire_with_live, ChatService, Repo};
use crate::{
    core_error::CoreError,
    diagnostics::{is_log_point, BoundedCounts},
    storage::{
        repositories::transcript::{prefix, TranscriptDetail, TranscriptRecord, TranscriptRow},
        Database, StorageError,
    },
};
use ottam_protocol::*;
use diesel::prelude::*;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::Notify;

// Reserve space for an immutable in-flight revision even if its live copy is
// replaced or evicted. The writer holds only one item at a time.
const CONVERSATION_BYTES: usize = 7 * 1024 * 1024;
const GLOBAL_BYTES: usize = 40 * 1024 * 1024;
const CONVERSATION_ITEMS: usize = 4095;
const GLOBAL_ITEMS: usize = 16383;
const CHECKPOINT_BYTES: usize = 64 * 1024;

async fn next_checkpoint(wake: &Notify) {
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(1)) => {},
        _ = wake.notified() => {},
    }
}

fn bound_metadata(update: &mut AgentUpdate) -> bool {
    fn cap(value: &mut serde_json::Value, limit: usize) -> bool {
        if let Some(text) = value.as_str().filter(|text| text.len() > limit) {
            *value = serde_json::Value::String(prefix(text, limit).into());
            true
        } else {
            false
        }
    }
    let mut truncated = false;
    match update {
        AgentUpdate::ToolCall { payload } | AgentUpdate::ToolCallUpdate { payload } => {
            if payload
                .get("toolCallId")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| id.len() > 4096)
            {
                *update = AgentUpdate::StreamGap { dropped: 1 };
                return true;
            }
            for key in [
                "name",
                "kind",
                "title",
                "status",
                "startedAt",
                "completedAt",
            ] {
                if let Some(value) = payload.get_mut(key) {
                    truncated |= cap(value, 4096);
                }
            }
            if let Some(locations) = payload
                .get_mut("locations")
                .and_then(serde_json::Value::as_array_mut)
            {
                truncated |= locations.len() > 128;
                locations.truncate(128);
                for location in locations.iter_mut() {
                    if let Some(path) = location.get_mut("path") {
                        truncated |= cap(path, 1024);
                    }
                }
                while serde_json::to_vec(locations).is_ok_and(|value| value.len() > 65536) {
                    locations.pop();
                    truncated = true;
                }
            }
        }
        AgentUpdate::Plan { payload } => {
            if let Some(entries) = payload
                .get_mut("entries")
                .and_then(serde_json::Value::as_array_mut)
            {
                truncated |= entries.len() > 128;
                entries.truncate(128);
                for entry in entries.iter_mut() {
                    for key in ["content", "priority", "status"] {
                        if let Some(value) = entry.get_mut(key) {
                            truncated |= cap(value, 1024);
                        }
                    }
                }
                while serde_json::to_vec(entries).is_ok_and(|value| value.len() > 65536) {
                    entries.pop();
                    truncated = true;
                }
            }
        }
        AgentUpdate::AsyncTaskSpawned {
            async_task_id,
            provider_tool_call_id,
            output_file_path,
            ..
        }
        | AgentUpdate::AsyncTaskProgress {
            async_task_id,
            provider_tool_call_id,
            output_file_path,
            ..
        }
        | AgentUpdate::AsyncTaskStateUpdate {
            async_task_id,
            provider_tool_call_id,
            output_file_path,
            ..
        } => {
            if async_task_id.len() > 4096
                || provider_tool_call_id
                    .as_ref()
                    .is_some_and(|id| id.len() > 4096)
            {
                *update = AgentUpdate::StreamGap { dropped: 1 };
                return true;
            }
            // A cut path names a different file; keep the prior one instead.
            if output_file_path
                .as_ref()
                .is_some_and(|path| path.len() > 4096)
            {
                *output_file_path = None;
                truncated = true;
            }
        }
        _ => {}
    }
    truncated
}

#[derive(Clone)]
pub(super) struct LiveItem {
    pub record: TranscriptRecord,
    pub chunks: Vec<Arc<str>>,
    saved_revision: i64,
    pub(super) body_start: usize,
    /// The revision that last replaced the body rather than appending to it.
    /// Status and progress revisions leave the body as it was, so a read made
    /// at any revision from this one on still sees the same bytes.
    body_revision: i64,
}

impl LiveItem {
    /// Whether a body read made at `revision` sees the body as it is now.
    pub(in crate::server::chat) fn body_unchanged_since(&self, revision: i64) -> bool {
        (self.body_revision..=self.record.row.revision).contains(&revision)
    }

    pub(super) fn to_wire(&self) -> Result<ConversationTranscriptItem, StorageError> {
        let mut wire = to_wire_with_live(self.record.clone(), true)?;
        wire.committed_revision = Some(self.saved_revision as u64);
        Ok(wire)
    }
    fn bytes(&self) -> usize {
        self.chunks.iter().map(|chunk| chunk.len()).sum::<usize>()
            + self.chunks.capacity() * std::mem::size_of::<Arc<str>>()
            + self.chunks.len() * 2 * std::mem::size_of::<usize>()
            + self.record.preview.capacity()
            + self
                .record
                .row
                .provider_message_id
                .as_ref()
                .map_or(0, String::capacity)
            + self.record.row.id.capacity()
            + self.record.row.adapter_metadata_json.capacity()
            + match &self.record.detail {
                TranscriptDetail::Tool { json, provider_id } => {
                    json.capacity() + provider_id.as_ref().map_or(0, String::capacity)
                }
                TranscriptDetail::Plan(json) => json.capacity(),
                TranscriptDetail::Permission { json, request_id } => {
                    json.capacity() + request_id.capacity()
                }
                TranscriptDetail::Gap { reason, .. } => reason.capacity(),
                TranscriptDetail::None => 0,
            }
            + 512
    }

    pub fn body(&self, offset: usize, limit: usize) -> Result<(String, Option<u64>), CoreError> {
        let mut at = self.body_start;
        let mut text = String::new();
        if offset > self.record.row.body_byte_count as usize {
            return Err(CoreError::new(
                "invalidParams",
                "Invalid transcript body offset",
            ));
        }
        for chunk in &self.chunks {
            if at + chunk.len() > offset && text.len() < limit {
                let skip = offset.saturating_sub(at);
                if !chunk.is_char_boundary(skip) {
                    return Err(CoreError::new("invalidParams", "Body offset splits UTF-8"));
                }
                let part = prefix(&chunk[skip..], limit - text.len());
                text.push_str(part);
                if part.len() < chunk.len() - skip {
                    break;
                }
            }
            at += chunk.len();
        }
        let end = offset + text.len();
        Ok((
            text,
            (end < self.record.row.body_byte_count as usize).then_some(end as u64),
        ))
    }
}

#[derive(Default)]
pub(super) struct ConversationMemory {
    pending_metadata: std::collections::VecDeque<PendingMetadata>,
    pub failure_revisions:
        std::collections::VecDeque<(String, ottam_protocol::ConversationSessionFailure)>,
    pub session_states: HashMap<String, ConversationSessionState>,
    pub items: BTreeMap<i64, LiveItem>,
    pub latest: Option<TranscriptRow>,
    pub sequence: u64,
    pub closed_runs: HashSet<String>,
    final_status: HashMap<String, String>,
    settled_runs: HashSet<String>,
    pub capture: HashMap<String, TranscriptCaptureStatus>,
    overflow_capture: Option<TranscriptCaptureStatus>,
    overflow_dirty: bool,
    pub workspace: Option<WorkspaceID>,
    pub instance: String,
    pub outbound: Option<crate::server::OutboundSink>,
    dirty_bytes: usize,
    last_admission: i64,
}

struct PendingMetadata {
    run: String,
    message: String,
    reasoning: bool,
    metadata: TranscriptAdapterMetadata,
}

impl ConversationMemory {
    fn merge_pending_metadata(&mut self, run: &str, update: &mut AgentUpdate) {
        let AgentUpdate::RichMessageChunk {
            text,
            message_id: Some(message),
            reasoning,
            metadata,
        } = update
        else {
            return;
        };
        if let Some(index) = self.pending_metadata.iter().position(|pending| {
            pending.run == run
                && pending.message == *message
                && pending.reasoning == *reasoning
                && pending.metadata.session_id == metadata.session_id
        }) {
            let pending = self
                .pending_metadata
                .remove(index)
                .expect("matched metadata");
            metadata.phase = metadata.phase.take().or(pending.metadata.phase);
            metadata.parent_message_id = metadata
                .parent_message_id
                .take()
                .or(pending.metadata.parent_message_id);
            metadata.parent_tool_call_id = metadata
                .parent_tool_call_id
                .take()
                .or(pending.metadata.parent_tool_call_id);
        }
        if !text.is_empty()
            || self.items.values().any(|item| {
                let row = &item.record.row;
                row.run_id.as_deref() == Some(run)
                    && row.provider_message_id.as_ref() == Some(message)
                    && row.kind
                        == if *reasoning {
                            "reasoning"
                        } else {
                            "assistantText"
                        }
                    && super::subagents::same_segment(&row.adapter_metadata_json, metadata)
            })
        {
            return;
        }
        let bytes = |pending: &PendingMetadata| {
            pending.run.len()
                + pending.message.len()
                + serde_json::to_vec(&pending.metadata).map_or(65537, |bytes| bytes.len())
        };
        let pending = PendingMetadata {
            run: run.into(),
            message: message.clone(),
            reasoning: *reasoning,
            metadata: metadata.clone(),
        };
        if bytes(&pending) > 65536 {
            return;
        }
        self.pending_metadata.push_back(pending);
        while self.pending_metadata.len() > 64
            || self.pending_metadata.iter().map(bytes).sum::<usize>() > 65536
        {
            self.pending_metadata.pop_front();
        }
    }
}

impl ConversationMemory {
    fn preserves_identity(&self, item: &LiveItem) -> bool {
        matches!(item.record.row.kind.as_str(), "tool" | "plan")
            && item
                .record
                .row
                .run_id
                .as_ref()
                .is_some_and(|run| !self.closed_runs.contains(run))
    }

    fn identity_usage(&self) -> (usize, usize) {
        self.items
            .values()
            .filter(|item| self.preserves_identity(item))
            .fold((0, 0), |(bytes, count), item| {
                (bytes + item.bytes(), count + 1)
            })
    }

    fn compact_gaps(&mut self) {
        // Repeated discarded replacements can refer to the same accepted tool
        // position. Canonicalize ranges before applying the global range cap.
        for capture in self.capture.values_mut() {
            capture.gaps.sort_by_key(|gap| gap.start_position);
            let mut merged: Vec<TranscriptCoverage> = Vec::new();
            for gap in std::mem::take(&mut capture.gaps) {
                if let Some(last) = merged
                    .last_mut()
                    .filter(|last| gap.start_position <= last.end_position.saturating_add(1))
                {
                    last.end_position = last.end_position.max(gap.end_position);
                } else {
                    merged.push(gap);
                }
            }
            capture.gaps = merged;
        }
        let mut ranges: Vec<_> = self
            .capture
            .iter_mut()
            .flat_map(|(run, capture)| {
                std::mem::take(&mut capture.gaps)
                    .into_iter()
                    .map(|gap| (run.clone(), gap))
                    .collect::<Vec<_>>()
            })
            .collect();
        ranges.sort_by_key(|(_, gap)| gap.start_position);
        let limit = 128
            - self
                .overflow_capture
                .as_ref()
                .map_or(0, |capture| capture.gaps.len());
        while ranges.len() > limit {
            let (_, first) = ranges.remove(0);
            ranges[0].1.start_position = first.start_position;
        }
        for (run, gap) in ranges {
            self.capture
                .get_mut(&run)
                .expect("capture owner")
                .gaps
                .push(gap);
        }
    }

    /// Running subagent threads per run this memory holds, zeros included.
    pub fn subagent_counts(&self) -> Vec<ConversationRunSubagentThreads> {
        super::subagents::counts(self.items.values().filter_map(|item| {
            let TranscriptDetail::Tool { json, .. } = &item.record.detail else {
                return None;
            };
            Some((
                item.record.row.run_id.clone()?,
                super::subagents::stored_thread(json)?,
            ))
        }))
    }

    pub fn capture_status(&self) -> Vec<TranscriptCaptureStatus> {
        self.capture
            .values()
            .chain(self.overflow_capture.iter())
            .cloned()
            .collect()
    }

    fn apply(&mut self, changes: Vec<AssemblyChange>) {
        for change in changes {
            let item = self
                .items
                .entry(change.row.position)
                .or_insert_with(|| LiveItem {
                    record: TranscriptRecord {
                        committed_revision: 0,
                        row: change.row.clone(),
                        detail: change.detail.clone(),
                        preview: String::new(),
                        run_status: None,
                    },
                    chunks: Vec::new(),
                    saved_revision: 0,
                    body_start: 0,
                    body_revision: change.row.revision,
                });
            if change.replace && change.body.is_some() {
                item.chunks.clear();
                item.body_start = 0;
                item.record.preview.clear();
                item.body_revision = change.row.revision;
            }
            if let Some(mut body) = change.body.as_deref() {
                if item.body_start > 0 && item.record.preview.len() < 4096 {
                    item.record
                        .preview
                        .push_str(prefix(body, 4096 - item.record.preview.len()));
                }
                self.dirty_bytes = self.dirty_bytes.saturating_add(body.len());
                while !body.is_empty() {
                    // Merge only the bounded tail, never copy a growing whole body.
                    let tail_size = item
                        .chunks
                        .last()
                        .map_or(CHECKPOINT_BYTES, |tail| tail.len());
                    if tail_size < CHECKPOINT_BYTES {
                        let part = prefix(body, CHECKPOINT_BYTES - tail_size);
                        if !part.is_empty() {
                            let mut tail = item.chunks.pop().expect("tail").to_string();
                            tail.push_str(part);
                            item.chunks.push(Arc::from(tail));
                            body = &body[part.len()..];
                            continue;
                        }
                    }
                    let part = prefix(body, CHECKPOINT_BYTES);
                    item.chunks.push(Arc::from(part));
                    body = &body[part.len()..];
                }
            }
            item.record.row = change.row.clone();
            item.record.detail = change.detail;
            if item.body_start == 0 {
                item.record.preview = item.body(0, 4096).expect("valid prefix").0;
            }
            if change.takes_latest
                && self
                    .latest
                    .as_ref()
                    .is_none_or(|last| last.position <= change.row.position)
            {
                self.latest = Some(change.row);
            }
        }
    }
}

/// Background tasks this core instance saw start and has not seen end. Kept
/// outside conversation memory, so evicting a conversation keeps them. A stored
/// running task absent here was recorded by an earlier instance, which no
/// adapter of this one can end, so it reads as lost.
#[derive(Default)]
pub(super) struct BackgroundTasks {
    /// Tool item id → its conversation and run.
    live: HashMap<String, (String, String)>,
    /// Each run's live count by conversation, zeros kept, for every run whose
    /// tasks this instance has seen.
    counts: HashMap<String, BTreeMap<String, u32>>,
}

impl BackgroundTasks {
    /// Records `item`'s new task state and returns the run's count if it moved.
    fn observe(
        &mut self,
        conversation: &str,
        run: &str,
        item: &str,
        state: AsyncTaskState,
    ) -> Option<ConversationRunBackgroundTasks> {
        let count = self
            .counts
            .entry(conversation.to_owned())
            .or_default()
            .entry(run.to_owned())
            .or_default();
        let before = *count;
        if state.is_live() {
            if self
                .live
                .insert(item.to_owned(), (conversation.to_owned(), run.to_owned()))
                .is_none()
            {
                *count += 1;
            }
        } else if self.live.remove(item).is_some() {
            *count = count.saturating_sub(1);
        }
        (*count != before).then(|| ConversationRunBackgroundTasks {
            run_id: ConversationRunID(run.to_owned()),
            live: *count,
        })
    }

    fn snapshot(&self, conversation: &str) -> Vec<ConversationRunBackgroundTasks> {
        self.counts
            .get(conversation)
            .into_iter()
            .flatten()
            .map(|(run, live)| ConversationRunBackgroundTasks {
                run_id: ConversationRunID(run.clone()),
                live: *live,
            })
            .collect()
    }

    fn forget(&mut self, conversation: &str) {
        self.counts.remove(conversation);
        self.live.retain(|_, (owner, _)| owner != conversation);
    }
}

/// Where a background-task revision lands.
pub(super) enum TaskRevision {
    Done(Box<ConversationTranscriptDeltaParams>),
    /// The run is closed and its tool is not in memory.
    NeedsStorage,
    /// A closed run refused it: a tool update with no background stamp.
    Refused,
}

/// A change to one tool's background task that a closed run still takes.
#[derive(Clone)]
pub(in crate::server) enum TaskChange {
    /// `async_task_*`, or a tool update carrying the background stamp.
    Update(AgentUpdate),
    /// Core lost the task: its adapter or session died before it ended.
    Lost {
        provider_tool_call_id: Option<String>,
        async_task_id: Option<String>,
    },
    /// A question the call asked, and what became of it. Revises only
    /// `tool.question`, in an open or a closed run.
    Question {
        provider_tool_call_id: String,
        question: ottam_protocol::TranscriptQuestion,
    },
}

impl TaskChange {
    /// Whether a closed run takes `update`. Anything else is refused before
    /// it costs a storage lookup.
    pub(in crate::server) fn of(update: &AgentUpdate) -> bool {
        match update {
            AgentUpdate::AsyncTaskSpawned { .. }
            | AgentUpdate::AsyncTaskProgress { .. }
            | AgentUpdate::AsyncTaskStateUpdate { .. } => true,
            AgentUpdate::ToolCallUpdate { payload } => {
                super::is_backgrounded(payload)
                    || payload.get(super::subagents::NATIVE_ANCHOR_KEY).is_some()
            }
            _ => false,
        }
    }

    /// The tool call and task the change names.
    fn locator(&self) -> (Option<&str>, Option<&str>) {
        match self {
            Self::Update(
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
                },
            ) => (provider_tool_call_id.as_deref(), Some(async_task_id)),
            Self::Update(AgentUpdate::ToolCallUpdate { payload }) => (
                payload
                    .get("toolCallId")
                    .and_then(serde_json::Value::as_str),
                None,
            ),
            Self::Update(_) => (None, None),
            Self::Lost {
                provider_tool_call_id,
                async_task_id,
            } => (provider_tool_call_id.as_deref(), async_task_id.as_deref()),
            Self::Question {
                provider_tool_call_id,
                ..
            } => (Some(provider_tool_call_id), None),
        }
    }

    /// Applies the change to the assembler's tool. `false` when a closed run
    /// refuses it.
    fn apply(&self, assembler: &mut TranscriptAssembler<'_>) -> Result<bool, StorageError> {
        let mut changed = Vec::new();
        match self {
            Self::Update(AgentUpdate::ToolCallUpdate { payload })
                if payload.get(super::subagents::NATIVE_ANCHOR_KEY).is_some() =>
            {
                return assembler.revise_subagent(payload, &mut changed)
            }
            Self::Update(AgentUpdate::ToolCallUpdate { payload }) => {
                return assembler.stamp_background(payload, &mut changed)
            }
            Self::Update(update) => {
                assembler.apply(update.clone())?;
            }
            Self::Lost { .. } => assembler.lose_background_task(&mut changed)?,
            Self::Question { question, .. } => {
                assembler.revise_question(question.clone(), &mut changed)?
            }
        }
        Ok(true)
    }
}

/// A run's tool item, by tool call id, else by the task id an earlier linked
/// frame recorded on it.
fn run_tool<'a>(
    items: &'a BTreeMap<i64, LiveItem>,
    run: &str,
    provider: Option<&str>,
    task: Option<&str>,
) -> Option<&'a LiveItem> {
    let mut tools = items.values().filter(|item| {
        item.record.row.run_id.as_deref() == Some(run)
            && matches!(item.record.detail, TranscriptDetail::Tool { .. })
    });
    if let Some(provider) = provider {
        if let Some(item) = tools.clone().find(|item| matches!(&item.record.detail, TranscriptDetail::Tool { provider_id, .. } if provider_id.as_deref() == Some(provider)))
        {
            return Some(item);
        }
    }
    let task = task?;
    tools.find(|item| match &item.record.detail {
        TranscriptDetail::Tool { json, .. } => {
            super::decode::<TranscriptTool>(json).is_ok_and(|tool| {
                tool.background_task.and_then(|task| task.id).as_deref() == Some(task)
            })
        }
        _ => false,
    })
}

pub(in crate::server) struct TranscriptCoordinator {
    pub(in crate::server::chat) payloads: Arc<super::super::payloads::PayloadService>,
    pub(super) conversations: Mutex<HashMap<String, ConversationMemory>>,
    /// Taken after `conversations` when both are held.
    pub(super) background: Mutex<BackgroundTasks>,
    /// Anchor items of the native subagents this instance saw running. A
    /// thread may outlive its run; one not here reads as disconnected.
    subagents: Mutex<HashSet<String>>,
    /// A delegated child's counts for a run that ended, by run id: they
    /// can't change, and the conversation list asks for them often.
    pub(in crate::server::chat) finished_activity:
        Mutex<HashMap<String, ottam_protocol::DelegationActivity>>,
    database: Database,
    wake: Arc<Notify>,
    started: AtomicBool,
    writer: tokio::sync::Mutex<()>,
    writer_cursor: Mutex<Option<String>>,
    initialization: tokio::sync::Mutex<()>,
    watermark_floor: AtomicU64,
    // Snapshot readers must not straddle the durable execution-status commit
    // and its in-memory terminal revision/boundary. Live updates and checkpoint
    // writes never acquire this gate.
    pub(in crate::server::chat) terminal_handoff: tokio::sync::RwLock<()>,
    #[cfg(test)]
    pub(in crate::server::chat) terminal_commit_gate: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
    #[cfg(test)]
    pub(super) snapshot_read_gate: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
}

impl TranscriptCoordinator {
    pub(in crate::server) fn start_payloads(&self) {
        self.payloads.start();
    }
    pub(super) fn minimum_snapshot_sequence(&self) -> u64 {
        self.watermark_floor.load(Ordering::Relaxed)
    }
    pub(in crate::server) fn new(database: Database) -> Arc<Self> {
        Arc::new_cyclic(|owner| Self {
            payloads: super::super::payloads::PayloadService::new(database.clone(), owner.clone()),
            conversations: Mutex::new(HashMap::new()),
            background: Mutex::new(BackgroundTasks::default()),
            subagents: Mutex::new(HashSet::new()),
            finished_activity: Mutex::new(HashMap::new()),
            database,
            wake: Arc::new(Notify::new()),
            started: AtomicBool::new(false),
            writer: tokio::sync::Mutex::new(()),
            writer_cursor: Mutex::new(None),
            initialization: tokio::sync::Mutex::new(()),
            watermark_floor: AtomicU64::new(0),
            terminal_handoff: tokio::sync::RwLock::new(()),
            #[cfg(test)]
            terminal_commit_gate: Mutex::new(None),
            #[cfg(test)]
            snapshot_read_gate: Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub(super) fn manual_checkpoints(&self) {
        self.started.store(true, Ordering::Release);
    }

    /// Drops a conversation from memory as capacity eviction does.
    #[cfg(test)]
    pub(super) fn evict(&self, conversation: &ConversationID) {
        if let Some(evicted) = self
            .conversations
            .lock()
            .expect("transcript state")
            .remove(&conversation.0)
        {
            self.watermark_floor
                .fetch_max(evicted.sequence, Ordering::Relaxed);
        }
    }

    /// Holds the checkpoint writer's lock, as a checkpoint in flight does.
    #[cfg(test)]
    pub(super) async fn hold_writer(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.writer.lock().await
    }

    fn start(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let weak = Arc::downgrade(self);
        let wake = self.wake.clone();
        tokio::spawn(async move {
            loop {
                next_checkpoint(&wake).await;
                let Some(coordinator) = weak.upgrade() else {
                    break;
                };
                coordinator.checkpoint().await;
                // Do not keep the service/database alive while no owner remains.
                if Arc::strong_count(&coordinator) == 1 {
                    break;
                }
            }
        });
    }

    pub(in crate::server::chat) async fn prepare(
        self: &Arc<Self>,
        chat: &ChatService,
        conversation: &ConversationID,
    ) -> Result<(), CoreError> {
        self.start();
        if self
            .conversations
            .lock()
            .expect("transcript state")
            .contains_key(&conversation.0)
        {
            return Ok(());
        }
        let _initialization = self.initialization.lock().await;
        if self
            .conversations
            .lock()
            .expect("transcript state")
            .contains_key(&conversation.0)
        {
            return Ok(());
        }
        {
            let mut states = self.conversations.lock().expect("transcript state");
            if states.len() >= 64 {
                if let Some(id) = states
                    .iter()
                    .find(|(_, state)| {
                        state.capture.is_empty()
                            && !state.overflow_dirty
                            && state
                                .items
                                .values()
                                .all(|item| item.saved_revision == item.record.row.revision)
                    })
                    .map(|(id, _)| id.clone())
                {
                    if let Some(evicted) = states.remove(&id) {
                        // An attached Swift cache can outlive this memory entry.
                        // Preserve its watermark even though SQLite has fewer,
                        // coalesced revision publications.
                        self.watermark_floor
                            .fetch_max(evicted.sequence, Ordering::Relaxed);
                    }
                } else {
                    return Err(CoreError::new(
                        "transcriptCapacity",
                        "Too many conversations are capturing output",
                    ));
                }
            }
        }
        let id = conversation.0.clone();
        self.database.reserve_transcript_positions(&id, 0).await?;
        let workspace = chat.workspace()?;
        let parsed = super::parse_conversation_id(conversation)?;
        let (latest, sequence) = self
            .database
            .run("transcript.initialize", move |connection| {
                super::active_conversation(connection, &workspace, &parsed)?;
                Ok((
                    Repo::latest(connection, &id)?,
                    Repo::sequence(connection, &id)?,
                ))
            })
            .await?;
        self.conversations
            .lock()
            .expect("transcript state")
            .entry(conversation.0.clone())
            .or_insert_with(|| ConversationMemory {
                last_admission: latest.as_ref().map_or(-1, |row| row.position),
                latest,
                sequence: (sequence as u64).max(self.watermark_floor.load(Ordering::Relaxed)),
                workspace: Some(chat.workspace_id.clone()),
                instance: chat
                    .transcript_instance
                    .clone()
                    .unwrap_or_else(super::core_instance),
                outbound: chat.transcript_outbound.clone(),
                ..Default::default()
            });
        Ok(())
    }

    pub(super) async fn update(
        self: &Arc<Self>,
        chat: &ChatService,
        conversation: &ConversationID,
        run: &ConversationRunID,
        update: AgentUpdate,
    ) -> Result<ConversationTranscriptDeltaParams, CoreError> {
        let delta = match self.record_update(chat, conversation, run, update).await? {
            Ok(delta) => delta,
            Err(change) => self.revise_stored(chat, conversation, run, change).await?,
        };
        self.observe_subagents(&delta.items);
        Ok(delta)
    }

    /// Tracks the native subagent threads a write started or ended.
    fn observe_subagents(&self, items: &[ConversationTranscriptItem]) {
        let mut live = None;
        for item in items {
            let Some(thread) = item
                .tool
                .as_ref()
                .and_then(|tool| tool.subagent_thread.as_ref())
                .filter(|thread| thread.source == SubagentThreadSource::NativeSession)
            else {
                continue;
            };
            let live = live.get_or_insert_with(|| self.subagents.lock().expect("subagents"));
            if thread.state.is_live() {
                live.insert(item.item_id.clone());
            } else {
                live.remove(&item.item_id);
            }
        }
    }

    /// `Err` hands back a closed run's lifecycle change for a tool memory no
    /// longer holds, for `revise_stored` once the lock is released.
    async fn record_update(
        self: &Arc<Self>,
        chat: &ChatService,
        conversation: &ConversationID,
        run: &ConversationRunID,
        mut update: AgentUpdate,
    ) -> Result<Result<ConversationTranscriptDeltaParams, TaskChange>, CoreError> {
        // Bounded before a closed run can take it, as an open run's is.
        let truncated = bound_metadata(&mut update);
        let lifecycle = TaskChange::of(&update);
        if let AgentUpdate::AgentImageChunk {
            content,
            message_id,
        } = update
        {
            update = image_call(content, message_id);
        }
        let (mut states, position) = loop {
            self.prepare(chat, conversation).await?;
            if let Err(error) = self.validate_run(conversation, run).await {
                // A finished run that memory no longer holds still takes its
                // tasks' lifecycle (rule 4).
                if lifecycle && error.kind == "runNoLongerActive" {
                    return Ok(Err(TaskChange::Update(update)));
                }
                return Err(error);
            }
            let position = self
                .database
                .reserve_transcript_positions(&conversation.0, 1)
                .await?;
            let states = self.conversations.lock().expect("transcript state");
            if states.contains_key(&conversation.0) {
                break (states, position);
            }
            // Another conversation's `prepare` evicted this one during the
            // awaits above, before a capture pinned it: load it again.
        };
        let identity_update = matches!(
            update,
            AgentUpdate::ToolCall { .. }
                | AgentUpdate::ToolCallUpdate { .. }
                | AgentUpdate::Plan { .. }
                | AgentUpdate::AsyncTaskSpawned { .. }
                | AgentUpdate::AsyncTaskProgress { .. }
                | AgentUpdate::AsyncTaskStateUpdate { .. }
        );
        let global_identity = if identity_update {
            states
                .values()
                .map(ConversationMemory::identity_usage)
                .fold((0, 0), |(bytes, count), (b, c)| (bytes + b, count + c))
        } else {
            (0, 0)
        };
        let state = states.get_mut(&conversation.0).expect("checked above");
        let mut delta = ConversationTranscriptDeltaParams {
            session_states: Vec::new(),
            workspace_id: chat.workspace_id.clone(),
            conversation_id: conversation.clone(),
            core_instance_id: state.instance.clone(),
            sequence: state.sequence,
            sequence_start: None,
            items: Vec::new(),
            body_appends: Vec::new(),
            capture: Vec::new(),
            recovery_required: false,
            background_tasks: Vec::new(),
            subagent_threads: Vec::new(),
        };
        if state.closed_runs.contains(&run.0) {
            // Decided under the same lock as `finish`: a closed run takes
            // lifecycle only, and revises nothing but `backgroundTask`.
            if lifecycle {
                let change = TaskChange::Update(update);
                match self.revise_in_memory(chat, state, conversation, run, &change)? {
                    TaskRevision::Done(delta) => return Ok(Ok(*delta)),
                    TaskRevision::NeedsStorage => return Ok(Err(change)),
                    TaskRevision::Refused => {}
                }
            }
            record_closed_run_drop(conversation, run);
            return Ok(Ok(delta));
        }
        if matches!(&update, AgentUpdate::RichMessageChunk { text, .. } | AgentUpdate::AgentMessageChunk { text, .. } | AgentUpdate::AgentThoughtChunk { text, .. } if !text.is_empty())
            || matches!(
                &update,
                AgentUpdate::ToolCall { .. } | AgentUpdate::ToolCallUpdate { .. }
            )
        {
            if let Some(observation) = state.session_states.get_mut(&run.0) {
                if observation.signal.activity.as_deref() == Some("retrying") {
                    observation.signal.activity = Some("active".into());
                    observation.signal.failure = None;
                    delta.session_states.push(observation.clone());
                }
            }
        }
        if truncated {
            if let Some(capture) = state.capture.get_mut(&run.0) {
                capture.state = Some("partial".into());
                capture.problem =
                    Some("Some transcript metadata exceeded the capture budget".into());
                capture.gaps.push(TranscriptCoverage {
                    start_position: position as u64,
                    end_position: position as u64,
                });
            }
        }
        state.merge_pending_metadata(&run.0, &mut update);
        let payload_updates = match &mut update {
            AgentUpdate::ToolCall { payload } | AgentUpdate::ToolCallUpdate { payload } => {
                super::super::payloads::extract(payload)
            }
            _ => Vec::new(),
        };
        let tool_id = match &update {
            AgentUpdate::ToolCall { payload } | AgentUpdate::ToolCallUpdate { payload } => payload
                .get("toolCallId")
                .and_then(serde_json::Value::as_str),
            _ => None,
        };
        // A lifecycle update names its tool call when it can; otherwise only an
        // earlier linked update recorded the task id on the tool.
        let tool = match &update {
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
            } => run_tool(
                &state.items,
                &run.0,
                provider_tool_call_id.as_deref(),
                Some(async_task_id),
            ),
            _ => tool_id.and_then(|id| run_tool(&state.items, &run.0, Some(id), None)),
        }
        .map(|item| item.record.clone());
        // A run has one plan per thread: a subagent's never revises its
        // parent's.
        let plan_thread = match &update {
            AgentUpdate::Plan { payload } => payload
                .get(super::subagents::NATIVE_THREAD_STAMP)
                .and_then(serde_json::Value::as_str),
            _ => None,
        };
        let plan = state
            .items
            .values()
            .find(|item| {
                item.record.row.run_id.as_deref() == Some(&run.0)
                    && item.record.row.kind == "plan"
                    && super::subagents::row_thread(&item.record.row.adapter_metadata_json)
                        .as_deref()
                        == plan_thread
            })
            .map(|item| item.record.clone());
        let metadata_target = match &update {
            AgentUpdate::RichMessageChunk {
                text,
                message_id: Some(id),
                metadata,
                reasoning,
            } if text.is_empty() => state
                .items
                .values()
                .rev()
                .find(|item| {
                    let row = &item.record.row;
                    row.run_id.as_deref() == Some(&run.0)
                        && row.provider_message_id.as_ref() == Some(id)
                        && row.kind
                            == if *reasoning {
                                "reasoning"
                            } else {
                                "assistantText"
                            }
                        && super::subagents::same_segment(&row.adapter_metadata_json, metadata)
                })
                .map(|item| item.record.row.clone()),
            _ => None,
        };
        // A subagent's text continues the latest row of its own thread, never
        // the parent's: its rows never take the conversation's latest. The
        // run's rows are the newest, so the search stops at an earlier run's.
        let thread_target = match &update {
            AgentUpdate::RichMessageChunk { metadata, .. } => {
                super::subagents::incoming_thread(metadata).map(|thread| {
                    state
                        .items
                        .values()
                        .rev()
                        .take_while(|item| item.record.row.run_id.as_deref() == Some(&run.0))
                        .find(|item| {
                            super::subagents::row_thread(&item.record.row.adapter_metadata_json)
                                .as_deref()
                                == Some(thread.as_str())
                        })
                        .map(|item| item.record.row.clone())
                })
            }
            _ => None,
        };
        let mut assembler = TranscriptAssembler {
            state: AssemblyState {
                latest: metadata_target.or_else(|| match thread_target {
                    Some(target) => target,
                    None => state.latest.clone(),
                }),
                tool,
                plan,
            },
            changes: Vec::new(),
            background: Vec::new(),
            conversation: &conversation.0,
            run: &run.0,
            position,
            now: chrono::Utc::now().timestamp_millis(),
        };
        let appended = match &update {
            AgentUpdate::RichMessageChunk { text, .. }
            | AgentUpdate::AgentMessageChunk { text, .. }
            | AgentUpdate::AgentThoughtChunk { text, .. } => Some(text.clone()),
            _ => None,
        };
        let ids = assembler.apply(update)?;
        let boundary = assembler.changes.iter().any(|change| {
            change.row.completion != "open"
                || !matches!(change.row.kind.as_str(), "assistantText" | "reasoning")
        });
        // Active tool/plan identities cannot be evicted: an omitted argument
        // field in a later update still refers to this exact saved body. Reject
        // an over-budget mutation before publication instead of forgetting an
        // accepted identity. This also bounds runs containing only tools.
        let previous_usage = if identity_update {
            state.identity_usage()
        } else {
            (0, 0)
        };
        let previous_latest = state.latest.clone();
        let loss_position = assembler
            .changes
            .iter()
            .find(|change| matches!(change.row.kind.as_str(), "tool" | "plan"))
            .map_or(position, |change| change.row.position);
        let previous_items: Vec<_> = if identity_update {
            assembler
                .changes
                .iter()
                .map(|change| {
                    (
                        change.row.position,
                        state.items.get(&change.row.position).cloned(),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        let background = std::mem::take(&mut assembler.background);
        state.apply(assembler.changes);
        if identity_update {
            let usage = state.identity_usage();
            if usage.0 > CONVERSATION_BYTES
                || usage.1 > CONVERSATION_ITEMS
                || global_identity.0 - previous_usage.0 + usage.0 > GLOBAL_BYTES
                || global_identity.1 - previous_usage.1 + usage.1 > GLOBAL_ITEMS
            {
                for (position, previous) in previous_items {
                    if let Some(item) = previous {
                        state.items.insert(position, item);
                    } else {
                        state.items.remove(&position);
                    }
                }
                state.latest = previous_latest;
                let capture =
                    state
                        .capture
                        .entry(run.0.clone())
                        .or_insert_with(|| TranscriptCaptureStatus {
                            run_id: run.clone(),
                            state: Some("open".into()),
                            problem: None,
                            gaps: Vec::new(),
                        });
                capture.state = Some("partial".into());
                capture.problem =
                    Some("Some tool or plan updates exceeded the capture budget".into());
                capture.gaps.push(TranscriptCoverage {
                    start_position: loss_position as u64,
                    end_position: loss_position as u64,
                });
                state.compact_gaps();
                state.sequence += 1;
                delta.sequence = state.sequence;
                delta.capture = state.capture_status();
                self.wake.notify_one();
                return Ok(Ok(delta));
            }
        }
        delta.background_tasks = self.observe_background(&conversation.0, &run.0, background);
        if !payload_updates.is_empty() {
            if let Some(item) = state.items.values_mut().find(|item| {
                ids.contains(&item.record.row.id)
                    && matches!(item.record.row.kind.as_str(), "tool" | "image")
            }) {
                let descriptors = self.payloads.submit(
                    &chat.workspace_id.0,
                    &conversation.0,
                    &run.0,
                    &item.record.row.id,
                    payload_updates,
                );
                if let TranscriptDetail::Tool { json, .. } = &mut item.record.detail {
                    let mut tool: TranscriptTool = super::decode(json)?;
                    for descriptor in descriptors {
                        tool.payloads.retain(|p| p.role != descriptor.role);
                        tool.payloads.push(descriptor);
                    }
                    *json = super::encode(&tool)?;
                }
            }
        }
        for id in ids {
            if let Some(item) = state.items.values().find(|item| item.record.row.id == id) {
                delta.items.push(item.to_wire()?);
            }
        }
        if delta.items.iter().any(|item| {
            item.tool
                .as_ref()
                .is_some_and(|tool| tool.subagent_thread.is_some())
        }) {
            delta.subagent_threads = state
                .subagent_counts()
                .into_iter()
                .filter(|count| count.run_id == *run)
                .collect();
        }
        if let (Some(text), Some(item)) = (appended, delta.items.last_mut()) {
            let mut offset = item.body_byte_count - text.len() as u64;
            // The client already holds the body up to `offset`, so the append
            // alone carries the change: the preview is a prefix of the body it
            // can rebuild. A new item (offset 0) still sends its preview.
            if offset > 0
                && matches!(
                    item.kind,
                    TranscriptItemKind::AssistantText | TranscriptItemKind::Reasoning
                )
            {
                item.body_preview = String::new();
                item.preview_omitted = true;
            }
            let mut rest = text.as_str();
            while !rest.is_empty() {
                let part = prefix(rest, CHECKPOINT_BYTES);
                delta.body_appends.push(TranscriptBodyAppend {
                    item_id: item.item_id.clone(),
                    revision: item.revision,
                    offset,
                    text: part.into(),
                });
                offset += part.len() as u64;
                rest = &rest[part.len()..];
            }
        }
        if !delta.items.is_empty() || !delta.session_states.is_empty() {
            state.sequence += 1;
        }
        delta.sequence = state.sequence;
        state
            .capture
            .entry(run.0.clone())
            .or_insert_with(|| TranscriptCaptureStatus {
                run_id: run.clone(),
                state: Some("open".into()),
                problem: None,
                gaps: Vec::new(),
            });
        if boundary || state.dirty_bytes >= CHECKPOINT_BYTES {
            state.dirty_bytes = 0;
            self.wake.notify_one();
        }
        self.enforce_bounds(&mut states);
        if let Some(state) = states.get(&conversation.0) {
            delta.capture = state.capture_status();
            delta.sequence = state.sequence;
        }
        Ok(Ok(delta))
    }

    pub(in crate::server::chat) fn accept_run(
        &self,
        conversation: &ConversationID,
        run: &ConversationRunID,
    ) {
        if let Some(state) = self
            .conversations
            .lock()
            .expect("transcript state")
            .get_mut(&conversation.0)
        {
            state
                .capture
                .entry(run.0.clone())
                .or_insert_with(|| TranscriptCaptureStatus {
                    run_id: run.clone(),
                    state: Some("open".into()),
                    problem: None,
                    gaps: Vec::new(),
                });
        }
    }

    async fn validate_run(
        &self,
        conversation: &ConversationID,
        run: &ConversationRunID,
    ) -> Result<(), CoreError> {
        if self
            .conversations
            .lock()
            .expect("transcript state")
            .get(&conversation.0)
            .is_some_and(|state| {
                state.capture.contains_key(&run.0) || state.closed_runs.contains(&run.0)
            })
        {
            return Ok(());
        }
        let conv = conversation.0.clone();
        let id = crate::storage::models::ConversationRunId::try_from(run.0.clone())?;
        self.database
            .run("transcript.validateRun", move |connection| {
                let run = crate::storage::repositories::RunRepository::get_run(connection, &id)?;
                if run.conversation_id.to_string() != conv {
                    return Err(StorageError::new(
                        "storageConstraint",
                        "Run belongs to another conversation",
                    ));
                }
                if !matches!(
                    run.status,
                    crate::storage::models::RunStatus::Queued
                        | crate::storage::models::RunStatus::Running
                ) {
                    return Err(StorageError::new(
                        "runNoLongerActive",
                        "Run has already finished",
                    ));
                }
                Ok(())
            })
            .await?;
        self.accept_run(conversation, run);
        Ok(())
    }

    pub(super) fn observe_admission(
        &self,
        conversation: &ConversationID,
        items: &mut Vec<ConversationTranscriptItem>,
    ) -> Result<(), CoreError> {
        let newest = items
            .iter()
            .filter(|item| item.kind == TranscriptItemKind::Message)
            .map(|item| item.position as i64)
            .max();
        self.observe_position(conversation, newest, items)
    }

    pub(in crate::server::chat) fn admitted(
        &self,
        conversation: &ConversationID,
        position: i64,
    ) -> Result<(), CoreError> {
        self.observe_position(conversation, Some(position), &mut Vec::new())?;
        let states = self.conversations.lock().expect("transcript state");
        if let Some(state) = states.get(&conversation.0) {
            self.notify_state(conversation, state, true);
            // The admitted message may already be older than the latest page
            // by the time its transaction returns. Recover the loaded coverage.
            if let Some(outbound) = &state.outbound {
                let _ = outbound.notify(serde_json::json!({"jsonrpc":"2.0",
                    "method":"conversation.transcriptRecoveryRequired", "params":{}}));
            }
        }
        Ok(())
    }

    fn observe_position(
        &self,
        conversation: &ConversationID,
        newest: Option<i64>,
        items: &mut Vec<ConversationTranscriptItem>,
    ) -> Result<(), CoreError> {
        let mut states = self.conversations.lock().expect("transcript state");
        let Some(state) = states.get_mut(&conversation.0) else {
            return Ok(());
        };
        if let Some(position) = newest.filter(|position| *position > state.last_admission) {
            state.last_admission = position;
            if let Some(latest) = &state.latest {
                if matches!(latest.kind.as_str(), "assistantText" | "reasoning")
                    && latest.completion == "open"
                {
                    if let Some(item) = state.items.get_mut(&latest.position) {
                        item.record.row.completion = "partial".into();
                        item.record.row.revision += 1;
                        items.retain(|wire| wire.item_id != item.record.row.id);
                        items.push(item.to_wire()?);
                    }
                }
            }
            state.latest = None;
            state.sequence += 1;
            self.wake.notify_one();
        }
        Ok(())
    }

    pub(in crate::server::chat) async fn permission(
        self: &Arc<Self>,
        chat: &ChatService,
        conversation: &ConversationID,
        run: &ConversationRunID,
        payload: &serde_json::Value,
    ) -> Result<(), CoreError> {
        let Some(value) = payload.get("transcriptPermission") else {
            return Ok(());
        };
        let mut permission: TranscriptPermission = serde_json::from_value(value.clone())
            .map_err(|_| CoreError::new("invalidParams", "Invalid transcript permission"))?;
        let truncated = permission.title.len() > 4096
            || permission.outcome_label.len() > 1024
            || permission
                .plan
                .as_ref()
                .is_some_and(|plan| plan.len() > 65536);
        permission.title = prefix(&permission.title, 4096).into();
        permission.outcome_label = prefix(&permission.outcome_label, 1024).into();
        if let Some(plan) = permission.plan.as_mut() {
            *plan = prefix(plan, 65536).into();
        }
        self.prepare(chat, conversation).await?;
        self.validate_run(conversation, run).await?;
        let position = self
            .database
            .reserve_transcript_positions(&conversation.0, 1)
            .await?;
        let mut states = self.conversations.lock().expect("transcript state");
        let state = states.get_mut(&conversation.0).expect("prepared");
        if state.closed_runs.contains(&run.0) {
            return Ok(());
        }
        if truncated {
            if let Some(capture) = state.capture.get_mut(&run.0) {
                capture.state = Some("partial".into());
                capture.problem =
                    Some("Some permission description exceeded the capture budget".into());
            }
        }
        if state.items.values().any(|item| matches!(&item.record.detail, TranscriptDetail::Permission { request_id, .. } if request_id == &permission.request_id)) { return Ok(()); }
        let mut assembler = TranscriptAssembler {
            state: AssemblyState {
                latest: state.latest.clone(),
                ..Default::default()
            },
            changes: Vec::new(),
            background: Vec::new(),
            conversation: &conversation.0,
            run: &run.0,
            position,
            now: chrono::Utc::now().timestamp_millis(),
        };
        // A subagent's decision is its thread's row: the parent's open text
        // segment stays open around it.
        let thread = payload
            .get("subagentThreadID")
            .and_then(serde_json::Value::as_str);
        if thread.is_none() {
            assembler.close(&mut Vec::new())?;
        }
        let mut row = assembler.new_row("permission")?;
        row.completion = "complete".into();
        if thread.is_some() {
            let mut metadata: super::subagents::StoredMetadata =
                super::decode(&row.adapter_metadata_json)?;
            metadata.resolve_thread(thread);
            row.adapter_metadata_json = super::encode(&metadata)?;
        }
        assembler.changes.push(AssemblyChange {
            row,
            detail: TranscriptDetail::Permission {
                request_id: permission.request_id.clone(),
                json: serde_json::to_string(&permission).expect("permission"),
            },
            body: None,
            replace: false,
            takes_latest: thread.is_none(),
        });
        state.apply(assembler.changes);
        state.sequence += 1;
        self.notify_state(conversation, state, true);
        self.enforce_bounds(&mut states);
        self.wake.notify_one();
        Ok(())
    }

    pub(in crate::server::chat) async fn finish(
        self: &Arc<Self>,
        chat: &ChatService,
        conversation: &ConversationID,
        run: &ConversationRunID,
        status: ConversationRunStatus,
        failure: Option<&str>,
    ) -> Result<(), CoreError> {
        self.prepare(chat, conversation).await?;
        let position = self
            .database
            .reserve_transcript_positions(&conversation.0, 1)
            .await?;
        let mut states = self.conversations.lock().expect("transcript state");
        let state = states.get_mut(&conversation.0).expect("prepared");
        let status_name = serde_json::to_value(&status)
            .expect("run status")
            .as_str()
            .expect("status string")
            .to_owned();
        let prior_status = state
            .final_status
            .insert(run.0.clone(), status_name.clone());
        state
            .pending_metadata
            .retain(|pending| pending.run != run.0);
        if let Some(observation) = state.session_states.get_mut(&run.0) {
            observation.live = false;
            observation.signal.activity = None;
        }
        if !state.closed_runs.insert(run.0.clone()) {
            let mut changed = prior_status.as_ref() != Some(&status_name);
            for item in state
                .items
                .values_mut()
                .filter(|item| item.record.row.run_id.as_deref() == Some(&run.0))
            {
                if item.record.run_status.as_deref() != Some(&status_name) {
                    item.record.run_status = Some(status_name.clone());
                    item.record.row.revision += 1;
                    changed = true;
                }
            }
            if changed {
                state.settled_runs.remove(&run.0);
                state
                    .capture
                    .entry(run.0.clone())
                    .or_insert_with(|| TranscriptCaptureStatus {
                        run_id: run.clone(),
                        state: Some("open".into()),
                        problem: None,
                        gaps: Vec::new(),
                    });
                if let Some(capture) = state.capture.get_mut(&run.0) {
                    if capture.state.as_deref() == Some("complete") {
                        capture.state = Some("open".into());
                    }
                }
                state.sequence += 1;
                self.notify_state(conversation, state, true);
                self.wake.notify_one();
            }
            return Ok(());
        }
        for (id, descriptor) in self.payloads.finish_run(&run.0) {
            if let Some(item) = state
                .items
                .values_mut()
                .find(|item| item.record.row.id == id)
            {
                if let TranscriptDetail::Tool { json, .. } = &mut item.record.detail {
                    let mut tool: TranscriptTool = super::decode(json)?;
                    tool.payloads.retain(|p| p.role != descriptor.role);
                    tool.payloads.push(descriptor);
                    *json = super::encode(&tool)?;
                    item.record.row.revision += 1;
                }
            }
            if let Some(capture) = state.capture.get_mut(&run.0) {
                capture.state = Some("partial".into());
            }
        }
        let suppress = matches!(
            failure,
            Some(
                "authRequired"
                    | "authRefreshBusy"
                    | "invalidAuthMethod"
                    | "selectionUnavailable"
                    | "agentNotFound"
                    | "adapterNotInstalled"
                    | "runtimeMissing"
                    | "runtimeInvalid"
                    | "runtimeUnsupported"
            )
        );
        let completion = if status == ConversationRunStatus::Completed {
            "complete"
        } else {
            "partial"
        };
        for item in state
            .items
            .values_mut()
            .filter(|item| item.record.row.run_id.as_deref() == Some(&run.0))
        {
            item.record.run_status = Some(status_name.clone());
            if let TranscriptDetail::Tool { json, .. } = &mut item.record.detail {
                if super::subagents::stored_thread(json)
                    .is_some_and(|thread| thread.state.is_live())
                {
                    let mut tool: TranscriptTool = super::decode(json)?;
                    if super::subagents::finish_run(&mut tool, &status) {
                        *json = super::encode(&tool)?;
                    }
                }
                // A question nobody answered by the run's end stays unanswered.
                let mut tool: TranscriptTool = super::decode(json)?;
                if super::settle_question(&mut tool) {
                    *json = super::encode(&tool)?;
                }
            }
            if suppress && matches!(item.record.row.kind.as_str(), "assistantText" | "reasoning") {
                item.record.row.completion = "suppressed".into();
                item.record.row.body_byte_count = 0;
                item.chunks.clear();
                item.body_start = 0;
                item.record.preview.clear();
                item.body_revision = item.record.row.revision + 1;
            } else if item.record.row.completion == "open" {
                item.record.row.completion = completion.into();
            }
            item.record.row.revision += 1;
        }
        let now = chrono::Utc::now().timestamp_millis();
        let row = TranscriptRow {
            id: uuid::Uuid::now_v7().to_string(),
            conversation_id: conversation.0.clone(),
            run_id: Some(run.0.clone()),
            position,
            revision: 1,
            kind: "runBoundary".into(),
            completion: completion.into(),
            provider_message_id: None,
            adapter_metadata_json: "{}".into(),
            body_byte_count: 0,
            created_at: now,
            updated_at: now,
        };
        state.latest = Some(row.clone());
        state.items.insert(
            position,
            LiveItem {
                record: TranscriptRecord {
                    committed_revision: 0,
                    row,
                    detail: TranscriptDetail::None,
                    preview: String::new(),
                    run_status: Some(status_name),
                },
                chunks: Vec::new(),
                saved_revision: 0,
                body_start: 0,
                body_revision: 1,
            },
        );
        state
            .capture
            .entry(run.0.clone())
            .or_insert_with(|| TranscriptCaptureStatus {
                run_id: run.clone(),
                state: Some("open".into()),
                problem: None,
                gaps: Vec::new(),
            });
        state.sequence += 1;
        self.notify_state(conversation, state, true);
        self.enforce_bounds(&mut states);
        self.wake.notify_one();
        Ok(())
    }

    fn notify_state(
        &self,
        conversation: &ConversationID,
        state: &ConversationMemory,
        include_items: bool,
    ) {
        if let (Some(outbound), Some(workspace)) = (&state.outbound, &state.workspace) {
            let items: Vec<_> = if include_items {
                state
                    .items
                    .values()
                    .filter_map(|item| item.to_wire().ok())
                    .collect()
            } else {
                Vec::new()
            };
            let subagent_threads = if include_items {
                super::subagents::counts(items.iter().filter_map(super::subagents::wire_thread))
            } else {
                Vec::new()
            };
            let delta = ConversationTranscriptDeltaParams {
                session_states: state.session_states.values().cloned().collect(),
                workspace_id: workspace.clone(),
                conversation_id: conversation.clone(),
                core_instance_id: state.instance.clone(),
                sequence: state.sequence,
                sequence_start: None,
                items,
                body_appends: Vec::new(),
                capture: state.capture_status(),
                recovery_required: false,
                background_tasks: Vec::new(),
                subagent_threads,
            };
            let _ = outbound.notify(
                serde_json::to_value(ProtocolNotification::new(
                    NOTIFICATION_CONVERSATION_TRANSCRIPT_DELTA,
                    delta,
                ))
                .expect("typed capture delta"),
            );
        }
    }

    fn enforce_bounds(&self, states: &mut HashMap<String, ConversationMemory>) {
        let mut total_bytes: usize = states
            .values()
            .flat_map(|state| state.items.values())
            .map(LiveItem::bytes)
            .sum();
        let mut total_items: usize = states.values().map(|state| state.items.len()).sum();
        for (conversation, state) in states.iter_mut() {
            let mut lost = false;
            // Repeated completed runs during a storage outage must not grow
            // unbounded tombstones. Coalesce older uncertainty into one range.
            while state.closed_runs.len() > 128 {
                let run = state.closed_runs.iter().min().cloned().expect("closed run");
                let positions: Vec<_> = state
                    .items
                    .values()
                    .filter(|item| item.record.row.run_id.as_ref() == Some(&run))
                    .map(|item| item.record.row.position as u64)
                    .collect();
                let unsettled = !state.settled_runs.contains(&run);
                if unsettled {
                    let overflow =
                        state
                            .overflow_capture
                            .get_or_insert_with(|| TranscriptCaptureStatus {
                                run_id: ConversationRunID(run.clone()),
                                state: Some("partial".into()),
                                problem: Some(
                                    "Older transcript capture exceeded the retention budget".into(),
                                ),
                                gaps: Vec::new(),
                            });
                    let prior_ranges = state
                        .capture
                        .get(&run)
                        .map(|capture| capture.gaps.clone())
                        .unwrap_or_default();
                    let first = positions
                        .iter()
                        .copied()
                        .chain(prior_ranges.iter().map(|range| range.start_position))
                        .min();
                    let last = positions
                        .iter()
                        .copied()
                        .chain(prior_ranges.iter().map(|range| range.end_position))
                        .max();
                    if let Some((first, last)) = first.zip(last) {
                        if let Some(range) = overflow.gaps.first_mut() {
                            range.start_position = range.start_position.min(first);
                            range.end_position = range.end_position.max(last);
                        } else {
                            overflow.gaps.push(TranscriptCoverage {
                                start_position: first,
                                end_position: last,
                            });
                        }
                    }
                    state.overflow_dirty = true;
                    lost = true;
                }
                state.items.retain(|_, item| {
                    if item.record.row.run_id.as_ref() != Some(&run) {
                        return true;
                    }
                    total_bytes = total_bytes.saturating_sub(item.bytes());
                    total_items -= 1;
                    false
                });
                state.closed_runs.remove(&run);
                state.final_status.remove(&run);
                state.settled_runs.remove(&run);
                state.capture.remove(&run);
                if state
                    .latest
                    .as_ref()
                    .is_some_and(|row| row.run_id.as_ref() == Some(&run))
                {
                    state.latest = None;
                }
            }
            let mut bytes: usize = state.items.values().map(LiveItem::bytes).sum();
            while bytes > CONVERSATION_BYTES
                || state.items.len() > CONVERSATION_ITEMS
                || total_bytes > GLOBAL_BYTES
                || total_items > GLOBAL_ITEMS
            {
                // Clean completed items remain available through local history.
                let position = state
                    .items
                    .iter()
                    .find(|(_, item)| {
                        item.saved_revision == item.record.row.revision
                            && item.record.row.completion != "open"
                            && (!matches!(item.record.row.kind.as_str(), "tool" | "plan")
                                || item
                                    .record
                                    .row
                                    .run_id
                                    .as_ref()
                                    .is_some_and(|run| state.closed_runs.contains(run)))
                    })
                    .map(|(position, _)| *position)
                    .or_else(|| {
                        state
                            .items
                            .iter()
                            .find(|(_, item)| !state.preserves_identity(item))
                            .map(|(position, _)| *position)
                    });
                let Some(position) = position else {
                    break;
                };
                let item = state.items.remove(&position).expect("selected item");
                let size = item.bytes();
                bytes = bytes.saturating_sub(size);
                total_bytes = total_bytes.saturating_sub(size);
                total_items -= 1;
                if item.saved_revision != item.record.row.revision {
                    lost = true;
                    if let Some(run) = &item.record.row.run_id {
                        let capture = state.capture.entry(run.clone()).or_insert_with(|| {
                            TranscriptCaptureStatus {
                                run_id: ConversationRunID(run.clone()),
                                state: Some("open".into()),
                                problem: None,
                                gaps: Vec::new(),
                            }
                        });
                        capture.state = Some("partial".into());
                        capture.problem = Some("Some output could not be retained".into());
                        capture.gaps.push(TranscriptCoverage {
                            start_position: position as u64,
                            end_position: position as u64,
                        });
                        capture.gaps.sort_by_key(|range| range.start_position);
                        while capture.gaps.len() > 128 {
                            let first = capture.gaps.remove(0);
                            capture.gaps[0].start_position = first.start_position;
                        }
                    }
                    if state
                        .latest
                        .as_ref()
                        .is_some_and(|last| last.id == item.record.row.id)
                    {
                        state.latest = None;
                    }
                }
            }
            state.compact_gaps();
            if lost {
                state.sequence += 1;
                self.notify_state(&ConversationID(conversation.clone()), state, false);
            }
        }
    }

    pub(super) async fn checkpoint(&self) {
        let _writer = self.writer.lock().await;
        // Keep only one immutable revision in flight, rotating conversations.
        let jobs = {
            let states = self.conversations.lock().expect("transcript state");
            let mut cursor = self.writer_cursor.lock().expect("checkpoint cursor");
            let mut candidates: Vec<_> = states
                .iter()
                .filter_map(|(id, state)| {
                    state
                        .items
                        .values()
                        .find(|item| item.saved_revision < item.record.row.revision)
                        .map(|item| (id, item))
                })
                .collect();
            candidates.sort_by_key(|(id, _)| *id);
            let selected = candidates
                .iter()
                .find(|(id, _)| cursor.as_ref().is_none_or(|last| *id > last))
                .or_else(|| candidates.first());
            selected.map(|(id, item)| {
                *cursor = Some((*id).clone());
                ((*id).clone(), (*item).clone())
            })
        };
        for (conversation, item) in jobs.into_iter() {
            let result = self.write_item(&item).await;
            if result.is_ok() {
                if let TranscriptDetail::Tool { json, .. } = &item.record.detail {
                    if let Ok(tool) = super::decode::<TranscriptTool>(json) {
                        for descriptor in &tool.payloads {
                            self.payloads.checkpointed(
                                &conversation,
                                &item.record.row.id,
                                descriptor,
                            );
                        }
                    }
                }
            }
            let mut states = self.conversations.lock().expect("transcript state");
            let Some(state) = states.get_mut(&conversation) else {
                continue;
            };
            if let Some(current) = state.items.get_mut(&item.record.row.position) {
                if result.is_ok() {
                    current.saved_revision = current.saved_revision.max(item.record.row.revision);
                    // Release committed prefixes without changing item identity or
                    // offsets. Replacements racing a write retain their new body.
                    if current.record.row.revision == item.record.row.revision
                        || (matches!(
                            current.record.row.kind.as_str(),
                            "assistantText" | "reasoning"
                        ) && current.record.row.completion != "suppressed"
                            && current.record.row.body_byte_count
                                >= item.record.row.body_byte_count)
                    {
                        let mut skip = (item.record.row.body_byte_count as usize)
                            .saturating_sub(current.body_start);
                        let mut retained = Vec::new();
                        for chunk in current.chunks.drain(..) {
                            if skip >= chunk.len() {
                                skip -= chunk.len();
                            } else {
                                retained.push(if skip == 0 {
                                    chunk
                                } else {
                                    let tail = Arc::from(&chunk[skip..]);
                                    skip = 0;
                                    tail
                                });
                            }
                        }
                        current.chunks = retained;
                        current.body_start = item.record.row.body_byte_count as usize;
                    }
                }
            }
            if let Some(run) = &item.record.row.run_id {
                if let Some(capture) = state.capture.get_mut(run) {
                    let previous = capture.problem.clone();
                    if result.is_err() {
                        capture.problem = Some("Transcript could not be saved; retrying".into());
                    } else if capture.state.as_deref() != Some("partial") {
                        capture.problem = None;
                    }
                    if capture.problem != previous {
                        state.sequence += 1;
                    }
                }
            }
            self.notify_state(&ConversationID(conversation), state, false);
            if result.is_ok()
                && states.values().any(|state| {
                    state
                        .items
                        .values()
                        .any(|item| item.saved_revision < item.record.row.revision)
                })
            {
                self.wake.notify_one();
            }
        }
        let overflows: Vec<_> = self
            .conversations
            .lock()
            .expect("transcript state")
            .iter()
            .filter(|(_, state)| state.overflow_dirty)
            .map(|(id, state)| {
                (
                    id.clone(),
                    state.sequence,
                    serde_json::to_string(
                        &state
                            .overflow_capture
                            .as_ref()
                            .map(|capture| capture.gaps.clone())
                            .unwrap_or_default(),
                    )
                    .expect("overflow gaps"),
                )
            })
            .collect();
        for (conversation, sequence, gaps) in overflows {
            let saved_conversation = conversation.clone();
            let result = self.database.run("transcript.recordOverflow", move |connection| {
                // A bounded SQLite subquery conservatively covers terminal
                // captures whose individual retry metadata had to be evicted.
                Ok(diesel::sql_query("UPDATE transcript_capture SET state = 'partial', gaps_json = ? WHERE run_id IN (SELECT c.run_id FROM transcript_capture c JOIN conversation_runs r ON r.id = c.run_id WHERE r.conversation_id = ? AND c.state = 'open' AND r.status NOT IN ('queued', 'running') LIMIT 128)")
                    .bind::<diesel::sql_types::Text, _>(gaps)
                    .bind::<diesel::sql_types::Text, _>(saved_conversation)
                    .execute(connection)?)
            }).await;
            if let Ok(count) = result {
                let mut states = self.conversations.lock().expect("transcript state");
                if let Some(state) = states.get_mut(&conversation) {
                    if count < 128 && state.sequence == sequence {
                        state.overflow_dirty = false;
                    } else {
                        self.wake.notify_one();
                    }
                }
            }
        }
        let completed: Vec<_> =
            self.conversations
                .lock()
                .expect("transcript state")
                .iter()
                .flat_map(|(conversation, state)| {
                    state
                        .closed_runs
                        .iter()
                        .filter(|run| {
                            !state.settled_runs.contains(*run)
                                && !self.payloads.pending(run)
                                && !state.items.values().any(|item| {
                                    item.record.row.run_id.as_ref() == Some(*run)
                                        && item.saved_revision < item.record.row.revision
                                })
                        })
                        .map(|run| {
                            (
                                conversation.clone(),
                                run.clone(),
                                state.final_status.get(run).cloned().unwrap_or_default(),
                                serde_json::to_string(
                                    &state
                                        .capture
                                        .get(run)
                                        .map(|capture| capture.gaps.clone())
                                        .unwrap_or_default(),
                                )
                                .expect("capture gaps"),
                                if state.capture.get(run).is_some_and(|capture| {
                                    capture.state.as_deref() == Some("partial")
                                }) {
                                    "partial"
                                } else {
                                    "complete"
                                },
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
        let losses: Vec<_> = self
            .conversations
            .lock()
            .expect("transcript state")
            .values()
            .flat_map(|state| {
                state
                    .capture
                    .values()
                    .filter(|capture| capture.state.as_deref() == Some("partial"))
                    .map(|capture| {
                        (
                            capture.run_id.0.clone(),
                            serde_json::to_string(&capture.gaps).expect("capture gaps"),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        for (run, gaps) in losses {
            let _ = self
                .database
                .run("transcript.recordLoss", move |connection| {
                    use crate::storage::schema::transcript_capture as capture;
                    diesel::update(capture::table.find(run))
                        .set((capture::state.eq("partial"), capture::gaps_json.eq(gaps)))
                        .execute(connection)?;
                    Ok(())
                })
                .await;
        }
        for (conversation, run, execution_status, gaps, capture_state) in completed {
            let saved_run = run.clone();
            let saved_status = execution_status.clone();
            let result = self
                .database
                .run("transcript.finishCapture", move |connection| {
                    // SQLite's correlated status fence makes reclassification
                    // and the final marker atomic without a second transaction.
                    let count = diesel::sql_query("UPDATE transcript_capture SET state = ?, gaps_json = ? WHERE run_id = ? AND EXISTS (SELECT 1 FROM conversation_runs WHERE id = ? AND status = ?)")
                        .bind::<diesel::sql_types::Text, _>(capture_state)
                        .bind::<diesel::sql_types::Text, _>(&gaps)
                        .bind::<diesel::sql_types::Text, _>(&saved_run)
                        .bind::<diesel::sql_types::Text, _>(&saved_run)
                        .bind::<diesel::sql_types::Text, _>(&saved_status)
                        .execute(connection)?;
                    Ok(count == 1)
                })
                .await;
            if matches!(result, Ok(true)) {
                let mut states = self.conversations.lock().expect("transcript state");
                if let Some(state) = states.get_mut(&conversation) {
                    // Reclassification may have created new revisions while
                    // SQLite was committing the previous final marker.
                    if state.items.values().any(|item| {
                        item.record.row.run_id.as_ref() == Some(&run)
                            && (item.saved_revision < item.record.row.revision
                                || item.record.run_status.as_deref() != Some(&execution_status))
                    }) {
                        self.wake.notify_one();
                        continue;
                    }
                    if let Some(capture) = state.capture.get_mut(&run) {
                        capture.state = Some(capture_state.into());
                        capture.problem = (capture_state == "partial")
                            .then(|| partial_capture_problem(&capture.gaps).into());
                    }
                    state.settled_runs.insert(run.clone());
                    self.payloads.retire_run(&run);
                    state.sequence += 1;
                    self.notify_state(&ConversationID(conversation), state, false);
                    state.capture.remove(&run);
                    if state.settled_runs.len() > 200 {
                        if let Some(old) = state.settled_runs.iter().find(|id| *id != &run).cloned()
                        {
                            state.settled_runs.remove(&old);
                            state.closed_runs.remove(&old);
                            state.final_status.remove(&old);
                            state
                                .items
                                .retain(|_, item| item.record.row.run_id.as_ref() != Some(&old));
                        }
                    }
                }
            } else {
                let mut states = self.conversations.lock().expect("transcript state");
                if let Some(state) = states.get_mut(&conversation) {
                    if let Some(capture) = state.capture.get_mut(&run) {
                        capture.problem =
                            Some("Capture completion could not be saved; retrying".into());
                    }
                    state.sequence += 1;
                    self.notify_state(&ConversationID(conversation), state, false);
                }
            }
        }
    }

    pub(in crate::server) async fn drain(&self) {
        loop {
            let before = self.pending_count();
            self.checkpoint().await;
            let remaining = self.pending_count();
            if remaining == 0 {
                break;
            }
            if remaining >= before {
                tokio::time::sleep(Duration::from_secs(1)).await;
            } else {
                tokio::task::yield_now().await;
            }
        }
    }

    fn pending_count(&self) -> usize {
        self.conversations
            .lock()
            .expect("transcript state")
            .values()
            .map(|state| {
                state
                    .items
                    .values()
                    .filter(|item| item.saved_revision < item.record.row.revision)
                    .count()
                    + usize::from(state.overflow_dirty)
                    + state
                        .closed_runs
                        .iter()
                        .filter(|run| !state.settled_runs.contains(*run))
                        .count()
            })
            .sum()
    }

    pub(in crate::server) fn disconnect(&self) {
        self.conversations.lock().expect("transcript state").clear();
    }

    pub(in crate::server::chat) fn forget(&self, conversation: &ConversationID) {
        self.conversations
            .lock()
            .expect("transcript state")
            .remove(&conversation.0);
        self.background
            .lock()
            .expect("background tasks")
            .forget(&conversation.0);
    }

    pub(in crate::server::chat) fn payload_updated(
        &self,
        conversation: &str,
        id: &str,
        descriptor: TranscriptPayload,
        generation: Option<u64>,
    ) {
        let mut states = self.conversations.lock().expect("transcript state");
        // Check while holding the same coordinator lock as Run cancellation, so
        // a delayed capture callback cannot overwrite its final descriptor.
        if generation.is_some_and(|generation| {
            !self
                .payloads
                .delivery_is_current(conversation, id, &descriptor.role, generation)
        }) {
            return;
        }
        let Some(state) = states.get_mut(conversation) else {
            return;
        };
        let Some(item) = state
            .items
            .values_mut()
            .find(|item| item.record.row.id == id)
        else {
            return;
        };
        if item.record.row.completion == "suppressed" {
            return;
        }
        let TranscriptDetail::Tool { json, .. } = &mut item.record.detail else {
            return;
        };
        let Ok(mut tool) = super::decode::<TranscriptTool>(json) else {
            return;
        };
        if tool
            .payloads
            .iter()
            .any(|p| p.role == descriptor.role && p.revision > descriptor.revision)
        {
            return;
        }
        let loss = descriptor.locally_truncated;
        tool.payloads.retain(|p| p.role != descriptor.role);
        tool.payloads.push(descriptor);
        let Ok(encoded) = super::encode(&tool) else {
            return;
        };
        if *json != encoded {
            *json = encoded;
            item.record.row.revision += 1;
            state.sequence += 1;
        }
        let changed = item.to_wire().ok();
        if loss {
            if let Some(run) = &item.record.row.run_id {
                if let Some(capture) = state.capture.get_mut(run) {
                    capture.state = Some("partial".into());
                    capture.problem =
                        Some("Some tool details could not be saved completely".into());
                }
            }
        }
        if let (Some(outbound), Some(workspace), Some(changed)) =
            (&state.outbound, &state.workspace, changed)
        {
            let delta = ConversationTranscriptDeltaParams {
                workspace_id: workspace.clone(),
                conversation_id: ConversationID(conversation.into()),
                core_instance_id: state.instance.clone(),
                sequence: state.sequence,
                sequence_start: None,
                items: vec![changed],
                body_appends: Vec::new(),
                capture: state.capture_status(),
                session_states: Vec::new(),
                recovery_required: false,
                background_tasks: Vec::new(),
                subagent_threads: Vec::new(),
            };
            let _ = outbound.notify(
                serde_json::to_value(ProtocolNotification::new(
                    NOTIFICATION_CONVERSATION_TRANSCRIPT_DELTA,
                    delta,
                ))
                .expect("payload delta"),
            );
        }
        self.wake.notify_one();
    }

    pub(in crate::server::chat) fn payload_work_finished(&self) {
        self.wake.notify_one();
    }

    /// Marks a task lost (rule 7). Its run may be open, when its adapter died
    /// mid-turn, or closed; a task that already ended keeps its state.
    /// Applies one tool revision a run takes open or closed: a lost task, a
    /// question's record.
    pub(super) async fn revise(
        self: &Arc<Self>,
        chat: &ChatService,
        conversation: &ConversationID,
        run: &ConversationRunID,
        change: TaskChange,
    ) -> Result<ConversationTranscriptDeltaParams, CoreError> {
        self.prepare(chat, conversation).await?;
        let revision = {
            let mut states = self.conversations.lock().expect("transcript state");
            match states.get_mut(&conversation.0) {
                Some(state) => self.revise_in_memory(chat, state, conversation, run, &change)?,
                None => TaskRevision::NeedsStorage,
            }
        };
        let delta = match revision {
            TaskRevision::Done(delta) => *delta,
            TaskRevision::NeedsStorage => {
                self.revise_stored(chat, conversation, run, change).await?
            }
            TaskRevision::Refused => self.current_delta(chat, conversation),
        };
        self.observe_subagents(&delta.items);
        Ok(delta)
    }

    /// Revises only the tool's `backgroundTask`, in memory, like
    /// `payload_updated`; the checkpoint writer persists it.
    fn revise_in_memory(
        &self,
        chat: &ChatService,
        state: &mut ConversationMemory,
        conversation: &ConversationID,
        run: &ConversationRunID,
        change: &TaskChange,
    ) -> Result<TaskRevision, CoreError> {
        let (provider, task) = change.locator();
        let Some(position) =
            run_tool(&state.items, &run.0, provider, task).map(|item| item.record.row.position)
        else {
            return Ok(TaskRevision::NeedsStorage);
        };
        let mut assembler = TranscriptAssembler {
            state: AssemblyState {
                tool: Some(state.items[&position].record.clone()),
                ..Default::default()
            },
            changes: Vec::new(),
            background: Vec::new(),
            conversation: &conversation.0,
            run: &run.0,
            position,
            now: chrono::Utc::now().timestamp_millis(),
        };
        if !change.apply(&mut assembler)? {
            return Ok(TaskRevision::Refused);
        }
        let mut delta = ConversationTranscriptDeltaParams {
            session_states: Vec::new(),
            workspace_id: chat.workspace_id.clone(),
            conversation_id: conversation.clone(),
            core_instance_id: state.instance.clone(),
            sequence: state.sequence,
            sequence_start: None,
            items: Vec::new(),
            body_appends: Vec::new(),
            capture: Vec::new(),
            recovery_required: false,
            background_tasks: self.observe_background(
                &conversation.0,
                &run.0,
                std::mem::take(&mut assembler.background),
            ),
            subagent_threads: Vec::new(),
        };
        if let Some(revised) = assembler.changes.pop() {
            let item = state.items.get_mut(&position).expect("found tool");
            item.record.row = revised.row;
            item.record.detail = revised.detail;
            delta.items.push(item.to_wire()?);
            state.sequence += 1;
            delta.sequence = state.sequence;
            delta.capture = state.capture_status();
            self.wake.notify_one();
        }
        Ok(TaskRevision::Done(Box::new(delta)))
    }

    /// Revises a closed run's tool that memory no longer holds, straight in
    /// storage. Memory never takes the record back. The writer lock keeps a
    /// checkpoint from publishing an older copy over this revision.
    async fn revise_stored(
        self: &Arc<Self>,
        chat: &ChatService,
        conversation: &ConversationID,
        run: &ConversationRunID,
        change: TaskChange,
    ) -> Result<ConversationTranscriptDeltaParams, CoreError> {
        enum Stored {
            /// No tool of the run has the call id.
            Missing,
            /// The change left the tool as it was.
            Unchanged,
            Refused,
            /// The revision, the run counts it moved, and storage's sequence
            /// after it.
            Revised(
                Box<TranscriptRecord>,
                Vec<ConversationRunBackgroundTasks>,
                u64,
            ),
        }
        // A frame the closed run cannot place counts as `closed_run`; a loss
        // with nothing to mark is not a drop.
        let counted = matches!(change, TaskChange::Update(_));
        // Only a tool call id finds a stored tool; once a task is anchored the
        // ownership index supplies it.
        let Some(provider) = change.locator().0.map(str::to_owned) else {
            if counted {
                record_closed_run_drop(conversation, run);
            }
            return Ok(self.current_delta(chat, conversation));
        };
        let _writer = self.writer.lock().await;
        let coordinator = Arc::clone(self);
        let (conv, run_id) = (conversation.0.clone(), run.0.clone());
        let now = chrono::Utc::now().timestamp_millis();
        let stored = self
            .database
            .run("transcript.reviseBackgroundTask", move |connection| {
                crate::storage::database::recorded_transaction(
                    connection,
                    "transcript.reviseBackgroundTask",
                    |connection| {
                        let Some(record) = Repo::run_tool(connection, &conv, &run_id, &provider)?
                        else {
                            return Ok(Stored::Missing);
                        };
                        let mut assembler = TranscriptAssembler {
                            state: AssemblyState {
                                tool: Some(record.clone()),
                                ..Default::default()
                            },
                            changes: Vec::new(),
                            background: Vec::new(),
                            conversation: &conv,
                            run: &run_id,
                            position: record.row.position,
                            now,
                        };
                        if !change.apply(&mut assembler)? {
                            return Ok(Stored::Refused);
                        }
                        let Some(revised) = assembler.changes.pop() else {
                            return Ok(Stored::Unchanged);
                        };
                        // Metadata only: the body and its checkpoint head stay.
                        if !Repo::save(connection, &revised.row, &revised.detail, "")? {
                            return Ok(Stored::Unchanged);
                        }
                        let sequence = Repo::sequence(connection, &conv)? as u64;
                        // Before the commit: a page that reads this revision
                        // must find a task it starts live, or the read rule
                        // would call it lost.
                        let counts =
                            coordinator.observe_background(&conv, &run_id, assembler.background);
                        Ok(Stored::Revised(
                            Box::new(TranscriptRecord {
                                committed_revision: revised.row.revision,
                                row: revised.row,
                                detail: revised.detail,
                                ..record
                            }),
                            counts,
                            sequence,
                        ))
                    },
                )
            })
            .await?;
        let mut delta = self.current_delta(chat, conversation);
        match stored {
            Stored::Unchanged => {}
            Stored::Missing | Stored::Refused => {
                if counted {
                    record_closed_run_drop(conversation, run);
                }
            }
            Stored::Revised(record, counts, stored_sequence) => {
                match self
                    .conversations
                    .lock()
                    .expect("transcript state")
                    .get_mut(&conversation.0)
                {
                    Some(state) => {
                        state.sequence += 1;
                        delta.sequence = state.sequence;
                        delta.capture = state.capture_status();
                    }
                    // The next `prepare` starts from storage's sequence, which
                    // this save advanced.
                    None => delta.sequence = delta.sequence.max(stored_sequence),
                }
                delta.items.push(to_wire_with_live(*record, false)?);
                delta.background_tasks = counts;
            }
        }
        Ok(delta)
    }

    /// A delta that changes nothing, at the conversation's current sequence.
    fn current_delta(
        &self,
        chat: &ChatService,
        conversation: &ConversationID,
    ) -> ConversationTranscriptDeltaParams {
        let states = self.conversations.lock().expect("transcript state");
        let state = states.get(&conversation.0);
        ConversationTranscriptDeltaParams {
            session_states: Vec::new(),
            workspace_id: chat.workspace_id.clone(),
            conversation_id: conversation.clone(),
            core_instance_id: state.map_or_else(
                || {
                    chat.transcript_instance
                        .clone()
                        .unwrap_or_else(super::core_instance)
                },
                |state| state.instance.clone(),
            ),
            sequence: state
                .map_or_else(|| self.minimum_snapshot_sequence(), |state| state.sequence),
            sequence_start: None,
            items: Vec::new(),
            body_appends: Vec::new(),
            capture: Vec::new(),
            recovery_required: false,
            background_tasks: Vec::new(),
            subagent_threads: Vec::new(),
        }
    }

    /// Records the background changes one write made to `run`'s tools and
    /// returns the run's count if it moved.
    fn observe_background(
        &self,
        conversation: &str,
        run: &str,
        changes: Vec<(String, AsyncTaskState)>,
    ) -> Vec<ConversationRunBackgroundTasks> {
        if changes.is_empty() {
            return Vec::new();
        }
        let mut background = self.background.lock().expect("background tasks");
        changes
            .into_iter()
            .filter_map(|(item, state)| background.observe(conversation, run, &item, state))
            .last()
            .into_iter()
            .collect()
    }

    pub(super) fn background_snapshot(
        &self,
        conversation: &str,
    ) -> Vec<ConversationRunBackgroundTasks> {
        self.background
            .lock()
            .expect("background tasks")
            .snapshot(conversation)
    }

    /// A stored task still running that this instance never saw start was
    /// recorded by an earlier core, and nothing can end it now (rule 7). Only
    /// for items read from storage, and only after the read: a task saved
    /// while the read ran is already in the live set.
    pub(super) fn read_lost(&self, items: &mut [ConversationTranscriptItem]) {
        {
            let subagents = self.subagents.lock().expect("subagents");
            for item in items.iter_mut() {
                let id = item.item_id.clone();
                if let Some(thread) = super::subagents::native_outliving(item) {
                    if !subagents.contains(&id) {
                        thread.state = SubagentThreadState::Disconnected;
                    }
                }
            }
        }
        let background = self.background.lock().expect("background tasks");
        for item in items {
            if let Some(task) = item
                .tool
                .as_mut()
                .and_then(|tool| tool.background_task.as_mut())
                .filter(|task| task.state.is_live())
            {
                if !background.live.contains_key(&item.item_id) {
                    task.state = AsyncTaskState::Lost;
                }
            }
        }
    }

    async fn write_item(&self, item: &LiveItem) -> Result<(), StorageError> {
        use crate::storage::schema::transcript_checkpoint_bodies as bodies;
        let existing_id = item.record.row.id.clone();
        let existing = self
            .database
            .run("transcript.checkPublished", move |connection| {
                use crate::storage::schema::transcript_items as items;
                Ok(items::table
                    .find(existing_id)
                    .select(items::revision)
                    .first::<i64>(connection)
                    .optional()?)
            })
            .await?;
        if let Some(revision) = existing {
            if revision == item.record.row.revision {
                return Ok(());
            }
            if revision > item.record.row.revision {
                return Err(StorageError::new(
                    "staleCheckpoint",
                    "A newer transcript revision is already committed",
                ));
            }
        }
        let id = item.record.row.id.clone();
        let base = item.body_start;
        let new_revision = item.record.row.revision;
        let body_revision = if base > 0 {
            self.database
                .run("transcript.bodyHead", move |connection| {
                    use crate::storage::schema::transcript_checkpoint_heads as heads;
                    Ok(heads::table
                        .find(&id)
                        .select(heads::revision)
                        .first::<i64>(connection)?)
                })
                .await?
        } else {
            new_revision
        };
        // Remove only unacknowledged suffixes left by a failed attempt. Published
        // prefixes are immutable, so a header switch never needs to copy them.
        loop {
            let id = item.record.row.id.clone();
            let removed = self
                .database
                .run("transcript.discardStagedSuffix", move |connection| {
                    let offsets = bodies::table
                        .filter(bodies::item_id.eq(&id))
                        .filter(bodies::revision.eq(body_revision))
                        .filter(bodies::byte_offset.ge(base as i64))
                        .limit(4)
                        .select(bodies::byte_offset)
                        .load::<i64>(connection)?;
                    for offset in &offsets {
                        diesel::delete(bodies::table.find((&id, body_revision, *offset)))
                            .execute(connection)?;
                    }
                    Ok(offsets.len())
                })
                .await?;
            if removed == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        let mut offset = base as i64;
        for chunk in &item.chunks {
            let chunk = chunk.clone();
            let row = item.record.row.clone();
            let at = offset;
            offset += chunk.len() as i64;
            self.database
                .run("transcript.stageBody", move |connection| {
                    diesel::insert_into(bodies::table)
                        .values((
                            bodies::conversation_id.eq(&row.conversation_id),
                            bodies::item_id.eq(&row.id),
                            bodies::revision.eq(body_revision),
                            bodies::byte_offset.eq(at),
                            bodies::body.eq(chunk.as_ref()),
                        ))
                        .on_conflict((bodies::item_id, bodies::revision, bodies::byte_offset))
                        .do_nothing()
                        .execute(connection)?;
                    Ok(())
                })
                .await?;
            tokio::task::yield_now().await;
        }
        let record = item.record.clone();
        self.database
            .run("transcript.publishCheckpoint", move |connection| {
                crate::storage::database::recorded_transaction(
                    connection,
                    "transcript.publishCheckpoint",
                    |connection| {
                        if !Repo::publish_checkpoint(
                            connection,
                            &record.row,
                            &record.detail,
                            body_revision,
                        )? {
                            return Err(StorageError::new(
                                "staleCheckpoint",
                                "Checkpoint revision was superseded",
                            ));
                        }
                        Ok(())
                    },
                )
            })
            .await?;
        loop {
            let id = item.record.row.id.clone();
            let removed = self
                .database
                .run("transcript.collectOldBodies", move |connection| {
                    use crate::storage::schema::transcript_checkpoint_heads as heads;
                    let head = heads::table
                        .find(&id)
                        .select(heads::revision)
                        .first::<i64>(connection)
                        .optional()?;
                    let Some(head) = head else {
                        return Ok(0);
                    };
                    let old = bodies::table
                        .filter(bodies::item_id.eq(&id))
                        .filter(bodies::revision.ne(head))
                        .limit(4)
                        .select((bodies::revision, bodies::byte_offset))
                        .load::<(i64, i64)>(connection)?;
                    for (revision, offset) in &old {
                        diesel::delete(bodies::table.find((&id, *revision, *offset)))
                            .execute(connection)?;
                    }
                    Ok(old.len())
                })
                .await?;
            if removed == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        Ok(())
    }
}

/// A finished run's transcript takes no more updates, so this one is lost.
/// Attribution stops routing to a run before it closes, so this should be
/// rare; it is counted and logged at powers of two.
fn record_closed_run_drop(conversation: &ConversationID, run: &ConversationRunID) {
    static DROPS: std::sync::LazyLock<BoundedCounts> = std::sync::LazyLock::new(Default::default);
    let (_, occurrences) = DROPS.record("transcript", "closed_run");
    if is_log_point(occurrences) {
        tracing::warn!(
            conversation_id = %conversation.0,
            run_id = %run.0,
            reason = "closed_run",
            occurrences,
            "agent update arrived for a run that already closed"
        );
    }
}

/// An image is stored as a completed call, so it rides the tool path: a
/// subagent's image takes its thread's stamp there.
pub(in crate::server) fn image_call(
    content: serde_json::Value,
    message_id: Option<String>,
) -> AgentUpdate {
    AgentUpdate::ToolCall {
        payload: serde_json::json!({"_ottamImage": true, "messageID": message_id, "title": "Image", "status": "completed", "content": [content]}),
    }
}

#[cfg(test)]
mod clock_tests {
    use super::super::tests::{fixture, text};

    #[tokio::test]
    async fn transcript_cache_eviction_preserves_the_attached_clients_watermark() {
        let (_home, chat, conversation, run) = fixture().await;
        let coordinator = &chat.transcript_coordinator;
        coordinator.manual_checkpoints();
        chat.record_transcript_update(&conversation, &run, text("saved", None))
            .await
            .unwrap();
        chat.finalize_run(
            &conversation,
            &run,
            ConversationRunStatus::Completed,
            None,
            serde_json::Value::Null,
        )
        .await
        .unwrap();
        coordinator.drain().await;
        let other = chat
            .create_conversation(
                WorkingRootSelection::WorkspaceFiles,
                Vec::new(),
                Some("Other".into()),
                None,
            )
            .await
            .unwrap()
            .conversation
            .conversation
            .conversation_id;
        {
            let mut states = coordinator.conversations.lock().unwrap();
            states.get_mut(&conversation.0).unwrap().sequence = 1000;
            for index in 0..63 {
                let mut state = ConversationMemory::default();
                state.capture.insert(
                    "active".into(),
                    TranscriptCaptureStatus {
                        run_id: ConversationRunID("active".into()),
                        state: Some("open".into()),
                        problem: None,
                        gaps: Vec::new(),
                    },
                );
                states.insert(format!("busy-{index}"), state);
            }
        }
        coordinator.prepare(&chat, &other).await.unwrap();
        let snapshot = chat
            .list_transcript(ConversationListTranscriptParams {
                workspace_id: chat.workspace_id.clone(),
                conversation_id: conversation.clone(),
                limit: None,
                cursor: None,
                subagent_thread_id: None,
            })
            .await
            .unwrap();
        assert!(snapshot.sequence >= 1000);
        {
            let mut states = coordinator.conversations.lock().unwrap();
            assert!(!states.contains_key(&conversation.0));
            states.retain(|id, _| !id.starts_with("busy-"));
        }
        coordinator.prepare(&chat, &conversation).await.unwrap();
        assert!(coordinator.conversations.lock().unwrap()[&conversation.0].sequence >= 1000);
    }

    #[tokio::test]
    async fn transcript_repeated_runs_during_write_failure_bound_retry_metadata() {
        let (_home, chat, conversation, run) = fixture().await;
        let coordinator = &chat.transcript_coordinator;
        coordinator.manual_checkpoints();
        chat.record_transcript_update(&conversation, &run, text("retained", None))
            .await
            .unwrap();
        let mut states = coordinator.conversations.lock().unwrap();
        let state = states.get_mut(&conversation.0).unwrap();
        let template = state.items.values().last().unwrap().clone();
        for index in 1..=600 {
            let run = format!("closed-{index:04}");
            let mut item = template.clone();
            item.record.row.run_id = Some(run.clone());
            item.record.row.id = format!("item-{index}");
            item.record.row.position = index;
            state.items.insert(index, item);
            state.closed_runs.insert(run.clone());
            state.final_status.insert(run.clone(), "completed".into());
            state.capture.insert(
                run.clone(),
                TranscriptCaptureStatus {
                    run_id: ConversationRunID(run),
                    state: Some("open".into()),
                    problem: None,
                    gaps: Vec::new(),
                },
            );
        }
        coordinator.enforce_bounds(&mut states);
        let state = &states[&conversation.0];
        assert_eq!(state.closed_runs.len(), 128);
        assert!(state.capture.len() <= 129);
        let overflow = state.overflow_capture.as_ref().unwrap();
        assert_eq!(overflow.state.as_deref(), Some("partial"));
        assert_eq!(overflow.gaps.len(), 1);
        assert_eq!(overflow.gaps[0].start_position, 1);
        assert_eq!(overflow.gaps[0].end_position, 472);
        assert!(state.overflow_dirty);
    }
    use super::*;

    #[tokio::test]
    async fn transcript_stale_staged_tool_revision_cannot_replace_newer_arguments() {
        let (_home, chat, conversation, run) = fixture().await;
        let coordinator = &chat.transcript_coordinator;
        coordinator.manual_checkpoints();
        chat.record_transcript_update(
            &conversation,
            &run,
            AgentUpdate::ToolCall {
                payload: serde_json::json!({"toolCallId":"tool", "rawInput":{"version":"old"}}),
            },
        )
        .await
        .unwrap();
        let old = coordinator.conversations.lock().unwrap()[&conversation.0]
            .items
            .values()
            .last()
            .unwrap()
            .clone();
        chat.record_transcript_update(
            &conversation,
            &run,
            AgentUpdate::ToolCallUpdate {
                payload: serde_json::json!({"toolCallId":"tool", "rawInput":["new", "🧠"]}),
            },
        )
        .await
        .unwrap();
        coordinator.write_item(&old).await.unwrap();
        coordinator.checkpoint().await;
        assert!(coordinator.write_item(&old).await.is_err());
        let row = coordinator.conversations.lock().unwrap()[&conversation.0]
            .items
            .values()
            .last()
            .unwrap()
            .record
            .row
            .clone();
        let body = chat
            .database
            .run("test.latestTool", move |connection| {
                Repo::read_body(
                    connection,
                    &row.conversation_id,
                    &row.id,
                    row.revision,
                    0,
                    65536,
                )
            })
            .await
            .unwrap()
            .0;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap(),
            serde_json::json!(["new", "🧠"])
        );
    }

    #[tokio::test]
    async fn transcript_deleted_conversation_rejects_an_inflight_revision() {
        let (_home, chat, conversation, run) = fixture().await;
        let coordinator = &chat.transcript_coordinator;
        coordinator.manual_checkpoints();
        chat.record_transcript_update(&conversation, &run, text("not resurrected", None))
            .await
            .unwrap();
        let queued = coordinator.conversations.lock().unwrap()[&conversation.0]
            .items
            .values()
            .last()
            .unwrap()
            .clone();
        chat.delete_conversation(&conversation).await.unwrap();
        assert!(coordinator.write_item(&queued).await.is_err());
        assert!(!coordinator
            .conversations
            .lock()
            .unwrap()
            .contains_key(&conversation.0));
        let saved = chat
            .database
            .run("test.deletedTranscript", move |connection| {
                Repo::list(connection, &conversation.0, None, 50, None)
            })
            .await
            .unwrap();
        assert!(saved.0.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn transcript_checkpoint_clock_coalesces_requests_and_ticks_after_one_second() {
        let wake = Arc::new(Notify::new());
        let signal = wake.clone();
        let task = tokio::spawn(async move {
            next_checkpoint(&signal).await;
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(999)).await;
        assert!(!task.is_finished());
        tokio::time::advance(Duration::from_millis(1)).await;
        task.await.unwrap();
        wake.notify_one();
        wake.notify_one();
        wake.notify_one();
        next_checkpoint(&wake).await;
        let start = tokio::time::Instant::now();
        next_checkpoint(&wake).await;
        assert_eq!(start.elapsed(), Duration::from_secs(1));
    }
}
