PRAGMA foreign_keys = ON;

CREATE TABLE workspaces (
    id TEXT PRIMARY KEY NOT NULL,
    display_name TEXT NOT NULL CHECK (length(display_name) > 0),
    created_at BIGINT NOT NULL CHECK (created_at >= 0),
    updated_at BIGINT NOT NULL CHECK (updated_at >= 0),
    last_opened_at BIGINT NOT NULL CHECK (last_opened_at >= 0),
    archived_at BIGINT CHECK (archived_at >= 0)
);

CREATE TABLE projects (
    id TEXT PRIMARY KEY NOT NULL,
    display_name TEXT NOT NULL CHECK (length(display_name) > 0),
    created_at BIGINT NOT NULL CHECK (created_at >= 0),
    updated_at BIGINT NOT NULL CHECK (updated_at >= 0),
    last_opened_at BIGINT NOT NULL CHECK (last_opened_at >= 0),
    archived_at BIGINT CHECK (archived_at >= 0)
);

CREATE TABLE project_sources (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('localGit')),
    display_name TEXT NOT NULL CHECK (length(display_name) > 0),
    canonical_path TEXT NOT NULL CHECK (length(canonical_path) > 0),
    is_primary INTEGER NOT NULL CHECK (is_primary IN (0, 1)),
    created_at BIGINT NOT NULL CHECK (created_at >= 0),
    updated_at BIGINT NOT NULL CHECK (updated_at >= 0),
    last_seen_at BIGINT NOT NULL CHECK (last_seen_at >= 0),
    archived_at BIGINT CHECK (archived_at >= 0),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE RESTRICT,
    UNIQUE (id, project_id)
);

CREATE UNIQUE INDEX project_sources_active_canonical_path
    ON project_sources(canonical_path)
    WHERE archived_at IS NULL AND kind = 'localGit';

CREATE UNIQUE INDEX project_sources_one_active_primary
    ON project_sources(project_id)
    WHERE archived_at IS NULL AND is_primary = 1;

CREATE TABLE workspace_projects (
    workspace_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    default_source_id TEXT NOT NULL,
    sort_order BIGINT NOT NULL CHECK (sort_order >= 0),
    added_at BIGINT NOT NULL CHECK (added_at >= 0),
    removed_at BIGINT CHECK (removed_at >= 0),
    PRIMARY KEY (workspace_id, project_id),
    FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE RESTRICT,
    FOREIGN KEY (default_source_id, project_id)
        REFERENCES project_sources(id, project_id) ON DELETE RESTRICT
);

CREATE UNIQUE INDEX workspace_projects_active_sort_order
    ON workspace_projects(workspace_id, sort_order)
    WHERE removed_at IS NULL;

CREATE TABLE conversations (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL,
    working_root_kind TEXT NOT NULL
        CHECK (working_root_kind IN ('workspaceFiles', 'project')),
    title TEXT NOT NULL,
    created_at BIGINT NOT NULL CHECK (created_at >= 0),
    updated_at BIGINT NOT NULL CHECK (updated_at >= 0),
    archived_at BIGINT CHECK (archived_at >= 0),
    -- Whether Ottam answers this conversation's tool permission requests
    -- itself (allow once) for agents without a bypass mode of their own.
    always_allow_tool_actions BOOLEAN NOT NULL DEFAULT 0,
    -- A delegated child's parent, and the named agent it was started as.
    -- Deleting a parent clears its children's link, so they become top-level
    -- conversations.
    parent_conversation_id TEXT,
    delegated_agent_name TEXT,
    -- A digest of the prompt the child was delegated with, so the parent's
    -- tool call can be matched to the child it started.
    delegation_key TEXT,
    delegated_task_title TEXT,
    -- The seed a delegated child's tile is drawn from, kept from when it
    -- started so a later shuffle leaves it as it was.
    delegated_avatar_seed BIGINT,
    -- The named agent a child was started as, so the child follows that
    -- agent's tile through a rename.
    delegated_named_agent_id TEXT,
    -- The parent's run whose `delegate` call started a child, so a client can
    -- show the child's card in that turn even when the agent's transcript does
    -- not name the call (OpenCode runs MCP tools inside its code-mode `execute`).
    delegated_from_run_id TEXT,
    FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE RESTRICT,
    UNIQUE (id, workspace_id)
);

CREATE INDEX conversations_parent ON conversations (parent_conversation_id)
    WHERE parent_conversation_id IS NOT NULL;

