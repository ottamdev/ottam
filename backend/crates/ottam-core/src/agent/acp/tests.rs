use super::super::types::*;
use super::actor::{
    map_and_record_auth_error, run_command_loop, send_ready, AcpCommandConnection, AdapterBackend,
    AdapterCommand, BackendContext, BackendReady, CommandNewSessionResponse, SharedState,
    COMMAND_CAPACITY,
};
use super::adapter::AcpHarnessAdapter;
use super::features::AcpFeature;
use super::session_config::SessionConfigSetter;
use super::steering::{steering_capability, SteerOutcome, SteerResponse};
use agent_client_protocol::schema::v1::{
    ContentBlock, SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOption,
    SetSessionConfigOptionRequest, StopReason,
};
use agent_client_protocol::ErrorCode;
use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch, Notify};
use tokio::task::JoinHandle;

#[test]
fn acp_normalization_preserves_supplied_and_absent_message_ids() {
    for (kind, reasoning) in [
        ("agent_message_chunk", false),
        ("agent_thought_chunk", true),
    ] {
        for id in [Some("provider-text-42"), None] {
            let mut value =
                serde_json::json!({"sessionUpdate":kind,"content":{"type":"text","text":"hello"}});
            if let Some(id) = id {
                value["messageId"] = serde_json::json!(id);
            }
            let update =
                serde_json::from_value::<agent_client_protocol::schema::v1::SessionUpdate>(value)
                    .unwrap();
            let normalized = super::protocol::normalize_update(&update);
            match normalized {
                HarnessUpdateKind::AgentMessageChunk(text, message_id) if !reasoning => {
                    assert_eq!(text, "hello");
                    assert_eq!(message_id.as_deref(), id);
                }
                HarnessUpdateKind::AgentThoughtChunk(text, message_id) if reasoning => {
                    assert_eq!(text, "hello");
                    assert_eq!(message_id.as_deref(), id);
                }
                other => panic!("unexpected normalized shape: {other:?}"),
            }
        }
    }
}

/// Oh-My-Pi runs MCP tools through `write xd://<tool>`; recorded with an MCP
/// server named `ottam`. Each call comes out as a call of that tool, with
/// omp's caption as its title, not as a file write.
#[test]
fn recorded_omp_device_writes_are_tool_calls() {
    let frames = serde_json::from_str::<Vec<serde_json::Value>>(include_str!(
        "../fixtures/omp-18.16.0-xd-devices.json"
    ))
    .unwrap();
    let calls = frames
        .iter()
        .filter(|frame| frame["params"]["update"]["sessionUpdate"] == "tool_call")
        .map(|frame| {
            let update =
                serde_json::from_value::<agent_client_protocol::schema::v1::SessionUpdate>(
                    frame["params"]["update"].clone(),
                )
                .unwrap();
            match super::xd_devices::rewrite_update(super::protocol::normalize_update(&update)) {
                HarnessUpdateKind::ToolCall(payload) => payload,
                other => panic!("unexpected normalized shape: {other:?}"),
            }
        })
        .map(|call| {
            (
                call["name"].as_str().unwrap_or_default().to_owned(),
                call["kind"].clone(),
                call["title"].clone(),
                call["rawInput"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        calls,
        [
            (
                "mcp__ottam_get_conversation_info".to_owned(),
                serde_json::json!("other"),
                serde_json::json!("Get conversation info"),
                serde_json::json!({}),
            ),
            (
                "mcp__ottam_message_parent".to_owned(),
                serde_json::json!("other"),
                serde_json::json!("Message parent done"),
                serde_json::json!({"message": "done"}),
            ),
        ]
    );
}

/// Each recorded agent reports a turn's tokens on its prompt response and how
/// full its context is on `usage_update`; core keeps both.
#[test]
fn recorded_sessions_carry_turn_usage_and_context_usage() {
    use agent_client_protocol::schema::v1::{PromptResponse, SessionUpdate};
    for (agent, recording) in [
        (
            "claude",
            include_str!("../fixtures/claude-agent-acp-0.100.0.json"),
        ),
        ("codex", include_str!("../fixtures/codex-acp-2.5.0.json")),
        ("opencode", include_str!("../fixtures/opencode-2.2.5.json")),
        ("omp", include_str!("../fixtures/omp-18.16.0.json")),
    ] {
        let messages = serde_json::from_str::<Vec<serde_json::Value>>(recording).unwrap();
        // A cancelled answer may carry no usage (Oh-My-Pi, OpenCode).
        let turns = messages
            .iter()
            .filter(|message| message["result"]["stopReason"] == "end_turn")
            .map(|message| {
                serde_json::from_value::<PromptResponse>(message["result"].clone())
                    .expect("prompt response decodes")
            })
            .collect::<Vec<_>>();
        assert!(!turns.is_empty(), "{agent} recorded a prompt response");
        for turn in turns {
            let usage = super::actor::turn_usage(
                turn.usage
                    .as_ref()
                    .unwrap_or_else(|| panic!("{agent} reports turn usage")),
            );
            assert!(usage.total_tokens > 0, "{agent}: {usage:?}");
            assert!(usage.input_tokens > 0, "{agent}: {usage:?}");
        }
        let contexts = messages
            .iter()
            .filter(|message| {
                message["params"]["update"]["sessionUpdate"].as_str() == Some("usage_update")
            })
            .map(|message| {
                let update =
                    serde_json::from_value::<SessionUpdate>(message["params"]["update"].clone())
                        .expect("usage_update decodes");
                assert!(matches!(update, SessionUpdate::UsageUpdate(_)));
                serde_json::from_value::<ottam_protocol::ConversationContextUsage>(
                    message["params"]["update"].clone(),
                )
                .expect("context usage reads from usage_update")
            })
            .collect::<Vec<_>>();
        assert!(!contexts.is_empty(), "{agent} recorded a usage_update");
        for context in contexts {
            assert!(
                context.size > 0 && context.used <= context.size,
                "{agent}: {context:?}"
            );
        }
    }
}

/// Recorded from the pinned adapters against Ottam's `initialize` (an
/// AIR client), scrubbed. A shape the pinned schema rejects is dropped inside
/// the SDK and never reaches core, so every message must decode.
#[test]
fn recorded_air_sessions_decode_with_the_pinned_schema() {
    use agent_client_protocol::schema::v1::{
        InitializeResponse, NewSessionResponse, PromptResponse, RequestPermissionRequest,
        SessionNotification,
    };
    let mut phases = Vec::new();
    let mut air_capabilities = Vec::new();
    let mut auth_methods = Vec::new();
    let recordings = [
        include_str!("../fixtures/codex-acp-2.5.0.json"),
        include_str!("../fixtures/claude-agent-acp-0.100.0.json"),
    ];
    for message in recordings
        .into_iter()
        .flat_map(|text| serde_json::from_str::<Vec<serde_json::Value>>(text).unwrap())
    {
        let params = message.get("params").cloned().unwrap_or_default();
        match (message["method"].as_str(), message["id"].as_u64()) {
            (Some("session/update"), _) => {
                let notification: SessionNotification =
                    serde_json::from_value(params).expect("session/update decodes");
                if let HarnessUpdateKind::RichMessageChunk { metadata, .. } =
                    super::protocol::normalize_update(&notification.update)
                {
                    phases.extend(metadata.phase);
                }
            }
            (Some("session/request_permission"), _) => {
                serde_json::from_value::<RequestPermissionRequest>(params)
                    .expect("permission request decodes");
            }
            (Some("_auth/status_update"), _) => {
                serde_json::from_value::<super::actor::AuthStatusUpdate>(params)
                    .expect("auth status decodes");
            }
            (None, Some(0)) => {
                let response =
                    serde_json::from_value::<InitializeResponse>(message["result"].clone())
                        .expect("initialize response decodes");
                auth_methods.extend(
                    response
                        .auth_methods
                        .iter()
                        .map(super::protocol::normalize_auth_method),
                );
                air_capabilities.push(
                    super::features::advertised_features(&response)
                        .into_iter()
                        .filter(|(feature, advertised)| {
                            *advertised && feature.air_capability().is_some()
                        })
                        .count(),
                );
            }
            (None, Some(1)) => {
                serde_json::from_value::<NewSessionResponse>(message["result"].clone())
                    .expect("session/new response decodes");
            }
            (None, Some(2)) => {
                serde_json::from_value::<PromptResponse>(message["result"].clone())
                    .expect("prompt response decodes");
            }
            other => panic!("unexpected fixture message {other:?}"),
        }
    }
    // AIR moved the Codex message phase from `_meta.codex` to
    // `_meta.jetbrains.air`. Every streamed chunk carries its phase.
    phases.dedup();
    assert_eq!(phases, ["commentary", "final_answer"]);
    // Every capability either adapter declares has a row: Codex 8, Claude 7.
    assert_eq!(air_capabilities, [8, 7]);
    // Codex's methods are adapter-run logins (`api-key` is kept out of the
    // picker by id); Claude's run the adapter's own command from `_meta`.
    let kinds = auth_methods
        .iter()
        .map(|method| (method.method_id.as_str(), method.kind))
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            ("api-key", HarnessAuthMethodKind::Agent),
            ("chat-gpt", HarnessAuthMethodKind::Agent),
            ("claude-ai-login", HarnessAuthMethodKind::Terminal),
            ("console-login", HarnessAuthMethodKind::Terminal),
        ]
    );
    let login = auth_methods[2].terminal_command.as_ref().unwrap();
    assert_eq!(login.program.as_deref(), Some("/usr/local/bin/node"));
    assert_eq!(
        login.args,
        [
            "/npx/claude-agent-acp",
            "--cli",
            "auth",
            "login",
            "--claudeai"
        ]
    );
    assert_eq!(login.label.as_deref(), Some("Claude Login"));
}

#[test]
fn async_task_frames_decode_for_both_adapters_and_typed_frames_pass_through() {
    use agent_client_protocol::schema::v1::SessionNotification;
    use ottam_protocol::{AsyncTaskState, AsyncTaskUsage};
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/async-tasks.json")).unwrap();
    let mut decoded = Vec::new();
    for adapter in ["claude", "claudeHeld", "codex"] {
        for frame in fixtures[adapter].as_array().unwrap() {
            let params = &frame["params"];
            let tag = params["update"]["sessionUpdate"].as_str().unwrap();
            let typed = serde_json::from_value::<SessionNotification>(params.clone());
            match super::protocol::decode_async_task_update("session/update", params) {
                // The typed handler still owns tool calls, the stamp included.
                None => assert!(typed.is_ok(), "{tag} reaches the typed handler"),
                // The typed parse rejects the tag, so the SDK would drop it.
                Some((session, update)) => {
                    assert!(typed.is_err(), "{tag} needs the raw handler");
                    assert_eq!(session.0, params["sessionId"].as_str().unwrap());
                    decoded.push(update);
                }
            }
        }
    }
    let task = |id: &str| AsyncTaskUpdate {
        async_task_id: id.into(),
        ..Default::default()
    };
    assert_eq!(
        decoded,
        [
            HarnessUpdateKind::AsyncTaskSpawned(AsyncTaskUpdate {
                tool_call_id: Some("toolu_01BackgroundDevServer".into()),
                name: Some("npm run dev".into()),
                task_type: Some("shell".into()),
                description: Some("Start the dev server".into()),
                output_file_path: Some("/tmp/claude/tasks/b7x1k2.output".into()),
                show_in_transcript: Some(true),
                can_stop: Some(true),
                ..task("b7x1k2")
            }),
            HarnessUpdateKind::AsyncTaskProgress(AsyncTaskUpdate {
                summary: Some("Compiled 12 modules".into()),
                last_tool_name: Some("Bash".into()),
                usage: Some(AsyncTaskUsage {
                    total_tokens: Some(1200),
                    tool_uses: Some(3),
                    duration_ms: Some(4500),
                }),
                ..task("b7x1k2")
            }),
            HarnessUpdateKind::AsyncTaskStateUpdate(
                AsyncTaskState::Completed,
                AsyncTaskUpdate {
                    summary: Some("Exited with code 0".into()),
                    ..task("b7x1k2")
                }
            ),
            // A task that ended before its tool call id arrived spawns
            // without one; the id comes on a later state.
            HarnessUpdateKind::AsyncTaskSpawned(AsyncTaskUpdate {
                name: Some("sleep 1".into()),
                task_type: Some("shell".into()),
                description: Some("Wait briefly".into()),
                show_in_transcript: Some(true),
                can_stop: Some(true),
                ..task("h3ld9q")
            }),
            HarnessUpdateKind::AsyncTaskStateUpdate(AsyncTaskState::Completed, task("h3ld9q")),
            HarnessUpdateKind::AsyncTaskStateUpdate(
                AsyncTaskState::Completed,
                AsyncTaskUpdate {
                    tool_call_id: Some("toolu_01HeldSleep".into()),
                    ..task("h3ld9q")
                }
            ),
            // Codex sends no description, and its task id is the call id.
            HarnessUpdateKind::AsyncTaskSpawned(AsyncTaskUpdate {
                tool_call_id: Some("call_background_dev".into()),
                name: Some("npm run dev".into()),
                task_type: Some("shell".into()),
                show_in_transcript: Some(false),
                can_stop: Some(true),
                ..task("call_background_dev")
            }),
            HarnessUpdateKind::AsyncTaskStateUpdate(
                AsyncTaskState::Stopped,
                AsyncTaskUpdate {
                    tool_call_id: Some("call_background_dev".into()),
                    ..task("call_background_dev")
                }
            ),
        ]
    );
}

#[test]
fn malformed_async_task_frames_are_claimed_as_unknown_updates() {
    let decode = |method: &str, params: serde_json::Value| {
        super::protocol::decode_async_task_update(method, &params)
    };
    // Claimed, so the typed parse never drops them silently; `Other` reaches
    // the unknown-update counter instead.
    for update in [
        serde_json::json!({"sessionUpdate": "async_task_spawned"}),
        serde_json::json!({"sessionUpdate": "async_task_progress", "asyncTaskId": ""}),
        // `lost` parses, but only core may declare a task lost.
        serde_json::json!({"sessionUpdate": "async_task_state_update", "asyncTaskId": "t", "state": "lost"}),
        serde_json::json!({"sessionUpdate": "async_task_state_update", "asyncTaskId": "t"}),
        serde_json::json!({"sessionUpdate": "async_task_future", "asyncTaskId": "t"}),
    ] {
        let tag = update["sessionUpdate"].as_str().unwrap().to_owned();
        let claimed = decode(
            "session/update",
            serde_json::json!({"sessionId": "s", "update": update.clone()}),
        );
        assert_eq!(
            claimed,
            Some((
                HarnessSessionId::new("s"),
                HarnessUpdateKind::Other {
                    kind: tag,
                    payload: update,
                }
            ))
        );
    }
    // Declined: `decode_session_update` records the first two as malformed
    // and parses `plan` typed; the actor never routes `_session/other` here.
    let spawned = serde_json::json!({"sessionUpdate": "async_task_spawned", "asyncTaskId": "t"});
    assert_eq!(
        decode(
            "session/update",
            serde_json::json!({"update": spawned.clone()})
        ),
        None
    );
    assert_eq!(
        decode(
            "session/update",
            serde_json::json!({"sessionId": 7, "update": spawned.clone()})
        ),
        None
    );
    assert_eq!(
        decode(
            "_session/other",
            serde_json::json!({"sessionId": "s", "update": spawned})
        ),
        None
    );
    assert_eq!(
        decode(
            "session/update",
            serde_json::json!({"sessionId": "s", "update": {"sessionUpdate": "plan", "entries": []}})
        ),
        None
    );
}

/// Every native subagent frame either recording carries decodes on the
/// parent's session; the typed parse would drop each one.
#[test]
fn native_subagent_frames_decode_for_both_adapters() {
    use agent_client_protocol::schema::v1::SessionNotification;
    use ottam_protocol::SubagentThreadState;
    for (recording, root) in [
        (
            include_str!("../fixtures/claude-native-subagents-0.85.1.json"),
            "claudeNativeTurn",
        ),
        (
            include_str!("../fixtures/codex-native-subagents-2.1.1.json"),
            "codexNativeTurn",
        ),
    ] {
        let fixture: serde_json::Value = serde_json::from_str(recording).unwrap();
        let mut spawned = Vec::new();
        let mut ended = Vec::new();
        for frame in fixture[root].as_array().unwrap() {
            let params = &frame["params"];
            let Some((session, update)) =
                super::protocol::decode_subagent_update(frame["method"].as_str().unwrap(), params)
            else {
                continue;
            };
            assert!(serde_json::from_value::<SessionNotification>(params.clone()).is_err());
            assert_eq!(session.0, params["sessionId"].as_str().unwrap());
            match update {
                HarnessUpdateKind::SubagentSpawned { child, name, .. } => {
                    spawned.push((child, name.expect("named")))
                }
                HarnessUpdateKind::SubagentStateUpdate { child, state } => {
                    ended.push((child, state))
                }
                other => panic!("{root}: {other:?}"),
            }
        }
        assert_eq!(spawned.len(), 3, "{root}");
        assert_eq!(ended.len(), 3, "{root}");
        for (child, state) in &ended {
            assert_eq!(*state, SubagentThreadState::Completed);
            assert!(spawned.iter().any(|(spawned, _)| spawned == child));
        }
    }

    // Claude's resumed run is a new session under the same id, whose prompt
    // is the parent's message; Codex sends no prompt.
    let claude: serde_json::Value = serde_json::from_str(include_str!(
        "../fixtures/claude-native-subagents-0.85.1.json"
    ))
    .unwrap();
    let resumed = claude["claudeNativeResumed"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|frame| {
            super::protocol::decode_subagent_update(
                frame["method"].as_str().unwrap(),
                &frame["params"],
            )
        })
        .map(|(_, update)| update);
    let Some(HarnessUpdateKind::SubagentSpawned { child, prompt, .. }) = resumed else {
        panic!("{resumed:?}");
    };
    assert_eq!(child, "a3700dd2c6acc37d1:generation:2");
    assert!(prompt.is_some_and(|prompt| prompt.contains("count the words")));

    let decode = |update: serde_json::Value| {
        super::protocol::decode_subagent_update(
            "session/update",
            &serde_json::json!({"sessionId": "s", "update": update}),
        )
        .map(|(_, update)| update)
    };
    // Claimed but malformed, so it reaches the unknown-update counter: no
    // child id, an empty one, a live state only core may set, a future tag.
    for update in [
        serde_json::json!({"sessionUpdate": "subagent_spawned"}),
        serde_json::json!({"sessionUpdate": "subagent_spawned", "subagentSessionId": ""}),
        serde_json::json!({"sessionUpdate": "subagent_state_update", "subagentSessionId": "c", "state": "running"}),
        serde_json::json!({"sessionUpdate": "subagent_state_update", "subagentSessionId": "c"}),
        serde_json::json!({"sessionUpdate": "subagent_update", "subagentSessionId": "c"}),
    ] {
        let kind = update["sessionUpdate"].as_str().unwrap().to_owned();
        assert_eq!(
            decode(update.clone()),
            Some(HarnessUpdateKind::Other {
                kind,
                payload: update
            })
        );
    }
    assert_eq!(
        decode(
            serde_json::json!({"sessionUpdate": "subagent_state_update", "subagentSessionId": "c", "state": "disconnected"})
        ),
        Some(HarnessUpdateKind::SubagentStateUpdate {
            child: "c".into(),
            state: SubagentThreadState::Disconnected
        })
    );
}

// Unhandled counts are process-wide, so each test records under its own agent.

#[test]
fn unhandled_session_updates_are_labeled_and_never_forwarded() {
    use crate::diagnostics::unhandled_count;
    use serde_json::json;
    let agent = "unhandled-updates-test";
    let decode = |params: serde_json::Value| super::protocol::decode_session_update(agent, &params);
    let unknown = json!({"sessionId": "s", "update": {
        "sessionUpdate": "brand_new_kind", "secretPayload": "PRIVATE-TEXT"}});
    assert_eq!(decode(unknown), None);
    assert_eq!(unhandled_count(agent, "update:brand_new_kind"), 1);
    // serde quotes this value in its error, which is why the error is never logged.
    let wrong_type = json!({"sessionId": "s", "update": {
        "sessionUpdate": "agent_message_chunk", "content": "PRIVATE-TEXT"}});
    assert_eq!(decode(wrong_type), None);
    // A nested unknown variant names its own tag, not the update's.
    let nested_unknown = json!({"sessionId": "s", "update": {
        "sessionUpdate": "agent_message_chunk", "content": {"type": "hologram"}}});
    assert_eq!(decode(nested_unknown), None);
    let no_session = json!({"update": {
        "sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi"}}});
    assert_eq!(decode(no_session), None);
    assert_eq!(unhandled_count(agent, "malformed:agent_message_chunk"), 3);
    // Core knows this kind even though the SDK doesn't.
    let declined_task = json!({"update": {
        "sessionUpdate": "async_task_spawned", "asyncTaskId": "t"}});
    assert_eq!(decode(declined_task), None);
    assert_eq!(unhandled_count(agent, "malformed:async_task_spawned"), 1);
    assert_eq!(unhandled_count(agent, "update:async_task_spawned"), 0);
}

