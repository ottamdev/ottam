//! Subagent threads (`sub:2`, `sub:3`).
//!
//! Legacy Claude marks a subagent's control call (`Agent` or `Task`) with
//! AIR's `subagent` flag and stamps each child's first report with the
//! control call's id as `parentToolUseId`. A thread is keyed by that id, so a
//! child that arrives before its control call, or in a later run, still names
//! the same thread.
//!
//! A native subagent session has no control call. Core writes a synthetic
//! anchor call at the spawn's position (`NATIVE_ANCHOR_KEY`), and the
//! forwarder stamps every update from the child's session with its thread.
//! Either way membership rides in the row's stored metadata JSON beside the
//! adapter's own metadata: no schema change.
use ottam_protocol::{
    ConversationRunID, ConversationRunStatus, ConversationRunSubagentThreads,
    ConversationTranscriptItem, SubagentThreadSource, SubagentThreadState,
    TranscriptAdapterMetadata, TranscriptSubagentFollowUp, TranscriptSubagentThread,
    TranscriptTool,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Longest stored thread name or task, in bytes.
const LABEL_BYTES: usize = 256;

/// Longest stored prompt or follow-up message, in bytes: shown in full in
/// the subagent's tab, so longer than a label.
pub(crate) const MESSAGE_BYTES: usize = 8 * 1024;

/// What a row's `adapter_metadata_json` holds: the adapter's metadata, whose
/// `subagentThreadID` core sets for a native child or derives here.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct StoredMetadata {
    #[serde(flatten)]
    pub adapter: TranscriptAdapterMetadata,
}

impl StoredMetadata {
    pub fn subagent_thread_id(&self) -> Option<&str> {
        self.adapter.subagent_thread_id.as_deref()
    }

    /// Membership is set once: by the native session the row's first update
    /// arrived on, else by the stored parent link, never a later payload.
    /// Claude sends the link only on a child's first report.
    pub fn resolve_thread(&mut self, native: Option<&str>) {
        if self.adapter.subagent_thread_id.is_none() {
            self.adapter.subagent_thread_id = native
                .map(str::to_owned)
                .or_else(|| self.adapter.parent_tool_call_id.as_deref().map(thread_id));
        }
    }
}

/// The thread incoming metadata names: a native child's, else the legacy
/// parent link's.
pub(super) fn incoming_thread(incoming: &TranscriptAdapterMetadata) -> Option<String> {
    incoming
        .subagent_thread_id
        .clone()
        .or_else(|| incoming.parent_tool_call_id.as_deref().map(thread_id))
}

/// The thread a stored row belongs to.
pub(super) fn row_thread(stored_json: &str) -> Option<String> {
    serde_json::from_str::<StoredMetadata>(stored_json)
        .ok()?
        .adapter
        .subagent_thread_id
}

/// Whether incoming text metadata continues a row's segment: same session
/// and same thread, so a subagent's text never joins its parent's.
pub(super) fn same_segment(stored_json: &str, incoming: &TranscriptAdapterMetadata) -> bool {
    serde_json::from_str::<StoredMetadata>(stored_json).is_ok_and(|prior| {
        prior.adapter.session_id == incoming.session_id
            && prior.adapter.subagent_thread_id == incoming_thread(incoming)
    })
}

/// A thread's anchor in a wire item, as `(item id, thread id, parent)`.
pub(super) fn wire_anchor(
    item: &ConversationTranscriptItem,
) -> Option<(String, String, Option<String>)> {
    let thread = item.tool.as_ref()?.subagent_thread.as_ref()?;
    Some((
        item.item_id.clone(),
        thread.id.clone(),
        thread.parent_thread_id.clone(),
    ))
}

/// The threads a thread's page shows, its own and every nested one, and
/// those threads' anchors.
pub(super) struct ThreadFamily {
    pub threads: Vec<String>,
    pub anchors: Vec<String>,
}

impl ThreadFamily {
    pub fn new(thread: &str, anchors: &[(String, String, Option<String>)]) -> Self {
        let mut threads = vec![thread.to_owned()];
        let mut index = 0;
        while let Some(parent) = threads.get(index).cloned() {
            for (_, child, _) in anchors
                .iter()
                .filter(|(_, _, of)| of.as_deref() == Some(parent.as_str()))
            {
                if !threads.contains(child) {
                    threads.push(child.clone());
                }
            }
            index += 1;
        }
        let mut anchors: Vec<String> = anchors
            .iter()
            .filter(|(_, id, _)| threads.contains(id))
            .map(|(item, ..)| item.clone())
            .collect();
        anchors.dedup();
        Self { threads, anchors }
    }

