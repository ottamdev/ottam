CREATE TABLE payload_revisions (
    id INTEGER PRIMARY KEY NOT NULL,
    payload_id TEXT NOT NULL,
    revision BIGINT NOT NULL CHECK (revision >= 0),
    workspace_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    role TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('staging', 'ready', 'interrupted')),
    upstream_truncated INTEGER CHECK (upstream_truncated IN (0, 1)),
    locally_truncated INTEGER NOT NULL DEFAULT 0
        CHECK (locally_truncated IN (0, 1)),
    capture_reason TEXT,
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    UNIQUE (payload_id, revision)
);

CREATE INDEX payload_revisions_owner
    ON payload_revisions (workspace_id, conversation_id, id);
CREATE INDEX payload_revisions_recovery
    ON payload_revisions (state, updated_at_ms, id);

CREATE TABLE payload_parts (
    id INTEGER PRIMARY KEY NOT NULL,
    revision_id BIGINT NOT NULL
        REFERENCES payload_revisions(id) ON DELETE CASCADE,
    part_index INTEGER NOT NULL CHECK (part_index >= 0 AND part_index < 128),
    name TEXT NOT NULL,
    format TEXT NOT NULL,
    source_path TEXT,
    committed_raw_bytes BIGINT NOT NULL DEFAULT 0
        CHECK (committed_raw_bytes >= 0),
    committed_encoded_bytes BIGINT NOT NULL DEFAULT 0
        CHECK (committed_encoded_bytes >= 0),
    chunk_count BIGINT NOT NULL DEFAULT 0 CHECK (chunk_count >= 0),
    final_sha256 BLOB CHECK (final_sha256 IS NULL OR length(final_sha256) = 32),
    unavailable_reason TEXT,
    UNIQUE (revision_id, part_index)
);

CREATE TABLE payload_chunks (
    part_id BIGINT NOT NULL REFERENCES payload_parts(id) ON DELETE CASCADE,
    chunk_index BIGINT NOT NULL CHECK (chunk_index >= 0),
    raw_offset BIGINT NOT NULL CHECK (raw_offset >= 0),
    raw_length BIGINT NOT NULL CHECK (raw_length > 0 AND raw_length <= 65536),
    codec TEXT NOT NULL CHECK (codec IN ('raw-v1', 'zstd-v1')),
    raw_sha256 BLOB NOT NULL CHECK (length(raw_sha256) = 32),
    encoded_bytes BLOB NOT NULL CHECK (length(encoded_bytes) > 0),
    PRIMARY KEY (part_id, chunk_index),
    UNIQUE (part_id, raw_offset)
);

CREATE TABLE payload_usage (
    workspace_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    logical_bytes BIGINT NOT NULL DEFAULT 0,
    encoded_bytes BIGINT NOT NULL DEFAULT 0,
    overhead_bytes BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (workspace_id, conversation_id)
);
CREATE TRIGGER payload_usage_revision_insert AFTER INSERT ON payload_revisions BEGIN
    INSERT INTO payload_usage (workspace_id, conversation_id, overhead_bytes)
    VALUES (NEW.workspace_id, NEW.conversation_id, 512 + length(CAST(NEW.payload_id AS BLOB)) + length(CAST(NEW.workspace_id AS BLOB)) + length(CAST(NEW.conversation_id AS BLOB)) + length(CAST(NEW.item_id AS BLOB)) + length(CAST(NEW.run_id AS BLOB)) + length(CAST(NEW.role AS BLOB)))
    ON CONFLICT(workspace_id, conversation_id) DO UPDATE SET overhead_bytes = overhead_bytes + excluded.overhead_bytes;
END;
CREATE TRIGGER payload_usage_part_insert AFTER INSERT ON payload_parts BEGIN
    UPDATE payload_usage SET overhead_bytes = overhead_bytes + 256 + length(CAST(NEW.name AS BLOB)) + length(CAST(NEW.format AS BLOB)) + COALESCE(length(CAST(NEW.source_path AS BLOB)), 0)
    WHERE (workspace_id, conversation_id) = (SELECT workspace_id, conversation_id FROM payload_revisions WHERE id = NEW.revision_id);
END;
CREATE TRIGGER payload_usage_part_update AFTER UPDATE OF committed_raw_bytes, committed_encoded_bytes, chunk_count ON payload_parts BEGIN
    UPDATE payload_usage SET logical_bytes = logical_bytes + NEW.committed_raw_bytes - OLD.committed_raw_bytes,
        encoded_bytes = encoded_bytes + NEW.committed_encoded_bytes - OLD.committed_encoded_bytes,
        overhead_bytes = overhead_bytes + (NEW.chunk_count - OLD.chunk_count) * 256
    WHERE (workspace_id, conversation_id) = (SELECT workspace_id, conversation_id FROM payload_revisions WHERE id = NEW.revision_id);
END;
CREATE TRIGGER payload_usage_revision_delete BEFORE DELETE ON payload_revisions BEGIN
    UPDATE payload_usage SET logical_bytes = logical_bytes - COALESCE((SELECT SUM(committed_raw_bytes) FROM payload_parts WHERE revision_id = OLD.id), 0),
        encoded_bytes = encoded_bytes - COALESCE((SELECT SUM(committed_encoded_bytes) FROM payload_parts WHERE revision_id = OLD.id), 0),
        overhead_bytes = overhead_bytes - 512 - length(CAST(OLD.payload_id AS BLOB)) - length(CAST(OLD.workspace_id AS BLOB)) - length(CAST(OLD.conversation_id AS BLOB)) - length(CAST(OLD.item_id AS BLOB)) - length(CAST(OLD.run_id AS BLOB)) - length(CAST(OLD.role AS BLOB))
            - COALESCE((SELECT SUM(256 + length(CAST(name AS BLOB)) + length(CAST(format AS BLOB)) + COALESCE(length(CAST(source_path AS BLOB)), 0) + chunk_count * 256) FROM payload_parts WHERE revision_id = OLD.id), 0)
    WHERE workspace_id = OLD.workspace_id AND conversation_id = OLD.conversation_id;
    DELETE FROM payload_usage WHERE workspace_id = OLD.workspace_id AND conversation_id = OLD.conversation_id
        AND logical_bytes = 0 AND encoded_bytes = 0 AND overhead_bytes = 0;
END;

-- Body residency is independent of revision readiness and retained manifests.
CREATE TABLE payload_cache (
    revision_id BIGINT PRIMARY KEY NOT NULL REFERENCES payload_revisions(id) ON DELETE CASCADE,
    evicted INTEGER NOT NULL DEFAULT 0 CHECK (evicted IN (0, 1)),
    accessed_at_ms BIGINT NOT NULL,
    agent_id TEXT,
    session_id TEXT,
    tool_call_id TEXT,
    cwd TEXT
);
CREATE INDEX payload_cache_lru ON payload_cache(evicted, accessed_at_ms, revision_id);
CREATE TABLE payload_cache_settings (
    id INTEGER PRIMARY KEY NOT NULL CHECK (id = 1),
    limit_gb BIGINT NOT NULL CHECK (limit_gb > 0 AND limit_gb <= 9223372036)
);
INSERT INTO payload_cache_settings VALUES (1, 5);