#[test]
fn unhandled_fields_are_recorded_and_the_update_still_forwarded() {
    use crate::diagnostics::unhandled_count;
    let agent = "unhandled-fields-test";
    let params = serde_json::json!({"sessionId": "s", "update": {
        "sessionUpdate": "agent_message_chunk",
        "content": {"type": "text", "text": "hi"},
        "brandNewField": "PRIVATE-FIELD"}});
    assert_eq!(
        super::protocol::decode_session_update(agent, &params),
        Some((
            HarnessSessionId::new("s"),
            HarnessUpdateKind::AgentMessageChunk("hi".into(), None)
        ))
    );
    // The round-trip check runs in debug builds only.
    assert_eq!(
        unhandled_count(agent, "field:agent_message_chunk"),
        u64::from(cfg!(debug_assertions))
    );
}

#[test]
fn unhandled_fields_excuse_defaults_only_where_the_sdk_omits_them() {
    use super::protocol::{is_omitted_default, to_json};
    use crate::diagnostics::dropped_paths;
    use agent_client_protocol::schema::v1::SessionNotification;
    use serde_json::json;
    let dropped = |params: serde_json::Value| {
        let tag = params["update"]["sessionUpdate"]
            .as_str()
            .unwrap()
            .to_owned();
        let typed =
            to_json(&serde_json::from_value::<SessionNotification>(params.clone()).unwrap());
        dropped_paths(&params, &typed, &|path, value| {
            is_omitted_default(&tag, path, value)
        })
    };
    // `ToolCall` serializes these defaults away; nothing is lost.
    let tool_call = json!({"sessionId": "s", "update": {"sessionUpdate": "tool_call",
        "toolCallId": "t", "title": "Run", "kind": "other", "status": "pending"}});
    assert!(dropped(tool_call).is_empty());
    // The same values anywhere else are fields the model doesn't have.
    let elsewhere = json!({"sessionId": "s", "status": "pending", "update": {
        "sessionUpdate": "agent_message_chunk",
        "content": {"type": "text", "text": "hi"}, "kind": "other"}});
    assert_eq!(dropped(elsewhere), ["status", "update.kind"]);
    // The SDK skips a list item it can't parse rather than rejecting the frame.
    let skipped_item = json!({"sessionId": "s", "update": {"sessionUpdate": "tool_call",
        "toolCallId": "t", "title": "Run", "content": [
            {"type": "content", "content": {"type": "text", "text": "ok"}},
            {"type": "hologram"}]}});
    assert_eq!(dropped(skipped_item), ["update.content[]"]);
}

/// What a pin bump must not lose: every recorded `session/update` survives
/// the typed round-trip whole, and every method the adapters send has a
/// handler. A failure names the field or method to model before bumping.
#[test]
fn unhandled_nothing_in_the_recorded_sessions() {
    use super::protocol::{is_omitted_default, to_json};
    use crate::diagnostics::dropped_paths;
    use agent_client_protocol::schema::v1::{
        CancelRequestNotification, CreateElicitationRequest, RequestPermissionRequest,
        SessionNotification,
    };
    let recorded = [
        include_str!("../fixtures/codex-acp-2.5.0.json"),
        include_str!("../fixtures/claude-agent-acp-0.100.0.json"),
        include_str!("../fixtures/opencode-2.2.5.json"),
        include_str!("../fixtures/omp-18.16.0.json"),
    ]
    .into_iter()
    .flat_map(|text| serde_json::from_str::<Vec<serde_json::Value>>(text).unwrap());
    let signals = serde_json::from_str::<Vec<serde_json::Value>>(include_str!(
        "../fixtures/session-signals.json"
    ))
    .unwrap()
    .into_iter()
    .map(|update| {
        serde_json::json!({"method": "session/update",
                    "params": {"sessionId": "s", "update": update}})
    });
    let mut checked = 0;
    for message in recorded.chain(signals) {
        let params = &message["params"];
        let tag = params["update"]["sessionUpdate"].as_str().unwrap_or("none");
        let (method, typed) = match message["method"].as_str() {
            Some(method @ "session/update") => (
                method,
                to_json(&serde_json::from_value::<SessionNotification>(params.clone()).unwrap()),
            ),
            Some(method @ "session/request_permission") => (
                method,
                to_json(
                    &serde_json::from_value::<RequestPermissionRequest>(params.clone()).unwrap(),
                ),
            ),
            Some(method @ "elicitation/create") => {
                if let Err(refusal) = super::elicitation::parse(params) {
                    panic!("core can't show a recorded form ({refusal:?}): {params}");
                }
                (
                    method,
                    to_json(
                        &serde_json::from_value::<CreateElicitationRequest>(params.clone())
                            .unwrap(),
                    ),
                )
            }
            Some(method @ "_auth/status_update") => (
                method,
                to_json(
                    &serde_json::from_value::<super::actor::AuthStatusUpdate>(params.clone())
                        .unwrap(),
                ),
            ),
            Some(method @ "$/cancel_request") => (
                method,
                to_json(
                    &serde_json::from_value::<CancelRequestNotification>(params.clone()).unwrap(),
                ),
            ),
            Some(method) => panic!("no handler claims {method}"),
            None => continue,
        };
        let dropped = dropped_paths(params, &typed, &|path, value| {
            is_omitted_default(tag, path, value)
        });
        assert!(dropped.is_empty(), "{method} loses {dropped:?}: {params}");
        checked += 1;
    }
    assert!(checked > 40, "only {checked} messages checked");
}

/// What the scripted connection answers a `_session/steering` request with.
/// `None` is the ordinary case: the steer joined the running turn.
#[derive(Clone)]
enum ScriptedSteer {
    Outcome(SteerOutcome),
    MethodNotFound,
}

#[derive(Clone, Default)]
struct ScriptedCommandConnection {
    calls: Arc<Mutex<Vec<String>>>,
    auth_started: Arc<Notify>,
    auth_release: Arc<Notify>,
    prompt_started: Arc<Notify>,
    prompt_release: Arc<Notify>,
    steer_answer: Arc<Mutex<Option<ScriptedSteer>>>,
    /// What `session/new` advertises; `None` is an agent with no selectors.
    config_options: Arc<Mutex<Option<Vec<SessionConfigOption>>>>,
    /// Holds `session/set_config_option` until `config_release`, like an
    /// agent busy with a turn.
    hold_config: Arc<std::sync::atomic::AtomicBool>,
    config_started: Arc<Notify>,
    config_release: Arc<Notify>,
}

impl ScriptedCommandConnection {
    fn record(&self, call: impl Into<String>) {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(call.into());
    }

    fn answer_steer_with(&self, answer: ScriptedSteer) {
        *self
            .steer_answer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(answer);
    }

    fn recorded_calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// Servers a scripted session was given, as ` mcp:<name>=<url>`; nothing
/// when it was given none, so calls without servers record as before.
fn recorded_mcp(servers: &[McpServerEntry]) -> String {
    servers
        .iter()
        .map(|server| format!(" mcp:{}={}", server.name, server.url))
        .collect()
}

impl AcpCommandConnection for ScriptedCommandConnection {
    fn authenticate(
        &self,
        method_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), agent_client_protocol::Error>> + Send + '_>> {
        Box::pin(async move {
            self.record(format!("authenticate:{method_id}"));
            self.auth_started.notify_one();
            self.auth_release.notified().await;
            Ok(())
        })
    }

