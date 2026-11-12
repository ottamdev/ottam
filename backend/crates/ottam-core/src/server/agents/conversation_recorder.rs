use super::conversation_runner::ConversationRunContext;
use super::events::send_conversation_notification;
use crate::core_error::CoreError;
use crate::server::chat::ChatService;
use crate::server::outbound::OutboundSink;
use ottam_protocol::{
    AgentUpdate, ConversationEventKind, ConversationID, ConversationRunID, ProjectID,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

const RECORDER_CAPACITY: usize = 128;
const RECORDER_BATCH_SIZE: usize = 32;
/// The most often one tool call's progress is recorded and published.
///
/// Some agents report progress far faster than anyone reads it: omp's subagent
/// batch re-sends every subagent's full progress about five times a second per
/// call, which with four calls running came to 13 MB of transcript revisions in
/// 75 seconds — enough to overflow the client's delivery budget and stall its
/// requests. Each update is a snapshot (`content`/`rawOutput` replace the last),
/// so recording only the latest one per interval loses nothing that stays on
/// screen.
pub(super) const TOOL_PROGRESS_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Default)]
struct LatestMetadata {
    title: Option<String>,
    events: Vec<(ConversationEventKind, Value)>,
}

enum RecorderCommand {
    MetadataReady,
    Event {
        event_kind: ConversationEventKind,
        payload: Value,
    },
    Flush(oneshot::Sender<Result<(), CoreError>>),
    Close(oneshot::Sender<Result<(), CoreError>>),
}

#[derive(Clone)]
pub(super) struct ConversationRecorderHandle {
    sender: mpsc::Sender<RecorderCommand>,
    chat: ChatService,
    conversation_id: ConversationID,
    run_id: ConversationRunID,
    outbound: OutboundSink,
    latest: Arc<Mutex<LatestMetadata>>,
    assembly_error: Arc<Mutex<Option<CoreError>>>,
    /// Held across each transcript write, so a progress flush and an inline
    /// update can never record one call's states out of order.
    progress: Arc<tokio::sync::Mutex<ToolProgress>>,
}

/// Rate-limits progress on open tool calls; see `TOOL_PROGRESS_INTERVAL`.
#[derive(Default)]
struct ToolProgress {
    calls: HashMap<String, ToolProgressCall>,
}

struct ToolProgressCall {
    recorded_at: Instant,
    status: Option<String>,
    /// Progress received since `recorded_at`, merged into one update.
    held: Option<Value>,
}

pub(super) struct ConversationRecorder {
    task: JoinHandle<()>,
}

impl ConversationRecorder {
    pub(super) fn spawn(
        chat: ChatService,
        conversation_id: ConversationID,
        run_id: ConversationRunID,
        project_id: Option<ProjectID>,
        outbound: OutboundSink,
    ) -> (ConversationRecorderHandle, Self) {
        let (sender, receiver) = mpsc::channel(RECORDER_CAPACITY);
        let latest = Arc::new(Mutex::new(LatestMetadata::default()));
        let handle = ConversationRecorderHandle {
            sender,
            chat: chat.clone(),
            conversation_id: conversation_id.clone(),
            run_id: run_id.clone(),
            outbound: outbound.clone(),
            latest: latest.clone(),
            assembly_error: Arc::new(Mutex::new(None)),
            progress: Arc::default(),
        };
        let task = tokio::spawn(recorder_loop(
            chat,
            conversation_id,
            run_id,
            project_id,
            outbound,
            receiver,
            latest,
        ));
        (handle, Self { task })
    }

    pub(super) async fn close(self, handle: &ConversationRecorderHandle) -> Result<(), CoreError> {
        handle.flush_tool_progress().await;
        let (close_tx, close_rx) = oneshot::channel();
        let close_result = match handle.sender.send(RecorderCommand::Close(close_tx)).await {
            Ok(()) => match close_rx.await {
                Ok(result) => result,
                Err(_) => Err(recorder_closed("acknowledging its close")),
            },
            Err(_) => Err(recorder_closed("acknowledging its close")),
        };
        let _ = self.task.await;
        close_result.and_then(|()| {
            handle
                .assembly_error
                .lock()
                .expect("assembly error")
                .clone()
                .map_or(Ok(()), Err)
        })
    }
}

