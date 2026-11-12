use ottam_protocol::{
    ProtocolNotification, TerminalID, TerminalOutputDeliveryState, TerminalOutputDeltaParams,
    TerminalSessionID, WorkspaceID, METHOD_TERMINAL_OUTPUT_DELTA,
};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

pub(super) const READ_BUFFER_BYTES: usize = 8 * 1024;
const DEFAULT_OUTPUT_CAP_BYTES: usize = 1024 * 1024;
const OUTPUT_NOTIFICATION_OVERHEAD_BYTES: usize = 512;

#[derive(Clone)]
pub struct TerminalEventSender {
    outbound: Arc<Mutex<Option<crate::server::OutboundSink>>>,
    terminal_output: TerminalOutputQueue,
}

impl TerminalEventSender {
    pub(crate) fn new(
        outbound: crate::server::OutboundSink,
        terminal_output: TerminalOutputQueue,
    ) -> Self {
        Self {
            outbound: Arc::new(Mutex::new(Some(outbound))),
            terminal_output,
        }
    }

    pub(super) fn send_notification<P: serde::Serialize>(&self, method: &str, params: P) {
        match serde_json::to_value(ProtocolNotification::new(method, params)) {
            Ok(value) => {
                let Ok(outbound) = self.outbound.lock() else {
                    tracing::warn!(method, "terminal notification sender lock is poisoned");
                    return;
                };
                if let Some(outbound) = outbound.as_ref() {
                    let _ = outbound.notify(value);
                }
            }
            Err(error) => {
                tracing::warn!(error = %error, method, "failed to encode terminal notification");
            }
        }
    }

    pub(super) fn send_terminal_output_delta(&self, params: TerminalOutputDeltaParams) {
        self.terminal_output.push(params);
    }

    pub(super) fn close(&self) {
        if let Ok(mut outbound) = self.outbound.lock() {
            *outbound = None;
        }
    }
}

#[derive(Clone)]
pub struct TerminalOutputQueue {
    inner: Arc<Mutex<TerminalOutputQueueState>>,
    notify: Arc<Notify>,
    max_bytes: usize,
}

impl TerminalOutputQueue {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(TerminalOutputQueueState::default())),
            notify: Arc::new(Notify::new()),
            max_bytes: max_bytes.max(READ_BUFFER_BYTES),
        }
    }

    /// A handle the writer task awaits to learn that new output was enqueued,
    /// replacing the old 10 ms poll.
    pub fn notify_handle(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }

    pub fn push(&self, mut params: TerminalOutputDeltaParams) {
        let session_id = params.session_id.clone();
        let output_bytes = output_delta_byte_count(&params);
        let estimated_bytes = params.data_base64.len() + OUTPUT_NOTIFICATION_OVERHEAD_BYTES;
        let Ok(mut state) = self.inner.lock() else {
            tracing::warn!("terminal output queue lock is poisoned");
            return;
        };
        state.drop_until_within_budget(self.max_bytes, estimated_bytes);
        state.apply_pending_drop_counts(&session_id, &mut params);
        let params_identity = TerminalOutputIdentity {
            terminal_id: params.terminal_id.clone(),
            workspace_id: params.workspace_id.clone(),
            sequence: params.sequence,
        };
        let carried_dropped_bytes = params.dropped_bytes.unwrap_or_default();
        let carried_dropped_chunks = params.dropped_chunks.unwrap_or_default();

        let value = match serde_json::to_value(ProtocolNotification::new(
            METHOD_TERMINAL_OUTPUT_DELTA,
            params,
        )) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(error = %error, "failed to encode terminal output notification");
                return;
            }
        };
        let queued_bytes = serde_json::to_string(&value)
            .map(|encoded| encoded.len())
            .unwrap_or(estimated_bytes);
        state.drop_until_within_budget(self.max_bytes, queued_bytes);
        state.queue.push_back(TerminalOutputEnvelope {
            session_id,
            terminal_id: params_identity.terminal_id,
            workspace_id: params_identity.workspace_id,
            sequence: params_identity.sequence,
            value,
            queued_bytes,
            output_bytes,
            carried_dropped_bytes,
            carried_dropped_chunks,
        });
        state.queued_bytes = state.queued_bytes.saturating_add(queued_bytes);
        drop(state);
        self.notify.notify_one();
    }

    pub fn try_pop(&self) -> Option<Value> {
        let mut state = self.inner.lock().ok()?;
        state.pop_front().map(|envelope| envelope.value)
    }

    pub fn try_pop_session(&self, session_id: &TerminalSessionID) -> Option<Value> {
        let mut state = self.inner.lock().ok()?;
        let index = state
            .queue
            .iter()
            .position(|envelope| envelope.session_id == *session_id)?;
        let envelope = state.queue.remove(index)?;
        state.queued_bytes = state.queued_bytes.saturating_sub(envelope.queued_bytes);
        Some(envelope.value)
    }

    pub fn take_drop_marker(&self, session_id: &TerminalSessionID) -> Option<Value> {
        let mut state = self.inner.lock().ok()?;
        let counts = state.pending_drops.remove(session_id)?;
        if counts.chunks == 0 {
            return None;
        }
        serde_json::to_value(ProtocolNotification::new(
            METHOD_TERMINAL_OUTPUT_DELTA,
            TerminalOutputDeltaParams {
                session_id: session_id.clone(),
                terminal_id: counts.terminal_id,
                workspace_id: counts.workspace_id,
                sequence: counts.sequence,
                byte_start: None,
                byte_end: None,
                data_base64: String::new(),
                cap_reached: false,
                replay: false,
                delivery_state: Some(TerminalOutputDeliveryState::Degraded),
                dropped_bytes: Some(counts.bytes),
                dropped_chunks: Some(counts.chunks),
                replay_suggested: true,
            },
        ))
        .ok()
    }
}

