use ottam_protocol::*;
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

#[test]
fn generated_initialize_request_fixture_matches_types() {
    assert_fixture_matches(
        "fixtures/initialize-request.json",
        &initialize_request_fixture(),
    );
    let decoded: InitializeRequest = read_fixture("fixtures/initialize-request.json");
    assert_eq!(decoded.method, "initialize");
}

#[test]
fn generated_initialize_response_fixture_matches_types() {
    assert_fixture_matches(
        "fixtures/initialize-response.json",
        &initialize_response_fixture(),
    );
    let decoded: InitializeResponse = read_fixture("fixtures/initialize-response.json");
    assert!(decoded.result.is_some());
}

#[test]
fn generated_protocol_error_response_fixture_matches_types() {
    assert_fixture_matches(
        "fixtures/protocol-error-response.json",
        &protocol_error_response_fixture(),
    );
    let decoded: ProtocolResponse<Value> = read_fixture("fixtures/protocol-error-response.json");
    assert!(decoded.error.is_some());
}

#[test]
fn generated_initialized_notification_fixture_matches_types() {
    assert_fixture_matches(
        "fixtures/initialized-notification.json",
        &initialized_notification_fixture(),
    );
    let decoded: InitializedNotification = read_fixture("fixtures/initialized-notification.json");
    assert_eq!(decoded.method, "initialized");
}

#[test]
fn generated_project_open_path_fixtures_match_types() {
    assert_fixture_matches(
        "fixtures/project-open-path-request.json",
        &project_open_path_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/project-open-path-response.json",
        &project_open_path_response_fixture(),
    );
    let request: ProjectOpenPathRequest = read_fixture("fixtures/project-open-path-request.json");
    let response: ProjectOpenPathResponse =
        read_fixture("fixtures/project-open-path-response.json");
    assert_eq!(request.method, "project.openPath");
    assert!(response.result.is_some());
}

#[test]
fn generated_git_diff_snapshot_fixtures_match_types() {
    assert_fixture_matches(
        "fixtures/git-diff-snapshot-request.json",
        &git_diff_snapshot_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/git-diff-snapshot-response.json",
        &git_diff_snapshot_response_fixture(),
    );
    let request: GitDiffSnapshotRequest = read_fixture("fixtures/git-diff-snapshot-request.json");
    let response: GitDiffSnapshotResponse =
        read_fixture("fixtures/git-diff-snapshot-response.json");
    assert_eq!(request.method, "git.diffSnapshot");
    assert!(response.result.is_some());
}

