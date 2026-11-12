// @generated automatically by Diesel CLI.

diesel::table! {
    agent_instances (id) {
        id -> Text,
        harness_id -> Text,
        label -> Text,
        home_json -> Text,
        created_at -> BigInt,
        deleted_at -> Nullable<BigInt>,
        claimed_at -> Nullable<BigInt>,
    }
}

diesel::table! {
    agent_sessions (id) {
        id -> Text,
        workspace_id -> Text,
        conversation_id -> Text,
        engine_id -> Text,
        external_session_id -> Nullable<Text>,
        source_scope_fingerprint -> Text,
        status -> Text,
        created_at -> BigInt,
        last_used_at -> BigInt,
        ended_at -> Nullable<BigInt>,
        end_reason -> Nullable<Text>,
        current_mode_id -> Nullable<Text>,
        available_commands -> Nullable<Text>,
        context_usage -> Nullable<Text>,
        history_seeded -> Bool,
    }
}

diesel::table! {
    conversation_attachments (id) {
        id -> Text,
        workspace_id -> Text,
        conversation_id -> Nullable<Text>,
        message_id -> Nullable<Text>,
        ordinal -> Nullable<BigInt>,
        kind -> Text,
        mime_type -> Text,
        byte_count -> BigInt,
        width -> BigInt,
        height -> BigInt,
        relative_path -> Text,
        source_kind -> Text,
        origin_path -> Nullable<Text>,
        created_at -> BigInt,
        claimed_at -> Nullable<BigInt>,
        idempotency_key -> Nullable<Text>,
    }
}

diesel::table! {
    conversation_messages (id) {
        id -> Text,
        conversation_id -> Text,
        role -> Text,
        body -> Text,
        created_at -> BigInt,
        source -> Text,
        note_json -> Nullable<Text>,
        note_delivered_at -> Nullable<BigInt>,
    }
}

diesel::table! {
    conversation_projects (conversation_id, project_id) {
        conversation_id -> Text,
        workspace_id -> Text,
        project_id -> Text,
        source_id -> Text,
        role -> Text,
        created_at -> BigInt,
        removed_at -> Nullable<BigInt>,
    }
}

diesel::table! {
    conversation_run_inputs (id) {
        id -> Text,
        workspace_id -> Text,
        conversation_id -> Text,
        message_id -> Text,
        requested_against_run_id -> Nullable<Text>,
        target_run_id -> Nullable<Text>,
        delivery_sequence -> Nullable<BigInt>,
        kind -> Text,
        retry_of_input_id -> Nullable<Text>,
        idempotency_key -> Text,
        status -> Text,
        delivery_mode -> Nullable<Text>,
        delivery_evidence -> Nullable<Text>,
        requested_at -> BigInt,
        delivery_attempted_at -> Nullable<BigInt>,
        delivered_at -> Nullable<BigInt>,
        ambiguity_detected_at -> Nullable<BigInt>,
        failed_at -> Nullable<BigInt>,
        failure_reason -> Nullable<Text>,
        resolved_at -> Nullable<BigInt>,
        resolution -> Nullable<Text>,
    }
}

diesel::table! {
    conversation_run_sources (run_id, canonical_path) {
        run_id -> Text,
        workspace_id -> Text,
        conversation_id -> Text,
        root_kind -> Text,
        project_id -> Nullable<Text>,
        source_id -> Nullable<Text>,
        canonical_path -> Text,
        role -> Text,
        created_at -> BigInt,
    }
}

diesel::table! {
    conversation_runs (id) {
        id -> Text,
        workspace_id -> Text,
        conversation_id -> Text,
        engine_id -> Text,
        model_id -> Nullable<Text>,
        agent_session_id -> Nullable<Text>,
        session_origin -> Nullable<Text>,
        status -> Text,
        request_options_json -> Nullable<Text>,
        event_sequence_cursor -> BigInt,
        queued_at -> BigInt,
        started_at -> Nullable<BigInt>,
        completed_at -> Nullable<BigInt>,
        stop_reason -> Nullable<Text>,
        error_json -> Nullable<Text>,
        origin -> Text,
        usage_json -> Nullable<Text>,
        history_replay_json -> Nullable<Text>,
    }
}

diesel::table! {
    conversations (id) {
        id -> Text,
        workspace_id -> Text,
        working_root_kind -> Text,
        title -> Text,
        created_at -> BigInt,
        updated_at -> BigInt,
        archived_at -> Nullable<BigInt>,
        always_allow_tool_actions -> Bool,
        parent_conversation_id -> Nullable<Text>,
        delegated_agent_name -> Nullable<Text>,
        delegation_key -> Nullable<Text>,
        delegated_task_title -> Nullable<Text>,
        delegated_avatar_seed -> Nullable<BigInt>,
        delegated_named_agent_id -> Nullable<Text>,
        delegated_from_run_id -> Nullable<Text>,
    }
}

diesel::table! {
    core_settings (key) {
        key -> Text,
        value -> Text,
    }
}