    fn new_session(
        &self,
        cwd: PathBuf,
        mcp_servers: Vec<McpServerEntry>,
        meta: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<CommandNewSessionResponse, agent_client_protocol::Error>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let meta = meta
                .map(|meta| format!(" meta={}", serde_json::Value::Object(meta)))
                .unwrap_or_default();
            self.record(format!(
                "new:{}{}{meta}",
                cwd.display(),
                recorded_mcp(&mcp_servers)
            ));
            let config_options = self
                .config_options
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            Ok(CommandNewSessionResponse {
                session_id: "new-session".to_string(),
                config_options,
            })
        })
    }

    fn load_session(
        &self,
        session_id: String,
        cwd: PathBuf,
        mcp_servers: Vec<McpServerEntry>,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<Option<Vec<SessionConfigOption>>, agent_client_protocol::Error>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            self.record(format!(
                "load:{session_id}:{}{}",
                cwd.display(),
                recorded_mcp(&mcp_servers)
            ));
            Ok(self.config_options.lock().expect("config options").clone())
        })
    }

    fn delete_session(
        &self,
        session_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), agent_client_protocol::Error>> + Send + '_>> {
        Box::pin(async move {
            self.record(format!("delete:{session_id}"));
            Ok(())
        })
    }

    fn close_session(
        &self,
        session_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), agent_client_protocol::Error>> + Send + '_>> {
        Box::pin(async move {
            self.record(format!("close:{session_id}"));
            Ok(())
        })
    }

    fn prompt(
        &self,
        session_id: String,
        _blocks: Vec<ContentBlock>,
    ) -> Pin<Box<dyn Future<Output = Result<StopReason, agent_client_protocol::Error>> + Send + '_>>
    {
        Box::pin(async move {
            self.record(format!("prompt:{session_id}"));
            self.prompt_started.notify_one();
            self.prompt_release.notified().await;
            Ok(StopReason::EndTurn)
        })
    }

    fn steer(
        &self,
        session_id: String,
        _blocks: Vec<ContentBlock>,
    ) -> Pin<Box<dyn Future<Output = Result<SteerOutcome, agent_client_protocol::Error>> + Send + '_>>
    {
        Box::pin(async move {
            self.record(format!("steer:{session_id}"));
            match self
                .steer_answer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
            {
                None => Ok(SteerOutcome::Injected),
                Some(ScriptedSteer::Outcome(outcome)) => Ok(outcome),
                Some(ScriptedSteer::MethodNotFound) => Err(agent_client_protocol::Error::new(
                    ErrorCode::MethodNotFound.into(),
                    "Method not found",
                )),
            }
        })
    }

    fn stop_async_task(
        &self,
        session_id: String,
        async_task_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<bool, agent_client_protocol::Error>> + Send + '_>> {
        Box::pin(async move {
            self.record(format!("stop_async_task:{session_id}:{async_task_id}"));
            Ok(true)
        })
    }

    fn cancel(&self, session_id: String) -> Result<(), agent_client_protocol::Error> {
        self.record(format!("cancel:{session_id}"));
        Ok(())
    }
}

impl SessionConfigSetter for ScriptedCommandConnection {
    fn set_session_config(
        &self,
        request: SetSessionConfigOptionRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<SessionConfigOption>, agent_client_protocol::Error>>
                + Send
                + '_,
        >,
    > {
        let agent_client_protocol::schema::v1::SessionConfigOptionValue::ValueId { value } =
            request.value
        else {
            panic!("expected select");
        };
        self.record(format!(
            "config:{}:{}:{}",
            request.session_id.0, request.config_id.0, value.0
        ));
        Box::pin(async move {
            if self.hold_config.load(std::sync::atomic::Ordering::SeqCst) {
                self.config_started.notify_one();
                self.config_release.notified().await;
            }
            Ok(vec![SessionConfigOption::select(
                request.config_id,
                "Mode",
                value,
                Vec::<SessionConfigSelectOption>::new(),
            )
            .category(SessionConfigOptionCategory::Mode)])
        })
    }
}

fn command_loop_shared() -> Arc<SharedState> {
    let (status_tx, _) = watch::channel(AdapterStatus::stopped());
    let updates_tx = HarnessUpdateSender::default();
    Arc::new(SharedState {
        status_tx,
        updates_tx,
        sessions: Mutex::new(BTreeMap::new()),
        update_sequences: Mutex::new(BTreeMap::new()),
        announced_commands: Mutex::new(BTreeMap::new()),
        child_sessions: None,
        sign_in_proof: SignInProof::Session,
    })
}

fn spawn_command_loop(
    connection: ScriptedCommandConnection,
    shared: Arc<SharedState>,
) -> (
    mpsc::Sender<AdapterCommand>,
    JoinHandle<Result<(), HarnessError>>,
) {
    let (command_tx, mut command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let task =
        tokio::spawn(
            async move { run_command_loop(&connection, &mut command_rx, &shared, None).await },
        );
    (command_tx, task)
}

#[tokio::test]
async fn existing_claude_refresh_wait_cancellation_keeps_command_transport_usable() {
    let home = tempfile::tempdir().unwrap();
    let lock = context_lock(home.path());
    std::fs::create_dir(&lock).unwrap();
    let shared = command_loop_shared();
    shared.set_lifecycle(AdapterLifecycleState::Ready, None);
    let (commands, mut receiver) = mpsc::channel(COMMAND_CAPACITY);
    let actor_lock = lock.clone();
    let actor = tokio::spawn(async move {
        run_command_loop(
            &ScriptedCommandConnection::default(),
            &mut receiver,
            &shared,
            Some(&actor_lock),
        )
        .await
    });
    let (reply, response) = oneshot::channel();
    commands
        .send(AdapterCommand::Shutdown { reply })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!actor.is_finished());
    drop(response); // Cancelling preparation closes its acknowledgement waiter.
    let (reply, response) = oneshot::channel();
    commands
        .send(AdapterCommand::NewSession {
            cwd: home.path().into(),
            config: SessionConfigRequest::default(),
            reply,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), response)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(lock.exists());
    std::fs::remove_dir(lock).unwrap();
    let (reply, response) = oneshot::channel();
    commands
        .send(AdapterCommand::Shutdown { reply })
        .await
        .unwrap();
    response.await.unwrap();
    actor.await.unwrap().unwrap();
}

#[tokio::test]
async fn explicit_auth_session_success_does_not_override_reported_sign_out() {
    use super::actor::AuthStatusUpdate;
    let shared = command_loop_shared();
    shared
        .status_tx
        .send_modify(|status| status.capabilities.auth_status = true);
    use agent_client_protocol::JsonRpcMessage;
    let signed_out = AuthStatusUpdate::parse_message(
        "_auth/status_update",
        &serde_json::json!({
            "authStatus": {"kind": "none", "label": "Not logged in"}
        }),
    )
    .expect("decode upstream notification");
    let connection = ScriptedCommandConnection::default();
    let (command_tx, mut command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let task_shared = Arc::clone(&shared);
    let task = tokio::spawn(async move {
        run_command_loop(&connection, &mut command_rx, &task_shared, None).await
    });
    let (reply, response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::NewSession {
            cwd: PathBuf::from("/tmp/project"),
            config: SessionConfigRequest::default(),
            reply,
        })
        .await
        .expect("queue session");
    let session_id = response
        .await
        .expect("response")
        .expect("session opens signed out");
    assert_eq!(shared.status().auth_state, HarnessAuthState::Unknown);
    assert!(!shared.status().credentials_seen);
    signed_out.record(&shared);
    let (reply, response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::LoadSession {
            session_id,
            cwd: PathBuf::from("/tmp/project"),
            mode_id: None,
            mcp_servers: Vec::new(),
            reply,
        })
        .await
        .expect("queue load");
    response
        .await
        .expect("response")
        .expect("session loads signed out");
    assert_eq!(shared.status().auth_state, HarnessAuthState::Missing);
    assert!(
        !shared.status().credentials_seen,
        "first login must not be called a lapse"
    );
    for kind in ["account", "api_key", "gateway", "external"] {
        AuthStatusUpdate {
            auth_status: serde_json::json!({"kind": kind}),
        }
        .record(&shared);
        assert_eq!(shared.status().auth_state, HarnessAuthState::Ready);
        signed_out.record(&shared);
    }
    AuthStatusUpdate {
        auth_status: serde_json::json!({"kind": "future"}),
    }
    .record(&shared);
    assert_eq!(shared.status().auth_state, HarnessAuthState::Missing);
    stop_command_loop(&command_tx, task).await;
}

async fn stop_command_loop(
    command_tx: &mpsc::Sender<AdapterCommand>,
    task: JoinHandle<Result<(), HarnessError>>,
) {
    let (reply, response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Shutdown { reply })
        .await
        .expect("command loop accepts shutdown");
    response.await.expect("shutdown acknowledged");
    task.await
        .expect("command loop task")
        .expect("command loop exits cleanly");
}

#[test]
fn auth_required_maps_to_missing_state() {
    let (status_tx, _) = watch::channel(AdapterStatus::stopped());
    let updates_tx = HarnessUpdateSender::default();
    let shared = SharedState {
        status_tx,
        updates_tx,
        sessions: Mutex::new(BTreeMap::new()),
        update_sequences: Mutex::new(BTreeMap::new()),
        announced_commands: Mutex::new(BTreeMap::new()),
        child_sessions: None,
        sign_in_proof: SignInProof::Session,
    };
    let error = agent_client_protocol::Error::from(ErrorCode::AuthRequired);
    let mapped = map_and_record_auth_error(&shared, &error);
    assert_eq!(mapped.kind, HarnessErrorKind::AuthRequired);
    assert_eq!(shared.status().auth_state, HarnessAuthState::Missing);
}

/// The whole point of the latch: an adapter that worked and then stopped
/// working is a lapse, and only a lapse should ever be described that way.
#[test]
fn credentials_seen_latches_once_the_adapter_has_authenticated() {
    let (status_tx, _) = watch::channel(AdapterStatus::stopped());
    let updates_tx = HarnessUpdateSender::default();
    let shared = SharedState {
        status_tx,
        updates_tx,
        sessions: Mutex::new(BTreeMap::new()),
        update_sequences: Mutex::new(BTreeMap::new()),
        announced_commands: Mutex::new(BTreeMap::new()),
        child_sessions: None,
        sign_in_proof: SignInProof::Session,
    };
    assert!(!shared.status().credentials_seen);

    shared.set_auth(HarnessAuthState::Ready, None);
    assert!(shared.status().credentials_seen);

    map_and_record_auth_error(
        &shared,
        &agent_client_protocol::Error::from(ErrorCode::AuthRequired),
    );
    let status = shared.status();
    assert_eq!(status.auth_state, HarnessAuthState::Missing);
    assert!(status.credentials_seen, "a lapse must stay distinguishable");

    // A restarted process has observed nothing, so the evidence is gone too.
    shared.begin_start();
    assert!(!shared.status().credentials_seen);
}

/// The cold case: never authenticated, so nothing to have lapsed.
#[test]
fn credentials_seen_stays_false_when_auth_was_never_established() {
    let (status_tx, _) = watch::channel(AdapterStatus::stopped());
    let updates_tx = HarnessUpdateSender::default();
    let shared = SharedState {
        status_tx,
        updates_tx,
        sessions: Mutex::new(BTreeMap::new()),
        update_sequences: Mutex::new(BTreeMap::new()),
        announced_commands: Mutex::new(BTreeMap::new()),
        child_sessions: None,
        sign_in_proof: SignInProof::Session,
    };
    shared.begin_start();
    map_and_record_auth_error(
        &shared,
        &agent_client_protocol::Error::from(ErrorCode::AuthRequired),
    );
    let status = shared.status();
    assert_eq!(status.auth_state, HarnessAuthState::Missing);
    assert!(!status.credentials_seen);
}

#[test]
fn restarting_an_adapter_clears_prior_generation_session_catalog() {
    let shared = command_loop_shared();
    shared.set_models(
        vec![AgentModelOption {
            model_id: "old".into(),
            display_name: "Old model".into(),
        }],
        Some("old".into()),
    );
    shared.set_session_selects(AgentSessionSelects::default());
    shared.mark_stopped("test");
    // Stopping preserves the last observation, but the next process must
    // create a session before those fields can count as a live observation.
    assert_eq!(shared.status().models.len(), 1);
    let generation = shared.status().generation;
    shared.begin_start();
    let status = shared.status();
    assert_eq!(status.generation, generation + 1);
    assert!(status.models.is_empty());
    assert!(status.default_model_id.is_none());
    assert!(status.session_selects.is_none());
}

#[test]
fn non_auth_failures_reset_auth_and_store_a_bounded_single_line_error() {
    let (status_tx, _) = watch::channel(AdapterStatus::stopped());
    let updates_tx = HarnessUpdateSender::default();
    let shared = SharedState {
        status_tx,
        updates_tx,
        sessions: Mutex::new(BTreeMap::new()),
        update_sequences: Mutex::new(BTreeMap::new()),
        announced_commands: Mutex::new(BTreeMap::new()),
        child_sessions: None,
        sign_in_proof: SignInProof::Session,
    };
    shared.set_auth(HarnessAuthState::Ready, None);
    let error = agent_client_protocol::Error::new(
        i32::from(ErrorCode::InternalError),
        format!("first line\n{}", "x".repeat(300)),
    );
    map_and_record_auth_error(&shared, &error);
    let status = shared.status();
    assert_eq!(status.auth_state, HarnessAuthState::Unknown);
    let last_error = status.last_error.expect("sanitized error");
    assert!(!last_error.contains('\n'));
    assert!(last_error.chars().count() <= 241);
}

#[test]
fn adapter_exit_marks_every_session_dead() {
    let (status_tx, _) = watch::channel(AdapterStatus::stopped());
    let updates_tx = HarnessUpdateSender::default();
    let shared = SharedState {
        status_tx,
        updates_tx,
        sessions: Mutex::new(BTreeMap::new()),
        update_sequences: Mutex::new(BTreeMap::new()),
        announced_commands: Mutex::new(BTreeMap::new()),
        child_sessions: None,
        sign_in_proof: SignInProof::Session,
    };
    shared.insert_session(HarnessSessionId::new("session-1"), PathBuf::from("/tmp"));
    shared.mark_dead(&format!("adapter\n{}", "x".repeat(300)));
    let sessions = shared.sessions();
    assert_eq!(sessions[0].state, HarnessSessionState::Dead);
    let last_error = sessions[0].last_error.as_deref().expect("session error");
    assert!(!last_error.contains('\n'));
    assert!(last_error.chars().count() <= 241);
    assert_eq!(shared.status().lifecycle, AdapterLifecycleState::Dead);
}

#[test]
fn commands_announced_before_a_session_opens_reach_its_summary() {
    let (status_tx, _) = watch::channel(AdapterStatus::stopped());
    let updates_tx = HarnessUpdateSender::default();
    let shared = SharedState {
        status_tx,
        updates_tx,
        sessions: Mutex::new(BTreeMap::new()),
        update_sequences: Mutex::new(BTreeMap::new()),
        announced_commands: Mutex::new(BTreeMap::new()),
        child_sessions: None,
        sign_in_proof: SignInProof::Session,
    };
    let session_id = HarnessSessionId::new("session-1");
    let payload = serde_json::json!({
        "sessionUpdate": "available_commands_update",
        "availableCommands": [{"name": "review", "description": "Review changes"}]
    });
    // Agents announce commands while `session/new` is still in flight.
    shared.send_update(
        session_id.clone(),
        HarnessUpdateKind::Other {
            kind: "available_commands_update".into(),
            payload: payload.clone(),
        },
    );
    shared.insert_session(session_id.clone(), PathBuf::from("/tmp"));
    assert_eq!(shared.sessions()[0].announced_commands, Some(payload));
    shared.remove_session(&session_id);
    shared.insert_session(session_id, PathBuf::from("/tmp"));
    assert_eq!(shared.sessions()[0].announced_commands, None);
}

#[test]
fn announced_modes_update_the_session_summary() {
    let (status_tx, _) = watch::channel(AdapterStatus::stopped());
    let updates_tx = HarnessUpdateSender::default();
    let shared = SharedState {
        status_tx,
        updates_tx,
        sessions: Mutex::new(BTreeMap::new()),
        update_sequences: Mutex::new(BTreeMap::new()),
        announced_commands: Mutex::new(BTreeMap::new()),
        child_sessions: None,
        sign_in_proof: SignInProof::Session,
    };
    let session_id = HarnessSessionId::new("session-1");
    shared.insert_session(session_id.clone(), PathBuf::from("/tmp"));
    shared.set_session_mode(&session_id, Some("plan".into()));
    let mode = || shared.sessions()[0].current_mode_id.clone();
    shared.send_update(
        session_id.clone(),
        HarnessUpdateKind::Other {
            kind: "current_mode_update".into(),
            payload: serde_json::json!({"sessionUpdate":"current_mode_update","currentModeId":"default"}),
        },
    );
    assert_eq!(mode().as_deref(), Some("default"));
    shared.send_update(
        session_id.clone(),
        HarnessUpdateKind::Other {
            kind: "config_option_update".into(),
            payload: serde_json::json!({"configOptions":[
                {"id":"model","category":"model","currentValue":"opus"},
                {"id":"mode","category":"mode","currentValue":"acceptEdits"}]}),
        },
    );
    assert_eq!(mode().as_deref(), Some("acceptEdits"));
    shared.send_update(
        session_id,
        HarnessUpdateKind::Other {
            kind: "usage_update".into(),
            payload: serde_json::json!({"used": 1}),
        },
    );
    assert_eq!(
        mode().as_deref(),
        Some("acceptEdits"),
        "other updates leave the mode"
    );
}

#[test]
fn loading_a_prompting_session_is_rejected_without_overwriting_state() {
    let (status_tx, _) = watch::channel(AdapterStatus::stopped());
    let updates_tx = HarnessUpdateSender::default();
    let shared = SharedState {
        status_tx,
        updates_tx,
        sessions: Mutex::new(BTreeMap::new()),
        update_sequences: Mutex::new(BTreeMap::new()),
        announced_commands: Mutex::new(BTreeMap::new()),
        child_sessions: None,
        sign_in_proof: SignInProof::Session,
    };
    let session_id = HarnessSessionId::new("session-1");
    shared.insert_session(session_id.clone(), PathBuf::from("/tmp"));
    shared.begin_prompt(&session_id).expect("begin prompt");

    let error = shared
        .ensure_loadable(&session_id)
        .expect_err("prompting session must not be loaded");
    assert_eq!(error.kind, HarnessErrorKind::SessionBusy);
    assert_eq!(shared.sessions()[0].state, HarnessSessionState::Prompting);
}

#[test]
fn cancellation_only_notifies_for_a_live_prompting_session() {
    let (status_tx, _) = watch::channel(AdapterStatus::stopped());
    let updates_tx = HarnessUpdateSender::default();
    let shared = SharedState {
        status_tx,
        updates_tx,
        sessions: Mutex::new(BTreeMap::new()),
        update_sequences: Mutex::new(BTreeMap::new()),
        announced_commands: Mutex::new(BTreeMap::new()),
        child_sessions: None,
        sign_in_proof: SignInProof::Session,
    };
    let session_id = HarnessSessionId::new("session-1");
    shared.insert_session(session_id.clone(), PathBuf::from("/tmp"));
    assert!(!shared.is_prompting(&session_id).expect("active session"));
    shared.begin_prompt(&session_id).expect("begin prompt");
    assert!(shared.is_prompting(&session_id).expect("prompting session"));
    shared.mark_sessions_dead("adapter exited");
    assert_eq!(
        shared
            .is_prompting(&session_id)
            .expect_err("dead session")
            .kind,
        HarnessErrorKind::AdapterDead
    );
    assert_eq!(
        shared
            .is_prompting(&HarnessSessionId::new("missing"))
            .expect_err("missing session")
            .kind,
        HarnessErrorKind::SessionNotFound
    );
}

#[tokio::test]
async fn command_loop_keeps_auth_responsive_and_cancels_it_on_shutdown() {
    let connection = ScriptedCommandConnection::default();
    let shared = command_loop_shared();
    let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));