#[derive(Default)]
struct TerminalOutputQueueState {
    queue: VecDeque<TerminalOutputEnvelope>,
    queued_bytes: usize,
    pending_drops: HashMap<TerminalSessionID, TerminalDropCounts>,
}

impl TerminalOutputQueueState {
    fn pop_front(&mut self) -> Option<TerminalOutputEnvelope> {
        let envelope = self.queue.pop_front()?;
        self.queued_bytes = self.queued_bytes.saturating_sub(envelope.queued_bytes);
        Some(envelope)
    }

    fn drop_until_within_budget(&mut self, max_bytes: usize, incoming_bytes: usize) {
        while self.queued_bytes.saturating_add(incoming_bytes) > max_bytes && !self.queue.is_empty()
        {
            let Some(envelope) = self.pop_front() else {
                break;
            };
            let counts = self.pending_drops.entry(envelope.session_id).or_default();
            counts.chunks = counts
                .chunks
                .saturating_add(1)
                .saturating_add(envelope.carried_dropped_chunks);
            counts.bytes = counts
                .bytes
                .saturating_add(envelope.output_bytes)
                .saturating_add(envelope.carried_dropped_bytes);
            counts.terminal_id = envelope.terminal_id.or(counts.terminal_id.take());
            counts.workspace_id = envelope.workspace_id.or(counts.workspace_id.take());
            counts.sequence = counts.sequence.max(envelope.sequence);
        }
    }

    fn apply_pending_drop_counts(
        &mut self,
        session_id: &TerminalSessionID,
        params: &mut TerminalOutputDeltaParams,
    ) {
        let Some(counts) = self.pending_drops.remove(session_id) else {
            return;
        };
        if counts.chunks == 0 {
            return;
        }
        params.delivery_state = Some(TerminalOutputDeliveryState::Degraded);
        params.dropped_chunks = Some(counts.chunks);
        params.dropped_bytes = Some(counts.bytes);
        params.replay_suggested = true;
    }
}

struct TerminalOutputEnvelope {
    session_id: TerminalSessionID,
    terminal_id: Option<TerminalID>,
    workspace_id: Option<WorkspaceID>,
    sequence: u64,
    value: Value,
    queued_bytes: usize,
    output_bytes: u64,
    carried_dropped_bytes: u64,
    carried_dropped_chunks: u64,
}

struct TerminalOutputIdentity {
    terminal_id: Option<TerminalID>,
    workspace_id: Option<WorkspaceID>,
    sequence: u64,
}