CREATE TABLE conversation_projects (
    conversation_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('primary', 'context')),
    created_at BIGINT NOT NULL CHECK (created_at >= 0),
    removed_at BIGINT CHECK (removed_at >= 0),
    PRIMARY KEY (conversation_id, project_id),
    FOREIGN KEY (conversation_id, workspace_id)
        REFERENCES conversations(id, workspace_id) ON DELETE CASCADE,
    FOREIGN KEY (workspace_id, project_id)
        REFERENCES workspace_projects(workspace_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (source_id, project_id)
        REFERENCES project_sources(id, project_id) ON DELETE RESTRICT
);

CREATE UNIQUE INDEX conversation_projects_one_active_primary
    ON conversation_projects(conversation_id)
    WHERE removed_at IS NULL AND role = 'primary';

CREATE TABLE agent_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    engine_id TEXT NOT NULL,
    external_session_id TEXT,
    source_scope_fingerprint TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('active', 'closed', 'lost')),
    created_at BIGINT NOT NULL CHECK (created_at >= 0),
    last_used_at BIGINT NOT NULL CHECK (last_used_at >= 0),
    ended_at BIGINT CHECK (ended_at >= 0),
    end_reason TEXT,
    -- The permission mode the agent last confirmed for this session: its
    -- acknowledgment at new/load/apply, or a mode change it announced itself
    -- (leaving plan mode). Restores and follow-ups reapply it; the Run's
    -- requested mode is only that Run's start-time choice. NULL until observed.
    current_mode_id TEXT,
    -- The slash commands the agent last announced for this session, as the JSON
    -- array of `ConversationSessionCommand` the app lists under `/`. Agents often
    -- announce them while the session is still opening, before any run records
    -- them; the run that claims the session writes them here. NULL until observed.
    available_commands TEXT,
    -- How full the session's context was when the agent last reported, as
    -- `ConversationContextUsage` JSON, so a chat's gauge survives a relaunch.
    -- NULL until observed.
    context_usage TEXT,
    -- Whether the session has been told what the conversation said before it
    -- existed. A session opened after an agent switch, or after its earlier
    -- session could not be restored, starts unseeded; its first wrapped prompt
    -- carries the earlier turns.
    history_seeded BOOLEAN NOT NULL DEFAULT 0,
    FOREIGN KEY (conversation_id, workspace_id)
        REFERENCES conversations(id, workspace_id) ON DELETE CASCADE,
    UNIQUE (id, conversation_id),
    UNIQUE (id, conversation_id, workspace_id, engine_id)
);

CREATE UNIQUE INDEX agent_sessions_external_identity
    ON agent_sessions(engine_id, external_session_id)
    WHERE external_session_id IS NOT NULL AND status = 'active';

CREATE UNIQUE INDEX agent_sessions_one_active_scope
    ON agent_sessions(conversation_id, engine_id, source_scope_fingerprint)
    WHERE status = 'active';

CREATE TABLE conversation_runs (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    engine_id TEXT NOT NULL,
    model_id TEXT,
    agent_session_id TEXT,
    session_origin TEXT CHECK (session_origin IN ('new', 'continued', 'restored')),
    status TEXT NOT NULL CHECK (
        status IN ('queued', 'running', 'completed', 'failed', 'canceled', 'interrupted')
    ),
    request_options_json TEXT CHECK (
        request_options_json IS NULL OR (
            json_valid(request_options_json) AND
            length(CAST(request_options_json AS BLOB)) <= 65536
        )
    ),
    event_sequence_cursor BIGINT NOT NULL DEFAULT 0 CHECK (event_sequence_cursor >= 0),
    queued_at BIGINT NOT NULL CHECK (queued_at >= 0),
    started_at BIGINT CHECK (started_at >= 0),
    completed_at BIGINT CHECK (completed_at >= 0),
    stop_reason TEXT,
    error_json TEXT CHECK (
        error_json IS NULL OR (
            json_valid(error_json) AND
            length(CAST(error_json AS BLOB)) <= 65536
        )
    ),
    -- Who started a run: the user, or the agent continuing on its own after a
    -- turn ended (an autonomous follow-up).
    origin TEXT NOT NULL DEFAULT 'user'
        CHECK (origin IN ('user', 'autonomous')),
    -- The tokens a run's turn used, as `ConversationRunUsage` JSON: the agent's
    -- prompt-answer usage with its scope and adapter. NULL until the answer
    -- reports it, so a finished run left NULL reads as "usage unavailable".
    usage_json TEXT,
    -- What a run's prompt replayed from earlier turns, as
    -- `ConversationHistoryReplay` JSON. NULL when it replayed nothing.
    history_replay_json TEXT,
    FOREIGN KEY (conversation_id, workspace_id)
        REFERENCES conversations(id, workspace_id) ON DELETE CASCADE,
    FOREIGN KEY (agent_session_id, conversation_id, workspace_id, engine_id)
        REFERENCES agent_sessions(id, conversation_id, workspace_id, engine_id)
        ON DELETE RESTRICT,
    UNIQUE (id, conversation_id),
    UNIQUE (id, workspace_id)
);

