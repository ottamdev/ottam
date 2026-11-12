use crate::core_error::CoreError;
use ottam_protocol::{ProtocolError, ProtocolResponse, RequestID};
use serde::Serialize;
use serde_json::Value;

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

pub(super) trait Tap: Sized {
    fn tap(self, f: impl FnOnce(&Self)) -> Self {
        f(&self);
        self
    }
}

impl<T> Tap for T {}

pub(super) fn response_status(response: &Value) -> &'static str {
    if response.get("error").is_some() {
        "error"
    } else {
        "success"
    }
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
