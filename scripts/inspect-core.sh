#!/usr/bin/env bash
# Blessed entry point for querying a ottam-core headlessly.
#
#   ./scripts/inspect-core.sh status
#   ./scripts/inspect-core.sh call agent.list '{}'
#   ./scripts/inspect-core.sh conversation <conversation-id> [method] [params]
#   ./scripts/inspect-core.sh listen --methods conversation.,agent.
#   ./scripts/inspect-core.sh call workspace.list --spawn
#
# Builds ottam-inspect quietly, resolves the home to inspect
# (--home > $OTTAM_HOME > this worktree's running instance via
# .build/run-app/*/owner.json > ~/.ottam), and execs the binary.
# See .claude/skills/core-inspect/SKILL.md.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BACKEND_DIR="$ROOT_DIR/backend"

# The binary resolves --home / $OTTAM_HOME itself; the wrapper only adds
# the worktree autodetect layer, and only for attach-mode invocations.
WANTS_HOME=1
for arg in "$@"; do
  case "$arg" in
    --home|--spawn) WANTS_HOME=0 ;;
  esac
done

resolve_worktree_home() {
  # A run-app.sh-launched instance records its state directory in owner.json.
  # Prefer an instance whose process is still alive.
  local owner_file state_dir pid
  for owner_file in "$ROOT_DIR"/.build/run-app/*/owner.json; do
    [ -f "$owner_file" ] || continue
    pid="$(/usr/bin/plutil -extract pid raw -o - "$owner_file" 2>/dev/null || true)"
    state_dir="$(/usr/bin/plutil -extract stateDirectory raw -o - "$owner_file" 2>/dev/null || true)"
    [ -n "$state_dir" ] || continue
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      printf '%s\n' "$state_dir"
      return 0
    fi
  done
  return 1
}

if [ "$WANTS_HOME" = "1" ] && [ -z "${OTTAM_HOME:-}" ]; then
  if WORKTREE_HOME="$(resolve_worktree_home)"; then
    export OTTAM_HOME="$WORKTREE_HOME"
  fi
fi

cargo build --quiet --manifest-path "$BACKEND_DIR/Cargo.toml" \
  -p ottam-inspect -p ottam-core

exec "$BACKEND_DIR/target/debug/ottam-inspect" "$@"
