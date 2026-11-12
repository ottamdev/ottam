use super::attribution::SessionAttribution;
use super::conversation_runner::{ConversationPermissionControl, FollowUpAttribution};
use crate::agent::{
    AgentId, HarnessPermissionBroker, HarnessPermissionFuture, HarnessPermissionOutcome,
    HarnessPermissionRequest, HarnessQuestionFuture, HarnessQuestionOutcome,
    HarnessQuestionRequest,
};
use crate::server::client_link::{ClientLink, ClientResponse};
use ottam_protocol::{
    AgentAskQuestionParams, AgentAskQuestionResult, AgentID, AgentPermissionOption,
    AgentPermissionOptionKind, AgentPermissionOutcome, AgentQuestion, AgentQuestionAnswer,
    AgentQuestionKind, AgentQuestionOutcome, AgentRequestPermissionParams,
    AgentRequestPermissionResult, AgentSessionID, ConversationEventKind, TranscriptQuestion,
    TranscriptQuestionAnswer, TranscriptQuestionState, METHOD_AGENT_ASK_QUESTION,
    METHOD_AGENT_REQUEST_PERMISSION,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use tokio_util::sync::CancellationToken;

type PendingPermissions = HashMap<(String, String), HashMap<u64, CancellationToken>>;

/// The plan a request asks to approve before the agent leaves plan mode.
///
/// Codex (`PlanReviewReporter`) and Claude (`ExitPlanMode`) both send the
/// plan's markdown as `rawInput.plan`. Codex marks the request `switch_mode`;
/// Claude's adapter sends an AIR client no `kind`. Any other kind is not a plan.
pub(super) fn plan_markdown(tool_call: &Value) -> Option<&str> {
    match tool_call.get("kind") {
        None | Some(Value::Null) => {}
        Some(Value::String(kind)) if kind == "switch_mode" => {}
        Some(_) => return None,
    }
    tool_call
        .get("rawInput")?
        .get("plan")?
        .as_str()
        .filter(|plan| !plan.trim().is_empty())
}

#[derive(Clone)]
pub(super) struct ServerPermissionBroker {
    client_link: ClientLink,
    client_supports_permissions: Arc<AtomicBool>,
    client_supports_questions: Arc<AtomicBool>,
    next_id: Arc<AtomicU64>,
    pending: Arc<StdMutex<PendingPermissions>>,
    attribution: SessionAttribution,
    /// The coordinator, which opens follow-up runs. Bound once both exist;
    /// unbound, a request on a session no run holds goes out unattributed.
    follow_ups: Arc<OnceLock<Weak<dyn FollowUpAttribution>>>,
}

impl ServerPermissionBroker {
    pub(super) fn new(client_link: ClientLink, attribution: SessionAttribution) -> Self {
        Self {
            client_link,
            client_supports_permissions: Arc::new(AtomicBool::new(false)),
            client_supports_questions: Arc::new(AtomicBool::new(false)),
            next_id: Arc::new(AtomicU64::new(1)),
            pending: Arc::new(StdMutex::new(HashMap::new())),
            attribution,
            follow_ups: Arc::new(OnceLock::new()),
        }
    }

    pub(super) fn bind_follow_ups<T: FollowUpAttribution + 'static>(&self, follow_ups: &Arc<T>) {
        let follow_ups: Weak<T> = Arc::downgrade(follow_ups);
        let _ = self.follow_ups.set(follow_ups);
    }

    pub(super) fn set_supported(&self, supported: bool) {
        self.client_supports_permissions
            .store(supported, Ordering::Release);
    }

    pub(super) fn set_questions_supported(&self, supported: bool) {
        self.client_supports_questions
            .store(supported, Ordering::Release);
    }

    /// Cancels what the session's turn is waiting on: its own requests and
    /// those of the subagents it runs, which ask on their own sessions.
    pub(super) fn cancel_session(&self, agent_id: &AgentId, session_id: &str) {
        let key = (agent_id.0.clone(), session_id.to_string());
        let mut keys = self.attribution.children_of(&key);
        keys.push(key);
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for key in keys {
            for token in pending
                .remove(&key)
                .into_iter()
                .flat_map(HashMap::into_values)
            {
                token.cancel();
            }
        }
    }

    pub(super) fn cancel_all(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for sessions in pending.values() {
            for token in sessions.values() {
                token.cancel();
            }
        }
        pending.clear();
    }

    fn remove_pending(&self, key: &(String, String), id: u64) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(tokens) = pending.get_mut(key) {
            tokens.remove(&id);
            if tokens.is_empty() {
                pending.remove(key);
            }
        }
    }
}