#[test]
fn generated_workspace_fixtures_match_types() {
    let fixtures = [
        (
            "fixtures/workspace-list-request.json",
            serde_json::to_value(workspace_list_request_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-list-response.json",
            serde_json::to_value(workspace_list_response_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-create-request.json",
            serde_json::to_value(workspace_create_request_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-create-response.json",
            serde_json::to_value(workspace_create_response_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-open-request.json",
            serde_json::to_value(workspace_open_request_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-open-response.json",
            serde_json::to_value(workspace_open_response_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-get-request.json",
            serde_json::to_value(workspace_get_request_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-get-response.json",
            serde_json::to_value(workspace_get_response_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-update-request.json",
            serde_json::to_value(workspace_update_request_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-update-response.json",
            serde_json::to_value(workspace_update_response_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-archive-request.json",
            serde_json::to_value(workspace_archive_request_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-archive-response.json",
            serde_json::to_value(workspace_archive_response_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-restore-request.json",
            serde_json::to_value(workspace_restore_request_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-restore-response.json",
            serde_json::to_value(workspace_restore_response_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-add-project-request.json",
            serde_json::to_value(workspace_add_project_request_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-add-project-response.json",
            serde_json::to_value(workspace_add_project_response_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-remove-project-request.json",
            serde_json::to_value(workspace_remove_project_request_fixture()).unwrap(),
        ),
        (
            "fixtures/workspace-remove-project-response.json",
            serde_json::to_value(workspace_remove_project_response_fixture()).unwrap(),
        ),
    ];
    for (path, expected) in fixtures {
        let actual: Value = read_fixture(path);
        assert_eq!(actual, expected, "fixture drifted: {path}");
    }
}

#[test]
fn generated_engine_list_fixtures_match_types() {
    assert_fixture_matches(
        "fixtures/engine-list-request.json",
        &engine_list_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/engine-list-response.json",
        &engine_list_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/engine-changed-notification.json",
        &engine_changed_notification_fixture(),
    );
    let request: EngineListRequest = read_fixture("fixtures/engine-list-request.json");
    let response: EngineListResponse = read_fixture("fixtures/engine-list-response.json");
    let changed: EngineChangedNotification =
        read_fixture("fixtures/engine-changed-notification.json");
    assert_eq!(request.method, "engine.list");
    assert!(response.result.is_some());
    assert_eq!(changed.method, "engine.changed");
}

#[test]
fn generated_agent_fixtures_match_types() {
    let fixtures = [
        (
            "fixtures/agent-list-request.json",
            serde_json::to_value(agent_list_request_fixture()).unwrap(),
        ),
        (
            "fixtures/agent-list-response.json",
            serde_json::to_value(agent_list_response_fixture()).unwrap(),
        ),
        (
            "fixtures/agent-auth-state-request.json",
            serde_json::to_value(agent_auth_state_request_fixture()).unwrap(),
        ),
        (
            "fixtures/agent-auth-state-response.json",
            serde_json::to_value(agent_auth_state_response_fixture()).unwrap(),
        ),
        (
            "fixtures/agent-request-permission-request.json",
            serde_json::to_value(agent_request_permission_request_fixture()).unwrap(),
        ),
        (
            "fixtures/agent-request-permission-response.json",
            serde_json::to_value(agent_request_permission_response_fixture()).unwrap(),
        ),
    ];
    for (path, expected) in fixtures {
        let actual: Value = read_fixture(path);
        assert_eq!(actual, expected, "fixture drifted: {path}");
    }
}

#[test]
fn generated_conversation_fixtures_match_types() {
    assert_fixture_matches(
        "fixtures/conversation-create-request.json",
        &conversation_create_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-create-response.json",
        &conversation_create_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-delete-request.json",
        &conversation_delete_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-delete-response.json",
        &conversation_delete_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-list-request.json",
        &conversation_list_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-list-response.json",
        &conversation_list_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-get-request.json",
        &conversation_get_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-get-response.json",
        &conversation_get_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-post-message-request.json",
        &conversation_post_message_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-post-message-response.json",
        &conversation_post_message_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-create-attachment-request.json",
        &conversation_create_attachment_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-create-attachment-response.json",
        &conversation_create_attachment_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-discard-attachment-request.json",
        &conversation_discard_attachment_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-discard-attachment-response.json",
        &conversation_discard_attachment_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-start-run-request.json",
        &conversation_start_run_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-start-run-response.json",
        &conversation_start_run_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-cancel-run-request.json",
        &conversation_cancel_run_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-cancel-run-response.json",
        &conversation_cancel_run_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-steer-run-request.json",
        &conversation_steer_run_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-steer-run-response.json",
        &conversation_steer_run_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-stop-background-task-request.json",
        &conversation_stop_background_task_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-stop-background-task-response.json",
        &conversation_stop_background_task_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-set-mode-request.json",
        &conversation_set_mode_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-set-mode-response.json",
        &conversation_set_mode_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-set-always-allow-tool-actions-request.json",
        &conversation_set_always_allow_tool_actions_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-set-always-allow-tool-actions-response.json",
        &conversation_set_always_allow_tool_actions_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/tool-permissions-get-request.json",
        &tool_permissions_get_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/tool-permissions-get-response.json",
        &tool_permissions_get_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/tool-permissions-set-request.json",
        &tool_permissions_set_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/tool-permissions-set-response.json",
        &tool_permissions_set_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-resolve-run-input-delivery-request.json",
        &conversation_resolve_run_input_delivery_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-resolve-run-input-delivery-response.json",
        &conversation_resolve_run_input_delivery_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-list-messages-request.json",
        &conversation_list_messages_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-list-messages-response.json",
        &conversation_list_messages_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-list-runs-request.json",
        &conversation_list_runs_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-list-runs-response.json",
        &conversation_list_runs_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-list-run-inputs-request.json",
        &conversation_list_run_inputs_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-list-run-inputs-response.json",
        &conversation_list_run_inputs_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-list-agent-sessions-request.json",
        &conversation_list_agent_sessions_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-list-agent-sessions-response.json",
        &conversation_list_agent_sessions_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-list-events-request.json",
        &conversation_list_events_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/conversation-list-events-response.json",
        &conversation_list_events_response_fixture(),
    );
    let create: ConversationCreateRequest =
        read_fixture("fixtures/conversation-create-request.json");
    let delete: ConversationDeleteRequest =
        read_fixture("fixtures/conversation-delete-request.json");
    let list: ConversationListRequest = read_fixture("fixtures/conversation-list-request.json");
    let get: ConversationGetRequest = read_fixture("fixtures/conversation-get-request.json");
    let post_message: ConversationPostMessageRequest =
        read_fixture("fixtures/conversation-post-message-request.json");
    let start_run: ConversationStartRunRequest =
        read_fixture("fixtures/conversation-start-run-request.json");
    let cancel_run: ConversationCancelRunRequest =
        read_fixture("fixtures/conversation-cancel-run-request.json");
    let steer_run: ConversationSteerRunRequest =
        read_fixture("fixtures/conversation-steer-run-request.json");
    let stop_background_task: ConversationStopBackgroundTaskRequest =
        read_fixture("fixtures/conversation-stop-background-task-request.json");
    let resolve_delivery: ConversationResolveRunInputDeliveryRequest =
        read_fixture("fixtures/conversation-resolve-run-input-delivery-request.json");
    let list_messages: ConversationListMessagesRequest =
        read_fixture("fixtures/conversation-list-messages-request.json");
    let list_events: ConversationListEventsRequest =
        read_fixture("fixtures/conversation-list-events-request.json");
    let create_response: ConversationCreateResponse =
        read_fixture("fixtures/conversation-create-response.json");
    let delete_response: ConversationDeleteResponse =
        read_fixture("fixtures/conversation-delete-response.json");
    let list_response: ConversationListResponse =
        read_fixture("fixtures/conversation-list-response.json");
    let get_response: ConversationGetResponse =
        read_fixture("fixtures/conversation-get-response.json");
    let post_message_response: ConversationPostMessageResponse =
        read_fixture("fixtures/conversation-post-message-response.json");
    let start_run_response: ConversationStartRunResponse =
        read_fixture("fixtures/conversation-start-run-response.json");
    let cancel_run_response: ConversationCancelRunResponse =
        read_fixture("fixtures/conversation-cancel-run-response.json");
    let steer_run_response: ConversationSteerRunResponse =
        read_fixture("fixtures/conversation-steer-run-response.json");
    let stop_background_task_response: ConversationStopBackgroundTaskResponse =
        read_fixture("fixtures/conversation-stop-background-task-response.json");
    let resolve_delivery_response: ConversationResolveRunInputDeliveryResponse =
        read_fixture("fixtures/conversation-resolve-run-input-delivery-response.json");
    let list_messages_response: ConversationListMessagesResponse =
        read_fixture("fixtures/conversation-list-messages-response.json");
    let list_events_response: ConversationListEventsResponse =
        read_fixture("fixtures/conversation-list-events-response.json");
    assert_eq!(create.method, "conversation.create");
    assert_eq!(delete.method, "conversation.delete");
    assert_eq!(list.method, "conversation.list");
    assert_eq!(get.method, "conversation.get");
    assert_eq!(post_message.method, "conversation.postMessage");
    assert_eq!(start_run.method, "conversation.startRun");
    assert_eq!(cancel_run.method, "conversation.cancelRun");
    assert_eq!(steer_run.method, "conversation.steerRun");
    assert_eq!(
        stop_background_task.method,
        "conversation.stopBackgroundTask"
    );
    assert_eq!(
        resolve_delivery.method,
        "conversation.resolveRunInputDelivery"
    );
    assert_eq!(list_messages.method, "conversation.listMessages");
    assert_eq!(list_events.method, "conversation.listEvents");
    assert!(create_response.result.is_some());
    assert!(delete_response.result.is_some());
    assert!(list_response.result.is_some());
    assert!(get_response.result.is_some());
    assert!(post_message_response.result.is_some());
    assert!(start_run_response.result.is_some());
    assert!(cancel_run_response.result.is_some());
    assert!(steer_run_response.result.is_some());
    assert!(stop_background_task_response.result.is_some());
    assert!(resolve_delivery_response.result.is_some());
    assert!(list_messages_response.result.is_some());
    assert!(list_events_response.result.is_some());
}

#[test]
fn generated_terminal_fixtures_match_types() {
    assert_fixture_matches(
        "fixtures/terminal-open-request.json",
        &terminal_open_request_fixture(),
    );
    assert_fixture_matches(
        "fixtures/terminal-open-response.json",
        &terminal_open_response_fixture(),
    );
    assert_fixture_matches(
        "fixtures/terminal-output-delta-notification.json",
        &terminal_output_delta_notification_fixture(),
    );
    let request: TerminalOpenRequest = read_fixture("fixtures/terminal-open-request.json");
    let response: TerminalOpenResponse = read_fixture("fixtures/terminal-open-response.json");
    let notification: TerminalOutputDeltaNotification =
        read_fixture("fixtures/terminal-output-delta-notification.json");
    assert_eq!(request.method, "terminal.open");
    assert!(response.result.is_some());
    assert_eq!(notification.method, "terminal.outputDelta");
}

#[test]
fn generated_swift_protocol_types_are_fresh() {
    let path = crate_root()
        .join("../../../app/Sources/OttamBackendClient/ProtocolTypes.generated.swift");
    let committed = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("failed to read {}: {error}", path.display());
    });
    assert_eq!(
        committed,
        ottam_protocol::swift::generate_swift_protocol_source(),
        "ProtocolTypes.generated.swift is stale; run: cargo run --manifest-path backend/Cargo.toml -p ottam-protocol --bin generate-protocol-artifacts"
    );
}

#[test]
fn conversation_start_run_params_decode_without_model_id() {
    let params: ottam_protocol::ConversationStartRunParams = serde_json::from_str(
        r#"{
            "workspaceID": "019b0000-0000-7000-8000-000000000010",
            "conversationID": "conversation_fixture",
            "engineID": "codex",
            "runInputID": "run_input_fixture"
        }"#,
    )
    .expect("params without modelID must decode");
    assert_eq!(params.model_id, None);
    let encoded = serde_json::to_value(&params).expect("encode");
    assert!(
        encoded.get("modelID").is_none(),
        "unset modelID must be omitted from the wire"
    );
}

#[test]
fn conversation_post_message_params_decode_without_attachment_ids() {
    let params: ottam_protocol::ConversationPostMessageParams = serde_json::from_str(
        r#"{
            "workspaceID": "019b0000-0000-7000-8000-000000000010",
            "conversationID": "conversation_fixture",
            "message": "No attachments here",
            "idempotencyKey": "client-key"
        }"#,
    )
    .expect("params without attachmentIDs must decode");
    assert!(params.attachment_ids.is_empty());
}

#[test]
fn conversation_message_decode_without_attachments() {
    let message: ottam_protocol::ConversationMessage = serde_json::from_str(
        r#"{
            "messageID": "message_fixture",
            "conversationID": "conversation_fixture",
            "role": "user",
            "body": "Predates attachments",
            "position": 0,
                        "createdAt": "2026-07-17T00:00:00Z"
        }"#,
    )
    .expect("message without attachments must decode");
    assert!(message.attachments.is_empty());
    // Re-encoding always emits the list: the Swift decoder round-trips it, and
    // the fixture gate compares re-encoded output byte for byte.
    let encoded = serde_json::to_value(&message).expect("encode");
    assert_eq!(
        encoded.get("attachments"),
        Some(&serde_json::json!([])),
        "an absent attachment list must re-encode as an empty array"
    );
}

/// ACP itself treats a method with no `type` as agent-run, so an auth method
/// that arrives without a `kind` must land on the same reading rather than on
/// the one that would have Ottam run a command.
#[test]
fn agent_auth_method_decode_without_kind() {
    let method: ottam_protocol::AgentAuthMethod = serde_json::from_str(
        r#"{
            "methodID": "chatgpt",
            "name": "Sign in with ChatGPT"
        }"#,
    )
    .expect("method without kind must decode");
    assert_eq!(method.kind, ottam_protocol::AgentAuthMethodKind::Agent);
}

/// `revealTerminal` is the one field a client acts on destructively — it opens
/// a window. Absent must mean "do not", never "assume yes".
#[test]
fn agent_sign_in_event_decode_without_reveal_terminal() {
    let event: ottam_protocol::AgentSignInEventParams = serde_json::from_str(
        r#"{
            "agentID": "codex",
            "authSessionID": "agent_auth_fixture",
            "delivery": "adapterBrowser",
            "phase": "awaitingUser"
        }"#,
    )
    .expect("event without revealTerminal must decode");
    assert!(!event.reveal_terminal);
    assert!(event.terminal.is_none());
}

fn assert_fixture_matches<T>(relative_path: &str, value: &T)
where
    T: Serialize,
{
    assert_json_file_matches(relative_path, value);
}

fn assert_json_file_matches<T>(relative_path: &str, value: &T)
where
    T: Serialize,
{
    let path = crate_root().join(relative_path);
    let actual = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("failed to read {}: {error}", path.display());
    });
    let mut expected = serde_json::to_string_pretty(value).unwrap();
    expected.push('\n');
    assert_eq!(actual, expected, "{relative_path} is stale");
}

fn read_fixture<T>(relative_path: &str) -> T
where
    T: serde::de::DeserializeOwned,
{
    let path = crate_root().join(relative_path);
    let data = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("failed to read {}: {error}", path.display());
    });
    serde_json::from_str(&data).unwrap_or_else(|error| {
        panic!("failed to decode {}: {error}", path.display());
    })
}

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn transcript_fixtures_preserve_revision_and_body_identity() {
    let page = conversation_list_transcript_response_fixture()
        .result
        .unwrap();
    let delta = conversation_transcript_delta_notification_fixture().params;
    let body = conversation_read_transcript_body_response_fixture()
        .result
        .unwrap();
    assert_eq!(page.items, delta.items);
    assert_eq!(body.item_id, page.items[0].item_id);
    assert_eq!(body.revision, page.items[0].revision);
    assert_eq!(body.text.len() as u64, page.items[0].body_byte_count);
}
