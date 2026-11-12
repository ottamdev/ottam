#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CORE_DIR="$ROOT_DIR/backend/crates/ottam-core"
TEMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ottam-diesel-schema.XXXXXX")"
trap 'rm -rf "$TEMP_DIR"' EXIT

SQLITE3="$(command -v sqlite3 || true)"
if [[ -z "$SQLITE3" ]]; then
  echo "Diesel schema gate requires sqlite3" >&2
  exit 1
fi

export DATABASE_URL="$TEMP_DIR/ottam.sqlite"
"$ROOT_DIR/scripts/diesel-cli.sh" migration run \
  --migration-dir "$CORE_DIR/migrations" \
  --database-url "$DATABASE_URL" >/dev/null
"$ROOT_DIR/scripts/diesel-cli.sh" print-schema \
  --database-url "$DATABASE_URL" > "$TEMP_DIR/schema.rs"

if ! diff -u "$CORE_DIR/src/storage/schema.rs" "$TEMP_DIR/schema.rs"; then
  echo "Checked-in Diesel schema is stale. Regenerate with:" >&2
  echo "  DATABASE_URL=<migrated-temp-db> ./scripts/diesel-cli.sh print-schema > $CORE_DIR/src/storage/schema.rs" >&2
  exit 1
fi

expected_tables=$'__diesel_schema_migrations\nagent_sessions\nconversation_attachments\nconversation_messages\nconversation_projects\nconversation_run_inputs\nconversation_run_sources\nconversation_runs\nconversations\ncore_settings\nmcp_servers\nnamed_agents\nproject_sources\nprojects\nrun_events\ntranscript_body_chunks\ntranscript_capture\ntranscript_checkpoint_bodies\ntranscript_checkpoint_heads\ntranscript_cursors\ntranscript_gaps\ntranscript_items\ntranscript_payloads\ntranscript_permissions\ntranscript_plans\ntranscript_tools\nworkspace_mcp_disabled\nworkspace_projects\nworkspaces'
expected_tables="${expected_tables/__diesel_schema_migrations/__diesel_schema_migrations$'\n'agent_instances}"
actual_tables="$("$SQLITE3" "$DATABASE_URL" \
  "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name;")"
if [[ "$actual_tables" != "$expected_tables" ]]; then
  echo "Diesel schema gate found an unexpected application table set:" >&2
  diff -u <(printf '%s\n' "$expected_tables") <(printf '%s\n' "$actual_tables") >&2 || true
  exit 1
fi

integrity_result="$("$SQLITE3" "$DATABASE_URL" 'PRAGMA integrity_check;')"
if [[ "$integrity_result" != "ok" ]]; then
  echo "Diesel schema gate integrity check failed: $integrity_result" >&2
  exit 1
fi

foreign_key_failures="$("$SQLITE3" "$DATABASE_URL" 'PRAGMA foreign_key_check;')"
if [[ -n "$foreign_key_failures" ]]; then
  echo "Diesel schema gate found foreign-key failures:" >&2
  echo "$foreign_key_failures" >&2
  exit 1
fi

payload_url="$TEMP_DIR/payloads.sqlite"
"$ROOT_DIR/scripts/diesel-cli.sh" migration run \
  --migration-dir "$CORE_DIR/src/storage/payload/migrations" \
  --database-url "$payload_url" >/dev/null
"$ROOT_DIR/scripts/diesel-cli.sh" print-schema \
  --sqlite-integer-primary-key-is-bigint \
  --database-url "$payload_url" > "$TEMP_DIR/payload-schema.rs"
if ! diff -u "$CORE_DIR/src/storage/payload/schema.rs" "$TEMP_DIR/payload-schema.rs"; then
  echo "Payload Diesel schema is stale. Regenerate from src/storage/payload/migrations with print-schema --sqlite-integer-primary-key-is-bigint." >&2
  exit 1
fi
payload_expected=$'__diesel_schema_migrations\npayload_cache\npayload_cache_settings\npayload_chunks\npayload_parts\npayload_revisions\npayload_usage'
payload_actual="$("$SQLITE3" "$payload_url" \
  "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name;")"
if [[ "$payload_actual" != "$payload_expected" ]]; then
  echo "Payload database has unexpected application tables" >&2
  exit 1
fi
if [[ "$("$SQLITE3" "$payload_url" 'PRAGMA integrity_check;')" != "ok" ]] \
  || [[ -n "$("$SQLITE3" "$payload_url" 'PRAGMA foreign_key_check;')" ]]; then
  echo "Payload database integrity/foreign-key check failed" >&2
  exit 1
fi
"$ROOT_DIR/scripts/diesel-cli.sh" migration redo \
  --migration-dir "$CORE_DIR/src/storage/payload/migrations" --database-url "$payload_url" >/dev/null
"$ROOT_DIR/scripts/diesel-cli.sh" print-schema \
  --sqlite-integer-primary-key-is-bigint \
  --database-url "$payload_url" > "$TEMP_DIR/payload-schema-redo.rs"
diff -u "$CORE_DIR/src/storage/payload/schema.rs" "$TEMP_DIR/payload-schema-redo.rs"

echo "Main and payload Diesel schema gates passed"