    let (auth_reply, auth_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Authenticate {
            method_id: "login".to_string(),
            reply: auth_reply,
        })
        .await
        .expect("starts authentication");
    connection.auth_started.notified().await;

    let (duplicate_reply, duplicate_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Authenticate {
            method_id: "login".to_string(),
            reply: duplicate_reply,
        })
        .await
        .expect("checks duplicate authentication");
    assert_eq!(
        duplicate_response
            .await
            .expect("duplicate authentication response")
            .expect_err("duplicate authentication is rejected")
            .kind,
        HarnessErrorKind::SessionBusy
    );

    // A browser login may wait indefinitely. The actor must still service
    // the session command that login exists to unblock.
    let (new_reply, new_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::NewSession {
            cwd: PathBuf::from("/tmp/project"),
            config: SessionConfigRequest::default(),
            reply: new_reply,
        })
        .await
        .expect("queues session creation while authenticating");
    assert_eq!(
        new_response.await.expect("new session response"),
        Ok(HarnessSessionId::new("new-session"))
    );

    let (shutdown_reply, shutdown_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Shutdown {
            reply: shutdown_reply,
        })
        .await
        .expect("queues shutdown");
    shutdown_response.await.expect("shutdown acknowledged");
    assert_eq!(
        auth_response
            .await
            .expect("authentication response")
            .expect_err("outstanding authentication is cancelled")
            .kind,
        HarnessErrorKind::RequestCancelled
    );
    task.await
        .expect("command loop task")
        .expect("command loop exits cleanly");
    assert_eq!(
        connection.recorded_calls(),
        vec![
            "authenticate:login".to_string(),
            "new:/tmp/project".to_string()
        ]
    );
}

/// The model-catalog probe's `_meta` rides `session/new` unchanged.
#[tokio::test]
async fn session_meta_reaches_session_new() {
    let connection = ScriptedCommandConnection::default();
    let shared = command_loop_shared();
    let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));
    let meta = serde_json::json!({"claudeCode": {"options": {"persistSession": false}}});
    let (reply, response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::NewSession {
            cwd: PathBuf::from("/tmp/project"),
            config: SessionConfigRequest {
                meta: meta.as_object().cloned(),
                ..SessionConfigRequest::default()
            },
            reply,
        })
        .await
        .expect("queues session creation");
    response.await.expect("new session response").expect("session opens");
    drop(command_tx);
    task.await
        .expect("command loop task")
        .expect("command loop exits cleanly");
    assert_eq!(
        connection.recorded_calls(),
        vec![format!("new:/tmp/project meta={meta}")]
    );
}

/// A model id picked from a stale catalog reaches `session/new` after the
/// agent stopped offering it. The session is refused — never inserted, so no
/// prompt can ride it — while the handshake still counts as proof of
/// credentials and the freshly advertised models are recorded for the next
/// pick.
#[tokio::test]
async fn a_stale_selection_refuses_the_session_but_keeps_what_the_handshake_learned() {
    let connection = ScriptedCommandConnection::default();
    *connection
        .config_options
        .lock()
        .expect("config options lock") = Some(vec![SessionConfigOption::select(
        "model",
        "Model",
        "opus[1m]",
        vec![
            SessionConfigSelectOption::new("opus[1m]", "Opus (1M context)"),
            SessionConfigSelectOption::new("claude-fable-5-1[1m]", "Fable"),
        ],
    )
    .category(SessionConfigOptionCategory::Model)]);
    let shared = command_loop_shared();
    let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));

    let (reply, response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::NewSession {
            cwd: PathBuf::from("/tmp/project"),
            config: SessionConfigRequest {
                model_id: Some("claude-fable-5[1m]".to_string()),
                mode_id: None,
                thought_level_id: None,
                mcp_servers: Vec::new(),
                meta: None,
            },
            reply,
        })
        .await
        .expect("queues session creation");
    let error = response
        .await
        .expect("new session response")
        .expect_err("a stale model id refuses the session");
    assert_eq!(error.kind, HarnessErrorKind::SelectionUnavailable);

    let status = shared.status();
    assert_eq!(status.auth_state, HarnessAuthState::Ready);
    assert_eq!(
        status
            .models
            .iter()
            .map(|model| model.model_id.as_str())
            .collect::<Vec<_>>(),
        ["opus[1m]", "claude-fable-5-1[1m]"],
        "the live list replaces the stale one even though the session was refused"
    );
    assert!(
        shared.sessions.lock().expect("sessions lock").is_empty(),
        "a refused session is never registered"
    );

    stop_command_loop(&command_tx, task).await;
}

#[tokio::test]
async fn command_loop_cancel_authentication_restores_state_and_is_idempotent() {
    check_cancel_authentication(None).await;
}

#[tokio::test]
async fn cancel_authentication_preserves_newer_notification_verdicts() {
    for kind in ["account", "none"] {
        check_cancel_authentication(Some(kind)).await;
    }
}

async fn check_cancel_authentication(notification_kind: Option<&str>) {
    let connection = ScriptedCommandConnection::default();
    let shared = command_loop_shared();
    shared.set_auth(
        if notification_kind == Some("none") {
            HarnessAuthState::Ready
        } else {
            HarnessAuthState::Missing
        },
        None,
    );
    let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));

    let (auth_reply, auth_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Authenticate {
            method_id: "login".to_string(),
            reply: auth_reply,
        })
        .await
        .expect("starts authentication");
    connection.auth_started.notified().await;
    assert_eq!(shared.status().auth_state, HarnessAuthState::Authenticating);

    if let Some(kind) = notification_kind {
        super::actor::AuthStatusUpdate {
            auth_status: serde_json::json!({"kind": kind}),
        }
        .record(&shared);
    }
    let expected = if notification_kind == Some("account") {
        HarnessAuthState::Ready
    } else {
        HarnessAuthState::Missing
    };

    let (cancel_reply, cancel_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::CancelAuthenticate {
            method_id: "login".to_string(),
            reply: cancel_reply,
        })
        .await
        .expect("cancels authentication");
    assert_eq!(cancel_response.await.expect("cancel response"), Ok(()));
    assert_eq!(
        auth_response
            .await
            .expect("authentication response")
            .expect_err("cancelled authentication returns an error")
            .kind,
        HarnessErrorKind::RequestCancelled
    );
    assert_eq!(shared.status().auth_state, expected);

    let (second_cancel_reply, second_cancel_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::CancelAuthenticate {
            method_id: "login".to_string(),
            reply: second_cancel_reply,
        })
        .await
        .expect("repeats authentication cancellation");
    assert_eq!(
        second_cancel_response
            .await
            .expect("second cancel response"),
        Ok(())
    );

    stop_command_loop(&command_tx, task).await;
    assert_eq!(
        connection.recorded_calls(),
        vec!["authenticate:login".to_string()]
    );
}

#[tokio::test]
async fn command_loop_cancel_keeps_session_busy_and_shutdown_retires_the_prompt() {
    let connection = ScriptedCommandConnection::default();
    let shared = command_loop_shared();
    let session_id = HarnessSessionId::new("session-1");
    shared.insert_session(session_id.clone(), PathBuf::from("/tmp/project"));
    let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));

    let (prompt_reply, mut prompt_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Prompt {
            session_id: session_id.clone(),
            prompt: HarnessPrompt::text("hold"),
            reply: prompt_reply,
        })
        .await
        .expect("starts prompt");
    connection.prompt_started.notified().await;

    let (cancel_reply, cancel_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Cancel {
            session_id: session_id.clone(),
            reply: cancel_reply,
        })
        .await
        .expect("queues cancellation");
    cancel_response
        .await
        .expect("cancel response")
        .expect("cancel notification sent");
    assert!(matches!(
        prompt_response.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(shared.sessions()[0].state, HarnessSessionState::Prompting);

    let (second_reply, second_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Prompt {
            session_id: session_id.clone(),
            prompt: HarnessPrompt::text("overlap"),
            reply: second_reply,
        })
        .await
        .expect("checks overlapping prompt");
    assert_eq!(
        second_response
            .await
            .expect("overlapping prompt response")
            .expect_err("session stays busy until remote completion")
            .kind,
        HarnessErrorKind::SessionBusy
    );

    connection.prompt_release.notify_one();
    assert_eq!(
        prompt_response
            .await
            .expect("prompt response")
            .expect("remote prompt completes"),
        HarnessStopReason::EndTurn
    );
    assert_eq!(shared.sessions()[0].state, HarnessSessionState::Active);

    let (shutdown_prompt_reply, shutdown_prompt_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Prompt {
            session_id: session_id.clone(),
            prompt: HarnessPrompt::text("shutdown race"),
            reply: shutdown_prompt_reply,
        })
        .await
        .expect("starts prompt before shutdown");
    connection.prompt_started.notified().await;
    let (shutdown_reply, shutdown_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Shutdown {
            reply: shutdown_reply,
        })
        .await
        .expect("queues shutdown with active prompt");
    shutdown_response.await.expect("shutdown acknowledged");
    assert_eq!(
        shutdown_prompt_response
            .await
            .expect("shutdown prompt response")
            .expect("shutdown resolves prompt"),
        HarnessStopReason::Cancelled
    );
    task.await
        .expect("command loop task")
        .expect("command loop exits cleanly");
    assert_eq!(shared.sessions()[0].state, HarnessSessionState::Active);
    assert_eq!(
        connection.recorded_calls(),
        vec![
            "prompt:session-1".to_string(),
            "cancel:session-1".to_string(),
            "prompt:session-1".to_string(),
            "cancel:session-1".to_string()
        ]
    );
}

/// Shutdown cancels, then lets the agent answer before the connection
/// closes: the turn ends on the agent's own `stopReason`, not one core made
/// up. (oh-my-pi before 18.2.1 dropped requests in flight at stdin EOF.)
#[tokio::test]
async fn shutdown_lets_a_cancelled_prompt_answer_before_closing() {
    let connection = ScriptedCommandConnection::default();
    let shared = command_loop_shared();
    let session_id = HarnessSessionId::new("session-1");
    shared.insert_session(session_id.clone(), PathBuf::from("/tmp/project"));
    let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));

    let (prompt_reply, prompt_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Prompt {
            session_id: session_id.clone(),
            prompt: HarnessPrompt::text("hold"),
            reply: prompt_reply,
        })
        .await
        .expect("starts prompt");
    connection.prompt_started.notified().await;
    let (shutdown_reply, shutdown_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Shutdown {
            reply: shutdown_reply,
        })
        .await
        .expect("queues shutdown");
    shutdown_response.await.expect("shutdown acknowledged");
    connection.prompt_release.notify_one();

    assert_eq!(
        prompt_response
            .await
            .expect("prompt response")
            .expect("the agent's answer"),
        HarnessStopReason::EndTurn
    );
    task.await
        .expect("command loop task")
        .expect("command loop exits cleanly");
    assert_eq!(
        connection.recorded_calls(),
        vec![
            "prompt:session-1".to_string(),
            "cancel:session-1".to_string()
        ]
    );
}

