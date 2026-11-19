//! Subagent activity an agent reports on its parent's session, tagged per
//! child ([`ChildSessionTags`]; OpenCode's `opencode/session/child_update`,
//! whose frames carry `_meta["opencode/child-session"]`).
//!
//! Core rewrites it into the AIR native subagent shape (`sub:3`) that the
//! forwarder already threads. A thread's state follows the child's own run
//! status, not the control call that launched it: OpenCode's control call
//! completes as soon as the child starts, and the child outlives the turn.
//!
//! - `created` (or, failing that, the child's first frame) announces the
//!   child with `subagent_spawned` on its parent's session, named from the
//!   control call that launched it. That call is held and never shown, since
//!   the thread's anchor shows it.
//! - `completed`, `failed`, and `interrupted` end the run with
//!   `subagent_state_update`.
//! - `running` after an end is a resumed run, announced as
//!   `<child>:generation:<n>` in the same thread. Its prompt is the parent's
//!   follow-up: a control call naming the child in its target field, held
//!   like a launch; one the agent refuses is released with its error. A
//!   follow-up to a running child joins that run, so it is released as an
//!   ordinary call for the registry's follow-up stamp.
//! - A control call that ends naming no child, or that is still open when
//!   the turn settles, is released as an ordinary call.
//!
//! A child's frames move to its current run's session. The tag names the
//! child but not its control call: a child takes the oldest unmatched launch
//! on its parent's session whose task is the child's title, and otherwise
//! the oldest unmatched one. The agent's tool-call ids keep their `<child>:`
//! prefix: two children number their calls alike.
use super::super::types::{ChildSessionTags, HarnessSessionId, HarnessUpdateKind};
use ottam_protocol::SubagentThreadState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

pub(super) struct ChildSessions {
    tags: ChildSessionTags,
    state: Mutex<State>,
}

/// One `opencode/session/child_update`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ChildUpdate {
    pub root_session_id: String,
    pub child_session_id: String,
    pub parent_session_id: String,
    pub depth: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(flatten)]
    pub event: ChildEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ChildEvent {
    Status {
        status: ChildStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<Value>,
    },
    /// One of the child's own `session/update`s, tagged.
    Update { update: Value },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ChildStatus {
    Created,
    Running,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Default)]
struct State {
    /// Held control calls, oldest first.
    controls: Vec<Control>,
    /// Each child seen this process, by session id. Never forgotten: a
    /// follow-up may resume a child at any later turn.
    children: HashMap<String, Child>,
    /// Roots Stop is closing, whose own output is dropped (`mute`).
    muted: HashSet<HarnessSessionId>,
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
    /// The child it launched, or the finished child it resumes.
    child: Option<String>,
}

struct Child {
    root: HarnessSessionId,
    parent: HarnessSessionId,
    /// The tag's title, which prefixes the child's call titles and forms.
    title: Option<String>,
    /// The thread's name, from the launching call or the title.
    name: Option<String>,
    /// Runs announced so far; the first is the child's own id.
    runs: u32,
    /// The current run has not ended.
    open: bool,
    /// The parent's follow-up waiting for the run it starts.
    next_prompt: Option<String>,
}

impl Child {
    /// The session the current run's frames and state belong to.
    fn key(&self, id: &str) -> String {
        run_key(id, self.runs)
    }
}