impl HarnessPermissionBroker for ServerPermissionBroker {
    fn request_permission(&self, request: HarnessPermissionRequest) -> HarnessPermissionFuture<'_> {
        Box::pin(async move {
            // A covered agent's request, or any request of a delegated
            // child's, may still be answered by core, so it needs its
            // conversation even without a client to ask.
            let key = (request.agent_id.0.clone(), request.session_id.0.clone());
            if !request.auto_approvable && !self.client_supports_permissions.load(Ordering::Acquire)
            {
                let child = match self.attribution.running_context(&key) {
                    Some(context) => context.is_delegated_child().await,
                    None => false,
                };
                if !child {
                    return HarnessPermissionOutcome::Cancelled;
                }
            }

            let pending_id = self.next_id.fetch_add(1, Ordering::Relaxed);
            // A Stop or the agent withdrawing the request ends the wait.
            let cancel = request.withdrawn.child_token();
            self.pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entry(key.clone())
                .or_default()
                .insert(pending_id, cancel.clone());
            // Rule 8: a session no run holds may belong to a follow-up that
            // is about to open, or that this request opens.
            let follow_ups = self.follow_ups.get().and_then(Weak::upgrade);
            let conversation_context = match self.attribution.running_context(&key) {
                Some(context) => Some(context),
                None => match follow_ups.as_ref() {
                    Some(follow_ups) => tokio::select! {
                        _ = cancel.cancelled() => None,
                        context = follow_ups.attribute(key.clone()) => context,
                    },
                    None => None,
                },
            };
            drop(follow_ups);
            // "Always allow tool actions": allow once, like Zed, so turning it
            // off restores prompts rather than leaving grants in the agent.
            // A plan is the user's to review, never a tool action.
            let auto_answer = match conversation_context.as_ref() {
                Some(context) if plan_markdown(&request.tool_call).is_none() => {
                    if request.auto_approvable || context.is_delegated_child().await {
                        auto_allow_option(&request.options, context).await
                    } else {
                        None
                    }
                }
                _ => None,
            };
            if auto_answer.is_none() && !self.client_supports_permissions.load(Ordering::Acquire) {
                self.remove_pending(&key, pending_id);
                return HarnessPermissionOutcome::Cancelled;
            }
            let _waiting = conversation_context
                .as_ref()
                .and_then(|context| context.activity.as_ref())
                .map(|activity| activity.permission());
            // Carried onto the resolved event below so a client can pair the two.
            // A run can have several approvals outstanding at once, and a client that
            // records what was decided about each has no other way to match them:
            // pairing by arrival order attributes the outcome to the wrong tool the
            // moment the user answers the second card first.
            let tool_call_id = request
                .tool_call
                .get("toolCallId")
                .and_then(Value::as_str)
                .map(str::to_owned);
            // A native subagent asks on its own session: name it.
            let (subagent_thread_id, subagent_name) = conversation_context
                .as_ref()
                .and_then(|_| self.attribution.child_label(&key))
                .map_or((None, None), |(thread, name)| (Some(thread), name));
            let params = AgentRequestPermissionParams {
                agent_id: AgentID(request.agent_id.0.clone()),
                session_id: AgentSessionID(request.session_id.0.clone()),
                workspace_id: conversation_context
                    .as_ref()
                    .map(|context| context.workspace_id.clone()),
                project_id: conversation_context
                    .as_ref()
                    .and_then(|context| context.project_id.clone()),
                conversation_id: conversation_context
                    .as_ref()
                    .map(|context| context.conversation_id.clone()),
                run_id: conversation_context
                    .as_ref()
                    .map(|context| context.run_id.clone()),
                subagent_thread_id,
                subagent_name,
                plan: plan_markdown(&request.tool_call).map(str::to_owned),
                tool_call: request.tool_call,
                options: request
                    .options
                    .into_iter()
                    .map(|option| AgentPermissionOption {
                        option_id: option.option_id,
                        name: option.name,
                        kind: match option.kind {
                            crate::agent::HarnessPermissionOptionKind::AllowOnce => {
                                AgentPermissionOptionKind::AllowOnce
                            }
                            crate::agent::HarnessPermissionOptionKind::AllowAlways => {
                                AgentPermissionOptionKind::AllowAlways
                            }
                            crate::agent::HarnessPermissionOptionKind::RejectOnce => {
                                AgentPermissionOptionKind::RejectOnce
                            }
                            crate::agent::HarnessPermissionOptionKind::RejectAlways => {
                                AgentPermissionOptionKind::RejectAlways
                            }
                            crate::agent::HarnessPermissionOptionKind::Other => {
                                AgentPermissionOptionKind::Other
                            }
                        },
                    })
                    .collect(),
            };
            if let Some(context) = conversation_context.as_ref() {
                context
                    .queue_event(
                        ConversationEventKind::PermissionRequested,
                        json!({
                            "toolCallId": tool_call_id,
                            "options": params.options,
                        }),
                    )
                    .await;
            }
            let transcript_request_id = uuid::Uuid::now_v7().to_string();

            let outcome = if let Some(option_id) = auto_answer.clone() {
                self.remove_pending(&key, pending_id);
                // Stop may land while the switch is read or the request is
                // recorded. Checked after removal, so no later Stop can reach it.
                if cancel.is_cancelled() {
                    AgentPermissionOutcome::Cancelled
                } else {
                    tracing::info!(
                        agent_id = %request.agent_id.0,
                        option_kind = "allow_once",
                        "answered a permission request: always allow tool actions"
                    );
                    AgentPermissionOutcome::Selected { option_id }
                }
            } else {
                // No timeout: like a question, the request waits for the user,
                // a Stop, or the agent. A timeout aborted tool calls the user
                // simply hadn't reached yet.
                let response = match serde_json::to_value(&params) {
                    Ok(value) => tokio::select! {
                        _ = cancel.cancelled() => None,
                        response = self
                            .client_link
                            .request(METHOD_AGENT_REQUEST_PERMISSION, value) => response.ok(),
                    },
                    Err(_) => None,
                };
                self.remove_pending(&key, pending_id);
                match response {
                    Some(ClientResponse::Success { result, .. }) => {
                        serde_json::from_value::<AgentRequestPermissionResult>(result)
                            .map(|result| result.outcome)
                            .unwrap_or(AgentPermissionOutcome::Cancelled)
                    }
                    _ => AgentPermissionOutcome::Cancelled,
                }
            };
            if let Some(context) = conversation_context.as_ref() {
                let mut payload = serde_json::to_value(&outcome)
                    .unwrap_or_else(|_| json!({"outcome": "cancelled"}));
                let selected = match &outcome {
                    AgentPermissionOutcome::Selected { option_id } => params
                        .options
                        .iter()
                        .find(|option| &option.option_id == option_id),
                    _ => None,
                };
                let decision = ottam_protocol::TranscriptPermission {
                    request_id: transcript_request_id,
                    provider_tool_call_id: tool_call_id.clone(),
                    title: params
                        .tool_call
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("Tool permission")
                        .to_owned(),
                    outcome_label: match selected {
                        Some(_) if auto_answer.is_some() => "Allowed automatically".to_owned(),
                        Some(option) => option.name.clone(),
                        None => "Cancelled".to_owned(),
                    },
                    allowed: selected.is_some_and(|option| {
                        matches!(
                            option.kind,
                            AgentPermissionOptionKind::AllowOnce
                                | AgentPermissionOptionKind::AllowAlways
                        )
                    }),
                    plan: params.plan.clone(),
                };
                if let Some(object) = payload.as_object_mut() {
                    object.insert(
                        "transcriptPermission".into(),
                        serde_json::to_value(decision).expect("typed permission serializes"),
                    );
                }
                // A subagent's decision is its thread's row, not the parent's.
                if let (Some(object), Some(thread)) =
                    (payload.as_object_mut(), params.subagent_thread_id.as_ref())
                {
                    object.insert("subagentThreadID".into(), json!(thread));
                }
                // Added alongside the outcome rather than wrapped around it, so a
                // client that only knows the outcome shape still decodes the payload.
                if let (Some(object), Some(tool_call_id)) =
                    (payload.as_object_mut(), tool_call_id.as_ref())
                {
                    object.insert("toolCallId".to_string(), json!(tool_call_id));
                }
                context
                    .queue_event(ConversationEventKind::PermissionResolved, payload)
                    .await;
            }
            match outcome {
                AgentPermissionOutcome::Cancelled => HarnessPermissionOutcome::Cancelled,
                AgentPermissionOutcome::Selected { option_id } => {
                    HarnessPermissionOutcome::Selected { option_id }
                }
            }
        })
    }

    fn ask_question(&self, request: HarnessQuestionRequest) -> HarnessQuestionFuture<'_> {
        Box::pin(self.ask(request))
    }
}