impl ConversationRecorderHandle {
    pub(super) async fn queue_signal(
        &self,
        signal: ottam_protocol::ConversationSessionSignal,
    ) -> Result<(), CoreError> {
        if let Some(mut signal) =
            self.chat
                .record_session_signal(&self.conversation_id, &self.run_id, signal)
        {
            // Durable observations never restore transient activity.
            signal.activity = None;
            self.queue_event(
                ConversationEventKind::SessionMetadata,
                serde_json::json!({"signal": signal}),
            )
            .await?;
        }
        Ok(())
    }
    pub(super) async fn adopt_title(&self, title: String) -> Result<(), CoreError> {
        if title.len() > 65536 {
            tracing::warn!("Session title exceeds the existing lifecycle JSON limit");
            return Ok(());
        }
        self.latest.lock().expect("metadata mailbox").title = Some(title);
        match self.sender.try_send(RecorderCommand::MetadataReady) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
            Err(_) => Err(recorder_closed("adopting the harness title")),
        }
    }

    pub(super) async fn queue_update(&self, update: AgentUpdate) -> Result<(), CoreError> {
        let mut progress = self.progress.lock().await;
        let update = match update {
            AgentUpdate::ToolCallUpdate { payload } => {
                match self.hold_progress(&mut progress, payload) {
                    Some(payload) => AgentUpdate::ToolCallUpdate { payload },
                    None => return Ok(()),
                }
            }
            // Text never depends on a tool's state, so it passes held progress.
            update @ (AgentUpdate::AgentMessageChunk { .. }
            | AgentUpdate::AgentThoughtChunk { .. }
            | AgentUpdate::RichMessageChunk { .. }) => update,
            // Anything else may read or close a tool; it sees every call's
            // latest state first.
            update => {
                self.record_held(&mut progress).await;
                update
            }
        };
        if let AgentUpdate::ToolCall { payload } | AgentUpdate::ToolCallUpdate { payload } = &update
        {
            let status = payload.get("status").and_then(Value::as_str);
            // A finished call's held progress went out merged into this
            // update, and nothing more is held for it, so it leaves the map.
            if let (Some(id), Some("completed" | "failed")) = (tool_call_id(payload), status) {
                progress.calls.remove(id);
            } else if let Some(id) = tool_call_id(payload) {
                let call = progress
                    .calls
                    .entry(id.to_owned())
                    .or_insert(ToolProgressCall {
                        recorded_at: Instant::now(),
                        status: None,
                        held: None,
                    });
                call.recorded_at = Instant::now();
                if let Some(status) = status {
                    call.status = Some(status.to_owned());
                }
            }
        }
        self.record(update).await
    }

    /// Returns the update to record now — merged with anything held for its
    /// call — or `None` after holding it for a scheduled flush. Only repeat
    /// progress on an open call is held: a first update, a status change, and
    /// new arguments (`rawInput`) all record at once.
    fn hold_progress(&self, progress: &mut ToolProgress, payload: Value) -> Option<Value> {
        let Some(id) = tool_call_id(&payload).map(str::to_owned) else {
            return Some(payload);
        };
        let Some(call) = progress.calls.get_mut(&id) else {
            return Some(payload);
        };
        let status = payload.get("status").and_then(Value::as_str);
        let repeat = status.is_none_or(|status| call.status.as_deref() == Some(status))
            && !matches!(call.status.as_deref(), Some("completed" | "failed"))
            && payload.get("rawInput").is_none();
        let due = call.recorded_at + TOOL_PROGRESS_INTERVAL;
        if repeat && Instant::now() < due {
            let schedule = call.held.is_none();
            merge_progress(&mut call.held, payload);
            if schedule {
                let handle = self.clone();
                tokio::spawn(async move {
                    tokio::time::sleep_until(due).await;
                    handle.record_held_call(&id).await;
                });
            }
            return None;
        }
        let mut merged = call.held.take();
        merge_progress(&mut merged, payload);
        merged
    }

    async fn record_held_call(&self, id: &str) {
        let mut progress = self.progress.lock().await;
        let Some(call) = progress.calls.get_mut(id) else {
            return;
        };
        let Some(payload) = call.held.take() else {
            return;
        };
        call.recorded_at = Instant::now();
        if self
            .record(AgentUpdate::ToolCallUpdate { payload })
            .await
            .is_err()
        {
            tracing::warn!(run_id = %self.run_id.0, "Held tool progress was not persisted");
        }
    }

    async fn record_held(&self, progress: &mut ToolProgress) {
        let held: Vec<Value> = progress
            .calls
            .values_mut()
            .filter_map(|call| {
                let payload = call.held.take()?;
                call.recorded_at = Instant::now();
                Some(payload)
            })
            .collect();
        for payload in held {
            if self
                .record(AgentUpdate::ToolCallUpdate { payload })
                .await
                .is_err()
            {
                tracing::warn!(run_id = %self.run_id.0, "Held tool progress was not persisted");
            }
        }
    }

    /// Records every held progress update now: before a flush barrier or close.
    async fn flush_tool_progress(&self) {
        let mut progress = self.progress.lock().await;
        self.record_held(&mut progress).await;
    }

    async fn record(&self, update: AgentUpdate) -> Result<(), CoreError> {
        let delta = match self
            .chat
            .record_transcript_update(&self.conversation_id, &self.run_id, update)
            .await
        {
            Ok(delta) => delta,
            Err(error) => {
                *self.assembly_error.lock().expect("assembly error") = Some(error.clone());
                return Err(error);
            }
        };
        if !delta.items.is_empty()
            || !delta.session_states.is_empty()
            || !delta.body_appends.is_empty()
            || !delta.capture.is_empty()
            || !delta.background_tasks.is_empty()
            || delta.recovery_required
        {
            let _ = self.outbound.notify_transcript_delta(delta);
        }
        Ok(())
    }

    pub(super) async fn queue_event(
        &self,
        event_kind: ConversationEventKind,
        payload: Value,
    ) -> Result<(), CoreError> {
        if event_kind == ConversationEventKind::PermissionResolved {
            self.chat
                .capture_permission(&self.conversation_id, &self.run_id, &payload)
                .await?;
            // The typed decision is already captured. A congested metadata
            // recorder must not hold the adapter's permission response hostage.
            return match self.sender.try_send(RecorderCommand::Event {
                event_kind,
                payload,
            }) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
                Err(_) => Err(recorder_closed("recording permission metadata")),
            };
        }
        if matches!(
            event_kind,
            ConversationEventKind::SessionMetadata | ConversationEventKind::Usage
        ) {
            if serde_json::to_vec(&payload).is_ok_and(|bytes| bytes.len() > 65536) {
                tracing::warn!("Session metadata exceeds the existing lifecycle JSON limit");
                return Ok(());
            }
            {
                let mut pending = self.latest.lock().expect("metadata mailbox");
                if let Some((_, prior)) = pending
                    .events
                    .iter_mut()
                    .find(|(kind, _)| kind == &event_kind)
                {
                    if let (Some(prior), Some(next)) = (prior.as_object_mut(), payload.as_object())
                    {
                        // Absent fields serialize as null; they must not erase
                        // a coalesced value such as an announced mode.
                        prior.extend(
                            next.iter()
                                .filter(|(_, value)| !value.is_null())
                                .map(|(key, value)| (key.clone(), value.clone())),
                        );
                    } else {
                        *prior = payload;
                    }
                } else {
                    pending.events.push((event_kind, payload));
                }
            }
            return match self.sender.try_send(RecorderCommand::MetadataReady) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
                Err(_) => return Err(recorder_closed("recording metadata")),
            };
        }
        self.sender
            .send(RecorderCommand::Event {
                event_kind,
                payload,
            })
            .await
            .map_err(|_| recorder_closed("persisting an event"))
    }

    pub(super) async fn flush(&self) -> Result<(), CoreError> {
        self.flush_tool_progress().await;
        if let Some(error) = self.assembly_error.lock().expect("assembly error").clone() {
            return Err(error);
        }
        let (flush_tx, flush_rx) = oneshot::channel();
        self.sender
            .send(RecorderCommand::Flush(flush_tx))
            .await
            .map_err(|_| {
                CoreError::new(
                    "conversationRecorderClosed",
                    "Chat recorder closed before its events were flushed",
                )
            })?;
        flush_rx.await.map_err(|_| {
            CoreError::new(
                "conversationRecorderClosed",
                "Chat recorder stopped before acknowledging its flush",
            )
        })?
    }
}

