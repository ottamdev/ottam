# Per-worktree evidence lifecycle helpers for scripts/ui-test.sh.

# A run that fails part way through leaves whatever it had already written in
# `.build/ui-tests/current`, and a run that fails before it even resets that
# directory leaves the previous run's screenshots and audits untouched. Mark
# the directory so partial or superseded evidence cannot be mistaken for the
# result of a passing run.
EVIDENCE_STALE_MARKER="STALE.txt"

mark_evidence_stale() {
  local reason="$1"
  [ -d "$CURRENT_EVIDENCE" ] && [ ! -L "$CURRENT_EVIDENCE" ] || return 0
  # The innermost failure is the specific one, and a wrapper script's exit trap
  # fires after it, so the first reason recorded is the one worth keeping.
  [ ! -f "$CURRENT_EVIDENCE/$EVIDENCE_STALE_MARKER" ] || return 0
  {
    printf 'This evidence does not describe a completed, passing UI test run.\n'
    printf 'Reason: %s\n' "$reason"
    printf 'Marked at: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'Screenshots and accessibility audits here may be partial or left\n'
    printf 'over from an earlier run. Re-run the command before trusting them.\n'
  } > "$CURRENT_EVIDENCE/$EVIDENCE_STALE_MARKER" 2>/dev/null || true
}
# shellcheck source=scripts/lib/process.sh
source "$(dirname "${BASH_SOURCE[0]}")/process.sh"
release_run_lock() {
  local owner_pid owner_start owner_token current_start
  [ -n "$RUN_LOCK_TOKEN" ] || return 0
  [ -d "$RUN_LOCK" ] || return 0
  owner_pid="$(cat "$RUN_LOCK/pid" 2>/dev/null || true)"
  owner_start="$(cat "$RUN_LOCK/start" 2>/dev/null || true)"
  owner_token="$(cat "$RUN_LOCK/token" 2>/dev/null || true)"
  current_start="$(process_start_identity "$$")"
  if [ "$owner_pid" = "$$" ] &&
     [ "$owner_start" = "$current_start" ] &&
     [ "$owner_token" = "$RUN_LOCK_TOKEN" ]; then
    rm -f "$RUN_LOCK/pid" "$RUN_LOCK/start" "$RUN_LOCK/token"
    rmdir "$RUN_LOCK" 2>/dev/null || true
  fi
  RUN_LOCK_TOKEN=""
}

finalize_run() {
  local status="$?"
  if [ "$status" -ne 0 ] && [ -n "${EVIDENCE_FAILURE_GUARD:-}" ]; then
    mark_evidence_stale "scripts/ui-test.sh exited with status $status"
  fi
  release_run_lock
  return "$status"
}

acquire_run_lock() {
  local attempt owner_pid owner_start live_start
  mkdir -p "$UI_TEST_ROOT"
  RUN_LOCK_TOKEN="$(uuidgen | tr '[:upper:]' '[:lower:]')"
  for attempt in 1 2 3; do
    if mkdir "$RUN_LOCK" 2>/dev/null; then
      printf '%s\n' "$$" > "$RUN_LOCK/pid"
      process_start_identity "$$" > "$RUN_LOCK/start"
      printf '%s\n' "$RUN_LOCK_TOKEN" > "$RUN_LOCK/token"
      trap finalize_run EXIT
      trap 'exit 130' INT
      trap 'exit 143' TERM
      return 0
    fi

    # Give a process that just created the directory time to publish ownership.
    if [ ! -s "$RUN_LOCK/pid" ] || [ ! -s "$RUN_LOCK/start" ]; then
      sleep 0.1
    fi
    owner_pid="$(cat "$RUN_LOCK/pid" 2>/dev/null || true)"
    owner_start="$(cat "$RUN_LOCK/start" 2>/dev/null || true)"
    live_start=""
    if [[ "$owner_pid" =~ ^[0-9]+$ ]]; then
      live_start="$(process_start_identity "$owner_pid")"
    fi
    if [ -n "$live_start" ] && [ "$live_start" = "$owner_start" ]; then
      echo "A UI test command is already running in this worktree (PID $owner_pid)." >&2
      echo "Wait for it to finish before replacing .build/ui-tests/current." >&2
      exit 75
    fi

    rm -f "$RUN_LOCK/pid" "$RUN_LOCK/start" "$RUN_LOCK/token"
    rmdir "$RUN_LOCK" 2>/dev/null || true
  done
  echo "Could not acquire the per-worktree UI test lock: $RUN_LOCK" >&2
  exit 75
}