#[tokio::test]
async fn restored_session_without_options_keeps_picker_and_offers_new_conversation_recovery() {
    let connection = ScriptedCommandConnection::default();
    let shared = command_loop_shared();
    let cached = crate::agent::AgentSessionSelects {
        default_mode_id: Some("auto".into()),
        ..Default::default()
    };
    shared.set_session_selects(cached.clone());
    let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));
    let (reply, response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::LoadSession {
            session_id: HarnessSessionId::new("restored"),
            cwd: PathBuf::from("/tmp/project"),
            mode_id: Some("auto".into()),
            mcp_servers: Vec::new(),
            reply,
        })
        .await
        .unwrap();
    let error = response.await.unwrap().unwrap_err();
    assert_eq!(error.kind, HarnessErrorKind::SelectionUnavailable);
    assert!(error.message.contains("Start a new conversation"));
    assert_eq!(shared.status().session_selects, Some(cached));
    assert!(shared.sessions().is_empty());
    assert_eq!(
        connection.recorded_calls(),
        vec!["load:restored:/tmp/project"]
    );
    stop_command_loop(&command_tx, task).await;
}

#[tokio::test]
async fn reloading_a_held_session_closes_it_first_when_the_agent_can() {
    let connection = ScriptedCommandConnection::default();
    let shared = command_loop_shared();
    shared.insert_session(HarnessSessionId::new("held"), PathBuf::from("/tmp/project"));
    let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));
    let load = || async {
        let (reply, response) = oneshot::channel();
        command_tx
            .send(AdapterCommand::LoadSession {
                session_id: HarnessSessionId::new("held"),
                cwd: PathBuf::from("/tmp/project"),
                mode_id: None,
                mcp_servers: Vec::new(),
                reply,
            })
            .await
            .unwrap();
        response.await.unwrap().expect("loads");
    };

    // Without `session/close`, the load goes ahead as before.
    load().await;
    assert_eq!(connection.recorded_calls(), vec!["load:held:/tmp/project"]);

    shared.status_tx.send_modify(|status| {
        status
            .capabilities
            .agent_advertises
            .insert(AcpFeature::SessionClose, true);
    });
    load().await;
    assert_eq!(
        connection.recorded_calls(),
        vec![
            "load:held:/tmp/project",
            "close:held",
            "load:held:/tmp/project"
        ]
    );
    assert_eq!(shared.sessions().len(), 1, "the reload holds it again");
    stop_command_loop(&command_tx, task).await;
}

/// oh-my-pi opens a session with no provider set up and offers models only
/// for providers it holds credentials for, so a session without a model is
/// signed out, and one with a model is signed in.
#[tokio::test]
async fn model_selector_proof_reads_a_session_without_models_as_signed_out() {
    let connection = ScriptedCommandConnection::default();
    let mode = SessionConfigOption::select(
        "mode",
        "Mode",
        "default",
        vec![SessionConfigSelectOption::new("default", "Default")],
    )
    .category(SessionConfigOptionCategory::Mode);
    *connection.config_options.lock().unwrap() = Some(vec![mode.clone()]);
    let (status_tx, _) = watch::channel(AdapterStatus::stopped());
    let updates_tx = HarnessUpdateSender::default();
    let shared = Arc::new(SharedState {
        status_tx,
        updates_tx,
        sessions: Mutex::new(BTreeMap::new()),
        update_sequences: Mutex::new(BTreeMap::new()),
        announced_commands: Mutex::new(BTreeMap::new()),
        child_sessions: None,
        sign_in_proof: SignInProof::ModelSelector,
    });
    let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));
    let new_session = || async {
        let (reply, response) = oneshot::channel();
        command_tx
            .send(AdapterCommand::NewSession {
                cwd: PathBuf::from("/tmp/project"),
                config: SessionConfigRequest::default(),
                reply,
            })
            .await
            .unwrap();
        response.await.unwrap().expect("session opens either way");
    };

    new_session().await;
    assert_eq!(shared.status().auth_state, HarnessAuthState::Missing);

    *connection.config_options.lock().unwrap() = Some(vec![
        mode,
        SessionConfigOption::select(
            "model",
            "Model",
            "anthropic/claude",
            vec![SessionConfigSelectOption::new("anthropic/claude", "Claude")],
        )
        .category(SessionConfigOptionCategory::Model),
    ]);
    new_session().await;
    assert_eq!(shared.status().auth_state, HarnessAuthState::Ready);
    stop_command_loop(&command_tx, task).await;
}

#[tokio::test]
async fn restored_session_reapplies_mode_before_becoming_promptable() {
    let connection = ScriptedCommandConnection::default();
    *connection.config_options.lock().unwrap() = Some(vec![SessionConfigOption::select(
        "permission-policy",
        "Mode",
        "default",
        vec![
            SessionConfigSelectOption::new("default", "Manual"),
            SessionConfigSelectOption::new("auto", "Auto"),
        ],
    )
    .category(SessionConfigOptionCategory::Mode)]);
    let shared = command_loop_shared();
    let session_id = HarnessSessionId::new("restored-session");
    let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));
    let (reply, response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::LoadSession {
            session_id: session_id.clone(),
            cwd: PathBuf::from("/tmp/project"),
            mode_id: Some("auto".into()),
            mcp_servers: Vec::new(),
            reply,
        })
        .await
        .unwrap();
    response.await.unwrap().unwrap();
    assert_eq!(
        connection.recorded_calls(),
        vec![
            "load:restored-session:/tmp/project",
            "config:restored-session:permission-policy:auto",
        ]
    );
    assert!(shared.sessions.lock().unwrap().contains_key(&session_id));
    stop_command_loop(&command_tx, task).await;
}

/// A live mode switch waits on the agent off the command loop: other commands
/// keep flowing while it is held, and one the agent never answers times out
/// instead of stalling the adapter.
#[tokio::test]
async fn live_mode_switch_runs_off_the_command_loop_and_times_out() {
    let connection = ScriptedCommandConnection::default();
    *connection.config_options.lock().unwrap() = Some(vec![SessionConfigOption::select(
        "mode",
        "Mode",
        "default",
        vec![
            SessionConfigSelectOption::new("default", "Manual"),
            SessionConfigSelectOption::new("acceptEdits", "Accept edits"),
            SessionConfigSelectOption::new("plan", "Plan"),
        ],
    )
    .category(SessionConfigOptionCategory::Mode)]);
    let shared = command_loop_shared();
    let session_id = HarnessSessionId::new("live-session");
    let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));
    let (reply, response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::LoadSession {
            session_id: session_id.clone(),
            cwd: PathBuf::from("/tmp/project"),
            mode_id: None,
            mcp_servers: Vec::new(),
            reply,
        })
        .await
        .unwrap();
    response.await.unwrap().unwrap();

    connection
        .hold_config
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (reply, applied) = oneshot::channel();
    command_tx
        .send(AdapterCommand::ApplyMode {
            session_id: session_id.clone(),
            mode_id: "acceptEdits".into(),
            reply,
        })
        .await
        .unwrap();
    connection.config_started.notified().await;
    let (reply, response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::NewSession {
            cwd: PathBuf::from("/tmp/other"),
            config: SessionConfigRequest::default(),
            reply,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), response)
        .await
        .expect("a held mode switch does not block other commands")
        .unwrap()
        .unwrap();
    connection.config_release.notify_one();
    applied.await.unwrap().unwrap();
    assert_eq!(
        shared.sessions.lock().unwrap()[&session_id]
            .current_mode_id
            .as_deref(),
        Some("acceptEdits")
    );

    tokio::time::pause();
    let (reply, unanswered) = oneshot::channel();
    command_tx
        .send(AdapterCommand::ApplyMode {
            session_id: session_id.clone(),
            mode_id: "plan".into(),
            reply,
        })
        .await
        .unwrap();
    let error = unanswered
        .await
        .unwrap()
        .expect_err("an unanswered switch times out");
    assert_eq!(error.kind, HarnessErrorKind::SelectionUnavailable);
    assert_eq!(
        shared.sessions.lock().unwrap()[&session_id]
            .current_mode_id
            .as_deref(),
        Some("acceptEdits"),
        "a switch the agent never confirmed is not recorded"
    );
    tokio::time::resume();
    connection.config_release.notify_one();
    stop_command_loop(&command_tx, task).await;
}

/// A held session's model switch goes out on the wire and becomes the
/// session's confirmed model; one refused before it goes out leaves the model
/// where it was, and one the agent never answers leaves it unknown.
#[tokio::test]
async fn live_model_switch_tracks_the_confirmed_model() {
    let connection = ScriptedCommandConnection::default();
    *connection.config_options.lock().unwrap() = Some(vec![SessionConfigOption::select(
        "model",
        "Model",
        "deepseek-v4-flash",
        vec![
            SessionConfigSelectOption::new("deepseek-v4-flash", "DeepSeek V4 Flash"),
            SessionConfigSelectOption::new("deepseek-v4.1-flash", "DeepSeek V4.1 Flash"),
        ],
    )
    .category(SessionConfigOptionCategory::Model)]);
    let shared = command_loop_shared();
    let session_id = HarnessSessionId::new("live-session");
    let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));
    let (reply, response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::LoadSession {
            session_id: session_id.clone(),
            cwd: PathBuf::from("/tmp/project"),
            mode_id: None,
            mcp_servers: Vec::new(),
            reply,
        })
        .await
        .unwrap();
    response.await.unwrap().unwrap();
    let model = || {
        shared.sessions.lock().unwrap()[&session_id]
            .current_model_id
            .clone()
    };
    assert_eq!(model().as_deref(), Some("deepseek-v4-flash"));

    let apply = |model_id: &str| {
        let (reply, response) = oneshot::channel();
        let command = AdapterCommand::ApplyModel {
            session_id: session_id.clone(),
            switch: ModelSwitch {
                model_id: Some(model_id.into()),
                ..ModelSwitch::default()
            },
            reply,
        };
        let command_tx = command_tx.clone();
        async move {
            command_tx.send(command).await.unwrap();
            response.await.unwrap()
        }
    };
    // A switch the agent never confirms may still land: the next run must
    // not trust the old model and skip switching back.
    connection
        .hold_config
        .store(true, std::sync::atomic::Ordering::SeqCst);
    tokio::time::pause();
    let error = apply("deepseek-v4.1-flash")
        .await
        .expect_err("an unanswered switch times out");
    assert_eq!(error.kind, HarnessErrorKind::SelectionUnavailable);
    assert_eq!(model(), None);
    tokio::time::resume();
    connection
        .hold_config
        .store(false, std::sync::atomic::Ordering::SeqCst);

    apply("deepseek-v4.1-flash").await.unwrap();
    assert!(connection
        .recorded_calls()
        .contains(&"config:live-session:model:deepseek-v4.1-flash".to_string()));
    assert_eq!(model().as_deref(), Some("deepseek-v4.1-flash"));

    let error = apply("gone").await.expect_err("an unlisted model is refused");
    assert_eq!(error.kind, HarnessErrorKind::SelectionUnavailable);
    assert_eq!(model().as_deref(), Some("deepseek-v4.1-flash"));
    stop_command_loop(&command_tx, task).await;
}

#[tokio::test]
async fn command_loop_rejects_load_and_delete_of_prompting_session_before_wire_io() {
    let connection = ScriptedCommandConnection::default();
    let shared = command_loop_shared();
    let session_id = HarnessSessionId::new("session-1");
    shared.insert_session(session_id.clone(), PathBuf::from("/tmp/project"));
    shared.begin_prompt(&session_id).expect("begin prompt");
    let (command_tx, task) = spawn_command_loop(connection.clone(), shared);

    let (load_reply, load_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::LoadSession {
            session_id: session_id.clone(),
            cwd: PathBuf::from("/tmp/project"),
            mode_id: None,
            mcp_servers: Vec::new(),
            reply: load_reply,
        })
        .await
        .expect("checks load during prompt");
    assert_eq!(
        load_response
            .await
            .expect("load response")
            .expect_err("prompting session load is rejected")
            .kind,
        HarnessErrorKind::SessionBusy
    );

    let (delete_reply, delete_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::DeleteSession {
            session_id,
            reply: delete_reply,
        })
        .await
        .expect("checks delete during prompt");
    assert_eq!(
        delete_response
            .await
            .expect("delete response")
            .expect_err("prompting session delete is rejected")
            .kind,
        HarnessErrorKind::SessionBusy
    );
    assert!(connection.recorded_calls().is_empty());
    stop_command_loop(&command_tx, task).await;
}

