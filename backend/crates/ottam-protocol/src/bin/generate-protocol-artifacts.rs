use ottam_protocol::*;
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// Each entry pairs the fixture function's name with its serialized value; the
/// output file name is derived from the function name (`foo_bar_fixture` →
/// `fixtures/foo-bar.json`), so adding a message is a one-ident change here.
macro_rules! fixture_entries {
    ($($fixture:ident),* $(,)?) => {
        vec![$(
            (
                stringify!($fixture),
                serde_json::to_value($fixture()).expect("fixture serializes"),
            ),
        )*]
    };
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // Protocol-message fixtures only. Building-block object fixtures
    // (`agent_summary_fixture`, `conversation_detail_fixture`, ...) are embedded in
    // these responses and are not written as standalone files.
    let fixture_directory = manifest_dir.join("fixtures");
    if fixture_directory.exists() {
        for entry in fs::read_dir(&fixture_directory)? {
            let path = entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
                fs::remove_file(path)?;
            }
        }
    }
    let fixtures: Vec<(&str, Value)> = fixture_entries![
        initialize_request_fixture,
        initialize_response_fixture,
        protocol_error_response_fixture,
        initialized_notification_fixture,
        core_exit_error_notification_fixture,
        workspace_list_request_fixture,
        workspace_list_response_fixture,
        workspace_create_request_fixture,
        workspace_create_response_fixture,
        workspace_open_request_fixture,
        workspace_open_response_fixture,
        workspace_get_request_fixture,
        workspace_get_response_fixture,
        workspace_update_request_fixture,
        workspace_update_response_fixture,
        workspace_archive_request_fixture,
        workspace_archive_response_fixture,
        workspace_restore_request_fixture,
        workspace_restore_response_fixture,
        workspace_add_project_request_fixture,
        workspace_add_project_response_fixture,
        workspace_remove_project_request_fixture,
        workspace_remove_project_response_fixture,
        project_open_path_request_fixture,
        project_open_path_response_fixture,
        git_diff_snapshot_request_fixture,
        git_diff_snapshot_response_fixture,
        engine_list_request_fixture,
        engine_list_response_fixture,
        engine_changed_notification_fixture,
        agent_list_request_fixture,
        storage_status_request_fixture,
        storage_update_request_fixture,
        storage_clear_cache_request_fixture,
        storage_status_response_fixture,
        agent_list_response_fixture,
        agent_home_info_request_fixture,
        agent_home_info_response_fixture,
        agent_auth_state_request_fixture,
        agent_auth_state_response_fixture,
        agent_sign_in_request_fixture,
        agent_sign_in_response_fixture,
        agent_sign_in_cancel_request_fixture,
        agent_sign_in_cancel_response_fixture,
        agent_sign_in_finish_request_fixture,
        agent_detect_setups_request_fixture,
        agent_detect_setups_response_fixture,
        agent_create_request_fixture,
        agent_create_managed_request_fixture,
        agent_rename_request_fixture,
        agent_delete_request_fixture,
        agent_create_response_fixture,
        agent_set_layout_request_fixture,
        agent_set_layout_response_fixture,
        agent_shutdown_readiness_request_fixture,
        agent_shutdown_readiness_response_fixture,
        agent_prepare_shutdown_request_fixture,
        agent_prepare_shutdown_response_fixture,
        agent_cancel_shutdown_request_fixture,
        agent_cancel_shutdown_response_fixture,
        agent_sign_in_finish_response_fixture,
        agent_sign_in_event_notification_fixture,
        agent_install_request_fixture,
        agent_install_response_fixture,
        agent_install_cancel_request_fixture,
        agent_install_cancel_response_fixture,
        agent_install_event_notification_fixture,
        agent_uninstall_request_fixture,
        agent_uninstall_response_fixture,
        agent_plan_usage_request_fixture,
        agent_plan_usage_response_fixture,
        agent_plan_usage_changed_notification_fixture,
        runtime_install_request_fixture,
        runtime_install_response_fixture,
        runtime_install_cancel_request_fixture,
        runtime_install_cancel_response_fixture,
        runtime_install_event_notification_fixture,
        agent_request_permission_request_fixture,
        agent_request_permission_response_fixture,
        agent_ask_question_request_fixture,
        agent_ask_question_response_fixture,
        cancel_request_notification_fixture,
        conversation_create_request_fixture,
        conversation_create_response_fixture,
        conversation_delete_request_fixture,
        conversation_delete_response_fixture,
        conversation_list_request_fixture,
        conversation_list_response_fixture,
        conversation_get_request_fixture,
        conversation_get_response_fixture,
        conversation_post_message_request_fixture,
        conversation_post_message_response_fixture,
        conversation_create_attachment_request_fixture,
        conversation_create_attachment_response_fixture,
        conversation_discard_attachment_request_fixture,
        conversation_discard_attachment_response_fixture,
        conversation_start_run_request_fixture,
        conversation_start_run_response_fixture,
        conversation_cancel_run_request_fixture,
        conversation_cancel_run_response_fixture,
        conversation_steer_run_request_fixture,
        conversation_steer_run_response_fixture,
        conversation_stop_background_task_request_fixture,
        conversation_stop_background_task_response_fixture,
        mcp_list_request_fixture,
        mcp_list_response_fixture,
        mcp_add_request_fixture,
        mcp_add_response_fixture,
        mcp_update_request_fixture,
        mcp_update_response_fixture,
        mcp_remove_request_fixture,
        mcp_remove_response_fixture,
        mcp_set_workspace_enabled_request_fixture,
        mcp_set_workspace_enabled_response_fixture,
        mcp_server_state_changed_notification_fixture,
        mcp_import_candidates_request_fixture,
        mcp_import_candidates_response_fixture,
        mcp_sign_in_request_fixture,
        mcp_sign_in_response_fixture,
        mcp_cancel_sign_in_request_fixture,
        mcp_cancel_sign_in_response_fixture,
        mcp_sign_out_request_fixture,
        mcp_sign_out_response_fixture,
        mcp_sign_in_finished_notification_fixture,
        mcp_sign_in_needed_notification_fixture,
        mcp_servers_changed_notification_fixture,
        named_agent_list_request_fixture,
        named_agent_list_response_fixture,
        named_agent_create_request_fixture,
        named_agent_create_response_fixture,
        named_agent_update_request_fixture,
        named_agent_update_response_fixture,
        named_agent_delete_request_fixture,
        named_agent_delete_response_fixture,
        conversation_continue_delegation_request_fixture,
        conversation_continue_delegation_response_fixture,
        conversation_send_to_parent_request_fixture,
        conversation_send_to_parent_response_fixture,
        conversation_list_changed_notification_fixture,
        conversation_set_mode_request_fixture,
        conversation_set_mode_response_fixture,
        conversation_set_always_allow_tool_actions_request_fixture,
        conversation_set_always_allow_tool_actions_response_fixture,
        tool_permissions_get_request_fixture,
        tool_permissions_get_response_fixture,
        tool_permissions_set_request_fixture,
        tool_permissions_set_response_fixture,
        conversation_resolve_run_input_delivery_request_fixture,
        conversation_resolve_run_input_delivery_response_fixture,
        conversation_list_messages_request_fixture,
        conversation_list_messages_response_fixture,
        conversation_list_runs_request_fixture,
        conversation_list_runs_response_fixture,
        conversation_list_run_inputs_request_fixture,
        conversation_list_run_inputs_response_fixture,
        conversation_list_agent_sessions_request_fixture,
        conversation_list_agent_sessions_response_fixture,
        conversation_list_events_request_fixture,
        conversation_list_events_response_fixture,
        conversation_event_delta_notification_fixture,
        conversation_list_transcript_request_fixture,
        conversation_list_transcript_response_fixture,
        conversation_read_transcript_body_request_fixture,
        conversation_read_transcript_body_response_fixture,
        conversation_read_transcript_payload_request_fixture,
        conversation_read_transcript_payload_response_fixture,
        conversation_transcript_delta_notification_fixture,
        terminal_open_request_fixture,
        terminal_open_response_fixture,
        terminal_output_delta_notification_fixture,
    ];
    for (fixture_fn, value) in fixtures {
        let path = manifest_dir
            .join("fixtures")
            .join(fixture_file_name(fixture_fn));
        write_pretty_json(&path, &value)?;
    }

    let generated_swift = manifest_dir
        .join("../../..")
        .join("app/Sources/OttamBackendClient/ProtocolTypes.generated.swift");
    fs::write(&generated_swift, swift::generate_swift_protocol_source())?;
    Ok(())
}

fn fixture_file_name(fixture_fn: &str) -> String {
    format!(
        "{}.json",
        fixture_fn.trim_end_matches("_fixture").replace('_', "-")
    )
}

fn write_pretty_json<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut encoded = serde_json::to_string_pretty(value)?;
    encoded.push('\n');
    fs::write(path, encoded)?;
    Ok(())
}