diesel::table! {
    mcp_servers (id) {
        id -> Text,
        name -> Text,
        transport_json -> Text,
        created_at -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::table! {
    named_agents (id) {
        id -> Text,
        name -> Text,
        engine_id -> Text,
        model_id -> Nullable<Text>,
        mode_id -> Nullable<Text>,
        instructions -> Text,
        created_at -> BigInt,
        updated_at -> BigInt,
        avatar_seed -> Nullable<BigInt>,
    }
}

diesel::table! {
    project_sources (id) {
        id -> Text,
        project_id -> Text,
        kind -> Text,
        display_name -> Text,
        canonical_path -> Text,
        is_primary -> Integer,
        created_at -> BigInt,
        updated_at -> BigInt,
        last_seen_at -> BigInt,
        archived_at -> Nullable<BigInt>,
    }
}

diesel::table! {
    projects (id) {
        id -> Text,
        display_name -> Text,
        created_at -> BigInt,
        updated_at -> BigInt,
        last_opened_at -> BigInt,
        archived_at -> Nullable<BigInt>,
    }
}

diesel::table! {
    run_events (id) {
        id -> Text,
        run_id -> Text,
        sequence -> BigInt,
        kind -> Text,
        payload_json -> Text,
        created_at -> BigInt,
    }
}

diesel::table! {
    transcript_body_chunks (item_id, byte_offset) {
        item_id -> Text,
        byte_offset -> BigInt,
        body -> Text,
    }
}

diesel::table! {
    transcript_capture (run_id) {
        run_id -> Text,
        state -> Text,
        gaps_json -> Text,
    }
}

diesel::table! {
    transcript_checkpoint_bodies (item_id, revision, byte_offset) {
        conversation_id -> Text,
        item_id -> Text,
        revision -> BigInt,
        byte_offset -> BigInt,
        body -> Text,
    }
}

diesel::table! {
    transcript_checkpoint_heads (item_id) {
        item_id -> Text,
        revision -> BigInt,
    }
}

diesel::table! {
    transcript_cursors (conversation_id) {
        conversation_id -> Text,
        sequence -> BigInt,
    }
}

diesel::table! {
    transcript_gaps (item_id) {
        item_id -> Text,
        end_position -> BigInt,
        dropped_updates -> BigInt,
        reason -> Text,
    }
}

diesel::table! {
    transcript_items (id) {
        id -> Text,
        conversation_id -> Text,
        run_id -> Nullable<Text>,
        position -> BigInt,
        revision -> BigInt,
        kind -> Text,
        completion -> Text,
        provider_message_id -> Nullable<Text>,
        adapter_metadata_json -> Text,
        body_byte_count -> BigInt,
        created_at -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::table! {
    transcript_payloads (payload_id, revision) {
        payload_id -> Text,
        revision -> BigInt,
        item_id -> Text,
        descriptor -> Text,
    }
}

diesel::table! {
    transcript_permissions (item_id) {
        item_id -> Text,
        request_id -> Text,
        decision_json -> Text,
    }
}

diesel::table! {
    transcript_plans (item_id) {
        item_id -> Text,
        entries_json -> Text,
    }
}

diesel::table! {
    transcript_tools (item_id) {
        item_id -> Text,
        provider_tool_call_id -> Nullable<Text>,
        metadata_json -> Text,
    }
}

diesel::table! {
    workspace_mcp_disabled (workspace_id, server_id) {
        workspace_id -> Text,
        server_id -> Text,
        disabled_at -> BigInt,
    }
}

diesel::table! {
    workspace_projects (workspace_id, project_id) {
        workspace_id -> Text,
        project_id -> Text,
        default_source_id -> Text,
        sort_order -> BigInt,
        added_at -> BigInt,
        removed_at -> Nullable<BigInt>,
    }
}

diesel::table! {
    workspaces (id) {
        id -> Text,
        display_name -> Text,
        created_at -> BigInt,
        updated_at -> BigInt,
        last_opened_at -> BigInt,
        archived_at -> Nullable<BigInt>,
    }
}

diesel::joinable!(conversation_attachments -> workspaces (workspace_id));
diesel::joinable!(conversation_messages -> conversations (conversation_id));
diesel::joinable!(conversations -> workspaces (workspace_id));
diesel::joinable!(project_sources -> projects (project_id));
diesel::joinable!(run_events -> conversation_runs (run_id));
diesel::joinable!(transcript_body_chunks -> transcript_items (item_id));
diesel::joinable!(transcript_capture -> conversation_runs (run_id));
diesel::joinable!(transcript_checkpoint_bodies -> conversations (conversation_id));
diesel::joinable!(transcript_checkpoint_heads -> transcript_items (item_id));
diesel::joinable!(transcript_cursors -> conversations (conversation_id));
diesel::joinable!(transcript_gaps -> transcript_items (item_id));
diesel::joinable!(transcript_items -> conversations (conversation_id));
diesel::joinable!(transcript_payloads -> transcript_items (item_id));
diesel::joinable!(transcript_permissions -> transcript_items (item_id));
diesel::joinable!(transcript_plans -> transcript_items (item_id));
diesel::joinable!(transcript_tools -> transcript_items (item_id));
diesel::joinable!(workspace_mcp_disabled -> mcp_servers (server_id));
diesel::joinable!(workspace_mcp_disabled -> workspaces (workspace_id));
diesel::joinable!(workspace_projects -> projects (project_id));
diesel::joinable!(workspace_projects -> workspaces (workspace_id));

diesel::allow_tables_to_appear_in_same_query!(
    agent_instances,
    agent_sessions,
    conversation_attachments,
    conversation_messages,
    conversation_projects,
    conversation_run_inputs,
    conversation_run_sources,
    conversation_runs,
    conversations,
    core_settings,
    mcp_servers,
    named_agents,
    project_sources,
    projects,
    run_events,
    transcript_body_chunks,
    transcript_capture,
    transcript_checkpoint_bodies,
    transcript_checkpoint_heads,
    transcript_cursors,
    transcript_gaps,
    transcript_items,
    transcript_payloads,
    transcript_permissions,
    transcript_plans,
    transcript_tools,
    workspace_mcp_disabled,
    workspace_projects,
    workspaces,
);
