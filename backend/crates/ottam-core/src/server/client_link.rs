use crate::server::outbound::OutboundSink;
use ottam_protocol::{
    CancelRequestParams, ProtocolError, ProtocolNotification, ProtocolRequest, RequestID,
    NOTIFICATION_CANCEL_REQUEST,
};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{oneshot, Notify};

type PendingSender = oneshot::Sender<ClientResponse>;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum ClientResponse {
    Success { id: RequestID, result: Value },
    Error { id: RequestID, error: ProtocolError },
}

impl ClientResponse {
    pub(super) fn id(&self) -> &RequestID {
        match self {
            Self::Success { id, .. } | Self::Error { id, .. } => id,
        }
    }
}

#[derive(Default)]
struct PendingClientRequestState {
    closed: bool,
    requests: HashMap<RequestID, PendingSender>,
}

#[derive(Clone, Default)]
pub(super) struct PendingClientRequests {
    inner: Arc<Mutex<PendingClientRequestState>>,
    pending_started: Arc<Notify>,
}

impl PendingClientRequests {
    pub(super) fn complete(&self, response: ClientResponse) -> bool {
        let sender = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .requests
            .remove(response.id());
        let Some(sender) = sender else {
            return false;
        };
        // A cancelled caller may drop its receiver after we remove the entry.
        // The response is still matched even if nobody remains to consume it.
        let _ = sender.send(response);
        true
    }

    pub(super) fn close(&self) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.closed = true;
        state.requests.clear();
    }

    pub(super) async fn wait_until_pending(&self) {
        loop {
            if !self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .requests
                .is_empty()
            {
                return;
            }
            self.pending_started.notified().await;
        }
    }

    fn insert(&self, id: RequestID, sender: PendingSender) -> bool {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.closed {
            return false;
        }
        state.requests.insert(id, sender);
        drop(state);
        self.pending_started.notify_one();
        true
    }

    /// `true` when the request was still waiting for its answer.
    fn remove(&self, id: &RequestID) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .requests
            .remove(id)
            .is_some()
    }
}

#[derive(Clone)]
pub(super) struct ClientLink {
    outbound: OutboundSink,
    pending: PendingClientRequests,
    next_id: Arc<AtomicU64>,
}

impl ClientLink {
    pub(super) fn new(outbound: OutboundSink) -> Self {
        Self {
            outbound,
            pending: PendingClientRequests::default(),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub(super) fn pending_requests(&self) -> PendingClientRequests {
        self.pending.clone()
    }

    pub(super) async fn request(
        &self,
        method: impl Into<String>,
        params: Value,
    ) -> Result<ClientResponse, ClientLinkError> {
        let id = RequestID::String(format!(
            "core/req/{}",
            self.next_id.fetch_add(1, Ordering::Relaxed)
        ));
        let (response_tx, response_rx) = oneshot::channel();
        if !self.pending.insert(id.clone(), response_tx) {
            return Err(ClientLinkError::ConnectionClosed);
        }
        let _pending_guard = PendingRequestGuard {
            pending: self.pending.clone(),
            outbound: self.outbound.clone(),
            id: id.clone(),
        };

        let request = serde_json::to_value(ProtocolRequest::new(id, method, params))
            .map_err(|error| ClientLinkError::Encode(error.to_string()))?;
        // Core→client requests are answered by the app's UI; they go to the
        // primary connection only, never to attached connections.
        self.outbound
            .request(request)
            .map_err(|_| ClientLinkError::OutboundClosed)?;

        response_rx
            .await
            .map_err(|_| ClientLinkError::ConnectionClosed)
    }
}

struct PendingRequestGuard {
    pending: PendingClientRequests,
    outbound: OutboundSink,
    id: RequestID,
}

impl Drop for PendingRequestGuard {
    /// A caller that stops waiting before the client answers withdraws the
    /// request, so the client drops its prompt instead of leaving it up.
    fn drop(&mut self) {
        if self.pending.remove(&self.id) {
            let _ = self.outbound.request(
                serde_json::to_value(ProtocolNotification::new(
                    NOTIFICATION_CANCEL_REQUEST,
                    CancelRequestParams {
                        id: self.id.clone(),
                    },
                ))
                .expect("typed cancel request"),
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ClientLinkError {
    Encode(String),
    OutboundClosed,
    ConnectionClosed,
}

impl fmt::Display for ClientLinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(error) => write!(formatter, "failed to encode client request: {error}"),
            Self::OutboundClosed => formatter.write_str("JSONL outbound writer terminated"),
            Self::ConnectionClosed => {
                formatter.write_str("client connection closed before responding")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn request_uses_core_string_id_and_completes() {
        let (sink, mut outbound_rx) = crate::server::outbound::test_sink();
        let link = ClientLink::new(sink);
        let pending = link.pending_requests();

        let request_task = tokio::spawn(async move {
            link.request("client.echo", json!({"message": "hello"}))
                .await
        });
        let outbound = outbound_rx.recv().await.expect("outbound request").frame;
        let id = outbound["id"].as_str().expect("string request id");
        assert_eq!(id, "core/req/1");
        assert_eq!(outbound["method"], json!("client.echo"));

        assert!(pending.complete(ClientResponse::Success {
            id: RequestID::String(id.to_string()),
            result: json!({"echoed": true}),
        }));
        let response = request_task
            .await
            .expect("request task joins")
            .expect("request completes");
        assert_eq!(
            response,
            ClientResponse::Success {
                id: RequestID::String("core/req/1".to_string()),
                result: json!({"echoed": true}),
            }
        );
    }

    #[tokio::test]
    async fn closing_pending_requests_releases_waiter() {
        let (sink, mut outbound_rx) = crate::server::outbound::test_sink();
        let link = ClientLink::new(sink);
        let pending = link.pending_requests();

        let request_task =
            tokio::spawn(async move { link.request("client.echo", json!({})).await });
        let _ = outbound_rx.recv().await.expect("outbound request");
        pending.close();

        let error = request_task
            .await
            .expect("request task joins")
            .expect_err("request fails");
        assert_eq!(error, ClientLinkError::ConnectionClosed);
    }

    #[tokio::test]
    async fn request_started_after_close_fails_without_writing() {
        let (sink, mut outbound_rx) = crate::server::outbound::test_sink();
        let link = ClientLink::new(sink);
        link.pending_requests().close();

        let error = link
            .request("client.echo", json!({}))
            .await
            .expect_err("request fails");
        assert_eq!(error, ClientLinkError::ConnectionClosed);
        assert!(outbound_rx.try_recv().is_err());
    }
}
