#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
CANONICAL_HOME="${HOME}/.ottam"
WORKSPACE_ARG=""
WORKSPACE_ARG_PROVIDED=0
FOREGROUND=0
LOGS=0
TELEMETRY=0
FORCE_ISOLATED=0
COMMAND="launch"
BUILD_CONFIGURATION_ARGUMENTS=()
# The dev loop restarts its own app, so an instance at this exact app URL with
# no owner manifest is reclaimed rather than reported. Callers that must never
# terminate a process they did not start themselves opt back into the refusal.
STRICT_OWNERSHIP="${OTTAM_RUN_APP_STRICT_OWNERSHIP:-0}"

usage() {
  cat <<'USAGE'
Usage: scripts/run-app.sh [options] [workspace-path]

Builds and launches Ottam with exact instance ownership. Linked worktrees
default to a stable isolated identity; the primary checkout retains the
canonical development identity unless --isolated is supplied.

An instance already live at this exact app URL is stopped and replaced, even
without an owner manifest; --strict-ownership refuses instead.

Options:
  --isolated    Force the stable worktree-specific development instance
  --status      Report only this checkout's selected instance
  --stop        Stop only this checkout's selected instance
  --fresh       Recoverably rotate isolated state, then launch
  --release     Build the staged app optimized (no DEBUG testing hooks)
  --profile     Build the staged app optimized with the DEBUG hooks kept
  --foreground  Run the exact staged executable with stdout/stderr attached
  --logs        Run in foreground and tee app output to the instance log
  --telemetry   Run in foreground and stream logs for the exact owned PID
  --strict-ownership
                Refuse to stop any instance this run did not start
  -h, --help    Show this help
USAGE
}

fail_usage() {
  echo "$1" >&2
  usage >&2
  exit 64
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --isolated)
      FORCE_ISOLATED=1
      shift
      ;;
    --status)
      [ "$COMMAND" = "launch" ] || fail_usage "Choose only one lifecycle command."
      COMMAND="status"
      shift
      ;;
    --stop)
      [ "$COMMAND" = "launch" ] || fail_usage "Choose only one lifecycle command."
      COMMAND="stop"
      shift
      ;;
    --fresh)
      [ "$COMMAND" = "launch" ] || fail_usage "Choose only one lifecycle command."
      COMMAND="fresh"
      shift
      ;;
    --release|--profile)
      BUILD_CONFIGURATION_ARGUMENTS=("$1")
      shift
      ;;
    --foreground)
      FOREGROUND=1
      shift
      ;;
    --logs)
      FOREGROUND=1
      LOGS=1
      shift
      ;;
    --telemetry)
      FOREGROUND=1
      TELEMETRY=1
      shift
      ;;
    --strict-ownership)
      STRICT_OWNERSHIP=1
      shift
      ;;
    --*)
      fail_usage "Unknown option: $1"
      ;;
    *)
      [ "$WORKSPACE_ARG_PROVIDED" = "0" ] || fail_usage "Only one workspace path is accepted."
      WORKSPACE_ARG="$1"
      WORKSPACE_ARG_PROVIDED=1
      shift
      ;;
  esac
done

GIT_DIR="$(git -C "$ROOT_DIR" rev-parse --path-format=absolute --git-dir 2>/dev/null || true)"
GIT_COMMON_DIR="$(git -C "$ROOT_DIR" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
if [ -z "$GIT_DIR" ] || [ -z "$GIT_COMMON_DIR" ]; then
  [ "$FORCE_ISOLATED" = "1" ] || {
    echo "Repository identity is unavailable; canonical launch is refused. Use --isolated." >&2
    exit 69
  }
  IS_LINKED_WORKTREE=1
else
  GIT_DIR="$(cd "$GIT_DIR" && pwd -P)"
  GIT_COMMON_DIR="$(cd "$GIT_COMMON_DIR" && pwd -P)"
  if [ "$GIT_DIR" = "$GIT_COMMON_DIR" ]; then
    IS_LINKED_WORKTREE=0
  else
    IS_LINKED_WORKTREE=1
  fi
