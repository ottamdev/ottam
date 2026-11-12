use super::responses::error_response;
use ottam_protocol::ProtocolRequest;
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Clone, Copy)]
pub(super) enum InvalidParamsLog {
    Warn,
    Silent,
}

pub(super) fn decode_params<T: DeserializeOwned>(
    request: &ProtocolRequest<Value>,
    logging: InvalidParamsLog,
) -> Result<T, Value> {
    serde_json::from_value(request.params.clone()).map_err(|error| {
        if matches!(logging, InvalidParamsLog::Warn) {
            tracing::warn!(error = %error, method = %request.method, "rejected invalid params");
        }
        error_response(
            request.id.clone(),
            -32602,
            format!("invalid params: {error}"),
        )
    })
}
