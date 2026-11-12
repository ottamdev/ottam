//! Typed outbound routing: every frame leaving the core names its destination.
//!
//! The server historically had one anonymous outbound channel because stdout
//! was the only place a frame could go. With attached connections (the
//! `core.sock` attach transport), responses must return to the connection that
//! asked, core→client requests must only ever reach the primary (the app owns
//! the UI that answers permission prompts), and notifications fan out to
//! everyone. `OutboundSink` encodes those three verbs; a router task demuxes
//! frames to the primary writer or the attached-connection registry.
//!
//! Prior art: codex app-server correlates by `(connection_id, request_id)` with
//! a vanilla JSON-RPC wire format. Same shape here — the origin is a typed
//! value threaded beside the request ID, never smuggled inside it.

use ottam_protocol::{
    ConversationTranscriptDeltaParams, ProtocolNotification, TranscriptCompletion,
    TranscriptItemKind, NOTIFICATION_CONVERSATION_TRANSCRIPT_DELTA,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// Identity of one client connection for the lifetime of the process.
///
/// `Primary` is the stdio client that spawned the core (the app): it owns the
/// process lifecycle and is the only connection that receives core→client
/// requests. `Attached` connections arrive over the attach socket and are
/// numbered by the accept loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ConnectionId {
    Primary,
    /// Numbered by the attach-socket accept loop.
    Attached(u64),
}

/// Where a frame goes. `Connection` targets one connection (a response to its
/// request); `Primary` is for core→client requests; `Broadcast` is for
/// notifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Destination {
    Primary,
    Connection(ConnectionId),
    Broadcast,
}

pub(crate) struct OutboundFrame {
    pub(crate) destination: Destination,
    pub(crate) frame: Value,
    _lease: Option<BudgetLease>,
}

const RECOVERY_METHOD: &str = "conversation.transcriptRecoveryRequired";
#[derive(Default)]
struct BudgetState {
    bytes: usize,
    frames: usize,
    recovery: bool,
}
#[derive(Clone, Default)]
struct TranscriptBudget(Arc<Mutex<BudgetState>>);
struct BudgetLease {
    budget: TranscriptBudget,
    bytes: usize,
    recovery: bool,
}
impl Drop for BudgetLease {
    fn drop(&mut self) {
        let mut state = self.budget.0.lock().expect("delivery budget");
        if self.recovery {
            state.recovery = false;
        } else {
            state.bytes -= self.bytes;
            state.frames -= 1;
        }
    }
}

/// A transcript frame retains its budget until the transport finishes writing it.
#[derive(serde::Serialize)]
#[serde(transparent)]
pub(super) struct BudgetedValue {
    value: Value,
    #[serde(skip)]
    _lease: Option<BudgetLease>,
    #[serde(skip)]
    queued_at: std::time::Instant,
}
impl BudgetedValue {
    /// When core queued the frame for the writer.
    pub(super) fn queued_at(&self) -> std::time::Instant {
        self.queued_at
    }
}
impl std::ops::Deref for BudgetedValue {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.value
    }
}
impl std::fmt::Debug for BudgetedValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.value.fmt(f)
    }
}
impl PartialEq<Value> for BudgetedValue {
    fn eq(&self, other: &Value) -> bool {
        &self.value == other
    }
}

impl TranscriptBudget {
    fn wrap(&self, mut value: Value) -> Option<BudgetedValue> {
        let method = value.get("method").and_then(Value::as_str);
        if !matches!(
            method,
            Some("conversation.transcriptDelta" | RECOVERY_METHOD)
        ) {
            return Some(BudgetedValue {
                value,
                _lease: None,
                queued_at: std::time::Instant::now(),
            });
        }
        let mut state = self.0.lock().expect("delivery budget");
        let bytes = serde_json::to_vec(&value).map_or(1024 * 1024 + 1, |bytes| bytes.len());
        let recovery = method == Some(RECOVERY_METHOD)
            || state.bytes.saturating_add(bytes) > 1024 * 1024
            || state.frames >= 256;
        if recovery {
            if state.recovery {
                return None;
            }
            state.recovery = true;
            value = serde_json::json!({"jsonrpc":"2.0", "method":RECOVERY_METHOD, "params":{}});
        } else {
            state.bytes += bytes;
            state.frames += 1;
        }
        Some(BudgetedValue {
            value,
            _lease: Some(BudgetLease {
                budget: self.clone(),
                bytes,
                recovery,
            }),
            queued_at: std::time::Instant::now(),
        })
    }
}