    pub fn contains(&self, item: &ConversationTranscriptItem) -> bool {
        item.subagent_thread_id
            .as_ref()
            .is_some_and(|thread| self.threads.contains(thread))
            || wire_anchor(item).is_some_and(|(_, thread, _)| self.threads.contains(&thread))
            || item
                .tool
                .as_ref()
                .and_then(|tool| tool.subagent_follow_up.as_ref())
                .is_some_and(|follow_up| self.threads.contains(&follow_up.thread_id))
    }
}

/// Stable within a conversation: Claude's tool call ids are unique.
pub(super) fn thread_id(control_call: &str) -> String {
    format!("subagent:{control_call}")
}

/// Opens a thread on a marked control call and keeps its label and state
/// current. An asynchronous launch completes the call at once; its thread
/// runs until the run ends (`finish_run`).
pub(super) fn track(tool: &mut TranscriptTool, payload: &Value) {
    if let Some(follow_up) = payload.get(NATIVE_FOLLOW_UP_STAMP) {
        let text = |key| follow_up.get(key).and_then(Value::as_str);
        if let (Some(thread_id), Some(message)) = (text("threadID"), text("message")) {
            tool.subagent_follow_up = Some(TranscriptSubagentFollowUp {
                thread_id: thread_id.to_owned(),
                message: bounded_to(message, MESSAGE_BYTES),
            });
        }
    }
    if let Some(native) = payload.get(NATIVE_ANCHOR_KEY) {
        track_native(tool, native);
        return;
    }
    if tool.subagent_thread.is_none() {
        let marked = payload
            .pointer("/_meta/jetbrains/air/subagent")
            .and_then(Value::as_bool)
            == Some(true);
        let control = matches!(
            payload
                .pointer("/_meta/claudeCode/toolName")
                .and_then(Value::as_str),
            Some("Agent" | "Task")
        );
        let Some(id) = tool
            .provider_tool_call_id
            .as_deref()
            .filter(|_| marked && control)
        else {
            return;
        };
        tool.subagent_thread = Some(TranscriptSubagentThread {
            id: thread_id(id),
            parent_thread_id: None,
            name: None,
            task: None,
            prompt: None,
            state: SubagentThreadState::Running,
            source: SubagentThreadSource::LegacyParentId,
            asynchronous: false,
        });
    }
    let status = tool.status.clone();
    let Some(thread) = tool.subagent_thread.as_mut() else {
        return;
    };
    if let Some(input) = payload.get("rawInput").filter(|input| input.is_object()) {
        let label = |key| input.get(key).and_then(Value::as_str).map(bounded);
        thread.name = label("subagent_type").or(thread.name.take());
        thread.task = label("description").or(thread.task.take());
        // The request says how the agent runs before any result does: Claude
        // runs it in the background unless asked not to.
        thread.asynchronous |= input
            .get("run_in_background")
            .and_then(Value::as_bool)
            .unwrap_or(true);
    }
    let response = payload.pointer("/_meta/claudeCode/toolResponse");
    if response
        .and_then(|response| response.get("isAsync"))
        .and_then(Value::as_bool)
        == Some(true)
        || response
            .and_then(|response| response.get("status"))
            .and_then(Value::as_str)
            == Some("async_launched")
    {
        thread.asynchronous = true;
    }
    if thread.state == SubagentThreadState::Running {
        match status.as_deref() {
            Some("failed") => thread.state = SubagentThreadState::Failed,
            Some("completed") if !thread.asynchronous => {
                thread.state = SubagentThreadState::Completed
            }
            _ => {}
        }
    }
}

/// The key core's synthetic anchor call carries for a native subagent: the
/// thread record on `subagent_spawned`, the state on `subagent_state_update`.
/// A native session has no control call, so core writes one at the spawn's
/// position (see `docs/architecture/backend/persistence.md`).
pub(crate) const NATIVE_ANCHOR_KEY: &str = "ottamSubagent";

/// The key the forwarder stamps on a native child's tool payloads, naming the
/// thread of the session they arrived on, as `ottamSessionID` names it.
pub(crate) const NATIVE_THREAD_STAMP: &str = "ottamSubagentThreadID";

/// The key the forwarder stamps on a parent's call that messaged a running
/// subagent: `{threadID, message}`.
pub(crate) const NATIVE_FOLLOW_UP_STAMP: &str = "ottamSubagentFollowUp";

/// The thread id of a native child session. A resumed subagent's
/// `:generation:<n>` session is a new run of the same subagent, so it joins
/// the thread its first session opened.
pub(crate) fn native_thread_id(child_session: &str) -> String {
    format!("subagent:{}", native_subagent_id(child_session))
}

/// The subagent a native child session belongs to: its id without the
/// `:generation:<n>` both adapters append to a resumed run's session.
pub(crate) fn native_subagent_id(child_session: &str) -> &str {
    match child_session.rsplit_once(":generation:") {
        Some((base, generation))
            if !base.is_empty()
                && !generation.is_empty()
                && generation.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            base
        }
        _ => child_session,
    }
}