/// The request's allow-once option, when its conversation always allows tool
/// actions. Without one, the request goes to the user.
async fn auto_allow_option(
    options: &[crate::agent::HarnessPermissionOption],
    context: &super::conversation_runner::ConversationRunContext,
) -> Option<String> {
    let option = options
        .iter()
        .find(|option| option.kind == crate::agent::HarnessPermissionOptionKind::AllowOnce)?;
    context
        .always_allow_tool_actions()
        .await
        .then(|| option.option_id.clone())
}

impl ServerPermissionBroker {
    async fn ask(&self, request: HarnessQuestionRequest) -> HarnessQuestionOutcome {
        if !self.client_supports_questions.load(Ordering::Acquire) {
            return HarnessQuestionOutcome::Cancelled;
        }
        let key = (request.agent_id.0.clone(), request.session_id.0.clone());
        let pending_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        // Stop, shutdown, and the agent's own withdrawal all end the wait.
        let cancel = request.withdrawn.child_token();
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(key.clone())
            .or_default()
            .insert(pending_id, cancel.clone());
        let follow_ups = self.follow_ups.get().and_then(Weak::upgrade);
        let context = match self.attribution.running_context(&key) {
            Some(context) => Some(context),
            None => match follow_ups.as_ref() {
                Some(follow_ups) => tokio::select! {
                    _ = cancel.cancelled() => None,
                    context = follow_ups.attribute(key.clone()) => context,
                },
                None => None,
            },
        };
        drop(follow_ups);
        let _waiting = context
            .as_ref()
            .and_then(|context| context.activity.as_ref())
            .map(|activity| activity.permission());
        let (subagent_thread_id, subagent_name) = context
            .as_ref()
            .and_then(|_| self.attribution.child_label(&key))
            .map_or((None, None), |(thread, name)| (Some(thread), name));
        let params = AgentAskQuestionParams {
            agent_id: AgentID(request.agent_id.0.clone()),
            session_id: AgentSessionID(request.session_id.0.clone()),
            workspace_id: context.as_ref().map(|context| context.workspace_id.clone()),
            project_id: context
                .as_ref()
                .and_then(|context| context.project_id.clone()),
            conversation_id: context
                .as_ref()
                .map(|context| context.conversation_id.clone()),
            run_id: context.as_ref().map(|context| context.run_id.clone()),
            subagent_thread_id,
            subagent_name,
            provider_tool_call_id: request.provider_tool_call_id.clone(),
            title: request.title,
            questions: request.questions,
        };
        let Ok(value) = serde_json::to_value(&params) else {
            self.remove_pending(&key, pending_id);
            return HarnessQuestionOutcome::Cancelled;
        };
        // The tool call records the question from the moment it is asked, so
        // a core that stops before an answer leaves it to recovery.
        let request_id = uuid::Uuid::now_v7().to_string();
        let record = |state, answers: Option<&[AgentQuestionAnswer]>| TranscriptQuestion {
            request_id: request_id.clone(),
            state,
            answers: recorded_answers(&params.questions, answers),
        };
        let recorder = context.as_ref().zip(params.provider_tool_call_id.as_ref());
        if let Some((context, call)) = recorder {
            // The agent sent the call before asking, but the call reaches the
            // transcript on the update forwarder's task, and a record for a
            // call the transcript doesn't have yet is dropped. Wait for it.
            // This covers a call recorded into this run; one the forwarder
            // holds or routes elsewhere still has no row here.
            if let Some(follow_ups) = self.follow_ups.get().and_then(Weak::upgrade) {
                tokio::select! {
                    _ = cancel.cancelled() => {}
                    () = follow_ups.record_sent_updates(&request.agent_id) => {}
                }
            }
            context
                .record_question(call.clone(), record(TranscriptQuestionState::Pending, None))
                .await;
        }

        // No timeout: the question waits for the user, a Stop, or the agent.
        let response = tokio::select! {
            _ = cancel.cancelled() => None,
            response = self.client_link.request(METHOD_AGENT_ASK_QUESTION, value) => response.ok(),
        };
        self.remove_pending(&key, pending_id);
        let outcome = match response {
            Some(ClientResponse::Success { result, .. }) => {
                serde_json::from_value::<AgentAskQuestionResult>(result)
                    .map(|result| result.outcome)
                    .unwrap_or(AgentQuestionOutcome::Cancelled)
            }
            _ => AgentQuestionOutcome::Cancelled,
        };
        if let Some((context, call)) = recorder {
            let settled = match &outcome {
                AgentQuestionOutcome::Submitted { answers } => {
                    record(TranscriptQuestionState::Answered, Some(answers))
                }
                AgentQuestionOutcome::Skipped => record(TranscriptQuestionState::Skipped, None),
                AgentQuestionOutcome::Cancelled => {
                    record(TranscriptQuestionState::NotAnswered, None)
                }
            };
            context.record_question(call.clone(), settled).await;
        }
        match outcome {
            AgentQuestionOutcome::Submitted { answers } => {
                HarnessQuestionOutcome::Submitted(answers)
            }
            AgentQuestionOutcome::Skipped => HarnessQuestionOutcome::Skipped,
            AgentQuestionOutcome::Cancelled => HarnessQuestionOutcome::Cancelled,
        }
    }
}