#[tokio::test]
async fn fake_backend_covers_auth_sessions_updates_cancellation_and_shutdown() {
    let backend = Arc::new(ScriptedBackend::default());
    let adapter = Arc::new(AcpHarnessAdapter::with_backend(
        test_definition(),
        7,
        backend.clone(),
    ));
    let started = adapter.start().await.expect("adapter start");
    assert_eq!(started.lifecycle, AdapterLifecycleState::Ready);
    assert_eq!(started.auth_state, HarnessAuthState::Unknown);
    assert_eq!(started.generation, 7);
    assert!(started.capabilities.load_session);
    assert_eq!(
        started.capabilities.steering,
        SteeringCapability::InterruptThenPrompt
    );
    let steer_error = adapter
        .steer(
            HarnessSessionId::new("missing"),
            HarnessPrompt::text("steer"),
        )
        .await
        .expect_err("ACP native steer is unsupported");
    assert_eq!(steer_error.kind, HarnessErrorKind::SteeringUnsupported);

    let mut status = adapter.status();
    let authenticate_adapter = Arc::clone(&adapter);
    let authentication =
        tokio::spawn(async move { authenticate_adapter.authenticate("login".to_string()).await });
    backend.auth_started.notified().await;
    status.changed().await.expect("auth status change");
    assert_eq!(status.borrow().auth_state, HarnessAuthState::Authenticating);
    backend.auth_release.notify_one();
    authentication
        .await
        .expect("authentication task")
        .expect("authentication");
    assert_eq!(
        adapter.status().borrow().auth_state,
        HarnessAuthState::Ready
    );

    let mut updates = adapter.subscribe_updates();
    let session_id = adapter
        .new_session(PathBuf::from("/tmp/new"), SessionConfigRequest::default())
        .await
        .expect("new session");
    let loaded_id = HarnessSessionId::new("loaded-session");
    adapter
        .load_session(loaded_id.clone(), PathBuf::from("/tmp/loaded"))
        .await
        .expect("load session");
    assert!(adapter
        .sessions()
        .iter()
        .any(|session| session.session_id == loaded_id));

    let stop_reason = adapter
        .prompt(session_id.clone(), HarnessPrompt::text("normal"))
        .await
        .expect("prompt");
    assert_eq!(stop_reason, HarnessStopReason::EndTurn);
    let first = updates.recv().await.expect("first update");
    let second = updates.recv().await.expect("second update");
    assert_eq!(
        [first.update, second.update],
        [
            HarnessUpdateKind::AgentMessageChunk("Hello ".to_string(), None),
            HarnessUpdateKind::AgentMessageChunk("from fake".to_string(), None)
        ]
    );

    let prompt_adapter = Arc::clone(&adapter);
    let prompt_session_id = session_id.clone();
    let active_prompt = tokio::spawn(async move {
        prompt_adapter
            .prompt(prompt_session_id, HarnessPrompt::text("hold"))
            .await
    });
    backend.prompt_started.notified().await;
    let busy = adapter
        .prompt(session_id.clone(), HarnessPrompt::text("second"))
        .await
        .expect_err("same-session prompt must be rejected");
    assert_eq!(busy.kind, HarnessErrorKind::SessionBusy);
    adapter
        .cancel(session_id.clone())
        .await
        .expect("cancel prompt");
    assert_eq!(
        active_prompt
            .await
            .expect("prompt task")
            .expect("prompt result"),
        HarnessStopReason::Cancelled
    );

    adapter.shutdown().await.expect("shutdown");
    assert_eq!(
        adapter.status().borrow().lifecycle,
        AdapterLifecycleState::Stopped
    );
    assert!(adapter
        .sessions()
        .iter()
        .all(|session| session.state == HarnessSessionState::Dead));
}

#[tokio::test]
async fn unexpected_backend_exit_fails_prompt_and_marks_sessions_dead() {
    let adapter =
        AcpHarnessAdapter::with_backend(test_definition(), 1, Arc::new(ScriptedBackend::default()));
    adapter.start().await.expect("adapter start");
    let session_id = adapter
        .new_session(PathBuf::from("/tmp/exit"), SessionConfigRequest::default())
        .await
        .expect("new session");
    let error = adapter
        .prompt(session_id, HarnessPrompt::text("exit_clean"))
        .await
        .expect_err("prompt should fail when backend exits");
    assert_eq!(error.kind, HarnessErrorKind::AdapterExited);

    let mut status = adapter.status();
    while status.borrow().lifecycle != AdapterLifecycleState::Dead {
        status.changed().await.expect("dead status");
    }
    assert!(adapter
        .sessions()
        .iter()
        .all(|session| session.state == HarnessSessionState::Dead));
}

#[tokio::test]
async fn shutdown_completes_an_active_prompt_as_cancelled() {
    let backend = Arc::new(ScriptedBackend::default());
    let adapter = Arc::new(AcpHarnessAdapter::with_backend(
        test_definition(),
        1,
        backend.clone(),
    ));
    adapter.start().await.expect("adapter start");
    let session_id = adapter
        .new_session(
            PathBuf::from("/tmp/shutdown"),
            SessionConfigRequest::default(),
        )
        .await
        .expect("new session");
    let prompt_adapter = Arc::clone(&adapter);
    let active_prompt = tokio::spawn(async move {
        prompt_adapter
            .prompt(session_id, HarnessPrompt::text("hold"))
            .await
    });
    backend.prompt_started.notified().await;

    adapter.shutdown().await.expect("shutdown");
    assert_eq!(
        active_prompt
            .await
            .expect("prompt task")
            .expect("prompt result"),
        HarnessStopReason::Cancelled
    );
}

#[tokio::test]
async fn shutdown_forcibly_closes_a_backend_that_cannot_process_commands() {
    let adapter = AcpHarnessAdapter::with_backend(test_definition(), 1, Arc::new(BlockingBackend));
    let started = adapter.start().await.expect("adapter start");
    assert!(!started.capabilities.load_session);

    tokio::time::timeout(Duration::from_secs(1), adapter.shutdown())
        .await
        .expect("shutdown must be bounded")
        .expect("shutdown");
    assert_eq!(
        adapter.status().borrow().lifecycle,
        AdapterLifecycleState::Stopped
    );
}

#[tokio::test]
async fn concurrent_shutdown_callers_share_the_same_bounded_shutdown() {
    let adapter = AcpHarnessAdapter::with_backend(test_definition(), 1, Arc::new(BlockingBackend));
    adapter.start().await.expect("adapter start");
    let first = adapter.shutdown();
    let second = adapter.shutdown();

    let (first, second) = tokio::join!(first, second);
    first.expect("first shutdown");
    second.expect("second shutdown");
    assert_eq!(
        adapter.status().borrow().lifecycle,
        AdapterLifecycleState::Stopped
    );
}

#[tokio::test]
async fn shutdown_can_cancel_an_adapter_stuck_during_initialization() {
    let adapter = Arc::new(AcpHarnessAdapter::with_backend(
        test_definition(),
        1,
        Arc::new(NeverReadyBackend),
    ));
    let mut status = adapter.status();
    let starting_adapter = Arc::clone(&adapter);
    let start = tokio::spawn(async move { starting_adapter.start().await });
    while status.borrow().lifecycle != AdapterLifecycleState::Starting {
        status.changed().await.expect("starting status");
    }

    tokio::time::timeout(Duration::from_secs(1), adapter.shutdown())
        .await
        .expect("shutdown must reach a starting adapter")
        .expect("shutdown");
    let start_error = start
        .await
        .expect("start task")
        .expect_err("start must be cancelled");
    assert_eq!(start_error.kind, HarnessErrorKind::AdapterExited);
    assert_eq!(
        adapter.status().borrow().lifecycle,
        AdapterLifecycleState::Stopped
    );
}

#[tokio::test]
async fn concurrent_starts_wait_for_and_share_one_initialization() {
    let backend = Arc::new(DelayedReadyBackend::default());
    let adapter = AcpHarnessAdapter::with_backend(test_definition(), 1, backend.clone());
    let first = adapter.start();
    let second = adapter.start();
    backend.release.notify_one();

    let (first, second) = tokio::join!(first, second);
    let first = first.expect("first start");
    let second = second.expect("second start");
    assert_eq!(first.lifecycle, AdapterLifecycleState::Ready);
    assert_eq!(first, second);
    assert_eq!(first.generation, 1);
    adapter.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn shutdown_forcibly_closes_a_backend_that_acks_but_does_not_exit() {
    let adapter = AcpHarnessAdapter::with_backend(
        test_definition(),
        1,
        Arc::new(AcknowledgingBlockingBackend),
    );
    adapter.start().await.expect("adapter start");

    tokio::time::timeout(Duration::from_secs(1), adapter.shutdown())
        .await
        .expect("shutdown must be bounded after acknowledgement")
        .expect("shutdown");
    assert_eq!(
        adapter.status().borrow().lifecycle,
        AdapterLifecycleState::Stopped
    );
}

fn test_definition() -> AgentDefinition {
    let definition = AgentDefinition::for_test("fake");
    AgentDefinition {
        terminal_login_reveal: super::super::types::TerminalLoginReveal::WhenStuck,
        launch: super::super::types::AgentLaunchSpec {
            command: "fake".to_string(),
            ..definition.launch.clone()
        },
        ..definition
    }
}

fn existing_claude_definition(home: &std::path::Path) -> AgentDefinition {
    let mut definition = test_definition();
    definition.instance = Some(crate::agent::AgentInstance {
        id: definition.agent_id.clone(),
        harness_id: crate::agent::HarnessId("claude".into()),
        label: "Claude — Your setup".into(),
        home: crate::agent::HomeLayout::Existing {
            path: home.into(),
            environment: BTreeMap::from([("CLAUDE_CONFIG_DIR".into(), None)]),
        },
        claimed: false,
    });
    definition
}

#[tokio::test]
async fn existing_claude_refresh_lock_refuses_shutdown_without_changing_live_adapter() {
    let home = tempfile::tempdir().unwrap();
    let adapter = AcpHarnessAdapter::with_backend(
        existing_claude_definition(home.path()),
        1,
        Arc::new(ScriptedBackend::default()),
    );
    adapter.start().await.unwrap();
    let lock = home
        .path()
        .join(super::super::refresh_lock::CLAUDE_REFRESH_LOCK);
    std::fs::create_dir(&lock).unwrap();
    let error = adapter.shutdown().await.unwrap_err();
    assert_eq!(error.kind, HarnessErrorKind::AuthRefreshBusy);
    assert_eq!(
        adapter.status().borrow().lifecycle,
        AdapterLifecycleState::Ready
    );
    assert!(lock.exists());
    // Abandoning the operation retains the same usable adapter and generation.
    adapter
        .new_session(home.path().into(), SessionConfigRequest::default())
        .await
        .unwrap();
    std::fs::remove_dir(lock).unwrap();
    adapter.shutdown().await.unwrap();
}

fn managed_claude_definition(home: &std::path::Path) -> AgentDefinition {
    let mut definition = test_definition();
    definition.agent_id = AgentId::new("claude");
    definition.launch.environment.push((
        "CLAUDE_CONFIG_DIR".into(),
        home.to_string_lossy().into_owned(),
    ));
    definition
}

/// A catalog probe stops its adapter right after `session/new`, which is when
/// the CLI refreshes an expired token. Killing it then lost the rotated token
/// and left `<home>.lock` behind for every later run.
#[tokio::test]
async fn managed_claude_shutdown_lets_a_refresh_in_its_home_finish() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let adapter = Arc::new(AcpHarnessAdapter::with_backend(
        managed_claude_definition(&home),
        1,
        Arc::new(ScriptedBackend::default()),
    ));
    adapter.start().await.unwrap();
    let legacy = super::super::refresh_lock::fixture::legacy_lock(&home);
    std::fs::create_dir(&legacy).unwrap();
    let stopping = adapter.clone();
    let shutdown = tokio::spawn(async move { stopping.shutdown().await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!shutdown.is_finished(), "stopping waits for the refresh");
    assert_eq!(
        adapter.status().borrow().lifecycle,
        AdapterLifecycleState::Ready
    );
    std::fs::remove_dir(&legacy).unwrap();
    tokio::time::timeout(Duration::from_secs(2), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn managed_claude_shutdown_does_not_wait_on_a_lock_whose_owner_exited() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    super::super::refresh_lock::fixture::abandoned_refresh(&home);
    let adapter = AcpHarnessAdapter::with_backend(
        managed_claude_definition(&home),
        1,
        Arc::new(ScriptedBackend::default()),
    );
    adapter.start().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), adapter.shutdown())
        .await
        .unwrap()
        .unwrap();
}

struct RefreshingProcessBackend {
    pid: Mutex<Option<u32>>,
    started_refresh: Notify,
}

struct TestChild(std::process::Child);
impl Drop for TestChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl AdapterBackend for RefreshingProcessBackend {
    fn run(
        self: Arc<Self>,
        mut context: BackendContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'static>> {
        Box::pin(async move {
            let child = TestChild(
                std::process::Command::new("/bin/sleep")
                    .arg("60")
                    .spawn()
                    .unwrap(),
            );
            *self.pid.lock().unwrap() = Some(child.0.id());
            let ready = BackendReady {
                auth_methods: vec![],
                agent_name: None,
                agent_version: None,
                capabilities: Default::default(),
            };
            send_ready(&context.ready_tx, context.shared.set_ready(&ready));
            if let Some(AdapterCommand::Shutdown { reply }) = context.command_rx.recv().await {
                std::fs::create_dir(context.definition.refresh_lock_path().unwrap()).unwrap();
                let _ = reply.send(());
                self.started_refresh.notify_one();
                // Mirrors a transport that acknowledged shutdown but remains
                // alive. Cancelling the supervisor destroys its child guard.
                while context.definition.refresh_lock_path().unwrap().exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            drop(child);
            Ok(())
        })
    }
}

#[tokio::test]
async fn existing_claude_refresh_started_during_shutdown_preserves_process_past_grace_period() {
    let home = tempfile::tempdir().unwrap();
    let backend = Arc::new(RefreshingProcessBackend {
        pid: Mutex::new(None),
        started_refresh: Notify::new(),
    });
    let adapter = Arc::new(AcpHarnessAdapter::with_backend(
        existing_claude_definition(home.path()),
        1,
        backend.clone(),
    ));
    adapter.start().await.unwrap();
    let stopping = adapter.clone();
    let shutdown = tokio::spawn(async move { stopping.shutdown().await });
    backend.started_refresh.notified().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let pid = backend.pid.lock().unwrap().unwrap() as i32;
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        0,
        "shutdown timer must preserve the live process"
    );
    assert!(!shutdown.is_finished());
    let lock = context_lock(home.path());
    assert!(lock.exists());
    std::fs::remove_dir(lock).unwrap();
    tokio::time::timeout(Duration::from_secs(2), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "the process stops after refresh completes"
    );
}

fn context_lock(home: &std::path::Path) -> PathBuf {
    home.join(super::super::refresh_lock::CLAUDE_REFRESH_LOCK)
}

#[tokio::test]
async fn existing_claude_dead_adapter_shutdown_finishes_and_can_restart() {
    let home = tempfile::tempdir().unwrap();
    let adapter = AcpHarnessAdapter::with_backend(
        existing_claude_definition(home.path()),
        1,
        Arc::new(ScriptedBackend::default()),
    );
    adapter.start().await.unwrap();
    let session = adapter
        .new_session(home.path().into(), SessionConfigRequest::default())
        .await
        .unwrap();
    adapter
        .prompt(session, HarnessPrompt::text("exit_clean"))
        .await
        .unwrap_err();
    let mut status = adapter.status();
    while status.borrow().lifecycle != AdapterLifecycleState::Dead {
        status.changed().await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(1), adapter.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        adapter.status().borrow().lifecycle,
        AdapterLifecycleState::Stopped
    );
    adapter.start().await.unwrap();
    adapter.shutdown().await.unwrap();
}

