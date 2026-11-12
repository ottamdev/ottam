//! Ottam's own tools, on the proxy's reserved `ottam` path. Every
//! session is given it, with or without user servers. What each tool does is
//! the server's: see [`OttamTools`] and `docs/architecture/backend/mcp.md`
//! § Delegation.
//!
//! Every call returns at once. A delegated child's report reaches its parent
//! later, as a note, so no tool waits on a child and no harness's tool
//! timeout applies.

use super::proxy::McpEndpoint;
use super::tokens::McpCaller;
use futures::future::BoxFuture;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer};
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// The path name, which no user server may take.
pub(crate) const OTTAM_ROUTE: &str = "ottam";

const DELEGATE: &str = "delegate";
const MESSAGE_CHILD: &str = "message_child";
const LIST_CHILDREN: &str = "list_children";
const STOP_CHILD: &str = "stop_child";
const MESSAGE_PARENT: &str = "message_parent";
const GET_SESSION_MESSAGES: &str = "get_session_messages";
const GET_SESSION_TOOLCALL_OUTPUT: &str = "get_session_toolcall_output";
const GET_CONVERSATION_INFO: &str = "get_conversation_info";

/// Tools only a parent may call: a child cannot delegate, so it has none of
/// its own to reach.
const PARENT_ONLY: [&str; 4] = [DELEGATE, MESSAGE_CHILD, LIST_CHILDREN, STOP_CHILD];

/// The agents a parent can delegate to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DelegationTargets {
    /// The named agents' names.
    pub(crate) named: Vec<String>,
    /// The installed agents, as `(id, display name)`.
    pub(crate) installed: Vec<(String, String)>,
}

/// A `delegate` call. `agent` names a named agent; without it, `engine`
/// names an installed agent, and without either the caller's own agent
/// runs the task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DelegateRequest {
    pub(crate) agent: Option<String>,
    pub(crate) engine: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) mode: Option<String>,
    /// Names the task in the parent and the sidebar; absent, the prompt's
    /// first line does.
    pub(crate) title: Option<String>,
    pub(crate) prompt: String,
}

/// What the tools do, given the conversation that called. An `Err` is the
/// message the agent sees as a failed call.
pub(crate) trait OttamTools: Send + Sync + 'static {
    /// Whether the caller's conversation is itself a delegated child, which
    /// is offered no delegation tools.
    fn is_child(&self, caller: &McpCaller) -> BoxFuture<'_, bool>;
    /// The agents `delegate` takes, listed in its definition.
    fn delegation_targets(&self) -> BoxFuture<'_, DelegationTargets>;
    fn delegate(
        &self,
        caller: McpCaller,
        request: DelegateRequest,
    ) -> BoxFuture<'_, Result<String, String>>;
    fn list_children(&self, caller: McpCaller) -> BoxFuture<'_, Result<String, String>>;
    /// `child` is a child's conversation id, or its task title or agent
    /// name when only one child has it.
    fn message_child(
        &self,
        caller: McpCaller,
        child: String,
        message: String,
    ) -> BoxFuture<'_, Result<String, String>>;
    fn stop_child(&self, caller: McpCaller, child: String)
        -> BoxFuture<'_, Result<String, String>>;
    fn message_parent(
        &self,
        caller: McpCaller,
        message: String,
    ) -> BoxFuture<'_, Result<String, String>>;
    /// The caller's own conversation: ids, agent, related agents and usage.
    fn conversation_info(&self, caller: McpCaller) -> BoxFuture<'_, Result<String, String>>;
    /// A page of a conversation's history: `conversation` absent reads a
    /// child's parent, or the caller's own conversation when it has none.
    fn session_messages(
        &self,
        caller: McpCaller,
        conversation: Option<String>,
        before: Option<u64>,
        limit: Option<u64>,
    ) -> BoxFuture<'_, Result<String, String>>;
    fn toolcall_output(
        &self,
        caller: McpCaller,
        conversation: Option<String>,
        tool_call_id: String,
        offset: Option<u64>,
    ) -> BoxFuture<'_, Result<String, String>>;
}

pub(crate) struct OttamEndpoint {
    tools: Arc<dyn OttamTools>,
}

