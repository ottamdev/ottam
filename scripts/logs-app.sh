#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAIL_LINES=80
MODE="all"
LAST_WINDOW="10m"
STREAM=0
# Every Ottam build logs under its bundle id: the release app under
# dev.ottam.Ottam, run-app instances under dev.ottam.Ottam.dev.<id>.
SUBSYSTEM=""

usage() {
  cat <<'USAGE'
Usage: scripts/logs-app.sh [--all|--swift|--backend] [--last 10m] [--stream] [--tail-lines N] [--subsystem ID]

Print or stream Ottam app/backend logs for humans and agents.

Options:
  --all           Show Swift unified logs and backend file logs (default)
  --swift         Show only Swift unified logs
  --backend       Show only backend trace/stderr logs
  --last WINDOW   log show window for Swift logs, e.g. 5m, 1h (default: 10m)
  --stream        Stream Swift unified logs and backend files until interrupted
  --tail-lines N  Number of backend log lines to print or tail (default: 80)
  --subsystem ID  Show Swift logs from one build only, by its bundle id (run-app
                  prints it as "Bundle identifier"); default: every Ottam build
  -h, --help      Show this help
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --all)
      MODE="all"
      shift
      ;;
    --swift)
      MODE="swift"
      shift
      ;;
    --backend)
      MODE="backend"
      shift
      ;;
    --last)
      if [ "$#" -lt 2 ]; then
        echo "--last requires a value" >&2
        exit 64
      fi
      LAST_WINDOW="$2"
      shift 2
      ;;
    --stream)
      STREAM=1
      shift
      ;;
    --tail-lines)
      if [ "$#" -lt 2 ]; then
        echo "--tail-lines requires a value" >&2
        exit 64
      fi
      TAIL_LINES="$2"
      shift 2
      ;;
    --subsystem)
      if [ "$#" -lt 2 ]; then
        echo "--subsystem requires a value" >&2
        exit 64
      fi
      SUBSYSTEM="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 64
      ;;
  esac
done

if ! [[ "$TAIL_LINES" =~ ^[0-9]+$ ]] || [ "$TAIL_LINES" -lt 1 ]; then
  echo "--tail-lines must be a positive integer" >&2
  exit 64
fi

resolved_ottam_home() {
  if [ -n "${OTTAM_HOME:-}" ]; then
    printf '%s\n' "$OTTAM_HOME"
  else
    printf '%s\n' "$HOME/.ottam"
  fi
}

resolved_log_dir() {
  if [ -n "${OTTAM_LOG_DIR:-}" ]; then
    printf '%s\n' "$OTTAM_LOG_DIR"
  else
    printf '%s/logs\n' "$(resolved_ottam_home)"
  fi
}

newest_file() {
  local pattern="$1"
  local exclude_pattern="${2:-}"
  local log_dir candidate candidate_mtime newest newest_mtime
  log_dir="$(resolved_log_dir)"
  newest=""
  newest_mtime=-1
  if [ ! -d "$log_dir" ]; then
    return 0
  fi
  if [ -n "$exclude_pattern" ]; then
    while IFS= read -r -d '' candidate; do
      candidate_mtime="$(stat -f '%m' "$candidate" 2>/dev/null || stat -c '%Y' "$candidate" 2>/dev/null || printf '0')"
      if [ "$candidate_mtime" -gt "$newest_mtime" ]; then
        newest_mtime="$candidate_mtime"
        newest="$candidate"
      fi
    done < <(find "$log_dir" -maxdepth 1 -type f -name "$pattern" ! -name "$exclude_pattern" -print0 2>/dev/null || true)
  else
    while IFS= read -r -d '' candidate; do
      candidate_mtime="$(stat -f '%m' "$candidate" 2>/dev/null || stat -c '%Y' "$candidate" 2>/dev/null || printf '0')"
      if [ "$candidate_mtime" -gt "$newest_mtime" ]; then
        newest_mtime="$candidate_mtime"
        newest="$candidate"
      fi
    done < <(find "$log_dir" -maxdepth 1 -type f -name "$pattern" -print0 2>/dev/null || true)
  fi
  if [ -n "$newest" ]; then
    printf '%s\n' "$newest"
  fi
}

print_context() {
  local home_dir log_dir trace_log stderr_log xcode_log
  home_dir="$(resolved_ottam_home)"
  log_dir="$(resolved_log_dir)"
  trace_log="$(newest_file 'core-*.log' 'core-*.stderr.log')"
  stderr_log="$(newest_file 'core-*.stderr.log')"
  xcode_log="${TMPDIR:-/tmp}/ottam-xcode-prepare-backend.log"

  echo "OTTAM_HOME=$home_dir"
  echo "OTTAM_LOG_DIR=$log_dir"
  echo "Newest backend trace log=${trace_log:-<none>}"
  echo "Newest backend stderr log=${stderr_log:-<none>}"
  echo "Xcode pre-action log=$xcode_log"
}

show_swift() {
  local predicate='subsystem BEGINSWITH "dev.ottam.Ottam"'
  if [ -n "$SUBSYSTEM" ]; then
    predicate="subsystem == \"$SUBSYSTEM\""
  fi
  if [ "$STREAM" = "1" ]; then
    log stream --style compact --level debug --predicate "$predicate"
  else
    log show --info --debug --last "$LAST_WINDOW" --style compact --predicate "$predicate"
  fi
}

show_backend() {
  local trace_log stderr_log
  trace_log="$(newest_file 'core-*.log' 'core-*.stderr.log')"
  stderr_log="$(newest_file 'core-*.stderr.log')"

  if [ -z "$trace_log" ] && [ -z "$stderr_log" ]; then
    echo "No backend logs found in $(resolved_log_dir)" >&2
    return 1
  fi

  if [ "$STREAM" = "1" ]; then
    tail_args=()
    if [ -n "$trace_log" ]; then
      tail_args+=("$trace_log")
    fi
    if [ -n "$stderr_log" ]; then
      tail_args+=("$stderr_log")
    fi
    tail -n "$TAIL_LINES" -F "${tail_args[@]}"
  else
    if [ -n "$trace_log" ]; then
      echo "--- $trace_log ---"
      tail -n "$TAIL_LINES" "$trace_log"
    fi
    if [ -n "$stderr_log" ]; then
      echo "--- $stderr_log ---"
      tail -n "$TAIL_LINES" "$stderr_log"
    fi
  fi
}

print_context

case "$MODE" in
  swift)
    show_swift
    ;;
  backend)
    show_backend
    ;;
  all)
    if [ "$STREAM" = "1" ]; then
      show_backend &
      backend_pid="$!"
      show_swift &
      swift_pid="$!"
      trap 'kill "$backend_pid" "$swift_pid" 2>/dev/null || true' EXIT
      wait "$backend_pid" "$swift_pid"
    else
      show_swift
      show_backend
    fi
    ;;
esac