prepare_current_evidence() {
  local resolved_root resolved_parent
  mkdir -p "$UI_TEST_ROOT"
  resolved_root="$(cd "$UI_TEST_ROOT" && pwd -P)"
  resolved_parent="$(cd "$(dirname "$CURRENT_EVIDENCE")" && pwd -P)"
  [ "$resolved_parent" = "$resolved_root" ] &&
    [ "$(basename "$CURRENT_EVIDENCE")" = "current" ] ||
    { echo "Refusing unsafe evidence reset: $CURRENT_EVIDENCE" >&2; exit 70; }
  [ ! -L "$CURRENT_EVIDENCE" ] ||
    { echo "Refusing to replace symlinked evidence: $CURRENT_EVIDENCE" >&2; exit 70; }
  if [ -e "$CURRENT_EVIDENCE" ]; then
    rm -rf -- "$CURRENT_EVIDENCE"
  fi
  mkdir -m 700 "$CURRENT_EVIDENCE"
}

archive_current_evidence() {
  local label="$1"
  local destination temporary_destination archive_timestamp
  [[ "$label" =~ ^[a-z0-9]+(-[a-z0-9]+)*$ ]] ||
    { echo "Archive names must be descriptive kebab-case (for example: claude-login)." >&2; exit 64; }
  [ -d "$CURRENT_EVIDENCE" ] && [ ! -L "$CURRENT_EVIDENCE" ] &&
    [ -n "$(find "$CURRENT_EVIDENCE" -mindepth 1 -print -quit)" ] ||
    { echo "There is no current UI test evidence to archive." >&2; exit 66; }

  mkdir -p "$ARCHIVE_ROOT"
  destination="$ARCHIVE_ROOT/$label"
  [ ! -e "$destination" ] && [ ! -L "$destination" ] ||
    { echo "Archive already exists: $destination" >&2; exit 73; }
  temporary_destination="$ARCHIVE_ROOT/.${label}.tmp-${RUN_LOCK_TOKEN}"
  [ ! -e "$temporary_destination" ] ||
    { echo "Temporary archive path already exists: $temporary_destination" >&2; exit 73; }
  mkdir -m 700 "$temporary_destination"
  if ! rsync -a \
      --exclude='home/' \
      --exclude='tmp/' \
      --exclude='control/' \
      --exclude='staged-app/' \
      --exclude='DerivedData/' \
      "$CURRENT_EVIDENCE/" "$temporary_destination/"; then
    rm -rf -- "$temporary_destination"
    exit 74
  fi
  archive_timestamp="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  jq -n \
    --arg name "$label" \
    --arg archived_at "$archive_timestamp" \
    '{
      schemaVersion: 1,
      name: $name,
      archivedAt: $archived_at,
      source: ".build/ui-tests/current",
      excludedRebuildableInternals: [
        "home",
        "tmp",
        "control",
        "staged-app",
        "DerivedData"
      ]
    }' > "$temporary_destination/archive.json"
  mv "$temporary_destination" "$destination"
  echo "Archived current UI test evidence: $destination"
}

set_public_command() {
  PUBLIC_COMMAND_JSON="$(
    {
      printf '%s\n' "$ROOT_DIR/scripts/ui-test.sh"
      printf '%s\n' "${ORIGINAL_ARGUMENTS[@]}"
    } | jq -R -s 'split("\n") | map(select(length > 0))'
  )"
  export OTTAM_UI_TEST_PUBLIC_COMMAND_JSON="$PUBLIC_COMMAND_JSON"
}

initialize_launcher_evidence() {
  LAUNCHER_RUN_ID="r$(uuidgen | tr -d '-' | tr '[:upper:]' '[:lower:]')"
  LAUNCHER_STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  mkdir -p "$CURRENT_EVIDENCE/logs" "$CURRENT_EVIDENCE/diagnostics"
}