fi

WORKTREE_HASH="$(printf '%s' "$ROOT_DIR" | shasum -a 256 | awk '{print $1}')"
WORKTREE_ID="w${WORKTREE_HASH:0:12}"
DISPLAY_NAME="$(/bin/bash "$ROOT_DIR/scripts/app-display-name.sh" --root "$ROOT_DIR" --role dev)"
if [ "$FORCE_ISOLATED" = "1" ] || [ "$IS_LINKED_WORKTREE" = "1" ]; then
  MODE="isolated"
  INSTANCE_ROOT="$ROOT_DIR/.build/run-app/$WORKTREE_ID"
  APP_NAME="Ottam Dev $WORKTREE_ID"
  BUNDLE_IDENTIFIER="dev.ottam.Ottam.dev.$WORKTREE_ID"
  EXECUTABLE_NAME="OttamDev-$WORKTREE_ID"
  APP_ROOT="$INSTANCE_ROOT/app"
  APP_DIR="$APP_ROOT/$APP_NAME.app"
  STATE_DIR="$INSTANCE_ROOT/home"
  LOG_DIR="$INSTANCE_ROOT/logs"
  TEMP_DIR="$INSTANCE_ROOT/tmp"
else
  MODE="canonical"
  INSTANCE_ROOT="$ROOT_DIR/.build/run-app/canonical"
  APP_NAME="Ottam"
  BUNDLE_IDENTIFIER="dev.ottam.Ottam"
  EXECUTABLE_NAME="Ottam"
  APP_ROOT="$ROOT_DIR/.build/app"
  APP_DIR="$APP_ROOT/Ottam.app"
  STATE_DIR="${OTTAM_HOME:-$CANONICAL_HOME}"
  LOG_DIR="$INSTANCE_ROOT/logs"
  TEMP_DIR="$INSTANCE_ROOT/tmp"
fi
OWNER_FILE="$INSTANCE_ROOT/owner.json"
APP_EXECUTABLE="$APP_DIR/Contents/MacOS/$EXECUTABLE_NAME"
PROCESS_LOG="$LOG_DIR/ottam-app.log"

if [ "$MODE" = "isolated" ] && [ -n "${OTTAM_HOME:-}" ]; then
  REQUESTED_HOME="$(mkdir -p "$OTTAM_HOME" && cd "$OTTAM_HOME" && pwd -P)"
  CANONICAL_HOME_PATH="$(mkdir -p "$CANONICAL_HOME" && cd "$CANONICAL_HOME" && pwd -P)"
  case "$REQUESTED_HOME/" in
    "$CANONICAL_HOME_PATH/"*)
      echo "Isolated OTTAM_HOME must not be the canonical Ottam home: $REQUESTED_HOME" >&2
      exit 64
      ;;
  esac
  case "$CANONICAL_HOME_PATH/" in
    "$REQUESTED_HOME/"*)
      echo "Isolated OTTAM_HOME must not contain the canonical Ottam home: $REQUESTED_HOME" >&2
      exit 64
      ;;
  esac
  while IFS= read -r OTHER_WORKTREE; do
    [ -d "$OTHER_WORKTREE" ] || continue
    OTHER_WORKTREE="$(cd "$OTHER_WORKTREE" && pwd -P)"
    [ "$OTHER_WORKTREE" != "$ROOT_DIR" ] || continue
    OTHER_HASH="$(printf '%s' "$OTHER_WORKTREE" | shasum -a 256 | awk '{print $1}')"
    OTHER_ID="w${OTHER_HASH:0:12}"
    OTHER_INSTANCE="$OTHER_WORKTREE/.build/run-app/$OTHER_ID"
    OTHER_STATE="$OTHER_INSTANCE/home"
    OTHER_OWNER="$OTHER_INSTANCE/owner.json"
    if [ -f "$OTHER_OWNER" ]; then
      RECORDED_STATE="$(plutil -extract stateDirectory raw -o - "$OTHER_OWNER" 2>/dev/null || true)"
      if [ -n "$RECORDED_STATE" ]; then
        OTHER_STATE="$RECORDED_STATE"
      fi
    fi
    case "$REQUESTED_HOME/" in
      "$OTHER_STATE/"*)
        echo "Isolated OTTAM_HOME overlaps another worktree state root: $OTHER_STATE" >&2
        exit 64
        ;;
    esac
    case "$OTHER_STATE/" in
      "$REQUESTED_HOME/"*)
        echo "Isolated OTTAM_HOME contains another worktree state root: $OTHER_STATE" >&2
        exit 64
        ;;
    esac
  done < <(
    git -C "$ROOT_DIR" worktree list --porcelain |
      sed -n 's/^worktree //p'
  )
  STATE_DIR="$REQUESTED_HOME"
