use schemars::JsonSchema;
use serde::{de, Deserialize, Deserializer, Serialize};
use serde_json::Value;

pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const METHOD_CORE_EXIT_ERROR: &str = "core.exitError";

/// Final best-effort notification before an unsuccessful Core exit. It may be
/// emitted before initialize, and is not emitted for signals or abrupt crashes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CoreExitError {
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

impl CoreExitError {
    pub fn server(message: impl Into<String>) -> Self {
        Self {
            kind: "coreServerError".into(),
            message: message.into(),
            details: None,
        }
    }
}

impl std::fmt::Display for CoreExitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.kind, self.message)?;
        if let Some(details) = &self.details {
            write!(formatter, ": {details}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum RequestID {
    Null,
    Number(i64),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolRequest<P> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsonrpc: Option<String>,
    pub id: RequestID,
    pub method: String,
    pub params: P,
}

impl<P> ProtocolRequest<P> {
    pub fn new(id: RequestID, method: impl Into<String>, params: P) -> Self {
        Self {
            jsonrpc: None,
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolResponse<T> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsonrpc: Option<String>,
    pub id: RequestID,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

impl<T> ProtocolResponse<T> {
    pub fn success(id: RequestID, result: T) -> Self {
        Self {
            jsonrpc: None,
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(id: RequestID, error: ProtocolError) -> Self {
        Self {
            jsonrpc: None,
            id,
            result: None,
            error: Some(error),
        }
    }
}

impl<'de, T> Deserialize<'de> for ProtocolResponse<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RawResponse<T> {
            jsonrpc: Option<String>,
            id: RequestID,
            result: Option<T>,
            error: Option<ProtocolError>,
        }

        let raw = RawResponse::<T>::deserialize(deserializer)?;
        match (raw.result.is_some(), raw.error.is_some()) {
            (true, true) => Err(de::Error::custom(
                "protocol response must not contain both result and error",
            )),
            (false, false) => Err(de::Error::custom(
                "protocol response must contain exactly one of result or error",
            )),
            _ => Ok(Self {
                jsonrpc: raw.jsonrpc,
                id: raw.id,
                result: raw.result,
                error: raw.error,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolNotification<P> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsonrpc: Option<String>,
    pub method: String,
    pub params: P,
}

impl<P> ProtocolNotification<P> {
    pub fn new(method: impl Into<String>, params: P) -> Self {
        Self {
            jsonrpc: None,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ProtocolError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(code: i64, message: impl Into<String>, data: Value) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(data),
        }
    }
}