CREATE UNIQUE INDEX conversation_runs_one_active
    ON conversation_runs(conversation_id)
    WHERE status IN ('queued', 'running');

CREATE TABLE conversation_messages (
    id TEXT PRIMARY KEY NOT NULL,
    conversation_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('user', 'system')),
    body TEXT NOT NULL CHECK (length(CAST(body AS BLOB)) <= 524288),
    created_at BIGINT NOT NULL CHECK (created_at >= 0),
    -- Who wrote a message: the user, Ottam starting a delegated child
    -- (`delegation`), or Ottam delivering notes to a parent
    -- (`noteDelivery`, not shown). A note from a child is a `system` message
    -- with `note_json`; `note_delivered_at` is set once a prompt carried it.
    source TEXT NOT NULL DEFAULT 'user'
        CHECK (source IN ('user', 'delegation', 'noteDelivery')),
    note_json TEXT,
    note_delivered_at BIGINT,
    FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE,
    FOREIGN KEY (id, conversation_id) REFERENCES transcript_items(id, conversation_id) ON DELETE RESTRICT,
    UNIQUE (id, conversation_id)
);

CREATE INDEX conversation_messages_conversation ON conversation_messages(conversation_id);

CREATE TABLE conversation_run_inputs (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    requested_against_run_id TEXT,
    target_run_id TEXT,
    delivery_sequence BIGINT CHECK (delivery_sequence >= 0),
    kind TEXT NOT NULL CHECK (kind IN ('initial', 'steer', 'retry')),
    retry_of_input_id TEXT,
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) > 0),
    status TEXT NOT NULL CHECK (
        status IN ('pending', 'sending', 'delivered', 'deliveryUnknown', 'failed', 'superseded')
    ),
    delivery_mode TEXT CHECK (
        delivery_mode IN ('prompt', 'nativeSteer', 'interruptThenPrompt')
    ),
    delivery_evidence TEXT CHECK (
        delivery_evidence IN ('adapterConfirmed', 'userMarkedDelivered')
    ),
    requested_at BIGINT NOT NULL CHECK (requested_at >= 0),
    delivery_attempted_at BIGINT CHECK (delivery_attempted_at >= 0),
    delivered_at BIGINT CHECK (delivered_at >= 0),
    ambiguity_detected_at BIGINT CHECK (ambiguity_detected_at >= 0),
    failed_at BIGINT CHECK (failed_at >= 0),
    failure_reason TEXT,
    resolved_at BIGINT CHECK (resolved_at >= 0),
    resolution TEXT CHECK (resolution IN ('markedDelivered', 'retryCreated')),
    FOREIGN KEY (conversation_id, workspace_id)
        REFERENCES conversations(id, workspace_id) ON DELETE CASCADE,
    FOREIGN KEY (message_id, conversation_id)
        REFERENCES conversation_messages(id, conversation_id) ON DELETE RESTRICT,
    FOREIGN KEY (requested_against_run_id, conversation_id)
        REFERENCES conversation_runs(id, conversation_id) ON DELETE RESTRICT,
    FOREIGN KEY (target_run_id, conversation_id)
        REFERENCES conversation_runs(id, conversation_id) ON DELETE RESTRICT,
    FOREIGN KEY (retry_of_input_id, conversation_id)
        REFERENCES conversation_run_inputs(id, conversation_id) ON DELETE RESTRICT,
    UNIQUE (id, conversation_id),
    UNIQUE (conversation_id, idempotency_key)
);

CREATE UNIQUE INDEX conversation_run_inputs_target_sequence
    ON conversation_run_inputs(target_run_id, delivery_sequence)
    WHERE target_run_id IS NOT NULL AND delivery_sequence IS NOT NULL;

