//! The `_session/steering` ACP extension: deliver a follow-up message into the
//! turn that is already running, instead of cancelling it and prompting again.
//!
//! Three adapters implement it and they do not agree on the idle case, so the
//! shapes here are deliberately permissive about what comes back:
//!
//! - claude-agent-acp and omp honour `_meta.steering.idleBehavior`, answering
//!   `promptRequired` and sending nothing when the turn has already settled.
//!   omp also names the queue it used (`"queue": "steering"`), which core
//!   ignores.
//! - codex-acp parses only `sessionId` and `prompt`, dropping `_meta`, so on a
//!   settled turn it starts a detached turn of its own and answers
//!   `startedNewTurn`. It is also the only one that answers `failed`.

use super::super::types::{HarnessError, HarnessErrorKind, SteeringCapability};
use agent_client_protocol::schema::v1::{ContentBlock, Meta};
use agent_client_protocol::ErrorCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The top-level `_meta` key every steering adapter advertises it under.
const STEERING_META_KEY: &str = "steering";

#[derive(Debug, Clone, Serialize, Deserialize, agent_client_protocol::JsonRpcRequest)]
#[serde(rename_all = "camelCase")]
#[request(method = "_session/steering", response = SteerResponse)]
pub(super) struct SteerRequest {
    pub(super) session_id: String,
    pub(super) prompt: Vec<ContentBlock>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub(super) meta: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, agent_client_protocol::JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub(super) struct SteerResponse {
    /// Left as a string rather than an enum: an adapter is free to add an
    /// outcome, and one this client does not recognise has to be reportable by
    /// name rather than fail to decode.
    pub(super) outcome: String,
    #[serde(default)]
    pub(super) reason: Option<String>,
}

/// What the agent says it did with the steered message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SteerOutcome {
    /// It joined the running turn. The run continues and its reply covers the
    /// steered message too.
    Injected,
    /// The turn had already settled and the agent left the message alone, so
    /// it is still the caller's to send.
    PromptRequired,
    /// The turn had already settled and the agent started a turn of its own
    /// for the message. Nobody is waiting on that turn, so the caller has to
    /// cancel it before sending the message properly.
    StartedNewTurn,
    /// The agent refused, or answered with something this client cannot act on.
    Unrecognized(String),
}

impl SteerRequest {
    pub(super) fn new(session_id: String, prompt: Vec<ContentBlock>) -> Self {
        Self {
            session_id,
            prompt,
            // Ask for the outcome that leaves the message unsent, so a turn
            // that settled under us stays the host's to resend rather than
            // becoming a turn nothing is waiting on. Claude and omp read this;
            // `startedNewTurn` is the answer from an adapter that does not.
            meta: Some(json!({STEERING_META_KEY: {"idleBehavior": "promptRequired"}})),
        }
    }
}

impl SteerResponse {
    pub(super) fn outcome(&self) -> SteerOutcome {
        match self.outcome.as_str() {
            "injected" => SteerOutcome::Injected,
            "promptRequired" => SteerOutcome::PromptRequired,
            "startedNewTurn" => SteerOutcome::StartedNewTurn,
            other => SteerOutcome::Unrecognized(match &self.reason {
                Some(reason) => format!("{other} ({reason})"),
                None => other.to_string(),
            }),
        }
    }
}

/// Whether the agent advertised the extension, read from the top-level `_meta`
/// of its `initialize` response. Every adapter spells it the same way.
///
/// An agent that says nothing keeps the interrupt-and-continue behavior every
/// ACP adapter had before this extension existed.
pub(super) fn steering_capability(meta: Option<&Meta>) -> SteeringCapability {
    let supported = meta
        .and_then(|meta| meta.get(STEERING_META_KEY))
        .and_then(|steering| steering.get("supported"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if supported {
        SteeringCapability::Native
    } else {
        SteeringCapability::InterruptThenPrompt
    }
}

/// An agent that never registered the method is one whose advertisement lied,
/// or one behind a proxy that dropped it. Either way it cannot steer, and the
/// caller should stop asking it to.
pub(super) fn map_steer_error(error: &agent_client_protocol::Error) -> HarnessError {
    if error.code == ErrorCode::MethodNotFound {
        return HarnessError::new(
            HarnessErrorKind::SteeringUnsupported,
            super::protocol::sanitize_error_message(&error.message),
        );
    }
    super::protocol::map_acp_error(error)
}
