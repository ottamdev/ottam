use crate::core_error::CoreError;
use ottam_protocol::{ProtocolError, ProtocolResponse, RequestID};
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

pub(super) fn success_response<T>(id: RequestID, result: T) -> Value
where
    T: Serialize,
{
    serde_json::to_value(ProtocolResponse::success(id, result)).expect("response serializes")
}

pub(super) fn error_response(id: RequestID, code: i64, message: impl Into<String>) -> Value {
    serde_json::to_value(ProtocolResponse::<Value>::failure(
        id,
        ProtocolError::new(code, message),
    ))
    .expect("error response serializes")
}

pub(super) fn core_error_response(id: RequestID, code: i64, error: CoreError) -> Value {
    let data = error.data();
    serde_json::to_value(ProtocolResponse::<Value>::failure(
        id,
        ProtocolError::with_data(code, error.message, data),
    ))
    .expect("error response serializes")
}

/// One line per answered request, in the caller's `handle_request` span so it
/// carries the method. A failure keeps core's own message, which is the only
/// record of why the app saw an error code; params and results stay out.
/// `queued_ms` is absent for a deferred response, which left the queue long
/// before it answered.
pub(super) fn log_completion(response: &Value, elapsed: Duration, queued_ms: Option<u128>) {
    let elapsed_ms = elapsed.as_millis();
    match response.get("error") {
        Some(error) => {
            // Bound before the macro, whose expansion shadows `Value`.
            let code = error.get("code").and_then(Value::as_i64);
            let message = code
                .filter(|code| logs_message(*code))
                .and_then(|_| error.get("message").and_then(Value::as_str))
                .unwrap_or_default();
            tracing::info!(
                queued_ms,
                elapsed_ms,
                status = "error",
                code,
                message,
                "request completed"
            );
        }
        None => tracing::info!(queued_ms, elapsed_ms, status = "success", "request completed"),
    }
}

/// Core's own failures use the JSON-RPC server-error range and word their
/// message themselves. Protocol errors outside it (invalid params, a bad
/// envelope) quote the decoder, which can echo the request's payload.
fn logs_message(code: i64) -> bool {
    (-32099..=-32000).contains(&code)
}

pub(super) fn request_id_from_invalid_envelope(value: &Value) -> RequestID {
    match value.get("id") {
        Some(Value::Null) => RequestID::Null,
        Some(Value::String(id)) => RequestID::String(id.clone()),
        Some(Value::Number(id)) => id
            .as_i64()
            .map(RequestID::Number)
            .unwrap_or(RequestID::Null),
        _ => RequestID::Null,
    }
}

pub(super) fn request_id_kind(id: &RequestID) -> &'static str {
    match id {
        RequestID::Null => "null",
        RequestID::Number(_) => "number",
        RequestID::String(_) => "string",
    }
}