CREATE TABLE conversation_run_sources (
    run_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    root_kind TEXT NOT NULL CHECK (root_kind IN ('workspaceFiles', 'project')),
    project_id TEXT,
    source_id TEXT,
    canonical_path TEXT NOT NULL CHECK (length(canonical_path) > 0),
    role TEXT NOT NULL CHECK (role IN ('primary', 'context')),
    created_at BIGINT NOT NULL CHECK (created_at >= 0),
    PRIMARY KEY (run_id, canonical_path),
    CHECK (
        (root_kind = 'workspaceFiles' AND project_id IS NULL AND source_id IS NULL) OR
        (root_kind = 'project' AND project_id IS NOT NULL AND source_id IS NOT NULL)
    ),
    FOREIGN KEY (run_id, conversation_id)
        REFERENCES conversation_runs(id, conversation_id) ON DELETE CASCADE,
    FOREIGN KEY (run_id, workspace_id)
        REFERENCES conversation_runs(id, workspace_id) ON DELETE CASCADE,
    FOREIGN KEY (conversation_id, project_id)
        REFERENCES conversation_projects(conversation_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (source_id, project_id)
        REFERENCES project_sources(id, project_id) ON DELETE RESTRICT
);

CREATE UNIQUE INDEX conversation_run_sources_one_primary
    ON conversation_run_sources(run_id)
    WHERE role = 'primary';

CREATE TABLE run_events (
    id TEXT PRIMARY KEY NOT NULL,
    run_id TEXT NOT NULL,
    sequence BIGINT NOT NULL CHECK (sequence >= 0),
    kind TEXT NOT NULL CHECK (
        kind IN (
            'runStatus', 'permissionRequested',
            'permissionResolved', 'steerRequested', 'steerDelivered',
            'runInterrupted', 'usage', 'sessionMetadata'
        )
    ),
    payload_json TEXT NOT NULL CHECK (
        json_valid(payload_json) AND
        length(CAST(payload_json AS BLOB)) <= 65536
    ),
    created_at BIGINT NOT NULL CHECK (created_at >= 0),
    FOREIGN KEY (run_id) REFERENCES conversation_runs(id) ON DELETE CASCADE,
    UNIQUE (run_id, sequence)
);

CREATE INDEX conversations_workspace_recent
    ON conversations (workspace_id, archived_at, updated_at, id);

CREATE INDEX conversation_runs_conversation_pagination
    ON conversation_runs (conversation_id, id);

CREATE INDEX agent_sessions_conversation_pagination
    ON agent_sessions (conversation_id, id);

CREATE INDEX conversation_run_inputs_target_delivery
    ON conversation_run_inputs (target_run_id, delivery_sequence);

CREATE TABLE conversation_attachments (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL,
    conversation_id TEXT,
    message_id TEXT,
    ordinal BIGINT CHECK (ordinal >= 0),
    kind TEXT NOT NULL CHECK (kind IN ('image')),
    mime_type TEXT NOT NULL
        CHECK (mime_type IN ('image/png', 'image/jpeg', 'image/gif', 'image/webp')),
    byte_count BIGINT NOT NULL CHECK (byte_count > 0 AND byte_count <= 3145728),
    width BIGINT NOT NULL CHECK (width > 0 AND width <= 20000),
    height BIGINT NOT NULL CHECK (height > 0 AND height <= 20000),
    relative_path TEXT NOT NULL CHECK (length(relative_path) > 0),
    source_kind TEXT NOT NULL CHECK (source_kind IN ('pasted', 'dropped', 'picked')),
    origin_path TEXT CHECK (origin_path IS NULL OR length(origin_path) > 0),
    created_at BIGINT NOT NULL CHECK (created_at >= 0),
    claimed_at BIGINT CHECK (claimed_at >= 0), idempotency_key TEXT,
    -- Claim state is all-or-none: either every claim column is set or none is.
    CHECK (
        (message_id IS NULL AND conversation_id IS NULL
            AND ordinal IS NULL AND claimed_at IS NULL) OR
        (message_id IS NOT NULL AND conversation_id IS NOT NULL
            AND ordinal IS NOT NULL AND claimed_at IS NOT NULL)
    ),
    -- A pasted image has no file on the user's disk to point back at; one the
    -- user picked or dropped always does.
    CHECK (source_kind <> 'pasted' OR origin_path IS NULL),
    CHECK (source_kind = 'pasted' OR origin_path IS NOT NULL),
    FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
    FOREIGN KEY (message_id, conversation_id)
        REFERENCES conversation_messages(id, conversation_id) ON DELETE RESTRICT
);

CREATE UNIQUE INDEX conversation_attachments_message_ordinal
    ON conversation_attachments (message_id, ordinal)
    WHERE message_id IS NOT NULL;

CREATE INDEX conversation_attachments_unclaimed_sweep
    ON conversation_attachments (workspace_id, created_at)
    WHERE message_id IS NULL;

CREATE INDEX conversation_attachments_message_lookup
    ON conversation_attachments (conversation_id, message_id, ordinal);

CREATE UNIQUE INDEX conversation_attachments_workspace_idempotency
    ON conversation_attachments (workspace_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

CREATE INDEX conversation_attachments_workspace_path
    ON conversation_attachments (workspace_id, relative_path);

CREATE TABLE transcript_items (
    id TEXT PRIMARY KEY NOT NULL,
    conversation_id TEXT NOT NULL,
    run_id TEXT,
    position BIGINT NOT NULL CHECK (position >= 0),
    revision BIGINT NOT NULL CHECK (revision > 0),
    kind TEXT NOT NULL CHECK (kind IN ('message', 'assistantText', 'reasoning', 'image', 'tool', 'plan', 'permission', 'gap', 'runBoundary')),
    completion TEXT NOT NULL CHECK (completion IN ('open', 'complete', 'partial', 'suppressed')),
    provider_message_id TEXT,
    adapter_metadata_json TEXT NOT NULL DEFAULT '{}',
    body_byte_count BIGINT NOT NULL DEFAULT 0 CHECK (body_byte_count >= 0),
    created_at BIGINT NOT NULL CHECK (created_at >= 0),
    updated_at BIGINT NOT NULL CHECK (updated_at >= 0),
    FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE,
    FOREIGN KEY (run_id, conversation_id) REFERENCES conversation_runs(id, conversation_id) ON DELETE RESTRICT,
    UNIQUE (conversation_id, position),
    UNIQUE (id, conversation_id)
);

CREATE INDEX transcript_items_run ON transcript_items(run_id, position);

CREATE TABLE transcript_body_chunks (
    item_id TEXT NOT NULL REFERENCES transcript_items(id) ON DELETE CASCADE,
    byte_offset BIGINT NOT NULL CHECK (byte_offset >= 0),
    body TEXT NOT NULL CHECK (length(CAST(body AS BLOB)) <= 65536),
    PRIMARY KEY (item_id, byte_offset)
);

CREATE TABLE transcript_tools (
    item_id TEXT PRIMARY KEY NOT NULL REFERENCES transcript_items(id) ON DELETE CASCADE,
    provider_tool_call_id TEXT,
    -- A serialized typed tool record: ordinary arguments and payload descriptors only.
    metadata_json TEXT NOT NULL CHECK (json_valid(metadata_json))
);

CREATE TABLE transcript_plans (
    item_id TEXT PRIMARY KEY NOT NULL REFERENCES transcript_items(id) ON DELETE CASCADE,
    entries_json TEXT NOT NULL CHECK (json_valid(entries_json))
);

CREATE TABLE transcript_permissions (
    item_id TEXT PRIMARY KEY NOT NULL REFERENCES transcript_items(id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    decision_json TEXT NOT NULL CHECK (json_valid(decision_json))
);

CREATE TABLE transcript_gaps (
    item_id TEXT PRIMARY KEY NOT NULL REFERENCES transcript_items(id) ON DELETE CASCADE,
    end_position BIGINT NOT NULL CHECK (end_position >= 0),
    dropped_updates BIGINT NOT NULL CHECK (dropped_updates >= 0),
    reason TEXT NOT NULL
);

CREATE TABLE transcript_cursors (
    conversation_id TEXT PRIMARY KEY NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    sequence BIGINT NOT NULL DEFAULT 0 CHECK (sequence >= 0)
);

CREATE TABLE transcript_payloads (
    payload_id TEXT NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    item_id TEXT NOT NULL REFERENCES transcript_items(id) ON DELETE CASCADE,
    descriptor TEXT NOT NULL CHECK (json_valid(descriptor)),
    PRIMARY KEY (payload_id, revision)
);
CREATE INDEX transcript_payloads_item ON transcript_payloads(item_id);

CREATE TABLE transcript_checkpoint_bodies (
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    byte_offset BIGINT NOT NULL CHECK (byte_offset >= 0),
    body TEXT NOT NULL,
    PRIMARY KEY (item_id, revision, byte_offset)
);
CREATE TABLE transcript_checkpoint_heads (
    item_id TEXT PRIMARY KEY NOT NULL REFERENCES transcript_items(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL CHECK (revision > 0)
);
CREATE TABLE transcript_capture (
    run_id TEXT PRIMARY KEY NOT NULL REFERENCES conversation_runs(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('open', 'complete', 'partial')),
    gaps_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(gaps_json))
);
CREATE TRIGGER transcript_capture_new_run AFTER INSERT ON conversation_runs BEGIN
    INSERT INTO transcript_capture (run_id, state) VALUES (NEW.id, 'open');
END;

CREATE TABLE agent_instances (
    id TEXT PRIMARY KEY NOT NULL,
    harness_id TEXT NOT NULL,
    label TEXT NOT NULL,
    home_json TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    deleted_at BIGINT,
    claimed_at BIGINT
);

-- The roster starts with one Ottam profile per harness, all unclaimed.
INSERT INTO agent_instances (id, harness_id, label, home_json, created_at) VALUES
    ('claude', 'claude', 'Claude Code', '{"kind":"managed"}', 0),
    ('codex', 'codex', 'Codex', '{"kind":"managed"}', 0),
    ('opencode', 'opencode', 'OpenCode', '{"kind":"managed"}', 0),
    ('omp', 'omp', 'Oh-My-Pi', '{"kind":"managed"}', 0);

CREATE TRIGGER claim_run_agent_instance AFTER INSERT ON conversation_runs BEGIN
    UPDATE agent_instances SET claimed_at = COALESCE(claimed_at, NEW.queued_at)
    WHERE id = NEW.engine_id;
END;

CREATE TRIGGER claim_session_agent_instance AFTER INSERT ON agent_sessions BEGIN
    UPDATE agent_instances SET claimed_at = COALESCE(claimed_at, NEW.created_at)
    WHERE id = NEW.engine_id;
END;

-- Serialize late run/session writes against instance deletion. Historical rows
-- remain untouched; only tombstoned identities are barred from new work.
CREATE TRIGGER prevent_removed_agent_run BEFORE INSERT ON conversation_runs
WHEN EXISTS (SELECT 1 FROM agent_instances WHERE id=NEW.engine_id AND deleted_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'This agent was removed'); END;

CREATE TRIGGER prevent_removed_agent_session BEFORE INSERT ON agent_sessions
WHEN EXISTS (SELECT 1 FROM agent_instances WHERE id=NEW.engine_id AND deleted_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'This agent was removed'); END;

-- Core-owned settings that belong to no workspace. Holds the last-set
-- always-allow value, which new conversations start from.
CREATE TABLE core_settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

-- MCP servers core proxies to every agent session. `transport_json` is the
-- protocol's McpServerTransport with secret values left out: those live in
-- the Keychain, keyed by server id. A remote server's OAuth sign-in is a
-- separate Keychain item, `<server id>/oauth`.
CREATE TABLE mcp_servers (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE,
    transport_json TEXT NOT NULL,
    created_at BIGINT NOT NULL CHECK (created_at >= 0),
    updated_at BIGINT NOT NULL CHECK (updated_at >= 0)
);

-- Servers a workspace turned off for its new sessions. Every other server is
-- on for every workspace.
CREATE TABLE workspace_mcp_disabled (
    workspace_id TEXT NOT NULL,
    server_id TEXT NOT NULL,
    disabled_at BIGINT NOT NULL CHECK (disabled_at >= 0),
    PRIMARY KEY (workspace_id, server_id),
    FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
    FOREIGN KEY (server_id) REFERENCES mcp_servers(id) ON DELETE CASCADE
);

-- Named agents a conversation can delegate to: an agent instance (engine)
-- with the model, mode and instructions its children start with.
CREATE TABLE named_agents (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
    engine_id TEXT NOT NULL,
    model_id TEXT,
    mode_id TEXT,
    instructions TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL CHECK (created_at >= 0),
    updated_at BIGINT NOT NULL CHECK (updated_at >= 0),
    -- The seed an agent's tile is drawn from. One without a seed draws from
    -- its name.
    avatar_seed BIGINT
);
