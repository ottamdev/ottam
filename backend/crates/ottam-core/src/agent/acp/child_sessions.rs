//! Subagent activity an agent projects onto its parent's session, tagged per
//! child ([`ChildSessionTags`]; OpenCode's `_meta["opencode/child-session"]`).
//!
//! Core rewrites it into the AIR native subagent shape (`sub:3`) that the
//! forwarder already threads. A child's frames move to the child's own
//! session id. Its first frame announces it with `subagent_spawned` on its
//! parent's session, named from the control call that launched it. That call's
//! end becomes `subagent_state_update`. The control call itself is held,
//! because the thread's anchor shows it. A control call that ends with no
//! child, or that is still open when the turn settles, is released as an
//! ordinary call.
//!
//! The tag names the child but not its control call. A child takes the
//! oldest unmatched control call on its parent's session whose task is the
//! child's title, and otherwise the oldest unmatched one. The agent's
//! tool-call ids keep their `<child>:` prefix: two children number their
//! calls alike.
use super::super::types::{ChildSessionTags, HarnessSessionId, HarnessUpdateKind};
use ottam_protocol::SubagentThreadState;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;

pub(super) struct ChildSessions {
    tags: ChildSessionTags,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Held control calls, oldest first.
    controls: Vec<Control>,
    /// Each child seen this process, by session id.
    children: HashMap<String, Child>,
}

struct Control {
    /// The session the turn runs on, which every child frame arrives on.
    root: HarnessSessionId,
    /// The session the call belongs to: the root, or a child's for a nested
    /// subagent.
    session: HarnessSessionId,
    call_id: String,
    /// The call's fields so far, merged from each update.
    payload: Value,
    child: Option<String>,
}

struct Child {
    root: HarnessSessionId,
    parent: HarnessSessionId,
    /// The tag's title, which prefixes the child's call titles and forms.
    title: Option<String>,
    open: bool,
    /// Ended before the last turn on its root settled.
    ended_before: bool,
}

/// What the tag says about one child.
struct Tag {
    id: String,
    parent: String,
    title: Option<String>,
}

impl ChildSessions {
    pub(super) fn new(tags: ChildSessionTags) -> Self {
        Self {
            tags,
            state: Mutex::default(),
        }
    }

    /// A new process: its sessions start over.
    pub(super) fn reset(&self) {
        *self.lock() = State::default();
    }

