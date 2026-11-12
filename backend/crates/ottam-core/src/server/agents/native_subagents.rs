//! The frames core writes for AIR native subagent sessions (`sub:3`).
//!
//! A native subagent has no control call: the adapter announces the child
//! session with `subagent_spawned` and ends it with `subagent_state_update`,
//! both on the parent's session. Core records the thread on a synthetic
//! anchor call at the spawn's position, so the thread rides the transcript's
//! tool path like a legacy Claude control call, and stamps everything the
//! child's own session sends with the thread it belongs to. Attribution of
//! the child session itself lives in `attribution.rs` (rule 9).
use crate::agent::{SubagentFollowUpTool, SubagentHandbackTool, SubagentMessageTools};
use crate::server::chat::{
    image_call, NATIVE_ANCHOR_KEY, NATIVE_FOLLOW_UP_STAMP, NATIVE_THREAD_STAMP,
};
use ottam_protocol::{AgentUpdate, SubagentThreadState, TranscriptAdapterMetadata};
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

/// The anchor call of one run of the subagent in `thread_id`. A resumed run
/// gets its own anchor in the same thread; its `prompt` is the parent's
/// message that resumed it. The state update finds it by `call_id`.
pub(super) fn anchor(
    call_id: &str,
    thread_id: &str,
    name: Option<String>,
    task: Option<String>,
    prompt: Option<String>,
) -> AgentUpdate {
    let mut record = Map::new();
    record.insert("id".into(), json!(thread_id));
    if let Some(name) = &name {
        record.insert("name".into(), json!(name));
    }
    if let Some(task) = task.filter(|task| !is_placeholder_task(task)) {
        record.insert("task".into(), json!(task));
    }
    if let Some(prompt) = prompt.filter(|prompt| !prompt.trim().is_empty()) {
        record.insert("prompt".into(), json!(prompt));
    }
    AgentUpdate::ToolCall {
        payload: json!({
            "toolCallId": call_id,
            "kind": "other",
            "title": name.unwrap_or_else(|| "Subagent".into()),
            "status": "in_progress",
            NATIVE_ANCHOR_KEY: Value::Object(record),
        }),
    }
}

/// The anchor's update for the run's end.
pub(super) fn state(call_id: &str, state: SubagentThreadState) -> AgentUpdate {
    AgentUpdate::ToolCallUpdate {
        payload: json!({
            "toolCallId": call_id,
            "status": if state == SubagentThreadState::Failed { "failed" } else { "completed" },
            NATIVE_ANCHOR_KEY: {"state": state},
        }),
    }
}

/// codex-acp sends no task of its own, only `Delegated task for <name>`.
/// Claude resumes a subagent whose background task finished with the task's
/// `<task-notification>` as its task: not a title either.
fn is_placeholder_task(task: &str) -> bool {
    task == "Delegated task"
        || task.starts_with("Delegated task for ")
        || task.starts_with("<task-notification>")
}

/// What a child's session sends that belongs in its thread. Its session
/// metadata (title, usage, status, commands, mode) is the child's own.
pub(super) fn is_content(update: &AgentUpdate) -> bool {
    matches!(
        update,
        AgentUpdate::RichMessageChunk { .. }
            | AgentUpdate::AgentMessageChunk { .. }
            | AgentUpdate::AgentThoughtChunk { .. }
            | AgentUpdate::AgentImageChunk { .. }
            | AgentUpdate::ToolCall { .. }
            | AgentUpdate::ToolCallUpdate { .. }
            | AgentUpdate::Plan { .. }
            | AgentUpdate::AsyncTaskSpawned { .. }
            | AgentUpdate::AsyncTaskProgress { .. }
            | AgentUpdate::AsyncTaskStateUpdate { .. }
    )
}

/// Stamps `update` with the thread of the session it arrived on. A plain
/// chunk becomes a rich one to carry the stamp, and an image the call it is
/// stored as. A plan is the thread's own, not the run's.
pub(super) fn stamp(update: AgentUpdate, thread: &str) -> AgentUpdate {
    match update {
        AgentUpdate::RichMessageChunk {
            text,
            message_id,
            reasoning,
            mut metadata,
        } => {
            metadata.subagent_thread_id = Some(thread.to_owned());
            AgentUpdate::RichMessageChunk {
                text,
                message_id,
                reasoning,
                metadata,
            }
        }
        AgentUpdate::AgentMessageChunk { text, message_id } => {
            rich(text, message_id, false, thread)
        }
        AgentUpdate::AgentThoughtChunk { text, message_id } => rich(text, message_id, true, thread),
        AgentUpdate::AgentImageChunk {
            content,
            message_id,
        } => stamp(image_call(content, message_id), thread),
        AgentUpdate::ToolCall { mut payload } => {
            stamp_payload(&mut payload, thread);
            AgentUpdate::ToolCall { payload }
        }
        AgentUpdate::ToolCallUpdate { mut payload } => {
            stamp_payload(&mut payload, thread);
            AgentUpdate::ToolCallUpdate { payload }
        }
        AgentUpdate::Plan { mut payload } => {
            stamp_payload(&mut payload, thread);
            AgentUpdate::Plan { payload }
        }
        update => update,
    }
}