#[tokio::test]
async fn existing_claude_shutdown_during_initialization_normalizes_stopped() {
    let home = tempfile::tempdir().unwrap();
    let backend = Arc::new(DelayedReadyBackend::default());
    let adapter = Arc::new(AcpHarnessAdapter::with_backend(
        existing_claude_definition(home.path()),
        1,
        backend.clone(),
    ));
    let starting = adapter.clone();
    let start = tokio::spawn(async move { starting.start().await });
    let mut status = adapter.status();
    while status.borrow().lifecycle != AdapterLifecycleState::Starting {
        status.changed().await.unwrap();
    }
    let stopping = adapter.clone();
    let stop = tokio::spawn(async move { stopping.shutdown().await });
    while status.borrow().lifecycle != AdapterLifecycleState::Stopping {
        status.changed().await.unwrap();
    }
    backend.release.notify_one();
    assert!(start.await.unwrap().is_err());
    tokio::time::timeout(Duration::from_secs(1), stop)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        adapter.status().borrow().lifecycle,
        AdapterLifecycleState::Stopped
    );
    adapter.shutdown().await.unwrap();
}

#[tokio::test]
async fn existing_claude_cancel_after_shutdown_commit_never_restores_ready() {
    let home = tempfile::tempdir().unwrap();
    let backend = Arc::new(RefreshingProcessBackend {
        pid: Mutex::new(None),
        started_refresh: Notify::new(),
    });
    let adapter = Arc::new(AcpHarnessAdapter::with_backend(
        existing_claude_definition(home.path()),
        1,
        backend.clone(),
    ));
    adapter.start().await.unwrap();
    let stopping = adapter.clone();
    let stop = tokio::spawn(async move { stopping.shutdown().await });
    backend.started_refresh.notified().await;
    stop.abort();
    assert!(stop.await.unwrap_err().is_cancelled());
    assert_eq!(
        adapter.status().borrow().lifecycle,
        AdapterLifecycleState::Stopping
    );
    let pid = backend.pid.lock().unwrap().unwrap() as i32;
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    std::fs::remove_dir(context_lock(home.path())).unwrap();
    tokio::time::timeout(Duration::from_secs(1), adapter.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        adapter.status().borrow().lifecycle,
        AdapterLifecycleState::Stopped
    );
    assert_ne!(unsafe { libc::kill(pid, 0) }, 0);
}

#[tokio::test]
async fn existing_claude_closed_command_channel_exits_instead_of_restarting_loop() {
    let home = tempfile::tempdir().unwrap();
    let pid_file = home.path().join("pid");
    let mut definition = existing_claude_definition(home.path());
    definition.launch.command = "/bin/sh".into();
    definition.launch.arguments = vec!["-c".into(), r#"
echo $$ > "$OTTAM_TEST_PID"
while IFS= read -r line; do
    case "$line" in
        *'"method":"initialize"'*)
            id=${line#*\"id\":}; id=${id%%,*}
            printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{},"authMethods":[]}}\n' "$id"
            ;;
    esac
done
"#.into()];
    definition.launch.environment.push((
        "OTTAM_TEST_PID".into(),
        pid_file.to_string_lossy().into_owned(),
    ));
    let adapter = AcpHarnessAdapter::with_generation_and_permission_broker(
        definition,
        1,
        Arc::new(CancelPermissionBroker),
    );
    tokio::time::timeout(Duration::from_secs(2), adapter.start())
        .await
        .unwrap()
        .unwrap();
    let pid: i32 = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    struct ProcessCleanup(i32);
    impl Drop for ProcessCleanup {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0, libc::SIGTERM);
            }
        }
    }
    let _cleanup = ProcessCleanup(pid);
    let lock = context_lock(home.path());
    std::fs::create_dir(&lock).unwrap();
    assert_eq!(
        adapter.shutdown().await.unwrap_err().kind,
        HarnessErrorKind::AuthRefreshBusy
    );
    drop(adapter); // production replay used to discard this failed shutdown
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    assert!(lock.exists());
    std::fs::remove_dir(lock).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while unsafe { libc::kill(pid, 0) } == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("channel EOF must close the real SDK transport and reap its child");
}

#[tokio::test]
async fn existing_claude_real_initialization_can_be_shutdown_without_a_reply() {
    let home = tempfile::tempdir().unwrap();
    let pid_file = home.path().join("initializing-pid");
    let mut definition = existing_claude_definition(home.path());
    definition.launch.command = "/bin/sh".into();
    definition.launch.arguments = vec![
        "-c".into(),
        "echo $$ > \"$OTTAM_TEST_PID\"; while IFS= read -r line; do :; done".into(),
    ];
    definition.launch.environment.push((
        "OTTAM_TEST_PID".into(),
        pid_file.to_string_lossy().into_owned(),
    ));
    let adapter = Arc::new(AcpHarnessAdapter::with_generation_and_permission_broker(
        definition,
        1,
        Arc::new(CancelPermissionBroker),
    ));
    let starting = adapter.clone();
    let start = tokio::spawn(async move { starting.start().await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !pid_file.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pid: i32 = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    struct ProcessCleanup(Option<i32>);
    impl Drop for ProcessCleanup {
        fn drop(&mut self) {
            if let Some(pid) = self.0 {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
        }
    }
    let mut cleanup = ProcessCleanup(Some(pid));
    assert_eq!(
        adapter.status().borrow().lifecycle,
        AdapterLifecycleState::Starting
    );
    tokio::time::timeout(Duration::from_secs(1), adapter.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(start.await.unwrap().is_err());
    assert_eq!(
        adapter.status().borrow().lifecycle,
        AdapterLifecycleState::Stopped
    );
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "Stopped must mean the real child was reaped"
    );
    cleanup.0 = None;
}

#[derive(Default)]
struct GatedCommandBackend {
    release: Notify,
}

#[tokio::test]
async fn existing_claude_initialize_error_preserves_transport_during_refresh() {
    let home = tempfile::tempdir().unwrap();
    let pid_file = home.path().join("failed-initialize-pid");
    let lock = context_lock(home.path());
    let mut definition = existing_claude_definition(home.path());
    definition.launch.command = "/bin/sh".into();
    definition.launch.arguments = vec!["-c".into(), r#"
echo $$ > "$OTTAM_TEST_PID"
while IFS= read -r line; do
    case "$line" in
        *'"method":"initialize"'*)
            mkdir "$OTTAM_TEST_LOCK"
            id=${line#*\"id\":}; id=${id%%,*}
            printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32603,"message":"initialize failed"}}\n' "$id"
            ;;
    esac
done
"#.into()];
    definition.launch.environment.extend([
        (
            "OTTAM_TEST_PID".into(),
            pid_file.to_string_lossy().into_owned(),
        ),
        (
            "OTTAM_TEST_LOCK".into(),
            lock.to_string_lossy().into_owned(),
        ),
    ]);
    let adapter = Arc::new(AcpHarnessAdapter::with_generation_and_permission_broker(
        definition,
        1,
        Arc::new(CancelPermissionBroker),
    ));
    let starting = adapter.clone();
    let start = tokio::spawn(async move { starting.start().await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !lock.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pid: i32 = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        0,
        "SDK must not close stdin during refresh after initialize fails"
    );
    assert!(!start.is_finished());
    std::fs::remove_dir(lock).unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(2), start)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    tokio::time::timeout(Duration::from_secs(1), adapter.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(unsafe { libc::kill(pid, 0) }, 0);
}

impl AdapterBackend for GatedCommandBackend {
    fn run(
        self: Arc<Self>,
        mut context: BackendContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'static>> {
        Box::pin(async move {
            let ready = BackendReady {
                auth_methods: vec![],
                agent_name: None,
                agent_version: None,
                capabilities: Default::default(),
            };
            send_ready(&context.ready_tx, context.shared.set_ready(&ready));
            self.release.notified().await;
            run_command_loop(
                &ScriptedCommandConnection::default(),
                &mut context.command_rx,
                &context.shared,
                context.definition.refresh_lock_path().as_deref(),
            )
            .await
        })
    }
}

#[tokio::test]
async fn existing_claude_cancel_while_command_queue_is_full_restores_ready() {
    let home = tempfile::tempdir().unwrap();
    let backend = Arc::new(GatedCommandBackend::default());
    let adapter = Arc::new(AcpHarnessAdapter::with_backend(
        existing_claude_definition(home.path()),
        1,
        backend.clone(),
    ));
    adapter.start().await.unwrap();
    let sender = adapter.command_sender().unwrap();
    for _ in 0..COMMAND_CAPACITY {
        let (reply, _) = oneshot::channel();
        sender
            .send(AdapterCommand::NewSession {
                cwd: home.path().into(),
                config: SessionConfigRequest::default(),
                reply,
            })
            .await
            .unwrap();
    }
    assert_eq!(sender.capacity(), 0);
    let stopping = adapter.clone();
    let stop = tokio::spawn(async move { stopping.shutdown().await });
    let mut status = adapter.status();
    while status.borrow().lifecycle != AdapterLifecycleState::Stopping {
        status.changed().await.unwrap();
    }
    tokio::task::yield_now().await;
    stop.abort();
    assert!(stop.await.unwrap_err().is_cancelled());
    assert_eq!(
        adapter.status().borrow().lifecycle,
        AdapterLifecycleState::Ready
    );
    backend.release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), adapter.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        adapter.status().borrow().lifecycle,
        AdapterLifecycleState::Stopped
    );
}

#[derive(Default)]
struct ScriptedBackend {
    auth_started: Notify,
    auth_release: Notify,
    prompt_started: Notify,
}

struct BlockingBackend;

struct AcknowledgingBlockingBackend;

struct NeverReadyBackend;

#[derive(Default)]
struct DelayedReadyBackend {
    release: Notify,
}

impl AdapterBackend for BlockingBackend {
    fn run(
        self: Arc<Self>,
        context: BackendContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'static>> {
        Box::pin(async move {
            let ready = BackendReady {
                auth_methods: Vec::new(),
                agent_name: Some("Blocking Agent".to_string()),
                agent_version: Some("1.0.0".to_string()),
                capabilities: HarnessCapabilities::default(),
            };
            let status = context.shared.set_ready(&ready).expect("ready transition");
            send_ready(&context.ready_tx, Ok(status));
            let _context = context;
            std::future::pending::<()>().await;
            Ok(())
        })
    }
}

impl AdapterBackend for NeverReadyBackend {
    fn run(
        self: Arc<Self>,
        context: BackendContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'static>> {
        Box::pin(async move {
            let _context = context;
            std::future::pending::<()>().await;
            Ok(())
        })
    }
}

impl AdapterBackend for DelayedReadyBackend {
    fn run(
        self: Arc<Self>,
        mut context: BackendContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'static>> {
        Box::pin(async move {
            self.release.notified().await;
            let ready = BackendReady {
                auth_methods: Vec::new(),
                agent_name: Some("Delayed Agent".to_string()),
                agent_version: Some("1.0.0".to_string()),
                capabilities: HarnessCapabilities::default(),
            };
            let status = context.shared.set_ready(&ready)?;
            send_ready(&context.ready_tx, Ok(status));
            if let Some(AdapterCommand::Shutdown { reply }) = context.command_rx.recv().await {
                let _ = reply.send(());
            }
            Ok(())
        })
    }
}

impl AdapterBackend for AcknowledgingBlockingBackend {
    fn run(
        self: Arc<Self>,
        mut context: BackendContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'static>> {
        Box::pin(async move {
            let ready = BackendReady {
                auth_methods: Vec::new(),
                agent_name: Some("Blocking Agent".to_string()),
                agent_version: Some("1.0.0".to_string()),
                capabilities: HarnessCapabilities::default(),
            };
            let status = context.shared.set_ready(&ready).expect("ready transition");
            send_ready(&context.ready_tx, Ok(status));
            if let Some(AdapterCommand::Shutdown { reply }) = context.command_rx.recv().await {
                let _ = reply.send(());
            }
            let _context = context;
            std::future::pending::<()>().await;
            Ok(())
        })
    }
}

impl AdapterBackend for ScriptedBackend {
    fn run(
        self: Arc<Self>,
        mut context: BackendContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'static>> {
        Box::pin(async move {
            let ready = BackendReady {
                auth_methods: vec![HarnessAuthMethod {
                    method_id: "login".to_string(),
                    name: "Login".to_string(),
                    description: None,
                    kind: HarnessAuthMethodKind::Agent,
                    terminal_command: None,
                }],
                agent_name: Some("Fake Agent".to_string()),
                agent_version: Some("1.0.0".to_string()),
                capabilities: HarnessCapabilities {
                    auth_status: false,
                    load_session: true,
                    delete_session: true,
                    steering: SteeringCapability::InterruptThenPrompt,
                    prompt_image: true,
                    ..Default::default()
                },
            };
            let status = context.shared.set_ready(&ready).expect("ready transition");
            send_ready(&context.ready_tx, Ok(status));
            let mut held_prompts = BTreeMap::new();

            while let Some(command) = context.command_rx.recv().await {
                match command {
                    AdapterCommand::Authenticate { reply, .. } => {
                        context
                            .shared
                            .set_auth(HarnessAuthState::Authenticating, None);
                        self.auth_started.notify_one();
                        self.auth_release.notified().await;
                        context.shared.set_auth(HarnessAuthState::Ready, None);
                        let _ = reply.send(Ok(()));
                    }
                    // This backend answers `Authenticate` inline, so by the
                    // time a cancel is read there is never anything to cancel.
                    AdapterCommand::CancelAuthenticate { reply, .. } => {
                        let _ = reply.send(Ok(()));
                    }
                    AdapterCommand::NewSession { cwd, reply, .. } => {
                        let session_id = HarnessSessionId::new("new-session");
                        context.shared.insert_session(session_id.clone(), cwd);
                        context.shared.set_auth(HarnessAuthState::Ready, None);
                        let _ = reply.send(Ok(session_id));
                    }
                    AdapterCommand::LoadSession {
                        session_id,
                        cwd,
                        reply,
                        ..
                    } => {
                        context.shared.insert_session(session_id, cwd);
                        context.shared.set_auth(HarnessAuthState::Ready, None);
                        let _ = reply.send(Ok(()));
                    }
                    AdapterCommand::DeleteSession { session_id, reply } => {
                        context.shared.remove_session(&session_id);
                        let _ = reply.send(Ok(()));
                    }
                    AdapterCommand::ApplyMode {
                        session_id,
                        mode_id,
                        reply,
                    } => {
                        context.shared.set_session_mode(&session_id, Some(mode_id));
                        let _ = reply.send(Ok(()));
                    }
                    AdapterCommand::ApplyModel { reply, .. } => {
                        let _ = reply.send(Ok(()));
                    }
                    AdapterCommand::Prompt {
                        session_id,
                        prompt,
                        reply,
                    } => {
                        if let Err(error) = context.shared.begin_prompt(&session_id) {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                        match prompt.text.as_str() {
                            "hold" => {
                                held_prompts.insert(session_id, reply);
                                self.prompt_started.notify_one();
                            }
                            "exit" => {
                                return Err(HarnessError::new(
                                    HarnessErrorKind::AdapterExited,
                                    "fake adapter exited",
                                ));
                            }
                            "exit_clean" => return Ok(()),
                            _ => {
                                for text in ["Hello ", "from fake"] {
                                    context.shared.send_update(
                                        session_id.clone(),
                                        HarnessUpdateKind::AgentMessageChunk(
                                            text.to_string(),
                                            None,
                                        ),
                                    );
                                }
                                let result = Ok(HarnessStopReason::EndTurn);
                                context.shared.finish_prompt(&session_id, &result);
                                let _ = reply.send(result);
                            }
                        }
                    }
                    // This backend advertises interrupt-and-continue, so the
                    // adapter refuses a steer before it ever reaches here.
                    AdapterCommand::Steer { reply, .. } => {
                        let _ = reply.send(Err(HarnessError::new(
                            HarnessErrorKind::SteeringUnsupported,
                            "the scripted backend does not steer",
                        )));
                    }
                    AdapterCommand::CancelOutOfTurn { reply, .. } => {
                        let _ = reply.send(Ok(()));
                    }
                    AdapterCommand::StopAsyncTask { reply, .. } => {
                        let _ = reply.send(Ok(false));
                    }
                    AdapterCommand::Cancel { session_id, reply } => {
                        if let Some(prompt_reply) = held_prompts.remove(&session_id) {
                            let result = Ok(HarnessStopReason::Cancelled);
                            context.shared.finish_prompt(&session_id, &result);
                            let _ = prompt_reply.send(result);
                            let _ = reply.send(Ok(()));
                        } else {
                            let _ = reply.send(Err(HarnessError::new(
                                HarnessErrorKind::SessionNotFound,
                                "no active prompt",
                            )));
                        }
                    }
                    AdapterCommand::Shutdown { reply } => {
                        for (session_id, prompt_reply) in held_prompts {
                            let result = Ok(HarnessStopReason::Cancelled);
                            context.shared.finish_prompt(&session_id, &result);
                            let _ = prompt_reply.send(result);
                        }
                        let _ = reply.send(());
                        return Ok(());
                    }
                }
            }
            Ok(())
        })
    }
}

#[test]
fn steering_capability_comes_from_the_agents_advertisement() {
    assert_eq!(
        steering_capability(None),
        SteeringCapability::InterruptThenPrompt
    );
    let mut silent = agent_client_protocol::schema::v1::Meta::new();
    silent.insert("goal".to_string(), serde_json::json!({"version": 1}));
    assert_eq!(
        steering_capability(Some(&silent)),
        SteeringCapability::InterruptThenPrompt
    );
    let mut declined = agent_client_protocol::schema::v1::Meta::new();
    declined.insert(
        "steering".to_string(),
        serde_json::json!({"supported": false}),
    );
    assert_eq!(
        steering_capability(Some(&declined)),
        SteeringCapability::InterruptThenPrompt
    );
    let mut advertised = agent_client_protocol::schema::v1::Meta::new();
    advertised.insert(
        "steering".to_string(),
        serde_json::json!({"supported": true}),
    );
    assert_eq!(
        steering_capability(Some(&advertised)),
        SteeringCapability::Native
    );
}

#[test]
fn steer_response_names_an_outcome_it_does_not_know() {
    let known: SteerResponse =
        serde_json::from_value(serde_json::json!({"outcome": "injected"})).expect("decodes");
    assert_eq!(known.outcome(), SteerOutcome::Injected);
    let idle: SteerResponse = serde_json::from_value(
        serde_json::json!({"outcome": "promptRequired", "reason": "noRunningTurn"}),
    )
    .expect("decodes");
    assert_eq!(idle.outcome(), SteerOutcome::PromptRequired);
    // codex-acp's catch-all, which claude-agent-acp's response type does not
    // contain. Neither does this client's, so it has to survive as a name.
    let failed: SteerResponse =
        serde_json::from_value(serde_json::json!({"outcome": "failed"})).expect("decodes");
    assert_eq!(
        failed.outcome(),
        SteerOutcome::Unrecognized("failed".to_string())
    );
    // Extra fields are an adapter moving ahead of this client, not corruption.
    let future: SteerResponse = serde_json::from_value(
        serde_json::json!({"outcome": "deferred", "reason": "permissionOpen", "extra": 1}),
    )
    .expect("decodes");
    assert_eq!(
        future.outcome(),
        SteerOutcome::Unrecognized("deferred (permissionOpen)".to_string())
    );
    // omp 18.12.6 names the queue it used on both answers.
    for (answer, outcome) in [
        ("injected", SteerOutcome::Injected),
        ("promptRequired", SteerOutcome::PromptRequired),
    ] {
        let omp: SteerResponse =
            serde_json::from_value(serde_json::json!({"outcome": answer, "queue": "steering"}))
                .expect("decodes");
        assert_eq!(omp.outcome(), outcome);
    }
}

#[tokio::test]
async fn command_loop_steers_an_active_prompt_without_disturbing_it() {
    let connection = ScriptedCommandConnection::default();
    let shared = command_loop_shared();
    let session_id = HarnessSessionId::new("session-1");
    shared.insert_session(session_id.clone(), PathBuf::from("/tmp/project"));
    let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));

    let (prompt_reply, prompt_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Prompt {
            session_id: session_id.clone(),
            prompt: HarnessPrompt::text("hold"),
            reply: prompt_reply,
        })
        .await
        .expect("starts prompt");
    connection.prompt_started.notified().await;

    let (steer_reply, steer_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Steer {
            session_id: session_id.clone(),
            prompt: HarnessPrompt::text("also check the tests"),
            reply: steer_reply,
        })
        .await
        .expect("queues steer");
    steer_response
        .await
        .expect("steer response")
        .expect("steer joins the running turn");
    // The turn is the prompt's, not the steer's: the session stays busy and the
    // original prompt is still the one that will answer.
    assert_eq!(shared.sessions()[0].state, HarnessSessionState::Prompting);

    connection.prompt_release.notify_one();
    assert_eq!(
        prompt_response
            .await
            .expect("prompt response")
            .expect("remote prompt completes"),
        HarnessStopReason::EndTurn
    );
    stop_command_loop(&command_tx, task).await;
    assert_eq!(
        connection.recorded_calls(),
        vec![
            "prompt:session-1".to_string(),
            "steer:session-1".to_string()
        ]
    );
}