impl OttamEndpoint {
    pub(crate) fn new(tools: Arc<dyn OttamTools>) -> Self {
        Self { tools }
    }
}

fn schema(value: Value) -> Arc<Map<String, Value>> {
    match value {
        Value::Object(map) => Arc::new(map),
        _ => Arc::new(Map::new()),
    }
}

/// What tells Claude these tools from its own agent tools, which the
/// descriptions repeat where a model might reach for those instead.
const NOT_PEERS: &str = "Delegated agents are Ottam conversations: your own agent and \
                         messaging tools (such as SendMessage and ListAgents) cannot reach \
                         them, and anything sent that way bypasses Ottam.";

/// The history tools and the conversation's own details, which parents and
/// children are both offered.
fn history_tools(for_child: bool) -> Vec<Tool> {
    let conversation = if for_child {
        "The conversation to read: your parent's by default, or your own by its id."
    } else {
        "The conversation to read: your own by default, or a delegated agent's id from \
         list_children, or its task title or agent name when only one has it."
    };
    vec![
        Tool::new(
            GET_CONVERSATION_INFO,
            "Describe the Ottam conversation you are running in: its id, title and \
             working folder; your agent, model and mode; the agents earlier turns ran \
             on; delegating and delegated agents; and token usage, per turn and how full \
             your context is, as the agent reported them.",
            schema(json!({"type": "object", "properties": {}})),
        ),
        Tool::new(
            GET_SESSION_MESSAGES,
            "Read a page of a conversation's history: the user's and agents' messages, \
             messages between delegating and delegated agents, and tool calls with their \
             call ids. Newest entries come first in the reading; long entries are cut \
             at 2,000 characters, and a page of one entry shows up to 64,000. Pass the \
             `before` it names to read further back.",
            schema(json!({
                "type": "object",
                "properties": {
                    "conversation": {"type": "string", "description": conversation},
                    "before": {"type": "integer", "description": "Read entries older than this position, as the previous page named."},
                    "limit": {"type": "integer", "description": "Entries to read, 1 to 50; 20 by default."}
                },
                "required": []
            })),
        ),
        Tool::new(
            GET_SESSION_TOOLCALL_OUTPUT,
            "Read one tool call's input, the contents of a file change it made, and its \
             output from a conversation's history, in pages of up to 16,000 bytes that \
             end on a whole character. Take the call id from get_session_messages.",
            schema(json!({
                "type": "object",
                "properties": {
                    "conversation": {"type": "string", "description": conversation},
                    "tool_call_id": {"type": "string", "description": "The call id get_session_messages shows."},
                    "offset": {"type": "integer", "description": "Where to continue the output, as the previous page named."}
                },
                "required": ["tool_call_id"]
            })),
        ),
    ]
}

