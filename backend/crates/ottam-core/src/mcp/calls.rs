//! Which chat an upstream server's request belongs to.
//!
//! A server's `elicitation/create` arrives on the shared connection with no
//! link to a call, so core gives it to the chat with calls in flight there,
//! and declines it when calls from more than one chat are in flight, since
//! it could belong to either. rmcp itself already refuses one that arrives
//! with no call in flight at all (SEP-2260).

use super::tokens::McpCaller;
use crate::agent::elicitation;
use crate::agent::{HarnessPermissionBroker, HarnessQuestionOutcome, HarnessQuestionRequest};
use ottam_protocol::{McpServerID, WorkspaceID};
use futures::future::BoxFuture;
use rmcp::model::{ElicitRequestParams, ElicitResult, ElicitationAction};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// The `_meta` key Claude Code stamps on each MCP call with the ACP tool
/// call it belongs to.
const CLAUDE_TOOL_USE_ID: &str = "claudecode/toolUseId";

/// The chat a call came from, and how to withdraw what it asked.
#[derive(Clone, Debug)]
pub(crate) struct CallOrigin {
    pub(crate) caller: McpCaller,
    /// The agent's tool call, when the harness names it.
    pub(crate) tool_call_id: Option<String>,
    /// Cancelled when the agent cancels the call or the call ends, so a
    /// question it asked is withdrawn once nothing waits on the answer.
    pub(crate) cancelled: CancellationToken,
}

impl CallOrigin {
    pub(crate) fn new(
        caller: McpCaller,
        meta: Option<&Map<String, Value>>,
        cancelled: CancellationToken,
    ) -> Self {
        let tool_call_id = meta
            .and_then(|meta| meta.get(CLAUDE_TOOL_USE_ID))
            .and_then(Value::as_str)
            .map(str::to_owned);
        Self {
            caller: caller.attributed(meta),
            tool_call_id,
            cancelled,
        }
    }
}

/// What reaches core from an upstream connection outside a call's reply.
pub(crate) trait UpstreamEvents: Send + Sync + 'static {
    /// The server's tools changed, for sessions in `workspace` when the
    /// connection is a local one.
    fn tools_changed(&self, server_id: &McpServerID, workspace: Option<&WorkspaceID>);
    /// Put a server's question to the chat a call came from.
    fn elicit(
        &self,
        origin: CallOrigin,
        request: ElicitRequestParams,
    ) -> BoxFuture<'static, ElicitResult>;
}

/// The calls in flight on one upstream connection.
#[derive(Clone, Default)]
pub(crate) struct InFlightCalls {
    calls: Arc<Mutex<HashMap<u64, CallOrigin>>>,
    next: Arc<AtomicU64>,
}

impl InFlightCalls {
    /// Track a call until the returned guard drops.
    pub(crate) fn track(&self, origin: CallOrigin) -> InFlightCall {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.lock().insert(id, origin);
        InFlightCall {
            calls: self.clone(),
            id,
        }
    }

