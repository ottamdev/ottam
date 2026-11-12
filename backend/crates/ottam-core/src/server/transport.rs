use crate::server::client_link::{ClientResponse, PendingClientRequests};
use crate::server::outbound::{ConnectionId, OutboundSink};
use crate::terminal::TerminalOutputQueue;
use ottam_protocol::{ProtocolError, RequestID};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc::error::{TryRecvError, TrySendError};
use tokio::sync::mpsc::{Sender, UnboundedReceiver};

/// One inbound line, stamped by the reader that framed it so the dispatcher
/// can route the response back to the connection that asked.
pub(super) struct InboundMessage {
    pub(super) origin: ConnectionId,
    pub(super) line: String,
    /// When the reader framed the line; the dispatcher logs how long it
    /// waited behind earlier requests.
    pub(super) received_at: std::time::Instant,
}

/// Core's stdout as the primary writer sees it. The client closing its read
/// end is a normal quit, not a server error: the first `BrokenPipe` cancels
/// `peer_closed`, which ends the primary reader, and every later write is
/// discarded so outbound senders keep draining while the server shuts down.
pub(super) struct PeerClosableOutput<W> {
    inner: W,
    peer_closed: tokio_util::sync::CancellationToken,
}

impl<W> PeerClosableOutput<W> {
    pub(super) fn new(inner: W, peer_closed: tokio_util::sync::CancellationToken) -> Self {
        Self { inner, peer_closed }
    }