    /// Rewrites one update that arrived on `session`. `params` is the raw
    /// `session/update`, whose tag a decoded chunk no longer carries. The
    /// updates come back in order, each with the session it now belongs to.
    pub(super) fn translate(
        &self,
        session: HarnessSessionId,
        update: HarnessUpdateKind,
        params: Option<&Value>,
    ) -> Vec<(HarnessSessionId, HarnessUpdateKind)> {
        let mut state = self.lock();
        let mut out = Vec::new();
        let root = session.clone();
        let tag = params
            .and_then(|params| params.get("update"))
            .and_then(|update| self.tag(update));
        let (session, update) = match tag {
            Some(tag) => {
                self.spawn(&mut state, &root, &tag, &mut out);
                let child = HarnessSessionId::new(tag.id.clone());
                (child, strip_title(update, tag.title.as_deref()))
            }
            None => (session, update),
        };
        match update {
            HarnessUpdateKind::ToolCall(payload) if self.is_control(&payload) => {
                let Some(call_id) = call_id(&payload) else {
                    out.push((session, HarnessUpdateKind::ToolCall(payload)));
                    return out;
                };
                state.controls.push(Control {
                    root,
                    session,
                    call_id,
                    payload,
                    child: None,
                });
                let index = state.controls.len() - 1;
                self.end_if_terminal(&mut state, index, &mut out);
            }
            HarnessUpdateKind::ToolCallUpdate(payload) => {
                let held = call_id(&payload).and_then(|id| {
                    state
                        .controls
                        .iter()
                        .position(|control| control.session == session && control.call_id == id)
                });
                match held {
                    Some(index) => {
                        merge(&mut state.controls[index].payload, payload);
                        self.end_if_terminal(&mut state, index, &mut out);
                    }
                    None => out.push((session, HarnessUpdateKind::ToolCallUpdate(payload))),
                }
            }
            HarnessUpdateKind::PromptSettled => {
                settle(&mut state, &session, &mut out);
                out.push((session, HarnessUpdateKind::PromptSettled));
            }
            update => out.push((session, update)),
        }
        out
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The session a permission request belongs to. A child asks on its
    /// root's session with the tag on its tool call; the request moves to
    /// the child's session, title unprefixed like its calls.
    pub(super) fn permission_session(
        &self,
        session: HarnessSessionId,
        tool_call: &mut Value,
    ) -> HarnessSessionId {
        let Some(tag) = self.tag(tool_call) else {
            return session;
        };
        if let Some(title) = tag.title.as_deref() {
            strip(tool_call, &format!("{title}: "));
        }
        HarnessSessionId::new(tag.id)
    }

    /// The session a form belongs to. A child's form carries no tag: its
    /// tool call id has the child's `<child>:` prefix, and its message the
    /// child's title, which comes off like its calls'.
    pub(super) fn question_session(
        &self,
        session: HarnessSessionId,
        tool_call_id: Option<&str>,
        message: &mut String,
    ) -> HarnessSessionId {
        let state = self.lock();
        let Some((id, child)) = tool_call_id
            .and_then(|call| call.split_once(':'))
            .and_then(|(child, _)| state.children.get_key_value(child))
            .filter(|(_, child)| child.root == session)
        else {
            return session;
        };
        if let Some(stripped) = child
            .title
            .as_deref()
            .and_then(|title| message.strip_prefix(&format!("{title}: ")))
        {
            *message = stripped.to_owned();
        }
        HarnessSessionId::new(id.clone())
    }

    /// The tag on a value carrying `_meta`: an update or a tool call.
    fn tag(&self, value: &Value) -> Option<Tag> {
        let tag = value.get("_meta")?.get(self.tags.meta_key)?;
        let text = |key| {
            tag.get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        Some(Tag {
            id: text("id")?,
            parent: text("parentID")?,
            title: text("title"),
        })
    }

    fn is_control(&self, payload: &Value) -> bool {
        payload.get("name").and_then(Value::as_str) == Some(self.tags.control_tool)
    }

    /// Announces a child the first time a frame names it.
    fn spawn(
        &self,
        state: &mut State,
        root: &HarnessSessionId,
        tag: &Tag,
        out: &mut Vec<(HarnessSessionId, HarnessUpdateKind)>,
    ) {
        if state.children.contains_key(&tag.id) {
            return;
        }
        let parent = HarnessSessionId::new(tag.parent.clone());
        let unmatched = |control: &&mut Control| {
            control.child.is_none() && &control.root == root && control.session == parent
        };
        let input = |control: &Control, key: &str| {
            control
                .payload
                .pointer(&format!("/rawInput/{key}"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
        };
        let title = tag.title.as_deref();
        let index = state
            .controls
            .iter_mut()
            .position(|control| {
                unmatched(&control)
                    && title.is_some()
                    && input(control, self.tags.name).as_deref() == title
            })
            .or_else(|| {
                state
                    .controls
                    .iter_mut()
                    .position(|control| unmatched(&control))
            });
        let (name, prompt) = match index {
            Some(index) => {
                let control = &mut state.controls[index];
                control.child = Some(tag.id.clone());
                (
                    input(control, self.tags.name).or_else(|| tag.title.clone()),
                    input(control, self.tags.prompt),
                )
            }
            None => (tag.title.clone(), None),
        };
        state.children.insert(
            tag.id.clone(),
            Child {
                root: root.clone(),
                parent: parent.clone(),
                title: tag.title.clone(),
                open: true,
                ended_before: false,
            },
        );
        out.push((
            parent,
            HarnessUpdateKind::SubagentSpawned {
                child: tag.id.clone(),
                name,
                task: None,
                prompt,
            },
        ));
    }

    /// Ends a held control call once its status is terminal: its child's
    /// thread ends with it, or with no child it is released as a call.
    fn end_if_terminal(
        &self,
        state: &mut State,
        index: usize,
        out: &mut Vec<(HarnessSessionId, HarnessUpdateKind)>,
    ) {
        let payload = &state.controls[index].payload;
        let ended = match payload.get("status").and_then(Value::as_str) {
            Some("completed") => SubagentThreadState::Completed,
            // Stop fails every open call with "Cancelled" before the turn
            // settles; the thread was stopped, not failed.
            Some("failed")
                if payload.pointer("/rawOutput/error").and_then(Value::as_str)
                    == Some("Cancelled") =>
            {
                SubagentThreadState::Cancelled
            }
            Some("failed") => SubagentThreadState::Failed,
            _ => return,
        };
        let control = state.controls.remove(index);
        let child = control.child.clone().or_else(|| {
            // The call's result names its child: one whose frames found no
            // call to match.
            let named = control
                .payload
                .pointer("/rawOutput/metadata/sessionID")
                .and_then(Value::as_str)?;
            let unmatched = state.children.contains_key(named)
                && !state
                    .controls
                    .iter()
                    .any(|other| other.child.as_deref() == Some(named));
            unmatched.then(|| named.to_owned())
        });
        match child.and_then(|child| state.children.get_mut(&child).map(|entry| (child, entry))) {
            Some((child, entry)) if entry.open => {
                entry.open = false;
                out.push((
                    entry.parent.clone(),
                    HarnessUpdateKind::SubagentStateUpdate {
                        child,
                        state: ended,
                    },
                ));
            }
            Some(_) => {}
            None => out.push((
                control.session,
                HarnessUpdateKind::ToolCall(control.payload),
            )),
        }
    }
}

/// The turn on `root` settled: what it held is released, and a child still
/// open stopped with it.
fn settle(
    state: &mut State,
    root: &HarnessSessionId,
    out: &mut Vec<(HarnessSessionId, HarnessUpdateKind)>,
) {
    let (settled, held): (Vec<_>, Vec<_>) = std::mem::take(&mut state.controls)
        .into_iter()
        .partition(|control| &control.root == root);
    state.controls = held;
    for control in settled
        .into_iter()
        .filter(|control| control.child.is_none())
    {
        out.push((
            control.session,
            HarnessUpdateKind::ToolCall(control.payload),
        ));
    }
    // Children that ended in an earlier turn are forgotten now. Those ending
    // in this one stay a turn longer, so a late frame still finds its thread
    // instead of announcing it again.
    state
        .children
        .retain(|_, child| &child.root != root || child.open || !child.ended_before);
    for child in state.children.values_mut() {
        if &child.root == root && !child.open {
            child.ended_before = true;
        }
    }
    let mut open: Vec<_> = state
        .children
        .iter_mut()
        .filter(|(_, child)| &child.root == root && child.open)
        .collect();
    open.sort_by(|left, right| left.0.cmp(right.0));
    for (id, child) in open {
        child.open = false;
        out.push((
            child.parent.clone(),
            HarnessUpdateKind::SubagentStateUpdate {
                child: id.clone(),
                state: SubagentThreadState::Cancelled,
            },
        ));
    }
}

fn call_id(payload: &Value) -> Option<String> {
    payload
        .get("toolCallId")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// A later update's fields replace the held call's.
fn merge(held: &mut Value, update: Value) {
    if let (Some(held), Value::Object(update)) = (held.as_object_mut(), update) {
        held.extend(update);
    }
}

/// The agent prefixes a child's call titles with the child's title, which
/// its thread already shows.
fn strip_title(update: HarnessUpdateKind, title: Option<&str>) -> HarnessUpdateKind {
    let Some(prefix) = title.map(|title| format!("{title}: ")) else {
        return update;
    };
    match update {
        HarnessUpdateKind::ToolCall(mut payload) => {
            strip(&mut payload, &prefix);
            HarnessUpdateKind::ToolCall(payload)
        }
        HarnessUpdateKind::ToolCallUpdate(mut payload) => {
            strip(&mut payload, &prefix);
            HarnessUpdateKind::ToolCallUpdate(payload)
        }
        update => update,
    }
}

fn strip(payload: &mut Value, prefix: &str) {
    let stripped = payload
        .get("title")
        .and_then(Value::as_str)
        .and_then(|title| title.strip_prefix(prefix))
        .filter(|title| !title.is_empty())
        .map(str::to_owned);
    if let Some(title) = stripped {
        payload["title"] = Value::String(title);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TAGS: ChildSessionTags = ChildSessionTags {
        meta_key: "opencode/child-session",
        control_tool: "subagent",
        name: "description",
        prompt: "prompt",
    };

    fn id(value: &str) -> HarnessSessionId {
        HarnessSessionId::new(value.to_owned())
    }

    /// Replays a recorded `session/update` stream through the normal decode,
    /// then the translator.
    fn replay(
        children: &ChildSessions,
        frames: &[Value],
    ) -> Vec<(HarnessSessionId, HarnessUpdateKind)> {
        frames
            .iter()
            .filter(|frame| frame["method"] == "session/update")
            .flat_map(|frame| {
                let (session, update) =
                    super::super::protocol::decode_session_update("opencode", &frame["params"])
                        .expect("decodes");
                children.translate(session, update, Some(&frame["params"]))
            })
            .collect()
    }

    fn update(session: &str, update: Value) -> Value {
        json!({"method": "session/update", "params": {"sessionId": session, "update": update}})
    }

    fn tag(child: &str, parent: &str, title: &str) -> Value {
        json!({"opencode/child-session": {"id": child, "parentID": parent, "depth": 1, "title": title}})
    }

    fn control(call: &str, status: &str, description: &str) -> Value {
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": call, "status": status,
            "rawInput": {"agent": "general", "description": description, "prompt": format!("Do {description}")}
        })
    }

    fn pending(call: &str) -> Value {
        json!({"sessionUpdate": "tool_call", "toolCallId": call, "name": "subagent", "title": "subagent", "kind": "think", "status": "pending", "rawInput": {}})
    }

    #[test]
    fn the_recorded_session_threads_each_child_under_its_own_call() {
        let frames: Vec<Value> =
            serde_json::from_str(include_str!("../fixtures/opencode-2.2.5.json")).unwrap();
        let children = ChildSessions::new(TAGS);
        let out = replay(&children, &frames);
        let root = id("ses_e6060458dffew6cdg4C305B8t6");
        let spawned: Vec<_> = out
            .iter()
            .filter_map(|(session, update)| match update {
                HarnessUpdateKind::SubagentSpawned {
                    child,
                    name,
                    prompt,
                    ..
                } => Some((
                    session.clone(),
                    child.clone(),
                    name.clone(),
                    prompt.is_some(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            spawned,
            [
                (
                    root.clone(),
                    "ses_e606010a4ffe2CJvcNSOY2oYmg".to_owned(),
                    Some("Read a.txt".to_owned()),
                    true
                ),
                (
                    root.clone(),
                    "ses_e605feb4effe8mnrPQQ4Q74tSO".to_owned(),
                    Some("Read b.txt".to_owned()),
                    true
                ),
            ]
        );
        // The control calls never reach the transcript; their ends do.
        let controls = ["functions.subagent:1", "functions.subagent:2"];
        assert!(!out.iter().any(|(_, update)| matches!(
            update,
            HarnessUpdateKind::ToolCall(payload) | HarnessUpdateKind::ToolCallUpdate(payload)
                if controls.contains(&payload["toolCallId"].as_str().unwrap_or_default())
        )));
        let ended: Vec<_> = out
            .iter()
            .filter_map(|(_, update)| match update {
                HarnessUpdateKind::SubagentStateUpdate { child, state } => {
                    Some((child.as_str(), *state))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            ended,
            [
                (
                    "ses_e606010a4ffe2CJvcNSOY2oYmg",
                    SubagentThreadState::Completed
                ),
                (
                    "ses_e605feb4effe8mnrPQQ4Q74tSO",
                    SubagentThreadState::Completed
                ),
            ]
        );
        // Each child's own frames moved to its session, titles unprefixed.
        let child = id("ses_e605feb4effe8mnrPQQ4Q74tSO");
        let titles: Vec<_> = out
            .iter()
            .filter(|(session, _)| session == &child)
            .filter_map(|(_, update)| match update {
                HarnessUpdateKind::ToolCall(payload) => payload["title"].as_str(),
                _ => None,
            })
            .collect();
        assert_eq!(titles, ["shell"]);
        assert!(out.iter().any(|(session, update)| session == &child
            && matches!(update, HarnessUpdateKind::AgentMessageChunk(text, _) if text.starts_with("The file `b.txt` contains"))));
        // The parent's own text stays on the root.
        assert!(out.iter().any(|(session, update)| session == &root
            && matches!(update, HarnessUpdateKind::AgentMessageChunk(text, _) if text.starts_with("I'll start by listing"))));
    }

    #[test]
    fn a_child_takes_the_call_whose_task_is_its_title() {
        let children = ChildSessions::new(TAGS);
        let frames = [
            update("root", pending("first")),
            update("root", pending("second")),
            update("root", control("first", "in_progress", "Alpha")),
            update("root", control("second", "in_progress", "Beta")),
            update(
                "root",
                json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "b"}, "_meta": tag("child-b", "root", "Beta")}),
            ),
            update("root", control("second", "completed", "Beta")),
        ];
        let out = replay(&children, &frames);
        assert!(matches!(
            &out[0],
            (session, HarnessUpdateKind::SubagentSpawned { child, name, prompt, .. })
                if session == &id("root") && child == "child-b" && name.as_deref() == Some("Beta") && prompt.as_deref() == Some("Do Beta")
        ));
        assert!(
            matches!(&out[1], (session, HarnessUpdateKind::AgentMessageChunk(text, _)) if session == &id("child-b") && text == "b")
        );
        assert!(matches!(
            &out[2],
            (_, HarnessUpdateKind::SubagentStateUpdate { child, state: SubagentThreadState::Completed }) if child == "child-b"
        ));
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn a_call_with_no_child_is_released_when_it_ends_or_the_turn_settles() {
        let children = ChildSessions::new(TAGS);
        let out = replay(
            &children,
            &[
                update("root", pending("refused")),
                update("root", control("refused", "failed", "Alpha")),
                update("root", pending("open")),
            ],
        );
        assert!(matches!(
            out.as_slice(),
            [(session, HarnessUpdateKind::ToolCall(payload))]
                if session == &id("root") && payload["status"] == "failed" && payload["rawInput"]["description"] == "Alpha"
        ));
        let settled = children.translate(id("root"), HarnessUpdateKind::PromptSettled, None);
        assert!(matches!(
            settled.as_slice(),
            [
                (_, HarnessUpdateKind::ToolCall(payload)),
                (_, HarnessUpdateKind::PromptSettled),
            ] if payload["toolCallId"] == "open"
        ));
    }

    #[test]
    fn a_child_still_open_when_the_turn_settles_is_cancelled() {
        let children = ChildSessions::new(TAGS);
        replay(
            &children,
            &[
                update("root", pending("call")),
                update(
                    "root",
                    json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "x"}, "_meta": tag("child", "root", "Alpha")}),
                ),
            ],
        );
        let settled = children.translate(id("root"), HarnessUpdateKind::PromptSettled, None);
        assert!(matches!(
            settled.as_slice(),
            [
                (session, HarnessUpdateKind::SubagentStateUpdate { child, state: SubagentThreadState::Cancelled }),
                (_, HarnessUpdateKind::PromptSettled),
            ] if session == &id("root") && child == "child"
        ));
    }

    #[test]
    fn stop_cancels_a_running_childs_thread() {
        let children = ChildSessions::new(TAGS);
        let out = replay(
            &children,
            &[
                update("root", pending("call")),
                update(
                    "root",
                    json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "x"}, "_meta": tag("child", "root", "Alpha")}),
                ),
                update(
                    "root",
                    json!({"sessionUpdate": "tool_call_update", "toolCallId": "call", "status": "failed", "rawOutput": {"error": "Cancelled"}}),
                ),
            ],
        );
        assert!(matches!(
            out.last(),
            Some((_, HarnessUpdateKind::SubagentStateUpdate { child, state: SubagentThreadState::Cancelled })) if child == "child"
        ));
    }

    #[test]
    fn a_childs_permission_request_moves_to_its_session() {
        let children = ChildSessions::new(TAGS);
        let mut tool_call = json!({"toolCallId": "child:edit", "title": "Alpha: edit", "_meta": tag("child", "root", "Alpha")});
        assert_eq!(
            children.permission_session(id("root"), &mut tool_call),
            id("child")
        );
        assert_eq!(tool_call["title"], "edit");
        let mut own = json!({"toolCallId": "edit", "title": "edit"});
        assert_eq!(
            children.permission_session(id("root"), &mut own),
            id("root")
        );
    }

    #[test]
    fn a_childs_form_moves_to_its_session() {
        let children = ChildSessions::new(TAGS);
        replay(
            &children,
            &[update(
                "root",
                json!({"sessionUpdate": "tool_call", "toolCallId": "child:read", "title": "Alpha: read",
                    "kind": "read", "status": "pending", "_meta": tag("child", "root", "Alpha")}),
            )],
        );
        let mut message = "Alpha: Questions".to_string();
        assert_eq!(
            children.question_session(id("root"), Some("child:ask"), &mut message),
            id("child")
        );
        assert_eq!(message, "Questions");
        let mut own = "Questions".to_string();
        assert_eq!(
            children.question_session(id("root"), Some("ask"), &mut own),
            id("root")
        );
        assert_eq!(
            children.question_session(id("root"), Some("stranger:ask"), &mut own),
            id("root")
        );
        assert_eq!(
            children.question_session(id("root"), None, &mut own),
            id("root")
        );
    }

    #[test]
    fn a_nested_child_is_announced_on_its_parent_childs_session() {
        let children = ChildSessions::new(TAGS);
        let mut nested = pending("outer:inner-call");
        nested["_meta"] = tag("outer", "root", "Outer");
        let out = replay(
            &children,
            &[
                update("root", pending("outer-call")),
                update("root", nested),
                update(
                    "root",
                    json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "deep"}, "_meta": {"opencode/child-session": {"id": "inner", "parentID": "outer", "depth": 2, "title": "Inner"}}}),
                ),
            ],
        );
        let spawned: Vec<_> = out
            .iter()
            .filter_map(|(session, update)| match update {
                HarnessUpdateKind::SubagentSpawned { child, .. } => {
                    Some((session.0.as_str(), child.as_str()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(spawned, [("root", "outer"), ("outer", "inner")]);
        assert!(
            matches!(out.last(), Some((session, HarnessUpdateKind::AgentMessageChunk(..))) if session == &id("inner"))
        );
    }
}