#[tokio::test]
async fn steering_an_idle_session_never_reaches_the_wire() {
    let connection = ScriptedCommandConnection::default();
    let shared = command_loop_shared();
    let session_id = HarnessSessionId::new("session-1");
    shared.insert_session(session_id.clone(), PathBuf::from("/tmp/project"));
    let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));

    let (steer_reply, steer_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Steer {
            session_id: session_id.clone(),
            prompt: HarnessPrompt::text("too late"),
            reply: steer_reply,
        })
        .await
        .expect("queues steer");
    assert_eq!(
        steer_response
            .await
            .expect("steer response")
            .expect_err("an idle session has nothing to steer")
            .kind,
        HarnessErrorKind::NoActivePrompt
    );

    let (missing_reply, missing_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Steer {
            session_id: HarnessSessionId::new("missing"),
            prompt: HarnessPrompt::text("nowhere"),
            reply: missing_reply,
        })
        .await
        .expect("queues steer");
    assert_eq!(
        missing_response
            .await
            .expect("steer response")
            .expect_err("an unknown session cannot be steered")
            .kind,
        HarnessErrorKind::SessionNotFound
    );

    stop_command_loop(&command_tx, task).await;
    // codex-acp would have started a detached turn for either of these, because
    // it ignores the `_meta` that asks for the message back. Neither left here.
    assert!(connection.recorded_calls().is_empty());
}

/// A command loop whose adapter has advertised native steering, so a demotion
/// is something the test can watch happen.
fn command_loop_shared_with_native_steering() -> Arc<SharedState> {
    let shared = command_loop_shared();
    shared.status_tx.send_modify(|status| {
        status.capabilities.steering = SteeringCapability::Native;
    });
    shared
}

#[tokio::test]
async fn an_agent_that_advertised_steering_it_lacks_is_demoted_once() {
    let connection = ScriptedCommandConnection::default();
    connection.answer_steer_with(ScriptedSteer::MethodNotFound);
    let shared = command_loop_shared_with_native_steering();
    let session_id = HarnessSessionId::new("session-1");
    shared.insert_session(session_id.clone(), PathBuf::from("/tmp/project"));
    let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));

    let (prompt_reply, prompt_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Prompt {
            session_id: session_id.clone(),
            prompt: HarnessPrompt::text("hold"),
            reply: prompt_reply,
        })
        .await
        .expect("starts prompt");
    connection.prompt_started.notified().await;
    assert_eq!(
        shared.status().capabilities.steering,
        SteeringCapability::Native
    );

    let (steer_reply, steer_response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::Steer {
            session_id: session_id.clone(),
            prompt: HarnessPrompt::text("follow-up"),
            reply: steer_reply,
        })
        .await
        .expect("queues steer");
    assert_eq!(
        steer_response
            .await
            .expect("steer response")
            .expect_err("the advertised method does not exist")
            .kind,
        HarnessErrorKind::SteeringUnsupported
    );
    // The advertisement was wrong, so the published capability is corrected
    // rather than re-tested on every later steer.
    assert_eq!(
        shared.status().capabilities.steering,
        SteeringCapability::InterruptThenPrompt
    );
    // The map behind the compatibility matrix must not keep the claim the
    // capability just retracted.
    assert_eq!(
        shared
            .status()
            .capabilities
            .agent_advertises
            .get(&AcpFeature::SessionSteering),
        Some(&false)
    );

    connection.prompt_release.notify_one();
    prompt_response
        .await
        .expect("prompt response")
        .expect("remote prompt completes");
    stop_command_loop(&command_tx, task).await;
    assert_eq!(
        connection.recorded_calls(),
        vec![
            "prompt:session-1".to_string(),
            "steer:session-1".to_string()
        ],
        "one round trip is enough to establish the capability was a lie"
    );
}

#[tokio::test]
async fn steer_outcomes_that_delivered_nothing_report_no_active_prompt() {
    for (answer, expected_kind, expected_calls) in [
        (
            ScriptedSteer::Outcome(SteerOutcome::PromptRequired),
            HarnessErrorKind::NoActivePrompt,
            vec![
                "prompt:session-1".to_string(),
                "steer:session-1".to_string(),
            ],
        ),
        (
            // The turn settled under the steer and the agent started one of its
            // own, holding the message. Nothing is waiting on that turn, so the
            // loop ends it — but the cancel races whatever it already ran, so
            // the outcome is ambiguous rather than a clean miss.
            ScriptedSteer::Outcome(SteerOutcome::StartedNewTurn),
            HarnessErrorKind::PromptExecution,
            vec![
                "prompt:session-1".to_string(),
                "steer:session-1".to_string(),
                "cancel:session-1".to_string(),
            ],
        ),
        (
            // codex-acp's catch-all. It throws part-way through delivering, so
            // this is ambiguous rather than a definitive rejection, and the
            // caller must not read it as one.
            ScriptedSteer::Outcome(SteerOutcome::Unrecognized("failed".to_string())),
            HarnessErrorKind::PromptExecution,
            vec![
                "prompt:session-1".to_string(),
                "steer:session-1".to_string(),
            ],
        ),
        (
            ScriptedSteer::MethodNotFound,
            HarnessErrorKind::SteeringUnsupported,
            vec![
                "prompt:session-1".to_string(),
                "steer:session-1".to_string(),
            ],
        ),
    ] {
        let connection = ScriptedCommandConnection::default();
        connection.answer_steer_with(answer);
        let shared = command_loop_shared();
        let session_id = HarnessSessionId::new("session-1");
        shared.insert_session(session_id.clone(), PathBuf::from("/tmp/project"));
        let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));

        let (prompt_reply, prompt_response) = oneshot::channel();
        command_tx
            .send(AdapterCommand::Prompt {
                session_id: session_id.clone(),
                prompt: HarnessPrompt::text("hold"),
                reply: prompt_reply,
            })
            .await
            .expect("starts prompt");
        connection.prompt_started.notified().await;

        let (steer_reply, steer_response) = oneshot::channel();
        command_tx
            .send(AdapterCommand::Steer {
                session_id: session_id.clone(),
                prompt: HarnessPrompt::text("follow-up"),
                reply: steer_reply,
            })
            .await
            .expect("queues steer");
        assert_eq!(
            steer_response
                .await
                .expect("steer response")
                .expect_err("nothing was delivered")
                .kind,
            expected_kind
        );

        connection.prompt_release.notify_one();
        prompt_response
            .await
            .expect("prompt response")
            .expect("remote prompt completes");
        stop_command_loop(&command_tx, task).await;
        assert_eq!(connection.recorded_calls(), expected_calls);
    }
}

/// Core's proxy entries ride both `session/new` and the `session/load` that
/// restores a session, so a restored session gets a fresh token.
#[tokio::test]
async fn session_new_and_load_carry_mcp_entries() {
    let connection = ScriptedCommandConnection::default();
    let shared = command_loop_shared();
    let (command_tx, task) = spawn_command_loop(connection.clone(), Arc::clone(&shared));
    let entry = |token: &str| McpServerEntry {
        name: "github".into(),
        url: "http://127.0.0.1:4000/mcp/github".into(),
        headers: vec![("Authorization".into(), format!("Bearer {token}"))],
    };
    let (reply, response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::NewSession {
            cwd: PathBuf::from("/tmp/project"),
            config: SessionConfigRequest {
                mcp_servers: vec![entry("first")],
                ..SessionConfigRequest::default()
            },
            reply,
        })
        .await
        .unwrap();
    response.await.unwrap().expect("session opens");
    let (reply, response) = oneshot::channel();
    command_tx
        .send(AdapterCommand::LoadSession {
            session_id: HarnessSessionId::new("restored"),
            cwd: PathBuf::from("/tmp/project"),
            mode_id: None,
            mcp_servers: vec![entry("second")],
            reply,
        })
        .await
        .unwrap();
    response.await.unwrap().expect("session restores");
    assert_eq!(
        connection.recorded_calls(),
        vec![
            "new:/tmp/project mcp:github=http://127.0.0.1:4000/mcp/github",
            "load:restored:/tmp/project mcp:github=http://127.0.0.1:4000/mcp/github",
        ]
    );
    stop_command_loop(&command_tx, task).await;
}

#[test]
fn mcp_entries_go_out_as_http_servers_with_their_headers() {
    let servers = super::actor::acp_mcp_servers(vec![McpServerEntry {
        name: "github".into(),
        url: "http://127.0.0.1:4000/mcp/github".into(),
        headers: vec![("Authorization".into(), "Bearer abc".into())],
    }]);
    assert_eq!(
        serde_json::to_value(&servers).unwrap(),
        serde_json::json!([{
            "type": "http",
            "name": "github",
            "url": "http://127.0.0.1:4000/mcp/github",
            "headers": [{"name": "Authorization", "value": "Bearer abc"}],
        }])
    );
}