fi

mkdir -p "$INSTANCE_ROOT" "$LOG_DIR" "$TEMP_DIR"


# shellcheck source=scripts/lib/run-app-lifecycle.sh
source "$ROOT_DIR/scripts/lib/run-app-lifecycle.sh"
if [ "$COMMAND" = "status" ]; then
  status_instance
  exit $?
fi
if [ "$COMMAND" = "stop" ]; then
  stop_instance || exit $?
  resolve_ownerless_instances \
    "the exact app URL and bundle identifier are live without an owner manifest"
  exit $?
fi

if [ "$COMMAND" = "fresh" ]; then
  [ "$MODE" = "isolated" ] || fail_usage "--fresh is available only for isolated instances."
  stop_instance
  if [ -d "$STATE_DIR" ]; then
    BACKUP_DIR="$INSTANCE_ROOT/home.backup.$(date -u +%Y%m%dT%H%M%SZ)"
    mv "$STATE_DIR" "$BACKUP_DIR"
    echo "Rotated prior state to $BACKUP_DIR"
  fi
fi

if [ -n "${OTTAM_WORKSPACE:-}" ]; then
  WORKSPACE_PATH="$OTTAM_WORKSPACE"
elif [ "$WORKSPACE_ARG_PROVIDED" = "1" ]; then
  WORKSPACE_PATH="$WORKSPACE_ARG"
else
  WORKSPACE_PATH="$PWD"
fi
[ -d "$WORKSPACE_PATH" ] || {
  echo "Workspace path does not exist: $WORKSPACE_PATH" >&2
  exit 66
}
WORKSPACE_PATH="$(cd "$WORKSPACE_PATH" && pwd -P)"

"$ROOT_DIR/scripts/fetch-ghosttykit.sh"

if [ -f "$OWNER_FILE" ]; then
  stop_instance
fi
stop_relaunch_process_if_requested
resolve_ownerless_instances \
  "the exact app URL and bundle identifier are live without valid ownership"

mkdir -p "$STATE_DIR" "$LOG_DIR" "$TEMP_DIR"
"$ROOT_DIR/scripts/build-app.sh" \
  --output-root "$APP_ROOT" \
  --app-name "$APP_NAME" \
  --display-name "$DISPLAY_NAME" \
  --bundle-id "$BUNDLE_IDENTIFIER" \
  --executable-name "$EXECUTABLE_NAME" \
  "${BUILD_CONFIGURATION_ARGUMENTS[@]+"${BUILD_CONFIGURATION_ARGUMENTS[@]}"}"

print_identity
echo "OTTAM_WORKSPACE=$WORKSPACE_PATH"

resolve_ownerless_instances "an exact app appeared while rebuilding"
LAUNCH_BASELINE_RECORDS="$(running_app_records || true)"

TELEMETRY_PID=""
OUTPUT_TAIL_PID=""
FOREGROUND_OWNER_PID=""
FOREGROUND_OWNER_START=""
cleanup_foreground() {
  if [ -n "$TELEMETRY_PID" ]; then
    kill "$TELEMETRY_PID" 2>/dev/null || true
    wait "$TELEMETRY_PID" 2>/dev/null || true
  fi
  if [ -n "$OUTPUT_TAIL_PID" ]; then
    kill "$OUTPUT_TAIL_PID" 2>/dev/null || true
    wait "$OUTPUT_TAIL_PID" 2>/dev/null || true
  fi
  if [ -n "$FOREGROUND_OWNER_PID" ] && [ -n "$FOREGROUND_OWNER_START" ]; then
    remove_foreground_owner_if_exited \
      "$FOREGROUND_OWNER_PID" \
      "$FOREGROUND_OWNER_START"
  fi
}