/// What the transcript keeps of each answer: option labels rather than
/// values, and nothing of a secure answer.
pub(super) fn recorded_answers(
    questions: &[AgentQuestion],
    answers: Option<&[AgentQuestionAnswer]>,
) -> Vec<TranscriptQuestionAnswer> {
    questions
        .iter()
        .map(|question| {
            let secure = matches!(question.kind, AgentQuestionKind::Text { secure: true, .. });
            let options = match &question.kind {
                AgentQuestionKind::SingleSelect { options, .. }
                | AgentQuestionKind::MultiSelect { options, .. } => options.as_slice(),
                _ => &[],
            };
            let answer = answers.and_then(|answers| {
                answers
                    .iter()
                    .find(|answer| answer.question_id == question.id)
            });
            let values = match answer {
                Some(answer) if !secure => answer
                    .values
                    .iter()
                    .map(|value| {
                        options
                            .iter()
                            .find(|option| &option.value == value)
                            .map_or_else(|| value.clone(), |option| option.label.clone())
                    })
                    .chain(
                        answer
                            .other
                            .as_deref()
                            .map(str::trim)
                            .filter(|text| !text.is_empty())
                            .map(str::to_owned),
                    )
                    .collect(),
                _ => Vec::new(),
            };
            TranscriptQuestionAnswer {
                prompt: question.prompt.clone(),
                values,
                secure,
            }
        })
        .collect()
}

impl ConversationPermissionControl for ServerPermissionBroker {
    fn cancel_session(&self, agent_id: &AgentId, session_id: &str) {
        ServerPermissionBroker::cancel_session(self, agent_id, session_id);
    }

    fn has_pending(&self, agent_id: &AgentId, session_id: &str) -> bool {
        let key = (agent_id.0.clone(), session_id.to_string());
        let mut keys = self.attribution.children_of(&key);
        keys.push(key);
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        keys.iter()
            .any(|key| pending.get(key).is_some_and(|waiting| !waiting.is_empty()))
    }
}