impl ConversationRunContext {
    pub(super) async fn queue_event(&self, event_kind: ConversationEventKind, payload: Value) {
        if self
            .recorder
            .queue_event(event_kind, payload)
            .await
            .is_err()
        {
            tracing::warn!(run_id = %self.run_id.0, "Chat recorder closed before event was persisted");
        }
    }
}

fn tool_call_id(payload: &Value) -> Option<&str> {
    payload.get("toolCallId").and_then(Value::as_str)
}

/// Folds `next` into the held update the way applying both would: top-level
/// fields replace, and `_meta` merges key by key so a later update that omits
/// an extension does not drop the earlier one's. Terminal output carries
/// delivered chunks, not snapshots, so its `data` appends instead.
pub(super) fn merge_progress(held: &mut Option<Value>, next: Value) {
    let Some(Value::Object(held)) = held else {
        *held = Some(next);
        return;
    };
    let Value::Object(next) = next else {
        return;
    };
    for (key, value) in next {
        match (key.as_str(), held.get_mut(&key), value) {
            ("_meta", Some(Value::Object(prior)), Value::Object(meta)) => {
                for (name, mut extension) in meta {
                    if matches!(name.as_str(), "terminal_output" | "terminal_output_delta") {
                        append_output(prior.get(&name), &mut extension);
                    }
                    prior.insert(name, extension);
                }
            }
            (_, _, value) => {
                held.insert(key, value);
            }
        }
    }
}