    fn settle<T>(&self, result: std::io::Result<T>, discarded: T) -> std::io::Result<T> {
        match result {
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {
                tracing::info!("client closed core's stdout; shutting down");
                self.peer_closed.cancel();
                Ok(discarded)
            }
            result => result,
        }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for PeerClosableOutput<W> {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buffer: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if this.peer_closed.is_cancelled() {
            return std::task::Poll::Ready(Ok(buffer.len()));
        }
        std::pin::Pin::new(&mut this.inner)
            .poll_write(context, buffer)
            .map(|result| this.settle(result, buffer.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.peer_closed.is_cancelled() {
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut this.inner)
            .poll_flush(context)
            .map(|result| this.settle(result, ()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.peer_closed.is_cancelled() {
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut this.inner)
            .poll_shutdown(context)
            .map(|result| this.settle(result, ()))
    }
}

pub(super) const MAX_JSONL_MESSAGE_BYTES: usize = 1_048_576;
const DISPATCH_OVERLOAD_ERROR_CODE: i64 = -32002;

/// Classification of an inbound JSON-RPC line, mirroring app-server's
/// `JSONRPCMessage`. Requests, notifications, and malformed envelopes keep
/// flowing through `handle_message`, which remains the sole source of protocol
/// errors. Valid responses bypass the serial dispatcher so they release a
/// pending core→client request even while the dispatch queue is full.
pub(super) enum JsonRpcMessage {
    Request(String),
    Notification(String),
    Response(ClientResponse),
    Error(ClientResponse),
}

fn classify_jsonrpc_message(line: String) -> JsonRpcMessage {
    let Ok(value) = serde_json::from_str::<Value>(&line) else {
        return JsonRpcMessage::Request(line);
    };

    let has_id = value.get("id").is_some();
    if value.get("method").is_none() && has_id {
        let id = serde_json::from_value::<RequestID>(value["id"].clone());
        let has_result = value.get("result").is_some();
        let has_error = value.get("error").is_some();
        match (id, has_result, has_error) {
            (Ok(id), true, false) => {
                return JsonRpcMessage::Response(ClientResponse::Success {
                    id,
                    result: value["result"].clone(),
                });
            }
            (Ok(id), false, true) => {
                if let Ok(error) = serde_json::from_value::<ProtocolError>(value["error"].clone()) {
                    return JsonRpcMessage::Error(ClientResponse::Error { id, error });
                }
            }
            _ => {}
        }
        return JsonRpcMessage::Request(line);
    }

    if has_id {
        JsonRpcMessage::Request(line)
    } else {
        JsonRpcMessage::Notification(line)
    }
}

async fn dispatch_request_line(
    origin: ConnectionId,
    line: String,
    dispatch_tx: &Sender<InboundMessage>,
    outbound: &OutboundSink,
    pending_client_requests: &PendingClientRequests,
) -> bool {
    let received_at = std::time::Instant::now();
    let line = match dispatch_tx.try_send(InboundMessage {
        origin,
        line,
        received_at,
    }) {
        Ok(()) => return true,
        Err(TrySendError::Closed(_)) => return false,
        Err(TrySendError::Full(message)) => message.line,
    };

    tokio::select! {
        biased;
        _ = pending_client_requests.wait_until_pending() => {
            let request_id = serde_json::from_str::<Value>(&line)
                .map(|value| {
                    crate::server::responses::request_id_from_invalid_envelope(&value)
                })
                .unwrap_or(RequestID::Null);
            tracing::warn!(
                request_id_kind = crate::server::responses::request_id_kind(&request_id),
                capacity = super::DISPATCH_CHANNEL_CAPACITY,
                "JSONL request rejected because dispatch queue is full"
            );
            let response = crate::server::responses::error_response(
                request_id,
                DISPATCH_OVERLOAD_ERROR_CODE,
                format!(
                    "JSONL dispatch queue is full (capacity {})",
                    super::DISPATCH_CHANNEL_CAPACITY
                ),
            );
            outbound.respond(origin, response).is_ok()
        }
        permit = dispatch_tx.reserve() => match permit {
            Ok(permit) => {
                permit.send(InboundMessage { origin, line, received_at });
                true
            }
            Err(_) => false,
        }
    }
}

async fn dispatch_notification_line(
    origin: ConnectionId,
    line: String,
    dispatch_tx: &Sender<InboundMessage>,
    pending_client_requests: &PendingClientRequests,
) -> bool {
    let received_at = std::time::Instant::now();
    let line = match dispatch_tx.try_send(InboundMessage {
        origin,
        line,
        received_at,
    }) {
        Ok(()) => return true,
        Err(TrySendError::Closed(_)) => return false,
        Err(TrySendError::Full(message)) => message.line,
    };

    tokio::select! {
        biased;
        _ = pending_client_requests.wait_until_pending() => {
            tracing::warn!(
                capacity = super::DISPATCH_CHANNEL_CAPACITY,
                "JSONL notification dropped because dispatch queue is full"
            );
            true
        }
        permit = dispatch_tx.reserve() => match permit {
            Ok(permit) => {
                permit.send(InboundMessage { origin, line, received_at });
                true
            }
            Err(_) => false,
        }
    }
}

async fn write_response<W>(output: &mut W, response: &Value) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
{
    let encoded = serde_json::to_string(response)
        .map_err(|error| format!("failed to encode response: {error}"))?;
    output
        .write_all(encoded.as_bytes())
        .await
        .map_err(|error| format!("failed to write response: {error}"))?;
    output
        .write_all(b"\n")
        .await
        .map_err(|error| format!("failed to write response: {error}"))?;
    output
        .flush()
        .await
        .map_err(|error| format!("failed to flush response: {error}"))
}

/// Single async writer task. Replaces the old 10 ms-poll writer thread: it
/// drains a batch of terminal output, then either writes an outbound message or
/// parks on the outbound channel / the terminal-output `Notify` until there is
/// more to do. Preserves per-message flush and the invariant that a session's
/// queued output (plus its drop marker) is flushed before `terminal.exited`.
pub(super) async fn writer_task<W>(
    mut outbound_rx: UnboundedReceiver<super::outbound::BudgetedValue>,
    terminal_output: TerminalOutputQueue,
    mut output: W,
) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
{
    const OUTPUT_DRAIN_BATCH: usize = 32;
    let notify = terminal_output.notify_handle();
    loop {
        let drained =
            drain_terminal_output_batch(&mut output, &terminal_output, OUTPUT_DRAIN_BATCH).await?;
        match outbound_rx.try_recv() {
            Ok(response) => {
                write_outbound(&mut output, &terminal_output, response).await?;
                continue;
            }
            Err(TryRecvError::Disconnected) => {
                drain_terminal_output_all(&mut output, &terminal_output).await?;
                return Ok(());
            }
            Err(TryRecvError::Empty) => {}
        }
        if drained > 0 {
            // The batch was capped; more output may remain — keep draining
            // (and re-checking outbound) before parking.
            continue;
        }
        tokio::select! {
            biased;
            message = outbound_rx.recv() => match message {
                Some(response) => write_outbound(&mut output, &terminal_output, response).await?,
                None => {
                    drain_terminal_output_all(&mut output, &terminal_output).await?;
                    return Ok(());
                }
            },
            _ = notify.notified() => {}
        }
    }
}

async fn write_outbound<W>(
    output: &mut W,
    terminal_output: &TerminalOutputQueue,
    response: super::outbound::BudgetedValue,
) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
{
    if let Some(session_id) = terminal_exit_session_id(&response) {
        drain_terminal_output_for_session(output, terminal_output, &session_id).await?;
    }
    write_response(output, &response).await?;
    // A frame that waited this long sat behind earlier output or a client
    // that stopped reading; either way the app saw it late.
    let waited = response.queued_at().elapsed();
    if waited >= SLOW_OUTBOUND_WRITE {
        tracing::info!(
            method = response
                .get("method")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("response"),
            waited_ms = waited.as_millis(),
            "outbound frame written late"
        );
    }
    Ok(())
}

const SLOW_OUTBOUND_WRITE: std::time::Duration = std::time::Duration::from_millis(100);

async fn drain_terminal_output_batch<W>(
    output: &mut W,
    terminal_output: &TerminalOutputQueue,
    max_items: usize,
) -> Result<usize, String>
where
    W: AsyncWrite + Unpin,
{
    let mut drained = 0;
    for _ in 0..max_items {
        let Some(value) = terminal_output.try_pop() else {
            break;
        };
        write_response(output, &value).await?;
        drained += 1;
    }
    Ok(drained)
}

async fn drain_terminal_output_all<W>(
    output: &mut W,
    terminal_output: &TerminalOutputQueue,
) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
{
    while let Some(value) = terminal_output.try_pop() {
        write_response(output, &value).await?;
    }
    Ok(())
}

async fn drain_terminal_output_for_session<W>(
    output: &mut W,
    terminal_output: &TerminalOutputQueue,
    session_id: &ottam_protocol::TerminalSessionID,
) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
{
    while let Some(value) = terminal_output.try_pop_session(session_id) {
        write_response(output, &value).await?;
    }
    if let Some(value) = terminal_output.take_drop_marker(session_id) {
        write_response(output, &value).await?;
    }
    Ok(())
}

fn terminal_exit_session_id(value: &Value) -> Option<ottam_protocol::TerminalSessionID> {
    if value.get("method")?.as_str()? != ottam_protocol::METHOD_TERMINAL_EXITED {
        return None;
    }
    value
        .get("params")?
        .get("sessionID")?
        .as_str()
        .map(|session_id| ottam_protocol::TerminalSessionID(session_id.to_string()))
}

/// Async reader task: bounded JSONL line reads. Requests and notifications go
/// to the bounded dispatcher queue, while responses complete pending
/// core→client requests directly. Ordinary saturation backpressures stdin. If
/// a core→client request is pending, saturated requests instead receive
/// `-32002` and notifications are dropped with a warning so the reader never
/// parks ahead of the response its waiter needs. An oversize line is discarded
/// through its newline and answered with `-32001`; the reader keeps serving the
/// messages after it. Every exit path closes pending requests so a waiter
/// cannot keep the process alive after stdin closes.
pub(super) async fn reader_task<R>(
    mut input: R,
    origin: ConnectionId,
    dispatch_tx: Sender<InboundMessage>,
    outbound: OutboundSink,
    pending_client_requests: PendingClientRequests,
) -> Result<(), String>
where
    R: AsyncBufRead + Unpin,
{
    use crate::server::responses::error_response;
    use crate::server::responses::request_id_kind;
    use ottam_protocol::RequestID;

    let result = loop {
        match read_bounded_jsonl_line(&mut input).await {
            Err(error) => break Err(error),
            Ok(None) => break Ok(()),
            Ok(Some(BoundedLine::Oversize)) => {
                tracing::warn!(
                    maximum_bytes = MAX_JSONL_MESSAGE_BYTES,
                    "oversized JSONL message rejected"
                );
                let response = error_response(
                    RequestID::Null,
                    -32001,
                    format!("JSONL message exceeds {MAX_JSONL_MESSAGE_BYTES} bytes"),
                );
                let _ = outbound.respond(origin, response);
            }
            Ok(Some(BoundedLine::Line(line))) => {
                if line.trim().is_empty() {
                    tracing::debug!("ignoring empty JSONL line");
                    continue;
                }
                match classify_jsonrpc_message(line) {
                    JsonRpcMessage::Request(line) => {
                        if !dispatch_request_line(
                            origin,
                            line,
                            &dispatch_tx,
                            &outbound,
                            &pending_client_requests,
                        )
                        .await
                        {
                            break Ok(());
                        }
                    }
                    JsonRpcMessage::Notification(line) => {
                        if !dispatch_notification_line(
                            origin,
                            line,
                            &dispatch_tx,
                            &pending_client_requests,
                        )
                        .await
                        {
                            break Ok(());
                        }
                    }
                    JsonRpcMessage::Response(response) | JsonRpcMessage::Error(response) => {
                        let id_kind = request_id_kind(response.id());
                        if !pending_client_requests.complete(response) {
                            tracing::warn!(
                                request_id_kind = id_kind,
                                "unmatched client response ignored"
                            );
                        }
                    }
                }
            }
        }
    };
    pending_client_requests.close();
    result
}

pub(super) enum BoundedLine {
    Line(String),
    Oversize,
}

pub(super) async fn read_bounded_jsonl_line<R>(input: &mut R) -> Result<Option<BoundedLine>, String>
where
    R: AsyncBufRead + Unpin,
{
    let mut buffer = Vec::new();

    loop {
        let available = input
            .fill_buf()
            .await
            .map_err(|error| format!("failed to read request: {error}"))?;
        if available.is_empty() {
            if buffer.is_empty() {
                return Ok(None);
            }
            return Ok(Some(BoundedLine::Line(
                String::from_utf8_lossy(&buffer).to_string(),
            )));
        }

        if let Some(newline_index) = available.iter().position(|byte| *byte == b'\n') {
            if buffer.len() + newline_index > MAX_JSONL_MESSAGE_BYTES {
                input.consume(newline_index + 1);
                return Ok(Some(BoundedLine::Oversize));
            }
            buffer.extend_from_slice(&available[..newline_index]);
            input.consume(newline_index + 1);
            return Ok(Some(BoundedLine::Line(
                String::from_utf8_lossy(&buffer).to_string(),
            )));
        }

        if buffer.len() + available.len() > MAX_JSONL_MESSAGE_BYTES {
            let count = available.len();
            input.consume(count);
            skip_to_line_end(input).await?;
            return Ok(Some(BoundedLine::Oversize));
        }

        buffer.extend_from_slice(available);
        let count = available.len();
        input.consume(count);
    }
}

/// Discards the rest of an oversize line, through its newline or EOF, so the
/// next read starts at the next message.
async fn skip_to_line_end<R>(input: &mut R) -> Result<(), String>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let available = input
            .fill_buf()
            .await
            .map_err(|error| format!("failed to read request: {error}"))?;
        if available.is_empty() {
            return Ok(());
        }
        if let Some(newline_index) = available.iter().position(|byte| *byte == b'\n') {
            input.consume(newline_index + 1);
            return Ok(());
        }
        let count = available.len();
        input.consume(count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ottam_protocol::RequestID;
    use serde_json::json;

    #[tokio::test]
    async fn read_bounded_jsonl_line_resyncs_after_oversize() {
        let mut bytes = vec![b'x'; MAX_JSONL_MESSAGE_BYTES + 4096];
        bytes.extend_from_slice(b"\n{\"id\":1}\n");
        // A small buffer forces the mid-line oversize branch.
        let mut input = tokio::io::BufReader::with_capacity(64, &bytes[..]);

        assert!(matches!(
            read_bounded_jsonl_line(&mut input).await,
            Ok(Some(BoundedLine::Oversize))
        ));
        match read_bounded_jsonl_line(&mut input).await {
            Ok(Some(BoundedLine::Line(line))) => assert!(
                line == "{\"id\":1}",
                "expected the next request, got a {}-byte line",
                line.len()
            ),
            Ok(Some(BoundedLine::Oversize)) => panic!("expected next line, got oversize"),
            other => panic!("expected next line, got {:?}", other.map(|line| line.is_some())),
        }
        assert!(matches!(read_bounded_jsonl_line(&mut input).await, Ok(None)));
    }

    #[test]
    fn classifies_success_and_error_responses() {
        let success = classify_jsonrpc_message(
            json!({"id": "core/req/1", "result": {"ok": true}}).to_string(),
        );
        let JsonRpcMessage::Response(success) = success else {
            panic!("expected response");
        };
        assert_eq!(success.id(), &RequestID::String("core/req/1".to_string()));

        let error = classify_jsonrpc_message(
            json!({"id": "core/req/2", "error": {"code": 42, "message": "no"}}).to_string(),
        );
        assert!(matches!(error, JsonRpcMessage::Error(_)));
    }

    /// A client answer must not queue behind dispatch work: with the queue full
    /// and a core→client request waiting, the reader rejects the excess request,
    /// drops the excess notification, and still delivers the answer.
    #[tokio::test]
    async fn a_full_dispatch_queue_rejects_excess_work_and_still_routes_the_answer() {
        let (sink, mut outbound_rx) = crate::server::outbound::test_sink();
        let link = crate::server::client_link::ClientLink::new(sink.clone());
        let pending = link.pending_requests();
        // Never drained: a dispatcher that is busy for the whole test.
        let (dispatch_tx, mut dispatch_rx) = tokio::sync::mpsc::channel(1);

        let waiter = tokio::spawn(async move { link.request("client.echo", json!({})).await });
        let request = outbound_rx.recv().await.expect("core→client request").frame;
        let input = [
            json!({"method": "initialized", "params": {}}),
            json!({"method": "initialized", "params": {}}),
            json!({"id": 75, "method": "initialize", "params": {}}),
            json!({"id": request["id"], "result": {"released": true}}),
        ]
        .iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>();

        // A reader that parked on the full queue would never reach EOF.
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            reader_task(
                input.as_bytes(),
                ConnectionId::Primary,
                dispatch_tx,
                sink,
                pending,
            ),
        )
        .await
        .expect("the reader never parked on the full queue")
        .expect("reader ends at EOF");

        let answer = waiter
            .await
            .expect("waiter joins")
            .expect("the answer reached the waiter");
        assert!(matches!(
            answer,
            ClientResponse::Success { result, .. } if result == json!({"released": true})
        ));
        let rejected = outbound_rx.recv().await.expect("overload response").frame;
        assert_eq!(rejected["id"], json!(75));
        assert_eq!(rejected["error"]["code"], json!(DISPATCH_OVERLOAD_ERROR_CODE));
        // Only the first notification fit; the second was dropped.
        assert!(dispatch_rx.try_recv().is_ok());
        assert!(dispatch_rx.try_recv().is_err());
    }

    /// Stdin closing while core waits on the client fails that wait, so the
    /// waiter cannot keep the process alive.
    #[tokio::test]
    async fn eof_releases_a_pending_client_request() {
        let (sink, mut outbound_rx) = crate::server::outbound::test_sink();
        let link = crate::server::client_link::ClientLink::new(sink.clone());
        let pending = link.pending_requests();
        let (dispatch_tx, _dispatch_rx) = tokio::sync::mpsc::channel(1);

        let waiter = tokio::spawn(async move { link.request("client.echo", json!({})).await });
        outbound_rx.recv().await.expect("core→client request");
        reader_task(
            &b""[..],
            ConnectionId::Primary,
            dispatch_tx,
            sink,
            pending,
        )
        .await
        .expect("reader ends at EOF");

        let released = tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .expect("EOF released the waiter");
        assert!(released.expect("waiter joins").is_err());
    }

    #[test]
    fn malformed_and_invalid_envelopes_stay_on_dispatch_path() {
        assert!(matches!(
            classify_jsonrpc_message("{not json".to_string()),
            JsonRpcMessage::Request(_)
        ));
        assert!(matches!(
            classify_jsonrpc_message(json!({"id": 7, "params": {}}).to_string()),
            JsonRpcMessage::Request(_)
        ));
        assert!(matches!(
            classify_jsonrpc_message(json!({"method": "initialized", "params": {}}).to_string()),
            JsonRpcMessage::Notification(_)
        ));
    }
}