#[derive(Default)]
struct TerminalDropCounts {
    chunks: u64,
    bytes: u64,
    terminal_id: Option<TerminalID>,
    workspace_id: Option<WorkspaceID>,
    sequence: u64,
}

fn output_delta_byte_count(params: &TerminalOutputDeltaParams) -> u64 {
    params
        .byte_start
        .zip(params.byte_end)
        .map(|(start, end)| end.saturating_sub(start))
        .unwrap_or(params.data_base64.len() as u64)
}

pub fn terminal_output_queue_max_bytes() -> usize {
    std::env::var("OTTAM_TERMINAL_OUTPUT_MAX_BYTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_OUTPUT_CAP_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    #[test]
    fn terminal_output_queue_bounds_live_output_and_reports_drops() {
        let queue = TerminalOutputQueue::new(READ_BUFFER_BYTES);
        let pushed_chunks = 80usize;
        for sequence in 1..=pushed_chunks as u64 {
            queue.push(output_delta(sequence, vec![b'x'; 512]));
        }

        let mut popped = Vec::new();
        while let Some(value) = queue.try_pop() {
            let typed: ProtocolNotification<TerminalOutputDeltaParams> =
                serde_json::from_value(value).expect("terminal output notification decodes");
            popped.push(typed.params);
        }

        assert!(!popped.is_empty());
        assert!(popped.len() < 24);
        let degraded = popped
            .iter()
            .find(|params| params.delivery_state == Some(TerminalOutputDeliveryState::Degraded))
            .expect("queue reports dropped output on a later notification");
        assert!(degraded.dropped_chunks.unwrap_or_default() > 0);
        assert!(degraded.dropped_bytes.unwrap_or_default() > 0);
        assert!(degraded.replay_suggested);
        let reported_dropped_chunks: u64 = popped
            .iter()
            .map(|params| params.dropped_chunks.unwrap_or_default())
            .sum();
        assert_eq!(
            reported_dropped_chunks as usize + popped.len(),
            pushed_chunks
        );
    }

    #[test]
    fn terminal_output_queue_can_emit_drop_marker_before_exit() {
        let queue = TerminalOutputQueue::new(READ_BUFFER_BYTES);
        for sequence in 1..=10 {
            queue.push(output_delta_for_session(
                "terminal_local_dropped_test",
                sequence,
                vec![b'a'; 512],
            ));
        }
        for sequence in 1..=24 {
            queue.push(output_delta_for_session(
                "terminal_local_other_test",
                sequence,
                vec![b'b'; 512],
            ));
        }

        while queue.try_pop().is_some() {}
        let marker = queue
            .take_drop_marker(&TerminalSessionID(
                "terminal_local_dropped_test".to_string(),
            ))
            .expect("pending drops produce a marker");
        let typed: ProtocolNotification<TerminalOutputDeltaParams> =
            serde_json::from_value(marker).expect("drop marker decodes");
        assert_eq!(
            typed.params.delivery_state,
            Some(TerminalOutputDeliveryState::Degraded)
        );
        assert!(typed.params.data_base64.is_empty());
        assert!(typed.params.replay_suggested);
    }

    fn output_delta(sequence: u64, bytes: Vec<u8>) -> TerminalOutputDeltaParams {
        output_delta_for_session("terminal_local_queue_test", sequence, bytes)
    }

    fn output_delta_for_session(
        session_id: &str,
        sequence: u64,
        bytes: Vec<u8>,
    ) -> TerminalOutputDeltaParams {
        TerminalOutputDeltaParams {
            session_id: TerminalSessionID(session_id.to_string()),
            terminal_id: Some(TerminalID("terminal_record_queue_test".to_string())),
            workspace_id: Some(WorkspaceID(
                "019b0000-0000-7000-8000-000000000009".to_string(),
            )),
            sequence,
            byte_start: Some((sequence - 1) * bytes.len() as u64),
            byte_end: Some(sequence * bytes.len() as u64),
            data_base64: STANDARD.encode(bytes),
            cap_reached: false,
            replay: false,
            delivery_state: None,
            dropped_bytes: None,
            dropped_chunks: None,
            replay_suggested: false,
        }
    }
}
