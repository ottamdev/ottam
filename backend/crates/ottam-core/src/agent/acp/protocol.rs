use super::super::types::{
    AgentDefinition, AsyncTaskUpdate, HarnessAuthMethod, HarnessAuthMethodKind,
    HarnessAuthTerminalCommand, HarnessError, HarnessErrorKind, HarnessPermissionOutcome,
    HarnessPrompt, HarnessSessionId, HarnessStopReason, HarnessUpdateKind,
};
use super::features::{air_capabilities, client_supports, AcpFeature};
use crate::diagnostics::{dropped_paths, record_unhandled, shape};
use agent_client_protocol::schema::v1::{
    AuthCapabilities, AuthMethod, AuthMethodTerminal, ClientCapabilities, ContentBlock,
    ElicitationCapabilities, ElicitationFormCapabilities, FileSystemCapabilities, ImageContent,
    Implementation, InitializeRequest, InitializeResponse, RequestPermissionOutcome,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate, StopReason, TextContent,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{AcpAgent, AcpAgentConfig, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(super) const CLIENT_NAME: &str = "ottam";
const CLIENT_TITLE: &str = "Ottam";

pub(super) fn build_transport(
    definition: &AgentDefinition,
) -> (
    super::shared_transport::RefreshSafeTransport,
    tokio::sync::oneshot::Receiver<super::shared_transport::SharedChild>,
) {
    // The SDK supports additions but not env_remove. env -u is an argv-only
    // launch boundary; it removes recorded-unset levers without a shell.
    let unset = unset_variables(definition, std::env::vars());
    let mut arguments = Vec::new();
    let command = if unset.is_empty() {
        &definition.launch.command
    } else {
        for key in unset {
            arguments.extend(["-u".into(), key]);
        }
        arguments.push(definition.launch.command.clone());
        "/usr/bin/env"
    };
    arguments.extend(definition.launch.arguments.iter().cloned());
    let config = AcpAgentConfig::new(command)
        .args(arguments)
        .envs(definition.launch.environment.iter().cloned());
    let agent = AcpAgent::new(config);
    // Wire logging writes prompts and replies to disk, so release builds
    // cannot turn it on.
    #[cfg(test_seams)]
    let agent = if std::env::var_os("OTTAM_ACP_WIRE_LOG").is_some() {
        agent.with_debug(|line, direction| {
            tracing::trace!(
                target: "acp.agent.wire",
                direction = line_direction_name(direction),
                line,
                "ACP adapter line (explicit wire logging enabled)"
            );
        })
    } else {
        agent
    };
    super::shared_transport::RefreshSafeTransport::new(agent, definition.refresh_lock_path())
}

/// The variables an adapter launch removes from what core inherited: the
/// instance's recorded-unset levers and, when core itself runs inside a
/// Claude Code session, that session's own. A terminal or tool in Claude Code
/// sets `CLAUDECODE` and `CLAUDE_*` for what it runs, among them its session
/// id and peer message socket and token. Left in place, a Claude agent's
/// `ListAgents` and `SendMessage` would reach the launching session. The
/// launch's own environment is kept, since `env -u` would remove it too.
fn unset_variables(
    definition: &AgentDefinition,
    inherited: impl Iterator<Item = (String, String)>,
) -> Vec<String> {
    let mut unset: Vec<_> = definition
        .instance
        .as_ref()
        .and_then(|instance| match &instance.home {
            super::super::HomeLayout::Existing { environment, .. } => Some(environment),
            _ => None,
        })
        .into_iter()
        .flat_map(|environment| environment.iter())
        .filter(|(_, value)| value.is_none())
        .map(|(key, _)| key.clone())
        .collect();
    let inherited: Vec<_> = inherited.map(|(key, _)| key).collect();
    if inherited.iter().any(|key| key == "CLAUDECODE") {
        let set_here = |key: &String| {
            definition
                .launch
                .environment
                .iter()
                .any(|(name, _)| name == key)
        };
        let host: Vec<_> = inherited
            .into_iter()
            .filter(|key| {
                (key == "CLAUDECODE" || key.starts_with("CLAUDE_"))
                    && !set_here(key)
                    && !unset.contains(key)
            })
            .collect();
        unset.extend(host);
    }
    unset
}

/// What an adapter launch removes from core's environment, for another
/// process started on the agent's behalf (the plan-usage probe).
pub(crate) fn launch_unset_variables(definition: &AgentDefinition) -> Vec<String> {
    unset_variables(definition, std::env::vars())
}

/// `_meta` key by which adapters offer an explicit `{command, args}` for a
/// terminal auth method, instead of leaving us to reconstruct one from the
/// launch spec. `claude-agent-acp` fills it with an absolute interpreter path
/// and its own resolved script, which is how we avoid re-resolving through
/// `npx` for the login run.
const TERMINAL_AUTH_META_KEY: &str = "terminal-auth";

/// Built from [`CLIENT_FEATURES`]: a row flip there changes the wire here,
/// and `client_features_table_matches_initialize_request` pins the result.
///
/// `terminal` stays off on purpose: it claims we implement the whole
/// `terminal/*` method family so agents can run their own tool terminals,
/// which Ottam does not. Setting it would invite mid-run requests we can
/// only fail. `elicitation.form` is the question panel; `elicitation.url`
/// (MCP OAuth) is unimplemented. Plan operations,
/// compaction, and notices sit behind SDK features core leaves off, and `nes`
/// behind one the SDK does not forward. `SdkStatus` records which is which.
/// `initialize` as sent: [`initialize_request`] plus the client capabilities
/// newer than the pinned schema, which its typed `ClientCapabilities` cannot
/// hold. Each is still a [`CLIENT_FEATURES`](super::features::CLIENT_FEATURES)
/// row, so the table stays the one source of what the wire advertises.
#[derive(Debug, Clone, Serialize, Deserialize, agent_client_protocol::JsonRpcRequest)]
#[serde(transparent)]
#[request(method = "initialize", response = InitializeResponse)]
pub(super) struct InitializeWire {
    params: Value,
}

pub(super) fn initialize_wire() -> InitializeWire {
    let mut params = to_json(&initialize_request());
    if client_supports(AcpFeature::Subagents) {
        if let Some(capabilities) = params
            .get_mut("clientCapabilities")
            .and_then(Value::as_object_mut)
        {
            capabilities.insert("subagents".into(), Value::Object(Default::default()));
        }
    }
    InitializeWire { params }
}

pub(super) fn initialize_request() -> InitializeRequest {
    let mut meta = agent_client_protocol::schema::v1::Meta::new();
    if client_supports(AcpFeature::AuthTerminal) {
        // `auth.terminal` and this flag gate the same thing for
        // `claude-agent-acp` — it withholds every terminal auth method unless
        // one of them is set — but only the `_meta` one makes it attach the
        // explicit command, so advertise both.
        meta.insert(TERMINAL_AUTH_META_KEY.to_string(), Value::Bool(true));
    }
    if client_supports(AcpFeature::TerminalOutputMeta) {
        meta.insert("terminal_output".into(), Value::Bool(true));
    }
    meta.insert(
        "jetbrains".into(),
        serde_json::json!({
            "air": {"version": 1, "capabilities": air_capabilities()}
        }),
    );
    let mut capabilities = ClientCapabilities::new()
        .fs(FileSystemCapabilities::new()
            .read_text_file(client_supports(AcpFeature::FsReadTextFile))
            .write_text_file(client_supports(AcpFeature::FsWriteTextFile)))
        .terminal(client_supports(AcpFeature::Terminal))
        .meta(meta);
    if client_supports(AcpFeature::AuthTerminal) {
        capabilities = capabilities.auth(AuthCapabilities::new().terminal(true));
    }
    if client_supports(AcpFeature::ElicitationForm) {
        capabilities = capabilities
            .elicitation(ElicitationCapabilities::new().form(ElicitationFormCapabilities::new()));
    }
    InitializeRequest::new(ProtocolVersion::V1)
        .client_info(
            Implementation::new(CLIENT_NAME, env!("CARGO_PKG_VERSION")).title(CLIENT_TITLE),
        )
        .client_capabilities(capabilities)
}

pub(super) fn normalize_auth_method(method: &AuthMethod) -> HarnessAuthMethod {
    let (kind, terminal_command) = match method {
        AuthMethod::Terminal(terminal) => (
            HarnessAuthMethodKind::Terminal,
            Some(terminal_auth_command(terminal)),
        ),
        // `Agent` is the untagged default, so it also absorbs any `type` the
        // schema doesn't model, such as the retired `env_var`. The enum is
        // `#[non_exhaustive]`; a newer variant is an adapter-run login too,
        // which is the safe reading: we do not invent a command.
        _ => (HarnessAuthMethodKind::Agent, None),
    };
    HarnessAuthMethod {
        method_id: method.id().0.to_string(),
        name: method.name().to_string(),
        description: method.description().map(str::to_string),
        kind,
        terminal_command,
    }
}

/// Read a terminal method's command line, preferring the explicit
/// `{command, args}` the adapter attaches under [`TERMINAL_AUTH_META_KEY`].
/// Without it the method's own `args` are meant to be appended to the agent's
/// launch command, which only the caller holding the definition can do.
fn terminal_auth_command(terminal: &AuthMethodTerminal) -> HarnessAuthTerminalCommand {
    let mut env = terminal
        .env
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<Vec<_>>();
    // `HashMap` iteration order is not stable, and this ends up on a command
    // line we compare in tests.
    env.sort();
    let meta = terminal
        .meta
        .as_ref()
        .and_then(|meta| meta.get(TERMINAL_AUTH_META_KEY));
    let Some(meta) = meta else {
        return HarnessAuthTerminalCommand {
            program: None,
            args: terminal.args.clone(),
            env,
            label: None,
        };
    };
    let program = meta
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_string);
    // A `_meta` block without a usable command is not a command at all: falling
    // back to its `args` alone would run the agent binary with the wrong ones.
    let args = match (&program, meta.get("args").and_then(Value::as_array)) {
        (Some(_), Some(args)) => args
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => terminal.args.clone(),
    };
    HarnessAuthTerminalCommand {
        program,
        args,
        env,
        label: meta
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

pub(super) fn validate_permission_outcome(
    outcome: HarnessPermissionOutcome,
    allowed_option_ids: &[String],
) -> RequestPermissionOutcome {
    match outcome {
        HarnessPermissionOutcome::Selected { option_id }
            if allowed_option_ids.contains(&option_id) =>
        {
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id))
        }
        HarnessPermissionOutcome::Selected { option_id } => {
            tracing::warn!(
                target: "ottam_core::agent",
                option_id = %option_id,
                "permission broker selected an unknown option; cancelling"
            );
            RequestPermissionOutcome::Cancelled
        }
        HarnessPermissionOutcome::Cancelled => RequestPermissionOutcome::Cancelled,
    }
}

pub(super) fn normalize_update(update: &SessionUpdate) -> HarnessUpdateKind {
    if let SessionUpdate::AgentMessageChunk(chunk) = update {
        if matches!(chunk.content, ContentBlock::Image(_)) {
            return HarnessUpdateKind::AgentImageChunk(
                to_json(&chunk.content),
                chunk.message_id.as_ref().map(|id| id.0.to_string()),
            );
        }
    }
    if let SessionUpdate::AgentMessageChunk(chunk) | SessionUpdate::AgentThoughtChunk(chunk) =
        update
    {
        let metadata = crate::agent::metadata::transcript_metadata(&to_json(chunk));
        if metadata != Default::default() {
            if let Some(text) = text_chunk(&chunk.content) {
                return HarnessUpdateKind::RichMessageChunk {
                    text,
                    message_id: chunk.message_id.as_ref().map(|id| id.0.to_string()),
                    reasoning: matches!(update, SessionUpdate::AgentThoughtChunk(_)),
                    metadata,
                };
            }
        }
    }
    match update {
        SessionUpdate::UserMessageChunk(chunk) => text_chunk(&chunk.content)
            .map(HarnessUpdateKind::UserMessageChunk)
            .unwrap_or_else(|| other_update("user_message_chunk", update)),
        SessionUpdate::AgentMessageChunk(chunk) => text_chunk(&chunk.content)
            .map(|text| {
                HarnessUpdateKind::AgentMessageChunk(
                    text,
                    chunk.message_id.as_ref().map(|id| id.0.to_string()),
                )
            })
            .unwrap_or_else(|| other_update("agent_message_chunk", update)),
        SessionUpdate::AgentThoughtChunk(chunk) => text_chunk(&chunk.content)
            .map(|text| {
                HarnessUpdateKind::AgentThoughtChunk(
                    text,
                    chunk.message_id.as_ref().map(|id| id.0.to_string()),
                )
            })
            .unwrap_or_else(|| other_update("agent_thought_chunk", update)),
        SessionUpdate::ToolCall(call) => HarnessUpdateKind::ToolCall(to_json(call)),
        SessionUpdate::ToolCallUpdate(tool_update) => {
            HarnessUpdateKind::ToolCallUpdate(to_json(tool_update))
        }
        SessionUpdate::Plan(plan) => HarnessUpdateKind::Plan(to_json(plan)),
        SessionUpdate::AvailableCommandsUpdate(_) => {
            other_update("available_commands_update", update)
        }
        SessionUpdate::CurrentModeUpdate(_) => other_update("current_mode_update", update),
        SessionUpdate::ConfigOptionUpdate(_) => other_update("config_option_update", update),
        SessionUpdate::SessionInfoUpdate(_) => other_update("session_info_update", update),
        SessionUpdate::UsageUpdate(_) => other_update("usage_update", update),
        _ => other_update("unknown", update),
    }
}

/// Decodes an AIR `async_task_*` `session/update`. SDK 2.2's `SessionUpdate`
/// has no catch-all, so these tags never reach `normalize_update`; the actor
/// claims them raw. `None` declines: not `session/update`, another tag, or no
/// session to attribute it to. A malformed lifecycle frame becomes `Other`, so
/// it is counted as an unknown update rather than lost to a parse error.
pub(super) fn decode_async_task_update(
    method: &str,
    params: &Value,
) -> Option<(HarnessSessionId, HarnessUpdateKind)> {
    if method != "session/update" {
        return None;
    }
    let update = params.get("update")?;
    let tag = update.get("sessionUpdate")?.as_str()?;
    if !tag.starts_with("async_task_") {
        return None;
    }
    let session_id = HarnessSessionId::new(params.get("sessionId")?.as_str()?);
    let decoded = serde_json::from_value::<AsyncTaskUpdate>(update.clone())
        .ok()
        .filter(|task| !task.async_task_id.is_empty())
        .and_then(|task| match tag {
            "async_task_spawned" => Some(HarnessUpdateKind::AsyncTaskSpawned(task)),
            "async_task_progress" => Some(HarnessUpdateKind::AsyncTaskProgress(task)),
            "async_task_state_update" => {
                let state: ottam_protocol::AsyncTaskState =
                    serde_json::from_value(update.get("state")?.clone()).ok()?;
                // Only core declares a task lost; an adapter cannot.
                (state != ottam_protocol::AsyncTaskState::Lost)
                    .then_some(HarnessUpdateKind::AsyncTaskStateUpdate(state, task))
            }
            _ => None,
        });
    let update = decoded.unwrap_or_else(|| HarnessUpdateKind::Other {
        kind: tag.to_string(),
        payload: update.clone(),
    });
    Some((session_id, update))
}

/// Decodes an AIR native subagent `session/update` (`subagent_spawned`,
/// `subagent_state_update`), the pins' draft of ACP's subagent RFD. Like the
/// async task frames, SDK 2.2 has no variant for them. `None` declines. A
/// malformed frame becomes `Other`, counted rather than lost.
///
/// Upstream's merged shape (`subagent_update` upserts plus
/// `session_message[_chunk]`, schema 1.10.1) maps onto the same two kinds
/// once the pins send it; only this decoder changes.
pub(super) fn decode_subagent_update(
    method: &str,
    params: &Value,
) -> Option<(HarnessSessionId, HarnessUpdateKind)> {
    if method != "session/update" {
        return None;
    }
    let update = params.get("update")?;
    let tag = update.get("sessionUpdate")?.as_str()?;
    if !tag.starts_with("subagent_") {
        return None;
    }
    let session_id = HarnessSessionId::new(params.get("sessionId")?.as_str()?);
    let child = update
        .get("subagentSessionId")
        .and_then(Value::as_str)
        .filter(|child| !child.is_empty())
        .map(str::to_owned);
    let label = |key| update.get(key).and_then(Value::as_str).map(str::to_owned);
    let decoded = child.and_then(|child| match tag {
        "subagent_spawned" => Some(HarnessUpdateKind::SubagentSpawned {
            child,
            name: label("name"),
            task: label("task"),
            prompt: label("prompt"),
        }),
        "subagent_state_update" => {
            let state: ottam_protocol::SubagentThreadState =
                serde_json::from_value(update.get("state")?.clone()).ok()?;
            // `running` is never terminal, and the adapters send only ends.
            (!state.is_live()).then_some(HarnessUpdateKind::SubagentStateUpdate { child, state })
        }
        _ => None,
    });
    let update = decoded.unwrap_or_else(|| HarnessUpdateKind::Other {
        kind: tag.to_string(),
        payload: update.clone(),
    });
    Some((session_id, update))
}

/// Decodes one `session/update` for `agent`, or records why core drops it.
///
/// A frame the typed parse rejects is recorded as `update:<tag>` when the SDK
/// doesn't know its kind, and `malformed:<tag>` otherwise. It is never
/// forwarded, so its payload stays off the live broadcast. In debug builds a
/// frame that parses but loses fields to the typed model is forwarded and
/// recorded as `field:<tag>`, with the lost paths as its shape.
pub(super) fn decode_session_update(
    agent: &str,
    params: &Value,
) -> Option<(HarnessSessionId, HarnessUpdateKind)> {
    if let Some(decoded) = decode_async_task_update("session/update", params)
        .or_else(|| decode_subagent_update("session/update", params))
    {
        return Some(decoded);
    }
    let tag = params
        .pointer("/update/sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or("none");
    match serde_json::from_value::<SessionNotification>(params.clone()) {
        Ok(notification) => {
            if cfg!(debug_assertions) {
                let dropped = dropped_paths(params, &to_json(&notification), &|path, value| {
                    is_omitted_default(tag, path, value)
                });
                if !dropped.is_empty() {
                    record_unhandled(agent, &format!("field:{tag}"), || dropped.join(","));
                }
            }
            Some((
                HarnessSessionId::new(notification.session_id.0.to_string()),
                normalize_update(&notification.update),
            ))
        }
        Err(error) => {
            // Matched, never logged: other serde errors quote payload values.
            // A nested unknown variant names its own tag, so it stays
            // malformed, as does an async task frame the decoder declined.
            let unknown = !tag.starts_with("async_task_")
                && !tag.starts_with("subagent_")
                && error
                    .to_string()
                    .contains(&format!("unknown variant `{tag}`"));
            let label = if unknown { "update" } else { "malformed" };
            record_unhandled(agent, &format!("{label}:{tag}"), || shape(params));
            None
        }
    }
}

/// Values the SDK's `ToolCall` leaves out when serializing because they are the
/// field's default (`ToolKind::is_default`, `ToolCallStatus::is_default`). On
/// a `tool_call` update they are on the wire and missing after a round-trip,
/// but nothing is lost. The same value at any other path is drift.
pub(super) fn is_omitted_default(tag: &str, path: &str, value: &Value) -> bool {
    tag == "tool_call"
        && matches!(
            (path, value.as_str()),
            ("update.kind", Some("other")) | ("update.status", Some("pending"))
        )
}

fn text_chunk(content: &ContentBlock) -> Option<String> {
    match content {
        ContentBlock::Text(text) => Some(text.text.clone()),
        _ => None,
    }
}

fn other_update(kind: &str, update: &SessionUpdate) -> HarnessUpdateKind {
    HarnessUpdateKind::Other {
        kind: kind.to_string(),
        payload: to_json(update),
    }
}

pub(super) fn to_json(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

pub(super) fn map_stop_reason(reason: StopReason) -> HarnessStopReason {
    match reason {
        StopReason::EndTurn => HarnessStopReason::EndTurn,
        StopReason::MaxTokens => HarnessStopReason::MaxTokens,
        StopReason::MaxTurnRequests => HarnessStopReason::MaxTurnRequests,
        StopReason::Refusal => HarnessStopReason::Refusal,
        StopReason::Cancelled => HarnessStopReason::Cancelled,
        _ => HarnessStopReason::Other,
    }
}

/// The adapter-reported failure category carried in a JSON-RPC error's `data`.
///
/// An expired Claude login does not arrive as ACP `auth_required`: the adapter
/// reports a plain internal error whose text happens to mention authentication,
/// and attaches the real category here. `claude-agent-acp` adds this field
/// precisely so clients can dispatch on it rather than matching message text,
/// which is never load-bearing. Observed on the wire:
///
/// ```text
/// {"code":-32603,"message":"Internal error: Failed to authenticate: OAuth
///  session expired and could not be refreshed",
///  "data":{"errorKind":"authentication_failed"}}
/// ```
///
/// Only the two genuinely-signed-out categories map to `AuthRequired`. The rest
/// of the adapter's vocabulary (`billing_error`, `rate_limit`, `overloaded`,
/// `server_error`, …) stays a prompt failure on purpose — offering "sign in" to
/// someone who is out of credits or rate-limited is wrong advice, and a signed-in
/// account would be marked signed-out for the rest of the session.
fn adapter_error_kind_requires_auth(error: &agent_client_protocol::Error) -> bool {
    error
        .data
        .as_ref()
        .and_then(|data| data.get("errorKind"))
        .and_then(Value::as_str)
        .is_some_and(|kind| matches!(kind, "authentication_failed" | "oauth_org_not_allowed"))
}

/// The word Claude Code coins for a refresh interrupted part-way through,
/// which appears in every phrasing of the message it fails with:
///
/// ```text
/// Failed to refresh OAuth token: another Claude Code process is refreshing
/// it or exited mid-refresh. This is usually transient; retry in a minute,
/// and if it persists close other Claude Code processes or sign in again
/// ```
///
/// Message text, against this module's own rule, because there is no field to
/// read: the failure happens before the first assistant message, so the
/// adapter's `lastAssistantError` is unset and `errorKind` is omitted
/// entirely. The tag below is what the adapter would forward if that ever
/// changed, and is checked first so this match can be deleted when it does.
/// With AIR failures negotiated, the same text arrives as the title of the
/// turn's terminal failure instead; see [`refresh_lock_turn`].
const REFRESH_LOCK_PHRASE: &str = "mid-refresh";
/// The CLI's own classifier tag for the condition.
const REFRESH_LOCK_ERROR_KIND: &str = "oauth_refresh_lock_timeout";

/// What Ottam says instead of the CLI's own advice.
///
/// Contention does not establish credential validity or lock ownership. By
/// the time a user reads this, core has already cleared a lock whose owner
/// exited and resent the turn, so what remains is a live holder or a lock
/// core cannot account for.
const REFRESH_LOCK_MESSAGE: &str = "A Claude Code token refresh is busy or was interrupted. \
     Try again shortly. If it persists, finish or cancel agent sign-ins, \
     then quit and reopen Ottam before trying again.";

/// Whether this error is a refresh blocked by a lock rather than a real
/// authentication problem.
fn is_refresh_lock_contention(error: &agent_client_protocol::Error) -> bool {
    let tagged = error
        .data
        .as_ref()
        .and_then(|data| data.get("errorKind"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == REFRESH_LOCK_ERROR_KIND);
    tagged || error.message.contains(REFRESH_LOCK_PHRASE)
}

/// A turn that ended in a blocked refresh, as the tagged error the adapter
/// sends when AIR failures aren't negotiated. With them, claude-agent-acp
/// ends the turn `end_turn` with a `provider_error` failure titled with the
/// CLI's text, which would otherwise reach the user verbatim, both as the
/// run's error and as the session's last report.
pub(super) fn refresh_lock_turn(
    reason: StopReason,
    signal: Option<&ottam_protocol::ConversationSessionSignal>,
) -> Option<agent_client_protocol::Error> {
    let blocked = reason != StopReason::Cancelled
        && signal
            .and_then(|signal| signal.failure.as_ref())
            .is_some_and(|failure| {
                failure.severity == "error" && failure.message.contains(REFRESH_LOCK_PHRASE)
            });
    blocked.then(|| {
        let mut error = agent_client_protocol::Error::from(ErrorCode::InternalError);
        error.data = Some(serde_json::json!({ "errorKind": REFRESH_LOCK_ERROR_KIND }));
        error
    })
}

/// Ottam's message on a blocked-refresh failure the adapter publishes
/// outside a turn: one with no active turn to attach to, or one restored with
/// its session.
pub(crate) fn reword_refresh_lock_failure(
    signal: &mut ottam_protocol::ConversationSessionSignal,
) {
    if let Some(failure) = signal
        .failure
        .as_mut()
        .filter(|failure| failure.message.contains(REFRESH_LOCK_PHRASE))
    {
        failure.message = REFRESH_LOCK_MESSAGE.into();
    }
}

/// oh-my-pi rejects a prompt while its session is still busy instead of
/// queueing it (#10707), with `-32003` and `data.reason` `session_busy`
/// (`utils/src/acp/transport.ts`).
const SESSION_BUSY_ERROR_CODE: i32 = -32003;

fn is_remote_session_busy(error: &agent_client_protocol::Error) -> bool {
    i32::from(error.code) == SESSION_BUSY_ERROR_CODE
        && error
            .data
            .as_ref()
            .and_then(|data| data.get("reason"))
            .and_then(Value::as_str)
            == Some("session_busy")
}

pub(super) fn map_acp_error(error: &agent_client_protocol::Error) -> HarnessError {
    // Cancellation is checked first: a turn the user cancelled is cancelled
    // regardless of what category the adapter had already attached to it.
    if error.code == ErrorCode::RequestCancelled {
        return HarnessError::new(
            HarnessErrorKind::RequestCancelled,
            sanitize_error_message(&error.message),
        );
    }
    // Before the auth check, because a lock is not a signed-out account and
    // the CLI has been observed describing it in both vocabularies. Offering
    // Sign In here would send the user through a login that fixes nothing.
    if is_refresh_lock_contention(error) {
        return HarnessError::new(HarnessErrorKind::AuthRefreshBusy, REFRESH_LOCK_MESSAGE);
    }
    if is_remote_session_busy(error) {
        return HarnessError::new(
            HarnessErrorKind::RemoteSessionBusy,
            sanitize_error_message(&error.message),
        );
    }
    let kind = if error.code == ErrorCode::AuthRequired || adapter_error_kind_requires_auth(error) {
        HarnessErrorKind::AuthRequired
    } else {
        HarnessErrorKind::Protocol
    };
    HarnessError::new(kind, sanitize_error_message(&error.message))
}

/// Build the ACP content blocks for one prompt: images first, then the text.
///
/// Images lead because the text almost always refers back to them ("what's
/// wrong with this?"), and this is the only place image bytes are read or
/// base64-encoded — everything upstream carries paths.
pub(super) async fn prompt_content_blocks(
    prompt: HarnessPrompt,
) -> Result<Vec<ContentBlock>, HarnessError> {
    use base64::Engine as _;

    let mut blocks = Vec::with_capacity(prompt.images.len() + 1);
    for image in prompt.images {
        let bytes = tokio::fs::read(&image.path).await.map_err(|error| {
            HarnessError::new(
                HarnessErrorKind::PromptExecution,
                format!("failed to read an attached image: {error}"),
            )
        })?;
        let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let mut content = ImageContent::new(data, image.mime_type);
        // `uri` names the file the user attached from, so an agent that can
        // read the workspace can go look at the original. A pasted image has
        // no such file and leaves it unset.
        content.uri = image.uri;
        blocks.push(ContentBlock::Image(content));
    }
    blocks.push(ContentBlock::Text(TextContent::new(prompt.text)));
    Ok(blocks)
}

pub(super) fn map_prompt_execution_error(error: &agent_client_protocol::Error) -> HarnessError {
    let mapped = map_acp_error(error);
    if mapped.kind == HarnessErrorKind::Protocol {
        HarnessError::new(HarnessErrorKind::PromptExecution, mapped.message)
    } else {
        mapped
    }
}

pub(super) fn map_observed_prompt(
    (reason, signal): (
        StopReason,
        Option<ottam_protocol::ConversationSessionSignal>,
    ),
) -> Result<HarnessStopReason, HarnessError> {
    if reason != StopReason::Cancelled {
        if let Some(signal) = signal.filter(|signal| {
            signal
                .failure
                .as_ref()
                .is_some_and(|failure| failure.revision.is_some() && failure.severity == "error")
        }) {
            let mut error = HarnessError::new(
                HarnessErrorKind::PromptExecution,
                signal
                    .failure
                    .as_ref()
                    .expect("terminal failure")
                    .message
                    .clone(),
            );
            error.terminal_signal = Some(Box::new(signal));
            return Err(error);
        }
    }
    Ok(map_stop_reason(reason))
}

pub(super) fn harness_to_acp_error(error: HarnessError) -> agent_client_protocol::Error {
    let code = match error.kind {
        HarnessErrorKind::AuthRequired => ErrorCode::AuthRequired,
        HarnessErrorKind::RequestCancelled => ErrorCode::RequestCancelled,
        _ => ErrorCode::InternalError,
    };
    agent_client_protocol::Error::new(i32::from(code), error.message)
}

pub(super) fn sanitize_error_message(message: &str) -> String {
    let single_line = message.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut characters = single_line.chars();
    let mut sanitized = characters.by_ref().take(240).collect::<String>();
    if characters.next().is_some() {
        sanitized.push('…');
    }
    sanitized
}

#[cfg(test_seams)]
fn line_direction_name(direction: agent_client_protocol::LineDirection) -> &'static str {
    use agent_client_protocol::LineDirection;
    match direction {
        LineDirection::Stdin => "stdin",
        LineDirection::Stdout => "stdout",
        LineDirection::Stderr => "stderr",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{ContentChunk, RequestPermissionResponse};

    /// Core started from a Claude Code session keeps that session's
    /// variables from its agents, but not the ones the launch sets itself.
    #[test]
    fn a_launch_drops_the_host_claude_sessions_variables() {
        let mut definition = AgentDefinition::for_test("claude");
        definition
            .launch
            .environment
            .push(("CLAUDE_CONFIG_DIR".into(), "/ottam/home".into()));
        let inherited = |pairs: &[&str]| {
            pairs
                .iter()
                .map(|key| (key.to_string(), "1".to_string()))
                .collect::<Vec<_>>()
                .into_iter()
        };
        let host = [
            "CLAUDECODE",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CLAUDE_EFFORT",
            "CLAUDE_CONFIG_DIR",
            "ANTHROPIC_BASE_URL",
            "PATH",
        ];
        assert_eq!(
            unset_variables(&definition, inherited(&host)),
            [
                "CLAUDECODE",
                "CLAUDE_CODE_MESSAGING_SOCKET",
                "CLAUDE_CODE_MESSAGING_TOKEN",
                "CLAUDE_EFFORT"
            ]
        );
        // Outside a Claude Code session, a user's own CLAUDE_* settings stay.
        assert!(unset_variables(&definition, inherited(&host[1..])).is_empty());
    }

    #[test]
    fn initialize_uses_acp_v1_and_ottam_identity() {
        let request = initialize_request();
        assert_eq!(request.protocol_version, ProtocolVersion::V1);
        let client_info = request.client_info.expect("client info");
        assert_eq!(client_info.name, CLIENT_NAME);
        assert_eq!(client_info.title.as_deref(), Some(CLIENT_TITLE));
        assert_eq!(client_info.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn terminal_metadata_distinguishes_failure_warning_and_cancellation() {
        let fixtures: Vec<Value> =
            serde_json::from_str(include_str!("../fixtures/session-signals.json")).unwrap();
        let failure = crate::agent::metadata::session_signal(&fixtures[5]);
        let response: agent_client_protocol::schema::v1::PromptResponse = serde_json::from_value(
            serde_json::json!({"stopReason":"end_turn","_meta":fixtures[5]["_meta"]}),
        )
        .unwrap();
        assert_eq!(
            crate::agent::metadata::session_signal(&to_json(&response)),
            failure
        );
        let error = map_observed_prompt((StopReason::EndTurn, failure.clone())).unwrap_err();
        assert_eq!(error.kind, HarnessErrorKind::PromptExecution);
        assert!(error.terminal_signal.is_some());
        assert_eq!(
            map_observed_prompt((StopReason::Cancelled, failure)).unwrap(),
            HarnessStopReason::Cancelled
        );
        let warning = crate::agent::metadata::session_signal(&fixtures[3]);
        assert_eq!(
            map_observed_prompt((StopReason::EndTurn, warning)).unwrap(),
            HarnessStopReason::EndTurn
        );
        for retry in [Value::Null, Value::Bool(false)] {
            let legacy = crate::agent::metadata::session_signal(&serde_json::json!({
                "_meta": {"codex": {"error": {"message": "Observed error", "willRetry": retry}}}
            }));
            assert!(legacy
                .as_ref()
                .unwrap()
                .failure
                .as_ref()
                .unwrap()
                .revision
                .is_none());
            assert_eq!(
                map_observed_prompt((StopReason::EndTurn, legacy)).unwrap(),
                HarnessStopReason::EndTurn
            );
        }
    }

    /// Adapters withhold their terminal auth methods from a client that does not
    /// ask for them. Ottam advertised no capabilities at all, so
    /// `claude-agent-acp` reported an empty `authMethods` list and there was no
    /// sign-in path to offer — assert on the serialized form, since it is the
    /// wire the adapter actually reads.
    #[test]
    fn initialize_advertises_terminal_auth_capabilities() {
        let request = serde_json::to_value(initialize_request()).expect("serialize");
        assert_eq!(
            request.pointer("/clientCapabilities/_meta/jetbrains/air/version"),
            Some(&serde_json::json!(1))
        );
        assert_eq!(
            request.pointer("/clientCapabilities/_meta/jetbrains/air/capabilities"),
            Some(&serde_json::json!([
                "sessionFailure",
                "asyncTasks",
                "nativeSubagentSessions"
            ]))
        );
        let capabilities = &request["clientCapabilities"];
        assert_eq!(capabilities["auth"]["terminal"], Value::Bool(true));
        assert_eq!(
            capabilities["_meta"][TERMINAL_AUTH_META_KEY],
            Value::Bool(true)
        );
        // Claiming the `terminal/*` family would invite tool-terminal requests we
        // cannot serve.
        assert_ne!(capabilities["terminal"], Value::Bool(true));
    }
    #[test]
    fn updates_are_normalized_without_losing_ordered_text() {
        let first = SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
            TextContent::new("Hello "),
        )));
        let second = SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
            TextContent::new("from Codex"),
        )));
        assert_eq!(
            normalize_update(&first),
            HarnessUpdateKind::AgentMessageChunk("Hello ".to_string(), None)
        );
        assert_eq!(
            normalize_update(&second),
            HarnessUpdateKind::AgentMessageChunk("from Codex".to_string(), None)
        );
    }

    #[test]
    fn session_info_update_keeps_title_at_payload_top_level() {
        // The server adopts harness session titles by reading `payload["title"]`
        // (see forward_conversation_update); this pins the normalized payload shape.
        let update = SessionUpdate::SessionInfoUpdate(
            agent_client_protocol::schema::v1::SessionInfoUpdate::new().title("Thread summary"),
        );
        match normalize_update(&update) {
            HarnessUpdateKind::Other { kind, payload } => {
                assert_eq!(kind, "session_info_update");
                assert_eq!(payload["title"], "Thread summary");
            }
            other => panic!("expected Other update, got {other:?}"),
        }
    }

    /// Builds the shape `claude-agent-acp` actually sends for a signed-out
    /// account: a generic internal error carrying the real category in `data`.
    fn internal_error_with_kind(kind: &str) -> agent_client_protocol::Error {
        let mut error = agent_client_protocol::Error::from(ErrorCode::InternalError);
        error.data = Some(serde_json::json!({ "errorKind": kind }));
        error
    }

    /// The captured wire payload from an expired Claude login. It arrives as
    /// `-32603`, not `auth_required`, so classifying on `code` alone reported a
    /// bare "prompt failed" and no sign-in path.
    #[test]
    fn adapter_error_kind_promotes_authentication_failures() {
        for kind in ["authentication_failed", "oauth_org_not_allowed"] {
            let mapped = map_acp_error(&internal_error_with_kind(kind));
            assert_eq!(
                mapped.kind,
                HarnessErrorKind::AuthRequired,
                "{kind} should be treated as signed out"
            );
        }
    }

    /// Every other category the adapter reports stays a prompt failure. Promoting
    /// these would offer "sign in" to someone who is merely out of credits or
    /// rate-limited, and would strand a signed-in account in a signed-out state.
    #[test]
    fn other_adapter_error_kinds_stay_prompt_failures() {
        for kind in [
            "billing_error",
            "rate_limit",
            "overloaded",
            "invalid_request",
            "model_not_found",
            "server_error",
            "max_output_tokens",
            "unknown",
        ] {
            let mapped = map_acp_error(&internal_error_with_kind(kind));
            assert_eq!(
                mapped.kind,
                HarnessErrorKind::Protocol,
                "{kind} should not be treated as signed out"
            );
        }
    }

    /// A refresh blocked by an abandoned lock is not a signed-out account: the
    /// credentials are valid and only the lock is in the way. Reporting it as
    /// `AuthRequired` would offer a sign-in that fixes nothing, and leaving it
    /// `Protocol` renders the CLI's own "close other Claude Code processes"
    /// advice as if it were the agent speaking.
    #[test]
    fn a_blocked_token_refresh_is_its_own_kind() {
        let mut error = agent_client_protocol::Error::from(ErrorCode::InternalError);
        error.message = "Internal error: Failed to refresh OAuth token: another Claude Code \
                         process is refreshing it or exited mid-refresh. This is usually \
                         transient; retry in a minute, and if it persists close other Claude \
                         Code processes or sign in again"
            .to_string();
        let mapped = map_acp_error(&error);
        assert_eq!(mapped.kind, HarnessErrorKind::AuthRefreshBusy);
        assert_eq!(mapped.message, REFRESH_LOCK_MESSAGE);
        // The CLI's advice is replaced, not merely reclassified.
        assert!(!mapped.message.contains("sign in again"));
    }

    /// The classifier tag the CLI already uses internally, in case the adapter
    /// starts forwarding it. Checked before the message match, so this keeps
    /// working when the wording changes.
    #[test]
    fn a_tagged_refresh_lock_failure_needs_no_message_match() {
        let mut error = internal_error_with_kind("oauth_refresh_lock_timeout");
        error.message = "something else entirely".to_string();
        assert_eq!(
            map_acp_error(&error).kind,
            HarnessErrorKind::AuthRefreshBusy
        );
    }

    /// The kind has to survive the prompt path: a refresh can be triggered by
    /// a turn as easily as by `session/new`, and `PromptExecution` would put
    /// the failure back in the agent's mouth.
    #[test]
    fn prompt_execution_mapping_preserves_a_blocked_refresh() {
        let mut error = internal_error_with_kind("oauth_refresh_lock_timeout");
        error.message = "Failed to refresh OAuth token".to_string();
        assert_eq!(
            map_prompt_execution_error(&error).kind,
            HarnessErrorKind::AuthRefreshBusy
        );
    }

    /// What claude-agent-acp 0.95.0 answers an AIR client with: a turn that
    /// ends normally, carrying the CLI's text as its terminal failure.
    #[test]
    fn a_blocked_refresh_reported_as_a_session_failure_is_its_own_kind() {
        let response = serde_json::json!({"_meta": {"jetbrains": {"air": {
            "version": 1,
            "sessionFailure": {
                "id": "failure-1", "revision": 1, "category": "provider_error",
                "severity": "error", "actions": ["retry"],
                "title": "Failed to refresh OAuth token: another Claude Code process is \
                          refreshing it or exited mid-refresh. This is usually transient; \
                          retry in a minute, and if it persists close other Claude Code \
                          processes or sign in again",
            },
        }}}});
        let signal = crate::agent::metadata::session_signal(&response);
        let error = refresh_lock_turn(StopReason::EndTurn, signal.as_ref()).unwrap();
        let mapped = map_prompt_execution_error(&error);
        assert_eq!(mapped.kind, HarnessErrorKind::AuthRefreshBusy);
        assert_eq!(mapped.message, REFRESH_LOCK_MESSAGE);
        assert!(mapped.terminal_signal.is_none(), "the CLI's text is not the last report");
        assert!(refresh_lock_turn(StopReason::Cancelled, signal.as_ref()).is_none());
        let fixtures: Vec<Value> =
            serde_json::from_str(include_str!("../fixtures/session-signals.json")).unwrap();
        let other = crate::agent::metadata::session_signal(&fixtures[5]);
        assert!(refresh_lock_turn(StopReason::EndTurn, other.as_ref()).is_none());
        // Published outside a turn, it is reworded rather than dropped.
        let mut published = signal.unwrap();
        reword_refresh_lock_failure(&mut published);
        assert_eq!(published.failure.unwrap().message, REFRESH_LOCK_MESSAGE);
        let mut unrelated = other.unwrap();
        let before = unrelated.clone();
        reword_refresh_lock_failure(&mut unrelated);
        assert_eq!(unrelated, before);
    }

    /// oh-my-pi's busy rejection (`RequestError.sessionBusy`). The prompt
    /// path keeps the kind so the runner can resend it; `-32003` alone is
    /// any agent's server error and stays a failure.
    #[test]
    fn omp_busy_rejection_maps_to_session_busy_on_the_prompt_path() {
        let mut error = agent_client_protocol::Error::from(ErrorCode::from(-32003));
        error.message = "Session is busy".to_string();
        error.data =
            Some(serde_json::json!({"reason": "session_busy", "hint": "steer|followUp|wait"}));
        assert_eq!(
            map_prompt_execution_error(&error).kind,
            HarnessErrorKind::RemoteSessionBusy
        );
        error.data = Some(serde_json::json!({"reason": "other"}));
        assert_eq!(
            map_prompt_execution_error(&error).kind,
            HarnessErrorKind::PromptExecution
        );
    }

    #[test]
    fn cancellation_wins_over_an_attached_auth_error_kind() {
        let mut error = agent_client_protocol::Error::from(ErrorCode::RequestCancelled);
        error.data = Some(serde_json::json!({ "errorKind": "authentication_failed" }));
        assert_eq!(
            map_acp_error(&error).kind,
            HarnessErrorKind::RequestCancelled
        );
    }

    #[test]
    fn absent_or_malformed_error_data_falls_back_to_the_code() {
        assert_eq!(
            map_acp_error(&agent_client_protocol::Error::from(
                ErrorCode::InternalError
            ))
            .kind,
            HarnessErrorKind::Protocol
        );
        let mut error = agent_client_protocol::Error::from(ErrorCode::InternalError);
        error.data = Some(serde_json::json!({ "errorKind": 7 }));
        assert_eq!(map_acp_error(&error).kind, HarnessErrorKind::Protocol);
        assert_eq!(
            map_acp_error(&agent_client_protocol::Error::from(ErrorCode::AuthRequired)).kind,
            HarnessErrorKind::AuthRequired
        );
    }

    /// The promoted kind has to survive the prompt path too, since that is where
    /// an expired login actually surfaces.
    #[test]
    fn prompt_execution_mapping_preserves_a_promoted_auth_failure() {
        let mapped = map_prompt_execution_error(&internal_error_with_kind("authentication_failed"));
        assert_eq!(mapped.kind, HarnessErrorKind::AuthRequired);
    }
    #[test]
    fn permission_policy_is_cancelled() {
        let response = RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled);
        assert!(matches!(
            response.outcome,
            RequestPermissionOutcome::Cancelled
        ));
    }

    #[test]
    fn permission_outcome_rejects_unknown_option_ids() {
        let allowed = vec!["allow-once".to_string()];
        assert_eq!(
            validate_permission_outcome(
                HarnessPermissionOutcome::Selected {
                    option_id: "unknown".to_string(),
                },
                &allowed,
            ),
            RequestPermissionOutcome::Cancelled
        );
        assert_eq!(
            validate_permission_outcome(
                HarnessPermissionOutcome::Selected {
                    option_id: "allow-once".to_string(),
                },
                &allowed,
            ),
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new("allow-once"))
        );
    }
}