write_launcher_failure() {
  local scenario_id="$1"
  local classification="$2"
  local phase="$3"
  local exit_status="$4"
  local log_path="$5"
  local ended_at message worktree_hash worktree_id source_commit dirty_state
  ended_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  message="launcher stage $phase exited with status $exit_status; see ${log_path#"$CURRENT_EVIDENCE"/}"
  worktree_hash="$(printf '%s' "$ROOT_DIR" | shasum -a 256 | awk '{print $1}')"
  worktree_id="w${worktree_hash:0:12}"
  source_commit="$(git -C "$ROOT_DIR" rev-parse HEAD 2>/dev/null || printf unavailable)"
  dirty_state="$(
    if git -C "$ROOT_DIR" diff --quiet &&
      git -C "$ROOT_DIR" diff --cached --quiet &&
      [ -z "$(git -C "$ROOT_DIR" ls-files --others --exclude-standard)" ]; then
      printf false
    else
      printf true
    fi
  )"

  jq -n \
    --arg scenario "$scenario_id" \
    '{
      schemaVersion: 1,
      id: $scenario,
      description: "UI validation launcher preparation",
      implementation: "scripts/ui-test.sh",
      tags: ["launcher"],
      requirements: []
    }' > "$CURRENT_EVIDENCE/scenario.json"
  jq -nc \
    --arg classification "$classification" \
    --arg phase "$phase" \
    --arg message "$message" \
    '{
      timestamp: (now | todateiso8601),
      event: "launcher_failed",
      classification: $classification,
      phase: $phase,
      message: $message
    }' > "$CURRENT_EVIDENCE/logs/actions.jsonl"
  jq -n \
    --arg classification "$classification" \
    --arg phase "$phase" \
    --arg message "$message" \
    --arg started_at "$LAUNCHER_STARTED_AT" \
    --arg ended_at "$ended_at" \
    --argjson exit_status "$exit_status" \
    '{
      schemaVersion: 1,
      classification: $classification,
      message: $message,
      primaryFailure: {
        classification: $classification,
        phase: $phase,
        message: $message
      },
      secondaryFailures: [],
      startedAt: $started_at,
      endedAt: $ended_at,
      cleanupVerified: true,
      launcherExitStatus: $exit_status
    }' > "$CURRENT_EVIDENCE/result.json"
  jq -n \
    --arg worktree_id "$worktree_id" \
    --arg run_id "$LAUNCHER_RUN_ID" \
    --arg scenario "$scenario_id" \
    --arg worktree_path "$ROOT_DIR" \
    --arg source_commit "$source_commit" \
    --arg classification "$classification" \
    --arg phase "$phase" \
    --arg message "$message" \
    --arg started_at "$LAUNCHER_STARTED_AT" \
    --arg ended_at "$ended_at" \
    --arg evidence "$CURRENT_EVIDENCE" \
    --arg log_path "${log_path#"$CURRENT_EVIDENCE"/}" \
    --arg logs "$CURRENT_EVIDENCE/logs" \
    --arg temporary "$CURRENT_EVIDENCE/tmp" \
    --arg derived_data "$CURRENT_EVIDENCE/DerivedData" \
    --arg control "$CURRENT_EVIDENCE/control" \
    --arg state "$CURRENT_EVIDENCE/home" \
    --argjson dirty_state "$dirty_state" \
    --argjson exact_command "$PUBLIC_COMMAND_JSON" \
    '{
      schemaVersion: 1,
      worktreeID: $worktree_id,
      runID: $run_id,
      scenarioID: $scenario,
      worktreePath: $worktree_path,
      sourceCommit: $source_commit,
      dirtyState: $dirty_state,
      validationProfile: "launcher",
      startedAt: $started_at,
      endedAt: $ended_at,
      paths: {
        state: $state,
        logs: $logs,
        temporary: $temporary,
        derivedData: $derived_data,
        control: $control,
        evidence: $evidence
      },
      fixtures: [],
      fixtureEndpoints: [],
      redactionWarnings: [],
      artifacts: [{
        kind: "launcherLog",
        path: $log_path,
        status: "diagnostic"
      }],
      actionIntervals: [],
      exclusiveInputIntervals: [],
      cleanupVerified: true,
      failureState: {
        primaryFailure: {
          classification: $classification,
          phase: $phase,
          message: $message
        },
        secondaryFailures: [],
        effectiveClassification: $classification
      },
      exactCommand: $exact_command
    }' > "$CURRENT_EVIDENCE/manifest.json"
  printf '%s\n' "$message" > "$CURRENT_EVIDENCE/diagnostics/failure.txt"
}

run_evidence_stage() {
  local scenario_id="$1"
  local classification="$2"
  local phase="$3"
  local log_name="$4"
  shift 4
  local log_path="$CURRENT_EVIDENCE/logs/$log_name"
  local status
  set +e
  (set -e; "$@") > "$log_path" 2>&1
  status="$?"
  set -e
  cat "$log_path"
  if [ "$status" -ne 0 ]; then
    if [ "$status" -eq 69 ]; then
      classification="environment_unavailable"
    fi
    write_launcher_failure \
      "$scenario_id" "$classification" "$phase" "$status" "$log_path"
    return "$status"
  fi
}