struct ClientSender {
    tx: UnboundedSender<BudgetedValue>,
    budget: TranscriptBudget,
}
impl ClientSender {
    fn send(&self, value: Value) -> Result<(), ()> {
        if self.tx.is_closed() {
            return Err(());
        }
        match self.budget.wrap(value) {
            Some(frame) => self.tx.send(frame).map_err(|_| ()),
            None => Ok(()),
        }
    }
}

/// The one handle handlers and services send through. Cheap to clone; a send
/// error means the router (and with it the primary writer) is gone, which is
/// the same "stdout broke" condition the raw channel used to signal.
#[derive(Clone)]
pub(crate) struct OutboundSink {
    tx: UnboundedSender<OutboundFrame>,
    budget: TranscriptBudget,
    text_hold: Arc<Mutex<TextHold>>,
}

impl OutboundSink {
    pub(super) fn new(tx: UnboundedSender<OutboundFrame>) -> Self {
        Self {
            tx,
            budget: TranscriptBudget::default(),
            text_hold: Arc::default(),
        }
    }

    /// A notification: primary plus every attached connection.
    pub(crate) fn notify(&self, frame: Value) -> Result<(), ()> {
        self.send(Destination::Broadcast, frame)
    }

    /// A transcript delta. One that only streams text may wait up to
    /// `TEXT_DELTA_INTERVAL` and merge with the deltas after it; anything else
    /// goes out at once, behind whatever was waiting.
    pub(crate) fn notify_transcript_delta(
        &self,
        delta: ConversationTranscriptDeltaParams,
    ) -> Result<(), ()> {
        if !streams_text_only(&delta) {
            return self.notify(transcript_frame(delta));
        }
        if self.tx.is_closed() {
            return Err(());
        }
        let mut hold = self.lock_hold();
        let delta = match hold.pending.as_mut() {
            Some(pending) => match merge_text_delta(pending, delta) {
                Ok(()) => return Ok(()),
                Err(delta) => {
                    self.flush_locked(&mut hold)?;
                    *delta
                }
            },
            None => delta,
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return self.send_now(Destination::Broadcast, transcript_frame(delta));
        };
        hold.pending = Some(delta);
        hold.generation += 1;
        let generation = hold.generation;
        let sink = self.clone();
        runtime.spawn(async move {
            tokio::time::sleep(TEXT_DELTA_INTERVAL).await;
            let mut hold = sink.lock_hold();
            if hold.generation == generation {
                let _ = sink.flush_locked(&mut hold);
            }
        });
        Ok(())
    }

    fn lock_hold(&self) -> std::sync::MutexGuard<'_, TextHold> {
        self.text_hold
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn flush_locked(&self, hold: &mut TextHold) -> Result<(), ()> {
        match hold.pending.take() {
            Some(delta) => self.send_now(Destination::Broadcast, transcript_frame(delta)),
            None => Ok(()),
        }
    }

    /// A core→client request: primary only. Attached connections must never
    /// receive (or be able to answer) the app's permission prompts.
    pub(crate) fn request(&self, frame: Value) -> Result<(), ()> {
        self.send(Destination::Primary, frame)
    }

    /// A response: back to the connection whose request produced it.
    pub(crate) fn respond(&self, origin: ConnectionId, frame: Value) -> Result<(), ()> {
        self.send(Destination::Connection(origin), frame)
    }

    /// Every frame leaves behind a held text delta, under the hold's lock, so
    /// clients see deltas in sequence order and after nothing they preceded.
    fn send(&self, destination: Destination, frame: Value) -> Result<(), ()> {
        let mut hold = self.lock_hold();
        self.flush_locked(&mut hold)?;
        self.send_now(destination, frame)
    }

    fn send_now(&self, destination: Destination, frame: Value) -> Result<(), ()> {
        if self.tx.is_closed() {
            return Err(());
        }
        let Some(frame) = self.budget.wrap(frame) else {
            return Ok(());
        };
        self.tx
            .send(OutboundFrame {
                destination,
                frame: frame.value,
                _lease: frame._lease,
            })
            .map_err(|_| ())
    }
}

