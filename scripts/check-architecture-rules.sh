#!/usr/bin/env bash
# Structural rules no test exercises. Uses grep only; Linux CI has no ripgrep.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

failures=0
CORE=backend/crates/ottam-core

# grep exits 2 on a missing path even when another path matched, so a renamed
# path fails the check instead of silently disabling it.
reject() {
  local label="$1" pattern="$2" matches status=0
  shift 2
  matches="$(grep -rnE "$pattern" "$@")" || status=$?
  case "$status" in
    0) printf 'Architecture rule: found %s:\n%s\n' "$label" "$matches" >&2; failures=1 ;;
    1) ;;
    *) echo "Architecture rule: cannot check $label (missing path?)" >&2; failures=1 ;;
  esac
}

require() {
  local label="$1" pattern="$2" status=0
  shift 2
  grep -rqE "$pattern" "$@" || status=$?
  case "$status" in
    0) ;;
    1) echo "Architecture rule: missing $label" >&2; failures=1 ;;
    *) echo "Architecture rule: cannot check $label (missing path?)" >&2; failures=1 ;;
  esac
}

# Diesel owns SQLite access.
reject "a raw rusqlite dependency" 'rusqlite' \
  backend/Cargo.toml backend/Cargo.lock backend/crates/*/Cargo.toml
# Payloads live in SQLite only; no spill files or file fallback.
reject "filesystem I/O in payload storage" \
  'std::fs|tokio::fs|fs::|File::|OpenOptions' \
  "$CORE/src/server/chat/payloads.rs" "$CORE/src/server/chat/payloads"
# Payload writes share the database's one writer.
require "the shared payload writer" 'database\.payload_worker\(\)' \
  "$CORE/src/server/chat/payloads/live_service.rs"
# Test agents are Rust binaries, not embedded scripts, and take their behavior
# from configuration, not from prompt text.
reject "an embedded Python test runtime" 'python3|import json|#!/usr/bin/env python' \
  "$CORE/tests/support" backend/crates/ottam-test-acp
reject "prompt-text-controlled test agent behavior" \
  'crash-after-sending|never-ack|hold this initial ACP turn' "$CORE/tests"
# Interrupted runs are recovered by the repository at startup, not by
# bootstrap SQL.
require "repository-owned startup recovery" 'RunRepository::recover_on_startup' \
  "$CORE/src/server/mod.rs"
reject "bootstrap-owned run recovery SQL" 'UPDATE[[:space:]]+conversation_runs' \
  "$CORE/src/storage/database.rs"
# One Core process serves every window; only the app runtime builds its client.
reject "Core client construction outside OttamAppRuntime" \
  'OttamCoreProcessClient[[:space:]]*\(' app/Sources/OttamApp \
  --exclude=OttamAppRuntime.swift
# Every Workspace root is addressed.
reject "an unaddressed Workspace root" 'WorkspaceRootView[[:space:]]*\([[:space:]]*\)' \
  app/Sources/OttamApp
# Split layout is per window (AppKit autosave), never global app storage.
reject "global Workspace split restoration" \
  '@AppStorage\([^)]*(workspace|chat\.(rail|browser))' app/Sources/OttamApp
# Empty Workspaces still register the agent permission handler.
WORKSPACE_MODEL=app/Sources/OttamApp/Workspace/ChatWorkspaceModel.swift
require "Chat permission registration" 'registerAgentPermissionHandler' "$WORKSPACE_MODEL"
if sed -n '/func installBackendHandlers(/,/^    }$/p' "$WORKSPACE_MODEL" |
  grep -q 'guard let projectID'; then
  echo "Architecture rule: installBackendHandlers skips empty Workspaces" >&2
  failures=1
fi

if [ "$failures" -ne 0 ]; then
  exit 1
fi
echo "Architecture rules passed"