fn track_native(tool: &mut TranscriptTool, anchor: &Value) {
    let thread = tool
        .subagent_thread
        .get_or_insert_with(|| TranscriptSubagentThread {
            id: anchor
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            parent_thread_id: None,
            name: None,
            task: None,
            prompt: None,
            state: SubagentThreadState::Running,
            source: SubagentThreadSource::NativeSession,
            // Its state is the adapter's, not the run's or the call's.
            asynchronous: true,
        });
    let label = |key| anchor.get(key).and_then(Value::as_str).map(bounded);
    thread.name = label("name").or(thread.name.take());
    thread.task = label("task").or(thread.task.take());
    thread.prompt = anchor
        .get("prompt")
        .and_then(Value::as_str)
        .map(|prompt| bounded_to(prompt, MESSAGE_BYTES))
        .or(thread.prompt.take());
    if let Some(state) = anchor
        .get("state")
        .and_then(|state| serde_json::from_value::<SubagentThreadState>(state.clone()).ok())
        .filter(|_| thread.state.is_live())
    {
        thread.state = state;
    }
}

/// A run's end ends its running legacy threads. Claude holds a turn open
/// while its subagents run, so a completed run's threads completed; otherwise
/// core knows only that the run stopped. A native subagent reports its own
/// end and may outlive its run: only Stop ends it here.
pub(super) fn finish_run(tool: &mut TranscriptTool, status: &ConversationRunStatus) -> bool {
    let Some(thread) = tool.subagent_thread.as_mut().filter(|thread| {
        thread.state.is_live()
            && (thread.source != SubagentThreadSource::NativeSession
                || matches!(status, ConversationRunStatus::Canceled))
    }) else {
        return false;
    };
    thread.state = match status {
        ConversationRunStatus::Completed => SubagentThreadState::Completed,
        ConversationRunStatus::Canceled => SubagentThreadState::Cancelled,
        _ => SubagentThreadState::Disconnected,
    };
    true
}

/// A stored running legacy thread whose run has ended was saved by a core
/// that stopped before the run finished; nothing can end it now. A native
/// thread may outlive its run: `read_lost` judges it.
pub(super) fn read_disconnected(item: &mut ConversationTranscriptItem) {
    let ended = item.run_status.as_ref().is_some_and(|status| {
        !matches!(
            status,
            ConversationRunStatus::Running | ConversationRunStatus::Queued
        )
    });
    if let Some(thread) = item
        .tool
        .as_mut()
        .and_then(|tool| tool.subagent_thread.as_mut())
        .filter(|thread| {
            ended && thread.state.is_live() && thread.source != SubagentThreadSource::NativeSession
        })
    {
        thread.state = SubagentThreadState::Disconnected;
    }
}

/// A native thread still running whose run has ended. It reads as
/// disconnected unless this core knows its subagent is live.
pub(super) fn native_outliving(
    item: &mut ConversationTranscriptItem,
) -> Option<&mut TranscriptSubagentThread> {
    let ended = item.run_status.as_ref().is_some_and(|status| {
        !matches!(
            status,
            ConversationRunStatus::Running | ConversationRunStatus::Queued
        )
    });
    item.tool
        .as_mut()
        .and_then(|tool| tool.subagent_thread.as_mut())
        .filter(|thread| {
            ended && thread.state.is_live() && thread.source == SubagentThreadSource::NativeSession
        })
}

/// The thread a stored tool JSON carries, read without decoding the rest.
pub(super) fn stored_thread(tool_json: &str) -> Option<TranscriptSubagentThread> {
    #[derive(Deserialize)]
    struct Probe {
        #[serde(default, rename = "subagentThread")]
        thread: Option<TranscriptSubagentThread>,
    }
    serde_json::from_str::<Probe>(tool_json).ok()?.thread
}

/// The `(run, thread)` a wire item carries, for `counts`.
pub(super) fn wire_thread(
    item: &ConversationTranscriptItem,
) -> Option<(String, &TranscriptSubagentThread)> {
    Some((
        item.run_id.as_ref()?.0.clone(),
        item.tool.as_ref()?.subagent_thread.as_ref()?,
    ))
}

/// Running threads per run, zeros included, over runs with any thread.
pub(super) fn counts<T: std::borrow::Borrow<TranscriptSubagentThread>>(
    threads: impl IntoIterator<Item = (String, T)>,
) -> Vec<ConversationRunSubagentThreads> {
    let mut runs = BTreeMap::<String, u32>::new();
    for (run, thread) in threads {
        *runs.entry(run).or_default() += u32::from(thread.borrow().state.is_live());
    }
    runs.into_iter()
        .map(|(run, live)| ConversationRunSubagentThreads {
            run_id: ConversationRunID(run),
            live,
        })
        .collect()
}

fn bounded(value: &str) -> String {
    bounded_to(value, LABEL_BYTES)
}

pub(crate) fn bounded_to(value: &str, limit: usize) -> String {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}