fn rich(text: String, message_id: Option<String>, reasoning: bool, thread: &str) -> AgentUpdate {
    AgentUpdate::RichMessageChunk {
        text,
        message_id,
        reasoning,
        metadata: TranscriptAdapterMetadata {
            subagent_thread_id: Some(thread.to_owned()),
            ..Default::default()
        },
    }
}

fn stamp_payload(payload: &mut Value, thread: &str) {
    if let Some(object) = payload.as_object_mut() {
        object.insert(NATIVE_THREAD_STAMP.into(), json!(thread));
    }
}

/// Most message calls watched at once; past it the oldest is forgotten.
const WATCHED_CALLS: usize = 256;

/// The calls an agent's [`SubagentMessageTools`] names, from the call until
/// its input arrives: the adapter sends the input in a later update.
#[derive(Default)]
pub(super) struct MessageCalls {
    calls: StdMutex<VecDeque<Watched>>,
}

struct Watched {
    agent: String,
    call_id: String,
    tool: MessageTool,
}

#[derive(Clone, Copy)]
enum MessageTool {
    Handback(SubagentHandbackTool),
    FollowUp(SubagentFollowUpTool),
}

/// A message a watched call carried.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum MessageCall {
    /// The subagent's final report.
    Handback { call_id: String, message: String },
    /// The parent's message to the subagent `target` names.
    FollowUp { target: String, message: String },
}

impl MessageCalls {
    /// Starts watching a call `tools` names. `tools` is read only for a new
    /// call, so the definition is looked up only when the call has a name.
    pub(super) fn observe(
        &self,
        agent: &str,
        update: &AgentUpdate,
        tools: impl FnOnce() -> SubagentMessageTools,
    ) -> Option<MessageCall> {
        let (payload, new) = match update {
            AgentUpdate::ToolCall { payload } => (payload, true),
            AgentUpdate::ToolCallUpdate { payload } => (payload, false),
            _ => return None,
        };
        let call_id = payload.get("toolCallId")?.as_str()?;
        let mut calls = self
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let watched = calls
            .iter()
            .position(|watched| watched.agent == agent && watched.call_id == call_id);
        let tool = match watched {
            Some(index) => calls[index].tool,
            None if new => {
                let name = payload.get("name")?.as_str()?;
                let tools = tools();
                match (tools.handback, tools.follow_up) {
                    (Some(handback), _) if handback.tool == name => MessageTool::Handback(handback),
                    (_, Some(follow_up)) if follow_up.tool == name => {
                        MessageTool::FollowUp(follow_up)
                    }
                    _ => return None,
                }
            }
            None => return None,
        };
        let input = payload.get("rawInput");
        let field = |name: &str| {
            input
                .and_then(|input| input.get(name))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
        };
        let message = match tool {
            MessageTool::Handback(handback) => {
                field(handback.message).map(|message| MessageCall::Handback {
                    call_id: call_id.to_owned(),
                    message,
                })
            }
            MessageTool::FollowUp(follow_up) => field(follow_up.target)
                .zip(field(follow_up.message))
                .map(|(target, message)| MessageCall::FollowUp { target, message }),
        };
        let ended = matches!(
            payload.get("status").and_then(Value::as_str),
            Some("completed" | "failed")
        );
        match watched {
            Some(index) if message.is_some() || ended => {
                calls.remove(index);
            }
            None if message.is_none() && !ended => {
                if calls.len() == WATCHED_CALLS {
                    calls.pop_front();
                }
                calls.push_back(Watched {
                    agent: agent.to_owned(),
                    call_id: call_id.to_owned(),
                    tool,
                });
            }
            _ => {}
        }
        message
    }
}

/// The subagent's report as text in its thread, after the call that carried
/// it. Its own message id keeps it from joining the subagent's last message.
pub(super) fn handback_text(call_id: &str, message: String, thread: &str) -> AgentUpdate {
    rich(message, Some(format!("handback:{call_id}")), false, thread)
}

/// Marks the parent's call that messaged the running subagent in `thread`.
pub(super) fn stamp_follow_up(update: &mut AgentUpdate, thread: &str, message: &str) {
    if let AgentUpdate::ToolCall { payload } | AgentUpdate::ToolCallUpdate { payload } = update {
        if let Some(object) = payload.as_object_mut() {
            object.insert(
                NATIVE_FOLLOW_UP_STAMP.into(),
                json!({"threadID": thread, "message": message}),
            );
        }
    }
}