/// How long a streaming-text transcript delta waits to merge with the next.
/// Agents send one per token, about 75 a second while reasoning, and each
/// drives a transcript update in the app; 30 a second still reads as streaming
/// and matches the app's text commit rate (`StreamingCommitLifecycleView`).
pub(crate) const TEXT_DELTA_INTERVAL: std::time::Duration = std::time::Duration::from_millis(33);

/// The most text one merged append carries, as `TranscriptBodyAppend` allows.
const APPEND_LIMIT: usize = 64 * 1024;

#[derive(Default)]
struct TextHold {
    pending: Option<ConversationTranscriptDeltaParams>,
    /// Bumped per hold, so a timer flushes only the hold it was armed for.
    generation: u64,
}

fn transcript_frame(delta: ConversationTranscriptDeltaParams) -> Value {
    serde_json::to_value(ProtocolNotification::new(
        NOTIFICATION_CONVERSATION_TRANSCRIPT_DELTA,
        delta,
    ))
    .expect("typed transcript delta")
}

/// Open text items growing by appends, and nothing else a client must see
/// in order with other frames.
fn streams_text_only(delta: &ConversationTranscriptDeltaParams) -> bool {
    !delta.body_appends.is_empty()
        && delta.session_states.is_empty()
        && delta.background_tasks.is_empty()
        && delta.subagent_threads.is_empty()
        && !delta.recovery_required
        && delta.items.iter().all(|item| {
            matches!(
                item.kind,
                TranscriptItemKind::AssistantText | TranscriptItemKind::Reasoning
            ) && item.completion == TranscriptCompletion::Open
        })
}

/// Folds `next` into `pending`, or hands it back when the two cannot merge
/// without the client losing anything: another conversation, a sequence gap,
/// appends that do not continue each other, or a preview the merge would drop.
fn merge_text_delta(
    pending: &mut ConversationTranscriptDeltaParams,
    next: ConversationTranscriptDeltaParams,
) -> Result<(), Box<ConversationTranscriptDeltaParams>> {
    if pending.conversation_id != next.conversation_id
        || pending.workspace_id != next.workspace_id
        || pending.core_instance_id != next.core_instance_id
        || next.sequence != pending.sequence + 1
    {
        return Err(Box::new(next));
    }
    // A header without a preview cannot replace one with it: the client
    // would keep a preview older than the item it first sees.
    let drops_preview = next.items.iter().any(|item| {
        item.preview_omitted
            && pending
                .items
                .iter()
                .any(|held| held.item_id == item.item_id && !held.preview_omitted)
    });
    let breaks_appends = next.body_appends.iter().any(|append| {
        pending
            .body_appends
            .iter()
            .rev()
            .find(|held| held.item_id == append.item_id)
            .is_some_and(|held| held.offset + held.text.len() as u64 != append.offset)
    });
    if drops_preview || breaks_appends {
        return Err(Box::new(next));
    }
    for item in next.items {
        match pending
            .items
            .iter_mut()
            .find(|held| held.item_id == item.item_id)
        {
            Some(held) => *held = item,
            None => pending.items.push(item),
        }
    }
    for append in next.body_appends {
        match pending
            .body_appends
            .iter_mut()
            .rev()
            .find(|held| held.item_id == append.item_id)
        {
            Some(held) if held.text.len() + append.text.len() <= APPEND_LIMIT => {
                held.text.push_str(&append.text);
            }
            _ => pending.body_appends.push(append),
        }
    }
    // The body only grew, so every append is valid at the latest header's
    // revision — the one the client checks it against.
    for append in &mut pending.body_appends {
        if let Some(item) = pending
            .items
            .iter()
            .find(|item| item.item_id == append.item_id)
        {
            append.revision = item.revision;
        }
    }
    pending.sequence_start = pending.sequence_start.or(Some(pending.sequence));
    pending.sequence = next.sequence;
    pending.capture = next.capture;
    Ok(())
}

/// Writers for the attached connections, keyed by their `Attached` number.
/// Registration happens on accept, removal on connection close; a send to a
/// dead or missing entry is dropped (the response has nowhere to go).
#[derive(Clone, Default)]
pub(super) struct AttachedRegistry {
    inner: Arc<Mutex<HashMap<u64, ClientSender>>>,
}