    /// The origin an unlinked request belongs to, when every call in flight
    /// comes from the same agent session. With several calls in flight, it
    /// names their tool call only when they share one, and is cancelled once
    /// all of them are.
    pub(crate) fn sole_origin(&self) -> Option<CallOrigin> {
        let calls = self.lock();
        let first = calls.values().next()?;
        let session = |origin: &CallOrigin| {
            (
                origin.caller.conversation_id.clone(),
                origin.caller.session_id.clone(),
            )
        };
        if !calls
            .values()
            .all(|origin| session(origin) == session(first))
        {
            return None;
        }
        if calls.len() == 1 {
            return Some(first.clone());
        }
        let tool_call_id = first.tool_call_id.clone().filter(|id| {
            calls
                .values()
                .all(|origin| origin.tool_call_id.as_ref() == Some(id))
        });
        let cancelled = CancellationToken::new();
        let tokens: Vec<CancellationToken> = calls
            .values()
            .map(|origin| origin.cancelled.clone())
            .collect();
        tokio::spawn({
            let cancelled = cancelled.clone();
            async move {
                futures::future::join_all(tokens.iter().map(CancellationToken::cancelled)).await;
                cancelled.cancel();
            }
        });
        Some(CallOrigin {
            caller: first.caller.clone(),
            tool_call_id,
            cancelled,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, CallOrigin>> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub(crate) struct InFlightCall {
    calls: InFlightCalls,
    id: u64,
}

impl Drop for InFlightCall {
    fn drop(&mut self) {
        // The call is over, so anything it asked is withdrawn.
        if let Some(origin) = self.calls.lock().remove(&self.id) {
            origin.cancelled.cancel();
        }
    }
}

/// Ask the user a server's form question in the calling chat's question
/// panel. A form core cannot show, or a call with no session yet, is
/// declined; a question nobody answered is cancelled.
pub(crate) async fn ask(
    broker: Arc<dyn HarnessPermissionBroker>,
    origin: CallOrigin,
    request: ElicitRequestParams,
) -> ElicitResult {
    let Some(session_id) = origin.caller.session_id.clone() else {
        return declined();
    };
    let Ok(mut params) = serde_json::to_value(&request) else {
        return declined();
    };
    params["sessionId"] = Value::String(session_id.0.clone());
    if let Some(tool_call_id) = &origin.tool_call_id {
        params["toolCallId"] = Value::String(tool_call_id.clone());
    }
    let form = match elicitation::parse_form(&params, false) {
        Ok(form) => form,
        Err(refusal) => {
            tracing::info!(
                refusal = refusal.label(),
                "MCP server asked for input core cannot show"
            );
            return declined();
        }
    };
    let outcome = broker
        .ask_question(HarnessQuestionRequest {
            agent_id: origin.caller.agent_id.clone(),
            session_id,
            provider_tool_call_id: form.tool_call_id.clone(),
            title: form.title.clone(),
            questions: form.questions.clone(),
            withdrawn: origin.cancelled.clone(),
        })
        .await;
    let reply = match outcome {
        HarnessQuestionOutcome::Submitted(answers) => elicitation::accept(form.content(&answers)),
        HarnessQuestionOutcome::Skipped => elicitation::decline(),
        HarnessQuestionOutcome::Cancelled => elicitation::cancel(),
    };
    serde_json::from_value(reply).unwrap_or_else(|_| declined())
}

pub(crate) fn declined() -> ElicitResult {
    ElicitResult::new(ElicitationAction::Decline)
}

#[cfg(test)]
mod tests {
    use super::{CallOrigin, InFlightCalls};
    use crate::agent::{AgentId, HarnessSessionId};
    use crate::mcp::tokens::McpCaller;
    use ottam_protocol::{ConversationID, WorkspaceID};
    use serde_json::{json, Value};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    fn origin(conversation: &str, tool_call: &str) -> CallOrigin {
        let caller = McpCaller {
            workspace_id: WorkspaceID("workspace".into()),
            conversation_id: ConversationID(conversation.into()),
            agent_id: AgentId("claude".into()),
            generation: 1,
            session_id: Some(HarnessSessionId::new(format!("{conversation}-session"))),
            shared: false,
            sessions: Vec::new(),
            servers: Vec::new(),
        };
        let Value::Object(meta) = json!({"claudecode/toolUseId": tool_call}) else {
            unreachable!()
        };
        CallOrigin::new(caller, Some(&meta), CancellationToken::new())
    }

    #[test]
    fn a_finished_call_withdraws_what_it_asked() {
        let calls = InFlightCalls::default();
        let call = origin("chat", "toolu_1");
        let cancelled = call.cancelled.clone();
        let tracked = calls.track(call);
        let asked = calls.sole_origin().expect("one chat");
        assert_eq!(asked.tool_call_id.as_deref(), Some("toolu_1"));
        drop(tracked);
        assert!(cancelled.is_cancelled());
        assert!(calls.sole_origin().is_none());
    }

    #[test]
    fn calls_from_two_chats_explain_no_question() {
        let calls = InFlightCalls::default();
        let _first = calls.track(origin("one", "toolu_1"));
        let _second = calls.track(origin("two", "toolu_2"));
        assert!(calls.sole_origin().is_none());
    }

    #[tokio::test]
    async fn parallel_calls_in_one_chat_name_no_tool_call_and_withdraw_together() {
        let calls = InFlightCalls::default();
        let first = calls.track(origin("chat", "toolu_1"));
        let second = calls.track(origin("chat", "toolu_2"));
        let asked = calls.sole_origin().expect("one chat");
        assert_eq!(asked.tool_call_id, None);
        drop(first);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!asked.cancelled.is_cancelled(), "one call still waits");
        drop(second);
        tokio::time::timeout(Duration::from_secs(1), asked.cancelled.cancelled())
            .await
            .expect("withdrawn once both calls ended");
    }
}