fn run_key(id: &str, run: u32) -> String {
    if run <= 1 {
        id.to_owned()
    } else {
        format!("{id}:generation:{run}")
    }
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

    /// The notification method carrying child status and frames.
    pub(super) fn updates_method(&self) -> &'static str {
        self.tags.updates
    }

    /// A new process: its sessions start over.
    pub(super) fn reset(&self) {
        *self.lock() = State::default();
    }

    /// The agent's own ids of `root`'s children whose current run has not
    /// ended, nested ones included, deepest first.
    pub(super) fn open_children(&self, root: &HarnessSessionId) -> Vec<String> {
        fn depth<'a>(children: &'a HashMap<String, Child>, mut id: &'a str) -> usize {
            let mut depth = 0;
            while let Some(child) = children.get(id) {
                depth += 1;
                id = &child.parent.0;
            }
            depth
        }
        let state = self.lock();
        let mut open: Vec<(usize, &String)> = state
            .children
            .iter()
            .filter(|(_, child)| child.open && child.root == *root)
            .map(|(id, _)| (depth(&state.children, id), id))
            .collect();
        open.sort();
        open.into_iter().rev().map(|(_, id)| id.clone()).collect()
    }

    /// Whether `root` has had a child this process. A child's report wakes
    /// `root` even once the child has ended, so Stop closes such a root.
    pub(super) fn has_children(&self, root: &HarnessSessionId) -> bool {
        self.lock()
            .children
            .values()
            .any(|child| child.root == *root)
    }

    /// While Stop closes `root`, the turn a child's report wakes on it is
    /// not the stopped run's: its own output is dropped. Its children's
    /// frames and ends still pass.
    pub(super) fn mute(&self, root: &HarnessSessionId, muted: bool) {
        let mut state = self.lock();
        if muted {
            state.muted.insert(root.clone());
        } else {
            state.muted.remove(root);
        }
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
                let child = current_key(&state, &tag.id);
                (
                    HarnessSessionId::new(child),
                    strip_title(update, tag.title.as_deref()),
                )
            }
            None if state.muted.contains(&session) => return out,
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
                self.advance(&mut state, index, &mut out);
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
                        self.advance(&mut state, index, &mut out);
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

    /// Rewrites one child update notification. `decode` turns a child frame,
    /// wrapped as a root `session/update`, into the update core reads.
    pub(super) fn child_update(
        &self,
        notification: ChildUpdate,
        decode: impl FnOnce(&Value) -> Option<(HarnessSessionId, HarnessUpdateKind)>,
    ) -> Vec<(HarnessSessionId, HarnessUpdateKind)> {
        let root = HarnessSessionId::new(notification.root_session_id.clone());
        let status = match notification.event {
            ChildEvent::Update { update } => {
                let params = json!({"sessionId": root.0, "update": update});
                return match decode(&params) {
                    Some((session, update)) => self.translate(session, update, Some(&params)),
                    None => Vec::new(),
                };
            }
            ChildEvent::Status { status, .. } => status,
        };
        let tag = Tag {
            id: notification.child_session_id,
            parent: notification.parent_session_id,
            title: notification.title.filter(|title| !title.is_empty()),
        };
        let mut state = self.lock();
        let mut out = Vec::new();
        let known = state.children.contains_key(&tag.id);
        match status {
            ChildStatus::Created | ChildStatus::Running if !known => {
                self.spawn(&mut state, &root, &tag, &mut out);
            }
            ChildStatus::Created => {}
            ChildStatus::Running => {
                let parent = parent_key(&state, &tag.parent);
                let child = state.children.get_mut(&tag.id).expect("known");
                if !child.open {
                    child.open = true;
                    child.runs += 1;
                    out.push((
                        parent,
                        HarnessUpdateKind::SubagentSpawned {
                            child: child.key(&tag.id),
                            name: child.name.clone(),
                            task: None,
                            prompt: child.next_prompt.take(),
                        },
                    ));
                }
            }
            ChildStatus::Completed | ChildStatus::Failed | ChildStatus::Interrupted => {
                if let Some(child) = state.children.get_mut(&tag.id).filter(|child| child.open) {
                    child.open = false;
                    out.push((
                        child.parent.clone(),
                        HarnessUpdateKind::SubagentStateUpdate {
                            child: child.key(&tag.id),
                            state: match status {
                                ChildStatus::Failed => SubagentThreadState::Failed,
                                ChildStatus::Interrupted => SubagentThreadState::Cancelled,
                                _ => SubagentThreadState::Completed,
                            },
                        },
                    ));
                }
            }
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
    /// the child's current run, title unprefixed like its calls.
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
        HarnessSessionId::new(current_key(&self.lock(), &tag.id))
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
        HarnessSessionId::new(child.key(id))
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

    fn input(&self, control: &Control, key: &str) -> Option<String> {
        control
            .payload
            .pointer(&format!("/rawInput/{key}"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    }

    /// Announces a child the first time its status or a frame names it.
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
        // A launch, not a follow-up naming some other child.
        let unmatched = |control: &Control| {
            control.child.is_none()
                && &control.root == root
                && control.session == parent
                && self.input(control, self.tags.target).is_none()
        };
        let title = tag.title.as_deref();
        let index = state
            .controls
            .iter()
            .position(|control| {
                unmatched(control)
                    && title.is_some()
                    && self.input(control, self.tags.name).as_deref() == title
            })
            .or_else(|| state.controls.iter().position(unmatched));
        let (name, prompt) = match index {
            Some(index) => {
                let control = &mut state.controls[index];
                control.child = Some(tag.id.clone());
                (
                    self.input(control, self.tags.name)
                        .or_else(|| tag.title.clone()),
                    self.input(control, self.tags.prompt),
                )
            }
            None => (tag.title.clone(), None),
        };
        let announced_on = parent_key(state, &tag.parent);
        state.children.insert(
            tag.id.clone(),
            Child {
                root: root.clone(),
                parent: announced_on.clone(),
                title: tag.title.clone(),
                name: name.clone(),
                runs: 1,
                open: true,
                next_prompt: None,
            },
        );
        out.push((
            announced_on,
            HarnessUpdateKind::SubagentSpawned {
                child: tag.id.clone(),
                name,
                task: None,
                prompt,
            },
        ));
    }

    /// Moves a held control call on after an update: a follow-up finds its
    /// child, and an ended call is consumed or released.
    fn advance(
        &self,
        state: &mut State,
        index: usize,
        out: &mut Vec<(HarnessSessionId, HarnessUpdateKind)>,
    ) {
        let control = &state.controls[index];
        if control.child.is_none() {
            if let Some(target) = self.input(control, self.tags.target) {
                let prompt = self.input(control, self.tags.prompt);
                match state.children.get_mut(&target) {
                    // Joins the running child's run: shown, and stamped.
                    Some(child) if child.open => {
                        let control = state.controls.remove(index);
                        out.push((control.session, HarnessUpdateKind::ToolCall(control.payload)));
                        return;
                    }
                    Some(child) => {
                        child.next_prompt = prompt;
                        state.controls[index].child = Some(target);
                    }
                    // A child this process never saw: shown as a call.
                    None => {}
                }
            }
        }
        let control = &state.controls[index];
        let status = control.payload.get("status").and_then(Value::as_str);
        if !matches!(status, Some("completed" | "failed")) {
            return;
        }
        let failed = status == Some("failed");
        let control = state.controls.remove(index);
        // A follow-up the agent refused starts no run: it shows with its
        // error, and its prompt is not the next run's.
        if failed && self.input(&control, self.tags.target).is_some() {
            if let Some(child) = control
                .child
                .as_deref()
                .and_then(|child| state.children.get_mut(child))
            {
                child.next_prompt = None;
            }
            out.push((
                control.session,
                HarnessUpdateKind::ToolCall(control.payload),
            ));
            return;
        }
        // The call's result names its child, for a launch whose child's
        // status found no call to match.
        let named = control
            .payload
            .pointer("/rawOutput/metadata/sessionID")
            .and_then(Value::as_str)
            .filter(|named| state.children.contains_key(*named));
        if control.child.is_none() && named.is_none() {
            out.push((
                control.session,
                HarnessUpdateKind::ToolCall(control.payload),
            ));
        }
    }
}

/// The session a child's thread is announced on: its parent's current run,
/// which for a nested child is a resumed run of the parent subagent.
fn parent_key(state: &State, parent: &str) -> HarnessSessionId {
    HarnessSessionId::new(current_key(state, parent))
}

/// `id`'s current run, or `id` itself when it is not a child.
fn current_key(state: &State, id: &str) -> String {
    state
        .children
        .get(id)
        .map_or_else(|| id.to_owned(), |child| child.key(id))
}

/// The turn on `root` settled: a control call it still holds with no child
/// is released. Children outlive the turn; only their status ends them.
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
        target: "sessionID",
        updates: "opencode/session/child_update",
    };

    fn id(value: &str) -> HarnessSessionId {
        HarnessSessionId::new(value.to_owned())
    }

    fn decode(params: &Value) -> Option<(HarnessSessionId, HarnessUpdateKind)> {
        super::super::protocol::decode_session_update("opencode", params)
    }

    /// Replays a recorded stream through the normal decode, then the
    /// translator: `session/update`s and child update notifications.
    fn replay(
        children: &ChildSessions,
        frames: &[Value],
    ) -> Vec<(HarnessSessionId, HarnessUpdateKind)> {
        frames
            .iter()
            .flat_map(|frame| match frame["method"].as_str() {
                Some("session/update") => {
                    let (session, update) = decode(&frame["params"]).expect("decodes");
                    children.translate(session, update, Some(&frame["params"]))
                }
                Some(method) if method == TAGS.updates => children.child_update(
                    serde_json::from_value(frame["params"].clone()).expect("child update"),
                    decode,
                ),
                _ => Vec::new(),
            })
            .collect()
    }

    fn update(session: &str, update: Value) -> Value {
        json!({"method": "session/update", "params": {"sessionId": session, "update": update}})
    }

    fn tag(child: &str, parent: &str, title: &str) -> Value {
        json!({"opencode/child-session": {"id": child, "parentID": parent, "depth": 1, "title": title}})
    }

    fn status(root: &str, child: &str, parent: &str, title: &str, status: &str) -> Value {
        json!({"method": TAGS.updates, "params": {
            "rootSessionId": root, "childSessionId": child, "parentSessionId": parent,
            "depth": 1, "title": title, "type": "status", "status": status}})
    }

    fn child_frame(root: &str, child: &str, title: &str, update: Value) -> Value {
        let mut update = update;
        update["_meta"] = tag(child, root, title);
        json!({"method": TAGS.updates, "params": {
            "rootSessionId": root, "childSessionId": child, "parentSessionId": root,
            "depth": 1, "title": title, "type": "update", "update": update}})
    }

    fn control(call: &str, status: &str, input: Value) -> Value {
        json!({"sessionUpdate": "tool_call_update", "toolCallId": call, "status": status, "rawInput": input})
    }

    fn launched(call: &str, child: &str) -> Value {
        json!({"sessionUpdate": "tool_call_update", "toolCallId": call, "status": "completed",
            "rawOutput": {"metadata": {"sessionID": child, "status": "running", "truncated": false}}})
    }

    fn pending(call: &str) -> Value {
        json!({"sessionUpdate": "tool_call", "toolCallId": call, "name": "subagent", "title": "subagent", "kind": "think", "status": "pending", "rawInput": {}})
    }

    fn task(description: &str) -> Value {
        json!({"agent": "general", "description": description, "prompt": format!("Do {description}")})
    }

    fn text(update: Value) -> Value {
        json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": update}})
    }

    fn spawned(out: &[(HarnessSessionId, HarnessUpdateKind)]) -> Vec<(String, String, Option<String>)> {
        out.iter()
            .filter_map(|(session, update)| match update {
                HarnessUpdateKind::SubagentSpawned { child, prompt, .. } => {
                    Some((session.0.clone(), child.clone(), prompt.clone()))
                }
                _ => None,
            })
            .collect()
    }

    fn ended(out: &[(HarnessSessionId, HarnessUpdateKind)]) -> Vec<(String, SubagentThreadState)> {
        out.iter()
            .filter_map(|(_, update)| match update {
                HarnessUpdateKind::SubagentStateUpdate { child, state } => {
                    Some((child.clone(), *state))
                }
                _ => None,
            })
            .collect()
    }

    fn shown_calls(out: &[(HarnessSessionId, HarnessUpdateKind)]) -> Vec<String> {
        out.iter()
            .filter_map(|(_, update)| match update {
                HarnessUpdateKind::ToolCall(payload) | HarnessUpdateKind::ToolCallUpdate(payload) => {
                    payload["toolCallId"].as_str().map(str::to_owned)
                }
                _ => None,
            })
            .collect()
    }

    /// The pinned recording: two parallel children that outlive the turn, a
    /// follow-up that resumes one, and a child still running when its turn
    /// is cancelled, which OpenCode lets finish.
    #[test]
    fn the_recorded_session_threads_each_child_and_its_resumed_run() {
        let frames: Vec<Value> =
            serde_json::from_str(include_str!("../fixtures/opencode-2.3.0.json")).unwrap();
        let children = ChildSessions::new(TAGS);
        let out = replay(&children, &frames);
        let root = "ses_e1374839fffeQbIInweYahf3uM";
        let a = "ses_e13746e4affeQADhhGGwBstHg0";
        let b = "ses_e13746e49ffe118DJnrdJze2ks";
        let b2 = format!("{b}:generation:2");
        let story_root = "ses_e1371fc39ffefPd7z1lsUFMsW4";
        let story = "ses_e1371e824ffecCo0klT0ZA6G8v";
        let announced: Vec<_> = out
            .iter()
            .filter_map(|(session, update)| match update {
                HarnessUpdateKind::SubagentSpawned {
                    child,
                    name,
                    prompt,
                    ..
                } => Some((
                    session.0.as_str(),
                    child.as_str(),
                    name.as_deref(),
                    prompt.as_deref().is_some_and(|prompt| prompt.starts_with("Follow-up:")),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            announced,
            [
                (root, a, Some("Read a.txt"), false),
                (root, b, Some("Read b.txt"), false),
                (root, b2.as_str(), Some("Read b.txt"), true),
                (story_root, story, Some("Story"), false),
            ]
        );
        assert_eq!(
            ended(&out),
            [
                (b.to_owned(), SubagentThreadState::Completed),
                (a.to_owned(), SubagentThreadState::Completed),
                (b2.clone(), SubagentThreadState::Completed),
                (story.to_owned(), SubagentThreadState::Completed),
            ]
        );
        // No control call reaches the transcript.
        let controls: Vec<_> = frames
            .iter()
            .filter(|frame| frame.pointer("/params/update/name") == Some(&json!("subagent")))
            .filter_map(|frame| frame.pointer("/params/update/toolCallId")?.as_str())
            .collect();
        assert_eq!(controls.len(), 4);
        assert!(shown_calls(&out)
            .iter()
            .all(|call| !controls.contains(&call.as_str())));
        // Each run's frames moved to it, titles unprefixed.
        let calls = |session: &str| -> Vec<String> {
            out.iter()
                .filter(|(owner, _)| owner.0 == session)
                .filter_map(|(_, update)| match update {
                    HarnessUpdateKind::ToolCall(payload) => {
                        payload["title"].as_str().map(str::to_owned)
                    }
                    _ => None,
                })
                .collect()
        };
        assert_eq!(calls(b), ["read"]);
        assert_eq!(calls(&b2), ["shell"]);
        // The parent's own text stays on the root.
        assert!(out.iter().any(|(session, update)| session.0 == root
            && matches!(update, HarnessUpdateKind::AgentMessageChunk(..))));
    }

    /// The launch, then the child's status before the call completes, then
    /// its frames after the turn settles: one thread, ended by the child.
    #[test]
    fn a_background_child_outlives_its_turn_and_ends_on_its_status() {
        let children = ChildSessions::new(TAGS);
        let mut out = replay(
            &children,
            &[
                update("root", pending("call")),
                update("root", control("call", "in_progress", task("Alpha"))),
                status("root", "child", "root", "Alpha", "created"),
                update("root", launched("call", "child")),
                status("root", "child", "root", "Alpha", "running"),
            ],
        );
        out.extend(children.translate(id("root"), HarnessUpdateKind::PromptSettled, None));
        out.extend(replay(
            &children,
            &[
                child_frame("root", "child", "Alpha", text(json!("a"))),
                status("root", "child", "root", "Alpha", "completed"),
            ],
        ));
        assert_eq!(
            spawned(&out),
            [("root".to_owned(), "child".to_owned(), Some("Do Alpha".to_owned()))]
        );
        assert!(shown_calls(&out).is_empty(), "{out:?}");
        assert!(out.iter().any(|(session, update)| session == &id("child")
            && matches!(update, HarnessUpdateKind::AgentMessageChunk(text, _) if text == "a")));
        assert_eq!(
            ended(&out),
            [("child".to_owned(), SubagentThreadState::Completed)]
        );
    }

    #[test]
    fn a_child_takes_the_call_whose_task_is_its_title() {
        let children = ChildSessions::new(TAGS);
        let out = replay(
            &children,
            &[
                update("root", pending("first")),
                update("root", pending("second")),
                update("root", control("first", "in_progress", task("Alpha"))),
                update("root", control("second", "in_progress", task("Beta"))),
                status("root", "child-b", "root", "Beta", "created"),
                update("root", launched("second", "child-b")),
            ],
        );
        assert_eq!(
            spawned(&out),
            [("root".to_owned(), "child-b".to_owned(), Some("Do Beta".to_owned()))]
        );
        // `first` is still held, unmatched; it is released when the turn settles.
        let settled = children.translate(id("root"), HarnessUpdateKind::PromptSettled, None);
        assert_eq!(shown_calls(&settled), ["first"]);
    }

    /// A follow-up to a finished child resumes it: a second run in the same
    /// thread, whose prompt is the follow-up's.
    #[test]
    fn a_follow_up_to_a_finished_child_is_its_next_run() {
        let children = ChildSessions::new(TAGS);
        let out = replay(
            &children,
            &[
                update("root", pending("launch")),
                update("root", control("launch", "in_progress", task("Alpha"))),
                status("root", "child", "root", "Alpha", "created"),
                update("root", launched("launch", "child")),
                status("root", "child", "root", "Alpha", "running"),
                status("root", "child", "root", "Alpha", "completed"),
                update("root", pending("again")),
                update(
                    "root",
                    control(
                        "again",
                        "in_progress",
                        json!({"description": "Alpha", "prompt": "Count them", "sessionID": "child"}),
                    ),
                ),
                update("root", launched("again", "child")),
                status("root", "child", "root", "Alpha", "running"),
                child_frame("root", "child", "Alpha", text(json!("3"))),
                status("root", "child", "root", "Alpha", "completed"),
            ],
        );
        assert_eq!(
            spawned(&out),
            [
                ("root".to_owned(), "child".to_owned(), Some("Do Alpha".to_owned())),
                (
                    "root".to_owned(),
                    "child:generation:2".to_owned(),
                    Some("Count them".to_owned())
                ),
            ]
        );
        assert!(shown_calls(&out).is_empty(), "{out:?}");
        assert!(out.iter().any(|(session, update)| session == &id("child:generation:2")
            && matches!(update, HarnessUpdateKind::AgentMessageChunk(text, _) if text == "3")));
        assert_eq!(
            ended(&out),
            [
                ("child".to_owned(), SubagentThreadState::Completed),
                ("child:generation:2".to_owned(), SubagentThreadState::Completed),
            ]
        );
    }

    /// A follow-up to a running child joins its run, so the call is shown
    /// for the follow-up stamp and no run starts.
    #[test]
    fn a_follow_up_to_a_running_child_is_released_as_a_call() {
        let children = ChildSessions::new(TAGS);
        let out = replay(
            &children,
            &[
                update("root", pending("launch")),
                update("root", control("launch", "in_progress", task("Alpha"))),
                status("root", "child", "root", "Alpha", "created"),
                update("root", launched("launch", "child")),
                update("root", pending("nudge")),
                update(
                    "root",
                    control(
                        "nudge",
                        "in_progress",
                        json!({"description": "Alpha", "prompt": "Faster", "sessionID": "child"}),
                    ),
                ),
                update("root", launched("nudge", "child")),
            ],
        );
        assert_eq!(spawned(&out).len(), 1);
        assert_eq!(shown_calls(&out), ["nudge", "nudge"]);
        assert!(matches!(
            out.iter().find(|(_, update)| matches!(update, HarnessUpdateKind::ToolCall(_))),
            Some((_, HarnessUpdateKind::ToolCall(payload)))
                if payload["name"] == "subagent" && payload["rawInput"]["sessionID"] == "child"
        ));
    }

    /// A follow-up the agent refuses starts no run: it shows with its error,
    /// and a later resume does not take its prompt.
    #[test]
    fn a_refused_follow_up_is_shown_and_its_prompt_dropped() {
        let children = ChildSessions::new(TAGS);
        let out = replay(
            &children,
            &[
                update("root", pending("launch")),
                update("root", control("launch", "in_progress", task("Alpha"))),
                status("root", "child", "root", "Alpha", "created"),
                update("root", launched("launch", "child")),
                status("root", "child", "root", "Alpha", "completed"),
                update("root", pending("again")),
                update(
                    "root",
                    control(
                        "again",
                        "in_progress",
                        json!({"description": "Alpha", "prompt": "Count them", "sessionID": "child"}),
                    ),
                ),
                update(
                    "root",
                    json!({"sessionUpdate": "tool_call_update", "toolCallId": "again",
                        "status": "failed", "rawOutput": {"error": "busy"}}),
                ),
                status("root", "child", "root", "Alpha", "running"),
            ],
        );
        let refused = out.iter().find_map(|(_, update)| match update {
            HarnessUpdateKind::ToolCall(payload) => Some(payload),
            _ => None,
        });
        assert!(
            refused.is_some_and(|payload| payload["toolCallId"] == "again"
                && payload["status"] == "failed"
                && payload["rawOutput"]["error"] == "busy"),
            "{out:?}"
        );
        assert_eq!(
            spawned(&out)[1],
            ("root".to_owned(), "child:generation:2".to_owned(), None)
        );
    }

    /// While Stop closes a root, the root's own output is dropped; its
    /// children's frames and ends still pass.
    #[test]
    fn a_muted_root_drops_its_own_output_only() {
        let children = ChildSessions::new(TAGS);
        replay(&children, &[status("root", "child", "root", "Alpha", "created")]);
        children.mute(&id("root"), true);
        let muted = replay(
            &children,
            &[
                update("root", text(json!("woken"))),
                child_frame("root", "child", "Alpha", text(json!("late"))),
                status("root", "child", "root", "Alpha", "interrupted"),
            ],
        );
        assert_eq!(muted.len(), 2, "{muted:?}");
        assert_eq!(muted[0].0, id("child"));
        assert_eq!(
            ended(&muted),
            [("child".to_owned(), SubagentThreadState::Cancelled)]
        );
        children.mute(&id("root"), false);
        assert_eq!(replay(&children, &[update("root", text(json!("next")))]).len(), 1);
    }

    #[test]
    fn an_interrupted_or_failed_child_ends_its_thread_so() {
        let children = ChildSessions::new(TAGS);
        let out = replay(
            &children,
            &[
                status("root", "stopped", "root", "Alpha", "created"),
                status("root", "stopped", "root", "Alpha", "interrupted"),
                status("root", "broken", "root", "Beta", "created"),
                json!({"method": TAGS.updates, "params": {
                    "rootSessionId": "root", "childSessionId": "broken", "parentSessionId": "root",
                    "depth": 1, "type": "status", "status": "failed", "error": {"message": "boom"}}}),
                // A second end of the same run says nothing new.
                status("root", "broken", "root", "Beta", "failed"),
            ],
        );
        assert_eq!(
            ended(&out),
            [
                ("stopped".to_owned(), SubagentThreadState::Cancelled),
                ("broken".to_owned(), SubagentThreadState::Failed),
            ]
        );
    }

    #[test]
    fn a_call_with_no_child_is_released_when_it_ends_or_the_turn_settles() {
        let children = ChildSessions::new(TAGS);
        let out = replay(
            &children,
            &[
                update("root", pending("refused")),
                update("root", control("refused", "failed", task("Alpha"))),
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

    /// A tagged frame for a child the status stream never named still
    /// announces it.
    #[test]
    fn a_childs_first_frame_announces_it_without_a_status() {
        let children = ChildSessions::new(TAGS);
        let out = replay(
            &children,
            &[
                update("root", pending("call")),
                update(
                    "root",
                    json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "x"}, "_meta": tag("child", "root", "Alpha")}),
                ),
            ],
        );
        assert_eq!(spawned(&out), [("root".to_owned(), "child".to_owned(), None)]);
        assert!(
            matches!(out.last(), Some((session, HarnessUpdateKind::AgentThoughtChunk(..))) if session == &id("child"))
        );
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
    fn a_resumed_childs_requests_move_to_its_current_run() {
        let children = ChildSessions::new(TAGS);
        replay(
            &children,
            &[
                status("root", "child", "root", "Alpha", "created"),
                status("root", "child", "root", "Alpha", "completed"),
                status("root", "child", "root", "Alpha", "running"),
            ],
        );
        let mut tool_call = json!({"toolCallId": "child:edit", "title": "Alpha: edit", "_meta": tag("child", "root", "Alpha")});
        assert_eq!(
            children.permission_session(id("root"), &mut tool_call),
            id("child:generation:2")
        );
        let mut message = "Alpha: Questions".to_string();
        assert_eq!(
            children.question_session(id("root"), Some("child:ask"), &mut message),
            id("child:generation:2")
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
                status("root", "outer", "root", "Outer", "created"),
                update("root", nested),
                json!({"method": TAGS.updates, "params": {
                    "rootSessionId": "root", "childSessionId": "inner", "parentSessionId": "outer",
                    "depth": 2, "title": "Inner", "type": "status", "status": "created"}}),
                json!({"method": TAGS.updates, "params": {
                    "rootSessionId": "root", "childSessionId": "inner", "parentSessionId": "outer",
                    "depth": 2, "title": "Inner", "type": "update", "update":
                    {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "deep"},
                     "_meta": {"opencode/child-session": {"id": "inner", "parentID": "outer", "depth": 2, "title": "Inner"}}}}}),
            ],
        );
        let spawned: Vec<_> = spawned(&out)
            .into_iter()
            .map(|(session, child, _)| (session, child))
            .collect();
        assert_eq!(
            spawned,
            [
                ("root".to_owned(), "outer".to_owned()),
                ("outer".to_owned(), "inner".to_owned())
            ]
        );
        assert!(
            matches!(out.last(), Some((session, HarnessUpdateKind::AgentMessageChunk(..))) if session == &id("inner"))
        );
    }

    #[test]
    fn open_children_are_a_roots_running_children_deepest_first() {
        let children = ChildSessions::new(TAGS);
        replay(
            &children,
            &[
                status("root", "outer", "root", "Outer", "created"),
                json!({"method": TAGS.updates, "params": {
                    "rootSessionId": "root", "childSessionId": "inner", "parentSessionId": "outer",
                    "depth": 2, "title": "Inner", "type": "status", "status": "created"}}),
                status("root", "done", "root", "Done", "created"),
                status("root", "done", "root", "Done", "completed"),
                status("other", "elsewhere", "other", "Elsewhere", "created"),
            ],
        );
        assert_eq!(children.open_children(&id("root")), ["inner", "outer"]);
        replay(&children, &[status("root", "done", "root", "Done", "running")]);
        assert_eq!(children.open_children(&id("root")), ["inner", "outer", "done"]);
    }

    #[test]
    fn a_child_update_round_trips() {
        for raw in [
            json!({"rootSessionId": "r", "childSessionId": "c", "parentSessionId": "r", "depth": 1,
                "title": "T", "type": "status", "status": "created"}),
            json!({"rootSessionId": "r", "childSessionId": "c", "parentSessionId": "r", "depth": 1,
                "type": "status", "status": "failed", "error": {"message": "boom"}}),
            json!({"rootSessionId": "r", "childSessionId": "c", "parentSessionId": "r", "depth": 1,
                "title": "T", "type": "update", "update": {"sessionUpdate": "agent_message_chunk"}}),
        ] {
            let typed: ChildUpdate = serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(serde_json::to_value(typed).unwrap(), raw);
        }
    }
}
