#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/ottam-run-app-test.XXXXXX")"
trap 'rm -rf "$test_root"' EXIT

INSTANCE_ROOT="$test_root/instance"
OWNER_FILE="$INSTANCE_ROOT/owner.json"
APP_DIR="$test_root/Ottam Test.app"
APP_EXECUTABLE="$APP_DIR/Contents/MacOS/OttamTest"
MODE="isolated"
WORKTREE_ID="wtest"
ROOT_DIR="$test_root/worktree"
BUNDLE_IDENTIFIER="dev.ottam.Ottam.dev.wtest"
STATE_DIR="$INSTANCE_ROOT/home"
LOG_DIR="$INSTANCE_ROOT/logs"
mkdir -p "$(dirname "$APP_EXECUTABLE")" "$ROOT_DIR" "$STATE_DIR" "$LOG_DIR"
printf '#!/bin/sh\nexit 0\n' > "$APP_EXECUTABLE"
chmod +x "$APP_EXECUTABLE"

# shellcheck source=scripts/lib/run-app-lifecycle.sh
source "$repo_root/scripts/lib/run-app-lifecycle.sh"

# A pending or cancelled Quit must survive the launcher's bounded wait.
(
  process_start_identity() { printf 'stable\n'; }
  sleep() { :; }
  kill() { echo "launcher must not force a pending Quit" >&2; exit 1; }
  if await_owned_exit 424242 stable >/dev/null 2>&1; then
    echo "launcher accepted an app that never completed Quit" >&2
    exit 1
  fi
  printf '424243\tstable\tfixture-helper\n' > "$test_root/refresh-helper.tsv"
  kill() { [ "$1" = "-TERM" ] || exit 42; }
  if terminate_owned_children "$test_root/refresh-helper.tsv" >/dev/null 2>&1; then
    echo "launcher accepted a helper that never completed shutdown" >&2
    exit 1
  else
    [ "$?" = "1" ]
  fi
)

# A descendant that exits between the pid listing and its `ps` lookup must be
# skipped, not abort the sweep: this harness runs under `set -euo pipefail`
# exactly like run-app.sh. PID 1 is live and never a descendant of this
# shell; 999999 is above the macOS pid ceiling, so it never exists.
(
  descendant_pids() {
    # Only the swept root has children; the protected sweep of this shell
    # sees none, so neither pid is exempted from capture.
    [ "$1" = "424242" ] && printf '999999\n1\n'
    return 0
  }
  snapshot="$test_root/vanished-child.tsv"
  capture_owned_children 424242 "$snapshot"
  [ "$(wc -l < "$snapshot" | tr -d ' ')" = "1" ]
  [ "$(cut -f1 "$snapshot")" = "1" ]
  [ -z "$(process_start_identity 999999)" ]
  [ -z "$(process_command 999999)" ]
)

before_record=$'10\tbefore'
after_record=$'10\tbefore\n20\tafter'
[ "$(unique_new_running_app_pid "$before_record" "$after_record")" = "20" ]
if unique_new_running_app_pid "$before_record" "$before_record" >/dev/null; then
  echo "launch discovery accepted no new process" >&2
  exit 1
fi
if unique_new_running_app_pid \
    "$before_record" \
    $'10\tbefore\n20\tafter\n30\tother' >/dev/null; then
  echo "launch discovery accepted multiple new processes" >&2
  exit 1
fi

running_app_records() {
  printf '20\tafter\n'
}
if refuse_ownerless_instances "test ownerless instance" >/dev/null 2>&1; then
  echo "ownerless live instance was not refused" >&2
  exit 1
fi
running_app_records() {
  return 0
}
refuse_ownerless_instances "test empty process set"

signal_calls=""
process_start_identity() {
  [ "$1" = "20" ] && printf 'after\n'
}
process_command() {
  [ "$1" = "20" ] && printf '%s\n' "$APP_EXECUTABLE"
}
kill() {
  signal_calls="${signal_calls}${signal_calls:+,}$1:$2"
  return 0
}
signal_app_instance_if_identity_matches "20" "after" TERM
[ "$signal_calls" = "-TERM:20" ]
if signal_app_instance_if_identity_matches "20" "reused" TERM; then
  echo "ownerless signal accepted a reused process identity" >&2
  exit 1
else
  [ "$?" = "2" ]
fi
[ "$signal_calls" = "-TERM:20" ]
unset -f kill