impl AttachedRegistry {
    pub(super) fn register(&self, id: u64, tx: UnboundedSender<BudgetedValue>) {
        self.lock().insert(
            id,
            ClientSender {
                tx,
                budget: TranscriptBudget::default(),
            },
        );
    }

    pub(super) fn remove(&self, id: u64) {
        self.lock().remove(&id);
    }

    fn send_to(&self, id: u64, frame: Value) {
        let mut inner = self.lock();
        if let Some(tx) = inner.get(&id) {
            if tx.send(frame).is_err() {
                inner.remove(&id);
            }
        }
    }

    fn broadcast(&self, frame: &Value) {
        self.lock().retain(|_, tx| tx.send(frame.clone()).is_ok());
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, ClientSender>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Demuxes sink frames to the primary writer and the attached registry.
///
/// Exits when either every `OutboundSink` clone has dropped (normal shutdown)
/// or the primary writer is gone (broken stdout) — dropping the receiver then
/// makes every subsequent sink send fail, which is how the dispatcher observes
/// the writer's death, exactly as it did with the raw channel.
pub(super) async fn router_task(
    mut rx: UnboundedReceiver<OutboundFrame>,
    primary_tx: UnboundedSender<BudgetedValue>,
    registry: AttachedRegistry,
) {
    let primary_tx = ClientSender {
        tx: primary_tx,
        budget: TranscriptBudget::default(),
    };
    while let Some(OutboundFrame {
        destination,
        frame,
        _lease,
    }) = rx.recv().await
    {
        match destination {
            Destination::Primary | Destination::Connection(ConnectionId::Primary) => {
                if primary_tx.send(frame).is_err() {
                    return;
                }
            }
            Destination::Connection(ConnectionId::Attached(id)) => {
                registry.send_to(id, frame);
            }
            Destination::Broadcast => {
                registry.broadcast(&frame);
                if primary_tx.send(frame).is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn test_sink() -> (OutboundSink, UnboundedReceiver<OutboundFrame>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (OutboundSink::new(tx), rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn transcript_delivery_budget_keeps_one_recovery_marker_and_preserves_control() {
        let budget = TranscriptBudget::default();
        let mut held = Vec::new();
        for sequence in 0..1000 {
            if let Some(frame) = budget.wrap(
                json!({"method":"conversation.transcriptDelta", "params":{"sequence":sequence}}),
            ) {
                held.push(frame);
            }
        }
        assert_eq!(held.len(), 257);
        assert_eq!(
            held.iter()
                .filter(|frame| frame["method"] == RECOVERY_METHOD)
                .count(),
            1
        );
        assert!(budget
            .wrap(json!({"method":"agent.requestPermission", "id":42}))
            .is_some());
        assert!(budget.wrap(json!({"id":43, "result":{}})).is_some());
        drop(held);
        let next = budget
            .wrap(json!({"method":"conversation.transcriptDelta", "params":{}}))
            .unwrap();
        assert_eq!(next["method"], "conversation.transcriptDelta");
    }

    #[tokio::test]
    async fn slow_attached_client_does_not_stall_the_primary_transcript() {
        let (sink, mut primary, registry, router) = routed();
        let (attached_tx, mut attached) = unbounded_channel();
        registry.register(1, attached_tx);
        for sequence in 0..400 {
            sink.notify(json!({"method":"conversation.transcriptDelta", "params":{"sequence":sequence, "text":"x".repeat(8192)}})).unwrap();
            let delivered = primary.recv().await.unwrap();
            assert_eq!(delivered["params"]["sequence"], sequence);
        }
        let mut frames = Vec::new();
        while let Ok(frame) = attached.try_recv() {
            frames.push(frame);
        }
        assert!(frames.len() < 130);
        assert_eq!(
            frames
                .iter()
                .filter(|frame| frame["method"] == RECOVERY_METHOD)
                .count(),
            1
        );
        drop(sink);
        router.await.unwrap();
    }

    fn text_item(id: &str, revision: u64, bytes: u64, omitted: bool) -> Value {
        json!({
            "itemID": id, "conversationID": "c", "runID": "r", "runStatus": null,
            "position": 1, "revision": revision, "kind": "reasoning", "completion": "open",
            "createdAt": "2026-10-16T00:00:00Z", "updatedAt": "2026-10-16T00:00:00Z",
            "providerMessageID": null, "bodyPreview": if omitted { "" } else { "ab" },
            "bodyByteCount": bytes, "previewOmitted": omitted, "message": null,
            "tool": null, "plan": null, "permission": null, "gap": null,
        })
    }

    fn text_delta(
        sequence: u64,
        item: Value,
        offset: u64,
        text: &str,
    ) -> ConversationTranscriptDeltaParams {
        let id = item["itemID"].clone();
        let revision = item["revision"].clone();
        serde_json::from_value(json!({
            "workspaceID": "w", "conversationID": "c", "coreInstanceID": "i",
            "sequence": sequence, "items": [item],
            "bodyAppends": [{"itemID": id, "revision": revision, "offset": offset, "text": text}],
        }))
        .unwrap()
    }

    fn drain(rx: &mut UnboundedReceiver<OutboundFrame>) -> Vec<Value> {
        let mut frames = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            frames.push(frame.frame);
        }
        frames
    }

    #[tokio::test(start_paused = true)]
    async fn streaming_text_deltas_merge_into_one_frame_per_interval() {
        let (sink, mut rx) = test_sink();
        sink.notify_transcript_delta(text_delta(5, text_item("t", 3, 2, false), 0, "ab"))
            .unwrap();
        sink.notify_transcript_delta(text_delta(6, text_item("t", 4, 4, true), 2, "cd"))
            .unwrap();
        // A new item's preview cannot be merged away, so it goes alone.
        let first = drain(&mut rx);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0]["params"]["items"][0]["bodyPreview"], "ab");
        tokio::time::sleep(TEXT_DELTA_INTERVAL).await;
        tokio::task::yield_now().await;
        assert_eq!(drain(&mut rx).len(), 1);

        for (sequence, offset) in [(7, 4), (8, 6), (9, 8)] {
            let item = text_item("t", sequence, offset + 2, true);
            sink.notify_transcript_delta(text_delta(sequence, item, offset, "xy"))
                .unwrap();
        }
        assert!(drain(&mut rx).is_empty());
        tokio::time::sleep(TEXT_DELTA_INTERVAL).await;
        tokio::task::yield_now().await;
        let merged = drain(&mut rx);
        assert_eq!(merged.len(), 1);
        let params = &merged[0]["params"];
        assert_eq!(params["sequenceStart"], 7);
        assert_eq!(params["sequence"], 9);
        assert_eq!(params["items"].as_array().unwrap().len(), 1);
        assert_eq!(params["items"][0]["revision"], 9);
        assert_eq!(
            params["bodyAppends"],
            json!([{"itemID": "t", "revision": 9, "offset": 4, "text": "xyxyxy"}])
        );
    }

    #[tokio::test(start_paused = true)]
    async fn any_other_frame_flushes_held_text_first() {
        let (sink, mut rx) = test_sink();
        sink.notify_transcript_delta(text_delta(7, text_item("t", 7, 6, true), 4, "xy"))
            .unwrap();
        sink.notify(json!({"method": "conversation.eventDelta"}))
            .unwrap();
        let frames = drain(&mut rx);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["params"]["sequence"], 7);
        assert!(frames[0]["params"].get("sequenceStart").is_none());
        assert_eq!(frames[1]["method"], "conversation.eventDelta");
        // The armed timer finds nothing left to send.
        tokio::time::sleep(TEXT_DELTA_INTERVAL * 2).await;
        assert!(drain(&mut rx).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn gaps_and_structural_deltas_are_never_merged() {
        let (sink, mut rx) = test_sink();
        sink.notify_transcript_delta(text_delta(7, text_item("t", 7, 6, true), 4, "xy"))
            .unwrap();
        // Not contiguous with the held append: sent apart, in order.
        sink.notify_transcript_delta(text_delta(8, text_item("t", 8, 10, true), 8, "zz"))
            .unwrap();
        // A sequence gap.
        sink.notify_transcript_delta(text_delta(10, text_item("t", 10, 12, true), 10, "zz"))
            .unwrap();
        // A completed item is structural and goes out at once.
        let mut done = text_delta(11, text_item("t", 11, 14, true), 12, "zz");
        done.items[0].completion = TranscriptCompletion::Complete;
        sink.notify_transcript_delta(done).unwrap();
        let sequences: Vec<_> = drain(&mut rx)
            .iter()
            .map(|frame| frame["params"]["sequence"].as_u64().unwrap())
            .collect();
        assert_eq!(sequences, [7, 8, 10, 11]);
    }

    fn routed() -> (
        OutboundSink,
        UnboundedReceiver<BudgetedValue>,
        AttachedRegistry,
        tokio::task::JoinHandle<()>,
    ) {
        let (frame_tx, frame_rx) = unbounded_channel();
        let (primary_tx, primary_rx) = unbounded_channel();
        let registry = AttachedRegistry::default();
        let router = tokio::spawn(router_task(frame_rx, primary_tx, registry.clone()));
        (OutboundSink::new(frame_tx), primary_rx, registry, router)
    }

    #[tokio::test]
    async fn respond_routes_to_the_origin_connection() {
        let (sink, mut primary_rx, registry, _router) = routed();
        let (attached_tx, mut attached_rx) = unbounded_channel();
        registry.register(7, attached_tx);

        sink.respond(ConnectionId::Attached(7), json!({"id": 1}))
            .expect("send");
        sink.respond(ConnectionId::Primary, json!({"id": 2}))
            .expect("send");

        assert_eq!(
            attached_rx.recv().await.expect("attached frame"),
            json!({"id": 1})
        );
        assert_eq!(
            primary_rx.recv().await.expect("primary frame"),
            json!({"id": 2})
        );
        assert!(attached_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn requests_reach_only_the_primary() {
        let (sink, mut primary_rx, registry, _router) = routed();
        let (attached_tx, mut attached_rx) = unbounded_channel();
        registry.register(1, attached_tx);

        sink.request(json!({"method": "client.ask"})).expect("send");

        assert_eq!(
            primary_rx.recv().await.expect("primary frame"),
            json!({"method": "client.ask"})
        );
        assert!(attached_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn notifications_broadcast_to_primary_and_attached() {
        let (sink, mut primary_rx, registry, _router) = routed();
        let (first_tx, mut first_rx) = unbounded_channel();
        let (second_tx, mut second_rx) = unbounded_channel();
        registry.register(1, first_tx);
        registry.register(2, second_tx);

        sink.notify(json!({"method": "agent.event"})).expect("send");

        for rx in [&mut primary_rx, &mut first_rx, &mut second_rx] {
            assert_eq!(
                rx.recv().await.expect("frame"),
                json!({"method": "agent.event"})
            );
        }
    }

    #[tokio::test]
    async fn dead_attached_receivers_are_pruned_and_do_not_block_broadcast() {
        let (sink, mut primary_rx, registry, _router) = routed();
        let (dead_tx, dead_rx) = unbounded_channel();
        registry.register(3, dead_tx);
        drop(dead_rx);

        sink.notify(json!({"n": 1})).expect("send");
        assert_eq!(primary_rx.recv().await.expect("frame"), json!({"n": 1}));
        assert!(registry.lock().is_empty());

        // A response to the dead connection is dropped, not an error.
        sink.respond(ConnectionId::Attached(3), json!({"id": 9}))
            .expect("send");
        sink.respond(ConnectionId::Primary, json!({"id": 10}))
            .expect("send");
        assert_eq!(primary_rx.recv().await.expect("frame"), json!({"id": 10}));
    }

    #[tokio::test]
    async fn primary_writer_death_fails_subsequent_sends() {
        let (sink, primary_rx, _registry, router) = routed();
        drop(primary_rx);

        // First send discovers the dead writer and stops the router.
        let _ = sink.respond(ConnectionId::Primary, json!({"id": 1}));
        router.await.expect("router joins");
        assert!(sink
            .respond(ConnectionId::Primary, json!({"id": 2}))
            .is_err());
    }

    #[tokio::test]
    async fn router_exits_when_every_sink_clone_drops() {
        let (frame_tx, frame_rx) = unbounded_channel();
        let (primary_tx, mut primary_rx) = unbounded_channel::<BudgetedValue>();
        let router = tokio::spawn(router_task(
            frame_rx,
            primary_tx,
            AttachedRegistry::default(),
        ));

        let sink = OutboundSink::new(frame_tx);
        let clone = sink.clone();
        drop(sink);
        drop(clone);

        router.await.expect("router joins");
        assert!(primary_rx.recv().await.is_none());
    }
}