/// The tools a parent is offered. Kept short: their definitions ride along
/// on every model request.
fn parent_tools(targets: &DelegationTargets) -> Vec<Tool> {
    let agent_description = if targets.named.is_empty() {
        "A named agent, which the user sets up with its own instructions, model \
         and mode. None exist yet."
            .to_owned()
    } else {
        format!(
            "A named agent, which the user set up with its own instructions, model \
             and mode: {}.",
            targets.named.join(", ")
        )
    };
    let installed: Vec<String> = targets
        .installed
        .iter()
        .map(|(id, name)| format!("{name} ({id})"))
        .collect();
    let engine_description = if installed.is_empty() {
        "An installed agent to run the task, by id or name. Leave out agent and \
         engine to run it on your own agent."
            .to_owned()
    } else {
        format!(
            "An installed agent to run the task, by id or name: {}. Leave out \
             agent and engine to run it on your own agent.",
            installed.join(", ")
        )
    };
    let child = json!({
        "type": "string",
        "description": "The delegated agent: its conversation id from list_children, or its task title or agent name when only one has it."
    });
    let mut tools = vec![
        Tool::new(
            DELEGATE,
            format!(
                "Hand a task to another Ottam agent: a named agent, any installed \
                 agent, or your own. It starts a new conversation with that agent and \
                 returns at once; keep working or end your turn. \
                 When the agent finishes, its report arrives in this conversation as a \
                 new message. Write everything the agent needs into the prompt. To follow \
                 up with an agent you already delegated to, use message_child rather than \
                 a new call. {NOT_PEERS}"
            ),
            schema(json!({
                "type": "object",
                "properties": {
                    "agent": {"type": "string", "description": agent_description},
                    "engine": {"type": "string", "description": engine_description},
                    "model": {"type": "string", "description": "A model the engine offers, by id or name. By default the engine's own, or yours when it runs on your agent."},
                    "mode": {"type": "string", "description": "A mode the engine offers, by id or name. By default its read-only mode; Oh-My-Pi's is a planning workflow, so it starts in `default` (asks before shell commands, not edits) unless you ask before acting. If you ask before acting, only a mode that asks too."},
                    "title": {"type": "string", "description": "A short name for the task, under 60 characters, shown to the user."},
                    "prompt": {"type": "string", "description": "The task, with the context it needs."}
                },
                "required": ["prompt"]
            })),
        ),
        Tool::new(
            MESSAGE_CHILD,
            format!(
                "Send a follow-up to an agent you delegated to, in its own conversation. \
                 An idle agent reads it at once; a working one reads it when its turn \
                 ends. Its reply arrives here as a new message, so do not wait or poll. \
                 Use this, not SendMessage: {NOT_PEERS}"
            ),
            schema(json!({
                "type": "object",
                "properties": {
                    "child": child,
                    "message": {"type": "string", "description": "What to tell or ask the agent."}
                },
                "required": ["child", "message"]
            })),
        ),
        Tool::new(
            LIST_CHILDREN,
            format!(
                "List the agents this conversation delegated to, with each one's state \
                 (working, waiting on a permission, idle, failed, stopped) and the start \
                 of its latest report. {NOT_PEERS}"
            ),
            schema(json!({"type": "object", "properties": {}})),
        ),
        Tool::new(
            STOP_CHILD,
            "Stop a delegated agent's running turn. Its conversation keeps what it did; \
             message_child starts it again.",
            schema(json!({
                "type": "object",
                "properties": {"child": child},
                "required": ["child"]
            })),
        ),
    ];
    tools.extend(history_tools(false));
    tools
}

/// The tools a delegated child is offered.
fn child_tools() -> Vec<Tool> {
    let mut tools = vec![message_parent_tool()];
    tools.extend(history_tools(true));
    tools
}

fn message_parent_tool() -> Tool {
    Tool::new(
        MESSAGE_PARENT,
        "Send a message to the agent that delegated this task to you, while you keep \
         working: when you are blocked, or with a finding it needs now. It wakes an idle \
         parent at once. You need not report the end of your task this way: your final \
         message reaches the parent when you finish.",
        schema(json!({
            "type": "object",
            "properties": {
                "message": {"type": "string", "description": "What the parent needs to know."}
            },
            "required": ["message"]
        })),
    )
}

fn text_result(result: Result<String, String>) -> CallToolResponse {
    match result {
        Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]).into(),
        Err(text) => CallToolResult::error(vec![ContentBlock::text(text)]).into(),
    }
}

fn string_argument(request: &CallToolRequestParams, name: &str) -> Result<String, String> {
    request
        .arguments
        .as_ref()
        .and_then(|arguments| arguments.get(name))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("`{name}` is required"))
}

/// A `delegate` call's arguments. A named agent brings its own model and
/// mode, so it takes neither, nor an engine.
fn delegate_request(request: &CallToolRequestParams) -> Result<DelegateRequest, String> {
    let optional = |name| string_argument(request, name).ok();
    let delegation = DelegateRequest {
        agent: optional("agent"),
        engine: optional("engine"),
        model: optional("model"),
        mode: optional("mode"),
        title: optional("title"),
        prompt: string_argument(request, "prompt")?,
    };
    if delegation.agent.is_some()
        && (delegation.engine.is_some() || delegation.model.is_some() || delegation.mode.is_some())
    {
        return Err(
            "A named agent has its own engine, model and mode: pass `agent` alone, or \
             `engine` with an optional `model` and `mode`."
                .to_owned(),
        );
    }
    Ok(delegation)
}

