//! ACP Subagent Sessions (the RFD in schema 1.10, newer than the pin): an
//! agent announces each child on its parent's session with
//! `subagent_update`, then streams the child's own updates under the child's
//! session id. Core already threads AIR's draft of the same idea
//! (`subagent_spawned`, `subagent_state_update`), so this folds the merged
//! shape onto those two kinds rather than adding a second path.
//!
//! `subagent_update` is an upsert, where AIR's kinds are events: the first
//! update for a child announces it and later ones patch it. Core's spawn is
//! not idempotent, so this remembers each child and announces it once, and
//! ends it once, on its first `idle`. A child woken again keeps its ended
//! thread; its new traffic still lands there.
//!
//! The announcement carries no prompt. The RFD delivers the assignment as the
//! child's first incoming `session_message`, right after it, so the spawn is
//! held until the child's first frame and takes that message's text. Any
//! other first frame releases it without one. Later `session_message`s are
//! left to the decoder, which counts them as unhandled.
use super::super::types::{HarnessSessionId, HarnessUpdateKind};
use ottam_protocol::SubagentThreadState;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
pub(super) struct SubagentSessions {
    children: Mutex<HashMap<String, Child>>,
}

struct Child {
    parent: HarnessSessionId,
    open: bool,
    /// The announcement, until the child's first frame.
    held: Option<Announcement>,
}

struct Announcement {
    name: Option<String>,
    task: Option<String>,
}

/// What one raw `session/update` turns into.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Translation {
    /// Updates to publish before the frame, each with its session.
    pub(super) updates: Vec<(HarnessSessionId, HarnessUpdateKind)>,
    /// The frame was this RFD's and is fully handled; otherwise it goes on to
    /// the decoder.
    pub(super) consumed: bool,
}

impl SubagentSessions {
    pub(super) fn translate(&self, params: &Value) -> Translation {
        let mut out = Translation::default();
        let (Some(session), Some(update)) = (
            params.get("sessionId").and_then(Value::as_str),
            params.get("update"),
        ) else {
            return out;
        };
        let tag = update.get("sessionUpdate").and_then(Value::as_str);
        let mut children = self
            .children
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if tag == Some("subagent_update") {
            // Without a child id the frame is malformed; the decoder counts it.
            let Some(id) = text(update, "sessionId") else {
                return out;
            };
            // A child announcing a grandchild is announced first.
            if let Some(parent) = children.get_mut(session) {
                announce(session, parent, None, &mut out.updates);
            }
            let child = children.entry(id.clone()).or_insert_with(|| Child {
                parent: HarnessSessionId::new(session),
                open: true,
                held: Some(Announcement {
                    name: None,
                    task: None,
                }),
            });
            if let Some(held) = &mut child.held {
                if let Some(title) = text(update, "title") {
                    held.name = Some(title);
                }
                if let Some(description) = text(update, "description") {
                    held.task = Some(description);
                }
            }
            if let Some(state) = ended(update.get("state")) {
                if child.open {
                    child.open = false;
                    announce(&id, child, None, &mut out.updates);
                    out.updates.push((
                        child.parent.clone(),
                        HarnessUpdateKind::SubagentStateUpdate { child: id, state },
                    ));
                }
            }
            out.consumed = true;
            return out;
        }
        let Some(child) = children
            .get_mut(session)
            .filter(|child| child.held.is_some())
        else {
            return out;
        };
        let incoming = tag == Some("session_message")
            && text(update, "senderSessionId").as_deref() != Some(session)
            && text(update, "recipientSessionId").is_none_or(|recipient| recipient == session);
        if incoming {
            announce(session, child, message_text(update), &mut out.updates);
            out.consumed = true;
        } else {
            announce(session, child, None, &mut out.updates);
        }
        out
    }
}

/// Publishes the held announcement, once.
fn announce(
    id: &str,
    child: &mut Child,
    prompt: Option<String>,
    updates: &mut Vec<(HarnessSessionId, HarnessUpdateKind)>,
) {
    if let Some(held) = child.held.take() {
        updates.push((
            child.parent.clone(),
            HarnessUpdateKind::SubagentSpawned {
                child: id.to_owned(),
                name: held.name,
                task: held.task,
                prompt,
            },
        ));
    }
}