/// Prefixes `next`'s chunk with the held one's from the same terminal.
fn append_output(held: Option<&Value>, next: &mut Value) {
    let (Some(held), Some(next)) = (held.and_then(Value::as_object), next.as_object_mut()) else {
        return;
    };
    if held.get("terminal_id") != next.get("terminal_id") {
        return;
    }
    if let (Some(Value::String(earlier)), Some(Value::String(later))) =
        (held.get("data"), next.get_mut("data"))
    {
        later.insert_str(0, earlier);
    }
}

fn recorder_closed(action: &str) -> CoreError {
    CoreError::new(
        "conversationRecorderClosed",
        format!("Chat recorder closed before {action}"),
    )
}

async fn recorder_loop(
    chat: ChatService,
    conversation_id: ConversationID,
    run_id: ConversationRunID,
    project_id: Option<ProjectID>,
    outbound: OutboundSink,
    mut receiver: mpsc::Receiver<RecorderCommand>,
    latest: Arc<Mutex<LatestMetadata>>,
) {
    let mut pending_control = None;
    loop {
        let command = match pending_control.take() {
            Some(command) => command,
            None => match receiver.recv().await {
                Some(command) => command,
                None => break,
            },
        };
        // Take the coalesced mailbox before a barrier as well as ordinary work.
        // Metadata never enters the command queue, so queued older values cannot
        // overwrite the newest title or session fields after this flush.
        let mut pending = std::mem::take(&mut *latest.lock().expect("metadata mailbox"));
        if let Some(title) = pending.title {
            match chat
                .update_conversation_title(&conversation_id, title.clone())
                .await
            {
                Ok(_) => pending.events.push((
                    ConversationEventKind::SessionMetadata,
                    serde_json::json!({"title": title}),
                )),
                Err(error) => {
                    tracing::warn!(kind = %error.kind, "Could not save latest session title")
                }
            }
        }
        if !pending.events.is_empty() {
            match chat
                .record_run_events(
                    &conversation_id,
                    &run_id,
                    project_id.clone(),
                    pending.events,
                )
                .await
            {
                Ok(events) => {
                    for event in events {
                        send_conversation_notification(&outbound, event);
                    }
                }
                Err(error) => {
                    tracing::warn!(kind = %error.kind, "Could not save latest session metadata")
                }
            }
        }
        match command {
            RecorderCommand::MetadataReady => {}
            RecorderCommand::Event {
                event_kind,
                payload,
            } => {
                let mut events = vec![(event_kind, payload)];
                while events.len() < RECORDER_BATCH_SIZE {
                    match receiver.try_recv() {
                        Ok(RecorderCommand::Event {
                            event_kind,
                            payload,
                        }) => events.push((event_kind, payload)),
                        Ok(control) => {
                            pending_control = Some(control);
                            break;
                        }
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => break,
                    }
                }
                match chat
                    .record_run_events(&conversation_id, &run_id, project_id.clone(), events)
                    .await
                {
                    Ok(params) => {
                        for params in params {
                            send_conversation_notification(&outbound, params);
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            run_id = %run_id.0,
                            error = %error,
                            "failed to persist Chat recorder batch"
                        );
                    }
                }
            }
            RecorderCommand::Flush(ack) => {
                let _ = ack.send(Ok(()));
            }
            RecorderCommand::Close(ack) => {
                let _ = ack.send(Ok(()));
                break;
            }
        }
    }
}