OPEN_ENVIRONMENT=(
  --env "OTTAM_HOME=$STATE_DIR"
  --env "OTTAM_LOG_DIR=$LOG_DIR"
  --env "OTTAM_WORKSPACE=$WORKSPACE_PATH"
  --env "OTTAM_INSTANCE_ID=$WORKTREE_ID"
  --env "TMPDIR=$TEMP_DIR/"
)
# `open` doesn't pass this shell's environment on, so forward the backend log
# controls (AGENTS.md, Runtime Logs) explicitly.
for name in OTTAM_LOG OTTAM_ACP_WIRE_LOG; do
  if [ -n "${!name:-}" ]; then
    OPEN_ENVIRONMENT+=(--env "$name=${!name}")
  fi
done
if [ -n "${OTTAM_DEBUG_TRANSCRIPT_DISABLE_FADES:-}" ]; then
  OPEN_ENVIRONMENT+=(
    --env "OTTAM_DEBUG_TRANSCRIPT_DISABLE_FADES=$OTTAM_DEBUG_TRANSCRIPT_DISABLE_FADES"
  )
fi
if [ -n "${OTTAM_DEBUG_TRANSCRIPT_DISABLE_FOLLOW_BOTTOM:-}" ]; then
  OPEN_ENVIRONMENT+=(
    --env "OTTAM_DEBUG_TRANSCRIPT_DISABLE_FOLLOW_BOTTOM=$OTTAM_DEBUG_TRANSCRIPT_DISABLE_FOLLOW_BOTTOM"
  )
fi
if [ -n "${OTTAM_DEBUG_TRANSCRIPT_SKIP_EXPLICIT_SCROLL_REFLECTION:-}" ]; then
  OPEN_ENVIRONMENT+=(
    --env "OTTAM_DEBUG_TRANSCRIPT_SKIP_EXPLICIT_SCROLL_REFLECTION=$OTTAM_DEBUG_TRANSCRIPT_SKIP_EXPLICIT_SCROLL_REFLECTION"
  )
fi
if [ -n "${OTTAM_DEBUG_TRANSCRIPT_DISABLE_RESPONSIVE_SCROLLING:-}" ]; then
  OPEN_ENVIRONMENT+=(
    --env "OTTAM_DEBUG_TRANSCRIPT_DISABLE_RESPONSIVE_SCROLLING=$OTTAM_DEBUG_TRANSCRIPT_DISABLE_RESPONSIVE_SCROLLING"
  )
fi
if [ -n "${OTTAM_DEBUG_TRANSCRIPT_FORCE_VISIBLE_REDRAW:-}" ]; then
  OPEN_ENVIRONMENT+=(
    --env "OTTAM_DEBUG_TRANSCRIPT_FORCE_VISIBLE_REDRAW=$OTTAM_DEBUG_TRANSCRIPT_FORCE_VISIBLE_REDRAW"
  )
fi
if [ -n "${OTTAM_DEBUG_STREAMING_TEXT_FPS:-}" ]; then
  OPEN_ENVIRONMENT+=(
    --env "OTTAM_DEBUG_STREAMING_TEXT_FPS=$OTTAM_DEBUG_STREAMING_TEXT_FPS"
  )
