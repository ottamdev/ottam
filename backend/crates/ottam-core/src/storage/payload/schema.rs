// @generated automatically by Diesel CLI.

diesel::table! {
    payload_cache (revision_id) {
        revision_id -> BigInt,
        evicted -> Integer,
        accessed_at_ms -> BigInt,
        agent_id -> Nullable<Text>,
        session_id -> Nullable<Text>,
        tool_call_id -> Nullable<Text>,
        cwd -> Nullable<Text>,
    }
}

diesel::table! {
    payload_cache_settings (id) {
        id -> BigInt,
        limit_gb -> BigInt,
    }
}

diesel::table! {
    payload_chunks (part_id, chunk_index) {
        part_id -> BigInt,
        chunk_index -> BigInt,
        raw_offset -> BigInt,
        raw_length -> BigInt,
        codec -> Text,
        raw_sha256 -> Binary,
        encoded_bytes -> Binary,
    }
}

diesel::table! {
    payload_parts (id) {
        id -> BigInt,
        revision_id -> BigInt,
        part_index -> Integer,
        name -> Text,
        format -> Text,
        source_path -> Nullable<Text>,
        committed_raw_bytes -> BigInt,
        committed_encoded_bytes -> BigInt,
        chunk_count -> BigInt,
        final_sha256 -> Nullable<Binary>,
        unavailable_reason -> Nullable<Text>,
    }
}

diesel::table! {
    payload_revisions (id) {
        id -> BigInt,
        payload_id -> Text,
        revision -> BigInt,
        workspace_id -> Text,
        conversation_id -> Text,
        item_id -> Text,
        run_id -> Text,
        role -> Text,
        state -> Text,
        upstream_truncated -> Nullable<Integer>,
        locally_truncated -> Integer,
        capture_reason -> Nullable<Text>,
        created_at_ms -> BigInt,
        updated_at_ms -> BigInt,
    }
}

diesel::table! {
    payload_usage (workspace_id, conversation_id) {
        workspace_id -> Text,
        conversation_id -> Text,
        logical_bytes -> BigInt,
        encoded_bytes -> BigInt,
        overhead_bytes -> BigInt,
    }
}

diesel::joinable!(payload_cache -> payload_revisions (revision_id));
diesel::joinable!(payload_chunks -> payload_parts (part_id));
diesel::joinable!(payload_parts -> payload_revisions (revision_id));

diesel::allow_tables_to_appear_in_same_query!(
    payload_cache,
    payload_cache_settings,
    payload_chunks,
    payload_parts,
    payload_revisions,
    payload_usage,
);