/// A whole number argument, which some agents send as a string.
fn number_argument(request: &CallToolRequestParams, name: &str) -> Result<Option<u64>, String> {
    let Some(value) = request
        .arguments
        .as_ref()
        .and_then(|arguments| arguments.get(name))
        .filter(|value| !value.is_null())
    else {
        return Ok(None);
    };
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        .map(Some)
        .ok_or_else(|| format!("`{name}` must be a whole number"))
}

impl McpEndpoint for OttamEndpoint {
    fn info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    fn list_tools(
        &self,
        caller: McpCaller,
        _request: Option<PaginatedRequestParams>,
    ) -> BoxFuture<'_, Result<ListToolsResult, ErrorData>> {
        Box::pin(async move {
            // A shared token's listing names no session, and its caller is
            // whichever conversation minted the token. Its sessions may be
            // parents or children, so it lists both sets and a call outside
            // the caller's role is refused below.
            let tools = if caller.shared {
                let mut tools = parent_tools(&self.tools.delegation_targets().await);
                tools.push(message_parent_tool());
                tools
            } else if self.tools.is_child(&caller).await {
                child_tools()
            } else {
                parent_tools(&self.tools.delegation_targets().await)
            };
            Ok(ListToolsResult::with_all_items(tools))
        })
    }

    fn call_tool(
        &self,
        caller: McpCaller,
        mut request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> BoxFuture<'_, Result<CallToolResponse, ErrorData>> {
        Box::pin(async move {
            let meta = request.meta.take();
            let meta = meta
                .as_ref()
                .map(|meta| &meta.0 .0)
                .or(Some(&context.meta.0 .0));
            let caller = caller.attributed(meta);
            if caller.shared && caller.session_id.is_none() {
                return Ok(text_result(Err(
                    "Ottam could not tell which conversation made this call".to_owned(),
                )));
            }
            let name = request.name.to_string();
            let is_child = self.tools.is_child(&caller).await;
            if PARENT_ONLY.contains(&name.as_str()) && is_child {
                return Ok(text_result(Err(
                    "A delegated agent cannot delegate; report back to your parent instead"
                        .to_owned(),
                )));
            }
            if name == MESSAGE_PARENT && !is_child {
                return Ok(text_result(Err(
                    "This conversation was not delegated, so it has no parent to message."
                        .to_owned(),
                )));
            }
            let conversation = || string_argument(&request, "conversation").ok();
            let result = match name.as_str() {
                DELEGATE => match delegate_request(&request) {
                    Ok(delegation) => self.tools.delegate(caller, delegation).await,
                    Err(error) => Err(error),
                },
                LIST_CHILDREN => self.tools.list_children(caller).await,
                GET_CONVERSATION_INFO => self.tools.conversation_info(caller).await,
                MESSAGE_CHILD => match (
                    string_argument(&request, "child"),
                    string_argument(&request, "message"),
                ) {
                    (Ok(child), Ok(message)) => {
                        self.tools.message_child(caller, child, message).await
                    }
                    (Err(error), _) | (_, Err(error)) => Err(error),
                },
                STOP_CHILD => match string_argument(&request, "child") {
                    Ok(child) => self.tools.stop_child(caller, child).await,
                    Err(error) => Err(error),
                },
                MESSAGE_PARENT => match string_argument(&request, "message") {
                    Ok(message) => self.tools.message_parent(caller, message).await,
                    Err(error) => Err(error),
                },
                GET_SESSION_MESSAGES => match (
                    number_argument(&request, "before"),
                    number_argument(&request, "limit"),
                ) {
                    (Ok(before), Ok(limit)) => {
                        self.tools
                            .session_messages(caller, conversation(), before, limit)
                            .await
                    }
                    (Err(error), _) | (_, Err(error)) => Err(error),
                },
                GET_SESSION_TOOLCALL_OUTPUT => match (
                    string_argument(&request, "tool_call_id"),
                    number_argument(&request, "offset"),
                ) {
                    (Ok(tool_call_id), Ok(offset)) => {
                        self.tools
                            .toolcall_output(caller, conversation(), tool_call_id, offset)
                            .await
                    }
                    (Err(error), _) | (_, Err(error)) => Err(error),
                },
                other => {
                    return Err(ErrorData::invalid_params(
                        format!("unknown tool {other}"),
                        None,
                    ))
                }
            };
            Ok(text_result(result))
        })
    }
}