fi
if [ "$FOREGROUND" = "1" ]; then
  trap cleanup_foreground EXIT
  : > "$PROCESS_LOG"
  /usr/bin/tail -n 0 -F "$PROCESS_LOG" &
  OUTPUT_TAIL_PID="$!"
  OPEN_OUTPUT_ARGS=(--stdout "$PROCESS_LOG" --stderr "$PROCESS_LOG")
  if [ "$LOGS" = "1" ]; then
    echo "Writing app process log to $PROCESS_LOG"
  fi
  /usr/bin/open -n -W \
    "${OPEN_OUTPUT_ARGS[@]}" \
    "${OPEN_ENVIRONMENT[@]}" \
    "$APP_DIR" &
  OPEN_PID="$!"
  for _ in {1..200}; do
    CURRENT_RECORDS="$(running_app_records || true)"
    APP_PID="$(
      unique_new_running_app_pid \
        "$LAUNCH_BASELINE_RECORDS" \
        "$CURRENT_RECORDS" || true
    )"
    [ -n "$APP_PID" ] && break
    RECORD_COUNT="$(sed '/^$/d' <<< "$CURRENT_RECORDS" | wc -l | tr -d ' ')"
    if [ "$RECORD_COUNT" -gt 1 ]; then
      echo "Multiple exact Ottam processes appeared after launch; refusing ownership." >&2
      exit 70
    fi
    sleep 0.1
  done
  [ -n "${APP_PID:-}" ] || {
    echo "Ottam did not register with LaunchServices." >&2
    report_foreign_bundle_instances
    exit 70
  }
  FOREGROUND_OWNER_PID="$APP_PID"
  FOREGROUND_OWNER_START="$(process_start_identity "$APP_PID")"
  [ -n "$FOREGROUND_OWNER_START" ] || {
    echo "Could not identify the foreground Ottam process." >&2
    exit 70
  }
  write_owner_manifest "$FOREGROUND_OWNER_PID" "$FOREGROUND_OWNER_START"
  echo "Owned PID: $FOREGROUND_OWNER_PID"
  echo "Owner manifest: $OWNER_FILE"
  if [ "$TELEMETRY" = "1" ]; then
    TELEMETRY_PREDICATE="processID == $APP_PID"
    for _ in {1..100}; do
      CHILD_PIDS="$(descendant_pids "$APP_PID")"
      [ -n "$CHILD_PIDS" ] && break
      sleep 0.1
    done
    while IFS= read -r CHILD_PID; do
      [ -n "$CHILD_PID" ] || continue
      TELEMETRY_PREDICATE="$TELEMETRY_PREDICATE OR processID == $CHILD_PID"
    done <<< "${CHILD_PIDS:-}"
    /usr/bin/log stream --style compact --level debug \
      --predicate "$TELEMETRY_PREDICATE" &
    TELEMETRY_PID="$!"
  fi
  wait "$OPEN_PID"
  exit $?
fi

/usr/bin/open -n \
  --stdout "$PROCESS_LOG" \
  --stderr "$PROCESS_LOG" \
  "${OPEN_ENVIRONMENT[@]}" \
  "$APP_DIR"
for _ in {1..200}; do
  CURRENT_RECORDS="$(running_app_records || true)"
  APP_PID="$(
    unique_new_running_app_pid \
      "$LAUNCH_BASELINE_RECORDS" \
      "$CURRENT_RECORDS" || true
  )"
  if [ -n "$APP_PID" ] &&
     kill -0 "$APP_PID" 2>/dev/null &&
     [ -n "$(process_start_identity "$APP_PID")" ]; then
    break
  fi
  RECORD_COUNT="$(sed '/^$/d' <<< "$CURRENT_RECORDS" | wc -l | tr -d ' ')"
  if [ "$RECORD_COUNT" -gt 1 ]; then
    echo "Multiple exact Ottam processes appeared after launch; refusing ownership." >&2
    exit 70
  fi
  sleep 0.1
done
if [ -z "${APP_PID:-}" ] || ! kill -0 "$APP_PID" 2>/dev/null; then
  echo "Ottam did not start; see $PROCESS_LOG" >&2
  report_foreign_bundle_instances
  exit 70
fi

PROCESS_START="$(process_start_identity "$APP_PID")"
[ -n "$PROCESS_START" ] || {
  echo "Ottam (PID $APP_PID) exited before it could be owned." >&2
  report_foreign_bundle_instances
  exit 70
}
write_owner_manifest "$APP_PID" "$PROCESS_START"

echo "Owned PID: $APP_PID"
echo "Owner manifest: $OWNER_FILE"