/// The terminal state an `idle` snapshot reports. `running` and `unknown`
/// end nothing.
fn ended(state: Option<&Value>) -> Option<SubagentThreadState> {
    let state = state?;
    if state.get("state").and_then(Value::as_str) != Some("idle") {
        return None;
    }
    Some(match state.get("stopReason").and_then(Value::as_str) {
        Some("cancelled") => SubagentThreadState::Cancelled,
        Some("error" | "refusal") => SubagentThreadState::Failed,
        _ => SubagentThreadState::Completed,
    })
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn message_text(update: &Value) -> Option<String> {
    let text = update
        .get("content")
        .and_then(Value::as_array)?
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ROOT: &str = "root";
    const CHILD: &str = "root/Scout";

    fn frame(session: &str, update: Value) -> Value {
        json!({"sessionId": session, "update": update})
    }

    fn announce_frame(update: Value) -> Value {
        let mut update = update;
        update["sessionUpdate"] = json!("subagent_update");
        update["sessionId"] = json!(CHILD);
        frame(ROOT, update)
    }

    fn spawned(prompt: Option<&str>) -> (HarnessSessionId, HarnessUpdateKind) {
        (
            HarnessSessionId::new(ROOT),
            HarnessUpdateKind::SubagentSpawned {
                child: CHILD.into(),
                name: Some("Scout".into()),
                task: None,
                prompt: prompt.map(str::to_owned),
            },
        )
    }

    fn ended_as(state: SubagentThreadState) -> (HarnessSessionId, HarnessUpdateKind) {
        (
            HarnessSessionId::new(ROOT),
            HarnessUpdateKind::SubagentStateUpdate {
                child: CHILD.into(),
                state,
            },
        )
    }

    /// oh-my-pi's order (can1357/oh-my-pi#15043): announce, assignment,
    /// the child's first tool call, idle.
    #[test]
    fn the_assignment_becomes_the_prompt_of_one_announcement() {
        let sessions = SubagentSessions::default();
        let announced = sessions.translate(&announce_frame(
            json!({"title": "Scout", "state": {"state": "running"}}),
        ));
        assert_eq!(
            announced,
            Translation {
                updates: vec![],
                consumed: true
            }
        );

        let assignment = sessions.translate(&frame(
            CHILD,
            json!({
                "sessionUpdate": "session_message",
                "messageId": "m1",
                "senderSessionId": ROOT,
                "recipientSessionId": CHILD,
                "content": [{"type": "text", "text": "Find the flaky test"}]
            }),
        ));
        assert_eq!(
            assignment,
            Translation {
                updates: vec![spawned(Some("Find the flaky test"))],
                consumed: true
            }
        );

        let tool = frame(
            CHILD,
            json!({"sessionUpdate": "tool_call", "toolCallId": "tc-1"}),
        );
        assert_eq!(sessions.translate(&tool), Translation::default());

        let idle = sessions.translate(&announce_frame(
            json!({"state": {"state": "idle", "stopReason": "end_turn"}}),
        ));
        assert_eq!(
            idle,
            Translation {
                updates: vec![ended_as(SubagentThreadState::Completed)],
                consumed: true
            }
        );
    }

    #[test]
    fn a_first_frame_without_an_assignment_releases_the_announcement() {
        let sessions = SubagentSessions::default();
        sessions.translate(&announce_frame(json!({"title": "Scout"})));
        let tool = frame(
            CHILD,
            json!({"sessionUpdate": "tool_call", "toolCallId": "tc-1"}),
        );
        assert_eq!(
            sessions.translate(&tool),
            Translation {
                updates: vec![spawned(None)],
                consumed: false
            }
        );
        // The child's outgoing message is not its assignment.
        let outgoing = frame(
            CHILD,
            json!({"sessionUpdate": "session_message", "messageId": "m2", "senderSessionId": CHILD}),
        );
        assert_eq!(sessions.translate(&outgoing), Translation::default());
    }

    /// An upsert repeats; core's spawn and end must not.
    #[test]
    fn repeated_updates_announce_and_end_once() {
        let sessions = SubagentSessions::default();
        sessions.translate(&announce_frame(json!({"title": "Scout"})));
        sessions.translate(&announce_frame(json!({"state": {"state": "running"}})));
        let cancelled = sessions.translate(&announce_frame(
            json!({"state": {"state": "idle", "stopReason": "cancelled"}}),
        ));
        assert_eq!(
            cancelled.updates,
            vec![spawned(None), ended_as(SubagentThreadState::Cancelled)]
        );
        for state in [
            json!({"state": "running"}),
            json!({"state": "idle", "stopReason": "error"}),
        ] {
            let again = sessions.translate(&announce_frame(json!({"state": state})));
            assert_eq!(
                again,
                Translation {
                    updates: vec![],
                    consumed: true
                }
            );
        }
    }

    #[test]
    fn errors_fail_and_other_stops_complete() {
        for (reason, state) in [
            (json!("error"), SubagentThreadState::Failed),
            (json!("refusal"), SubagentThreadState::Failed),
            (json!("max_tokens"), SubagentThreadState::Completed),
            (Value::Null, SubagentThreadState::Completed),
        ] {
            let sessions = SubagentSessions::default();
            let idle = sessions.translate(&announce_frame(
                json!({"title": "Scout", "state": {"state": "idle", "stopReason": reason}}),
            ));
            assert_eq!(idle.updates, vec![spawned(None), ended_as(state)]);
        }
    }

    /// A grandchild is announced on its parent child's session.
    #[test]
    fn a_nested_child_is_announced_on_its_parent() {
        let sessions = SubagentSessions::default();
        sessions.translate(&announce_frame(json!({"title": "Scout"})));
        let nested = sessions.translate(&frame(
            CHILD,
            json!({"sessionUpdate": "subagent_update", "sessionId": "root/Helper", "title": "Helper",
                   "state": {"state": "idle", "stopReason": "end_turn"}}),
        ));
        assert!(nested.consumed);
        assert_eq!(
            nested.updates,
            vec![
                spawned(None),
                (
                    HarnessSessionId::new(CHILD),
                    HarnessUpdateKind::SubagentSpawned {
                        child: "root/Helper".into(),
                        name: Some("Helper".into()),
                        task: None,
                        prompt: None,
                    },
                ),
                (
                    HarnessSessionId::new(CHILD),
                    HarnessUpdateKind::SubagentStateUpdate {
                        child: "root/Helper".into(),
                        state: SubagentThreadState::Completed,
                    },
                ),
            ]
        );
    }

    #[test]
    fn frames_outside_the_rfd_pass_through() {
        let sessions = SubagentSessions::default();
        let chunk = frame(ROOT, json!({"sessionUpdate": "agent_message_chunk"}));
        assert_eq!(sessions.translate(&chunk), Translation::default());
        let malformed = frame(ROOT, json!({"sessionUpdate": "subagent_update"}));
        assert_eq!(sessions.translate(&malformed), Translation::default());
    }
}