running_app_records() {
  printf '20\tafter\n'
}
ownerless_cleanup_calls=""
capture_owned_children() {
  ownerless_cleanup_calls="${ownerless_cleanup_calls}${ownerless_cleanup_calls:+,}capture:$1"
  printf '21\tchild-start\t/usr/bin/ottam-core\n' > "$2"
}
kill() {
  ownerless_cleanup_calls="${ownerless_cleanup_calls}${ownerless_cleanup_calls:+,}$1:$2"
}
await_owned_exit() {
  ownerless_cleanup_calls="${ownerless_cleanup_calls}${ownerless_cleanup_calls:+,}await:$1"
}
terminate_owned_children() {
  [ -s "$1" ]
  ownerless_cleanup_calls="${ownerless_cleanup_calls}${ownerless_cleanup_calls:+,}children"
}
reclaim_ownerless_instances "test ownerless cleanup" >/dev/null
[ "$ownerless_cleanup_calls" = "capture:20,-TERM:20,await:20,children" ]
unset -f running_app_records capture_owned_children kill await_owned_exit \
  terminate_owned_children

process_start_identity() {
  [ "$1" = "$$" ] && printf 'stable-start\n'
}
process_command() {
  [ "$1" = "$$" ] && printf '%s\n' "$APP_EXECUTABLE"
}
write_owner_manifest "$$" "stable-start"
owner_matches_live_process

# Replacing the staged bytes is the one ownership mismatch a normal rebuild is
# allowed to reinterpret. Every manifest field and the live command still have
# to identify this exact instance.
printf '#!/bin/sh\nexit 1\n' > "$APP_EXECUTABLE"
owner_process_matches_manifest
if owner_matches_live_process; then
  echo "replaced executable unexpectedly matched the recorded hash" >&2
  exit 1
fi

process_command() {
  [ "$1" = "$$" ] && printf '/usr/bin/not-ottam\n'
}
signal_calls=""
kill() {
  if [ "$1" = "-0" ]; then
    return 0
  fi
  signal_calls="${signal_calls}${signal_calls:+,}$1:$2"
  return 0
}
if stop_instance >/dev/null 2>&1; then
  echo "conflicting live command was accepted as rebuild drift" >&2
  exit 1
fi
[ -z "$signal_calls" ]
[ -e "$OWNER_FILE" ]
unset -f kill

process_command() {
  [ "$1" = "$$" ] && printf '%s\n' "$APP_EXECUTABLE"
}
signal_calls=""
capture_owned_children() {
  : > "$2"
}
signal_process_if_identity_matches() {
  signal_calls="${signal_calls}${signal_calls:+,}$3:$1"
}
await_owned_exit() {
  return 0
}
terminate_owned_children() {
  return 0
}
stop_instance >/dev/null
[ "$signal_calls" = "TERM:$$" ]
[ ! -e "$OWNER_FILE" ]
unset -f capture_owned_children signal_process_if_identity_matches \
  await_owned_exit terminate_owned_children

write_owner_manifest "$$" "stable-start"
process_start_identity() {
  [ "$1" = "$$" ] && printf 'reused-start\n'
}
if owner_process_identity_is_live; then
  echo "PID reuse passed the process-start identity check" >&2
  exit 1
fi
if owner_matches_live_process; then
  echo "PID reuse passed exact ownership validation" >&2
  exit 1
fi
stop_instance >/dev/null
[ ! -e "$OWNER_FILE" ]

(
  UI_TEST_ROOT="$test_root/ui-tests"
  CURRENT_EVIDENCE="$UI_TEST_ROOT/current"
  ARCHIVE_ROOT="$UI_TEST_ROOT/archive"
  RUN_LOCK="$UI_TEST_ROOT/.run-lock"
  RUN_LOCK_TOKEN=""
  mkdir -p "$RUN_LOCK"
  printf '999999\n' > "$RUN_LOCK/pid"
  printf 'stale-start\n' > "$RUN_LOCK/start"
  printf 'stale-token\n' > "$RUN_LOCK/token"
  # shellcheck source=scripts/lib/ui-test-evidence.sh
  source "$repo_root/scripts/lib/ui-test-evidence.sh"
  acquire_run_lock
  [ "$(cat "$RUN_LOCK/pid")" = "$$" ]
  release_run_lock
  [ ! -d "$RUN_LOCK" ]
)

echo "run-app lifecycle tests passed"
