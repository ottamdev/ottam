# Shared exact-process lifecycle helpers for scripts/run-app.sh.
#
# Lookups of a process that already exited answer empty and succeed; see
# lib/process.sh.
# shellcheck source=scripts/lib/process.sh
source "$(dirname "${BASH_SOURCE[0]}")/process.sh"

process_command() {
  ps -p "$1" -o command= 2>/dev/null | sed 's/^[[:space:]]*//' || true
}

descendant_pids() {
  ps -axo pid=,ppid= |
    awk -v root="$1" '
      {
        process_id[NR] = $1
        parent_id[NR] = $2
      }
      END {
        owned[root] = 1
        changed = 1
        while (changed) {
          changed = 0
          for (row = 1; row <= NR; row++) {
            if (owned[parent_id[row]] && !owned[process_id[row]]) {
              owned[process_id[row]] = 1
              changed = 1
            }
          }
        }
        for (row = 1; row <= NR; row++) {
          if (process_id[row] != root && owned[process_id[row]]) {
            print process_id[row]
          }
        }
      }
    '
}

capture_owned_children() {
  local root_pid="$1"
  local output="$2"
  local child_pid child_start child_command protected_pid
  local protected_pids
  # An in-app reload starts this script beneath the old app process. Preserve
  # the coordinator shell and anything it launches while stopping app helpers.
  protected_pids="$(
    {
      printf '%s\n' "$$"
      descendant_pids "$$"
    } | sed '/^[[:space:]]*$/d'
  )"
  : > "$output"
  while IFS= read -r child_pid; do
    [ -n "$child_pid" ] || continue
    while IFS= read -r protected_pid; do
      if [ "$child_pid" = "$protected_pid" ]; then
        child_pid=""
        break
      fi
    done <<< "$protected_pids"
    [ -n "$child_pid" ] || continue
    child_start="$(process_start_identity "$child_pid")"
    child_command="$(process_command "$child_pid")"
    [ -n "$child_start" ] && [ -n "$child_command" ] || continue
    printf '%s\t%s\t%s\n' \
      "$child_pid" "$child_start" "$child_command" >> "$output"
  done < <(descendant_pids "$root_pid")
}

terminate_owned_children() {
  local snapshot="$1"
  local child_pid child_start child_command
  while IFS=$'\t' read -r child_pid child_start child_command; do
    [ -n "$child_pid" ] || continue
    if [ "$(process_start_identity "$child_pid")" = "$child_start" ]; then
      kill -TERM "$child_pid"
    fi
  done < "$snapshot"
  for _ in {1..50}; do
    local survivor=0
    while IFS=$'\t' read -r child_pid child_start child_command; do
      if [ -n "$child_pid" ] &&
         [ "$(process_start_identity "$child_pid")" = "$child_start" ]; then
        survivor=1
        break
      fi
    done < "$snapshot"
    [ "$survivor" = "1" ] || return 0
    sleep 0.1
  done
  echo "Owned helpers are still completing shutdown; preserving them without a force-kill fallback." >&2
  return 1
}

# Every live process registered under this instance's bundle identifier, as
# `pid<TAB>bundle path`. Both the exact-instance check and the foreign-checkout
# diagnostic below are views onto this one list.
bundle_instance_records() {
  /usr/bin/lsappinfo list 2>/dev/null |
      awk -v expected_bundle="$BUNDLE_IDENTIFIER" '
        function emit() {
          if (bundle == expected_bundle && pid != "") {
            print pid "\t" path
          }
        }
        /^[[:space:]]*[0-9]+\)/ {
          emit()
          bundle = ""
          path = ""
          pid = ""
          next
        }
        /^[[:space:]]*bundleID="/ {
          value = $0
          sub(/^[[:space:]]*bundleID="/, "", value)
          sub(/"[[:space:]]*$/, "", value)
          bundle = value
          next
        }
        /^[[:space:]]*bundle path="/ {
          value = $0
          sub(/^[[:space:]]*bundle path="/, "", value)
          sub(/"[[:space:]]*$/, "", value)
          path = value
          next
        }
        /^[[:space:]]*pid = [0-9]+/ {
          value = $0
          sub(/^[[:space:]]*pid = /, "", value)
          sub(/[^0-9].*$/, "", value)
          pid = value
          next
        }
        END { emit() }
      '
}

# The exact instance: this bundle identifier staged at this app URL.
running_app_records() {
  local pid path start command
  while IFS=$'\t' read -r pid path; do
    [ -n "$pid" ] || continue
    [ "$path" = "$APP_DIR" ] || continue
    start="$(process_start_identity "$pid")"
    [ -n "$start" ] || continue
    command="$(process_command "$pid")"
    case "$command" in
      "$APP_EXECUTABLE"|"$APP_EXECUTABLE "*)
        ;;
      *)
        continue
        ;;
    esac
    printf '%s\t%s\n' "$pid" "$start"
  done < <(bundle_instance_records)
}

# The same bundle identifier staged somewhere else — another checkout, or a
# script that launched `.build/app/Ottam.app` directly. LaunchServices keys
# instances by bundle identifier, so one of these makes `open -n` hand back the
# foreign instance instead of a fresh one, and the launch below never registers.
# Reporting it is all this does: another checkout's process is not ours to stop.
foreign_bundle_instance_records() {
  local pid path
  while IFS=$'\t' read -r pid path; do
    [ -n "$pid" ] || continue
    [ "$path" != "$APP_DIR" ] || continue
    [ -n "$(process_start_identity "$pid")" ] || continue
    printf '%s\t%s\n' "$pid" "$path"
  done < <(bundle_instance_records)
}

report_foreign_bundle_instances() {
  local records
  records="$(foreign_bundle_instance_records || true)"
  [ -n "$records" ] || return 0
  echo "The bundle identifier $BUNDLE_IDENTIFIER is live from another location:" >&2
  printf '%s\n' "$records" >&2
  echo "LaunchServices reuses that instance, so this app URL cannot start." >&2
  echo "Stop it in the checkout that owns it, or use --isolated here." >&2
}

unique_new_running_app_pid() {
  local before_records="$1"
  local after_records="$2"
  local new_records count
  new_records="$(
    awk -F $'\t' '
      NR == FNR {
        if (NF) {
          before[$1 FS $2] = 1
        }
        next
      }
      NF && !before[$1 FS $2] {
        print
      }
    ' <(printf '%s\n' "$before_records") <(printf '%s\n' "$after_records")
  )"
  count="$(sed '/^$/d' <<< "$new_records" | wc -l | tr -d ' ')"
  [ "$count" = "1" ] || return 1
  awk -F $'\t' 'NF { print $1 }' <<< "$new_records"
}

refuse_ownerless_instances() {
  local context="$1"
  local records
  records="$(running_app_records || true)"
  [ -z "$records" ] || {
    echo "Refusing launch because $context:" >&2
    printf '%s\n' "$records" >&2
    return 70
  }
}

# Stops instances that match this bundle identifier *and* this app URL exactly,
# without a manifest to vouch for them.
#
# The exactness is the whole safety property, and it is the same one the owner
# manifest protects: no matching on the name `Ottam`, no foreign app URL, no
# other worktree's state. Under those constraints an unowned instance is not an
# ambiguity to report — it is the app the caller just asked to restart, launched
# by a run that was interrupted before it could record ownership.
#
# `--strict-ownership` keeps the refusal for callers that must never terminate a
# process they did not start themselves.
reclaim_ownerless_instances() {
  local context="$1"
  local records pid start signal_status child_snapshot
  records="$(running_app_records || true)"
  [ -n "$records" ] || return 0
  echo "Reclaiming an unowned instance because $context:"
  while IFS=$'\t' read -r pid start; do
    [ -n "$pid" ] || continue
    app_instance_process_is_live "$pid" "$start" || continue
    echo "  Stopping unowned PID $pid"
    child_snapshot="$(mktemp "$INSTANCE_ROOT/ownerless-children.XXXXXX")"
    capture_owned_children "$pid" "$child_snapshot"
    signal_status=0
    signal_app_instance_if_identity_matches "$pid" "$start" TERM || signal_status="$?"
    if [ "$signal_status" != "0" ]; then
      # The exact process disappeared between discovery and the signal. That is
      # already a completed root reclaim; never follow a reused PID. The child
      # snapshot still belongs to the exact app identity observed above.
      if [ "$signal_status" != "2" ]; then
        rm -f "$child_snapshot"
        echo "Could not stop unowned PID $pid." >&2
        return 70
      fi
    fi
    if [ "$signal_status" = "0" ] && ! await_owned_exit "$pid" "$start"; then
      rm -f "$child_snapshot"
      echo "Unowned PID $pid survived the stop request." >&2
      return 70
    fi
    if ! terminate_owned_children "$child_snapshot"; then
      echo "An exact child process owned by unowned PID $pid survived cleanup." >&2
      rm -f "$child_snapshot"
      return 70
    fi
    rm -f "$child_snapshot"
  done <<< "$records"
}

resolve_ownerless_instances() {
  if [ "${STRICT_OWNERSHIP:-0}" = "1" ]; then
    refuse_ownerless_instances "$1"
  else
    reclaim_ownerless_instances "$1"
  fi
}

owner_value() {
  plutil -extract "$1" raw -o - "$OWNER_FILE" 2>/dev/null || true
}

owner_process_identity_is_live() {
  [ -f "$OWNER_FILE" ] || return 1
  local pid recorded_start
  pid="$(owner_value pid)"
  recorded_start="$(owner_value processStartIdentity)"
  [[ "$pid" =~ ^[0-9]+$ ]] || return 1
  [ -n "$recorded_start" ] || return 1
  [ "$(process_start_identity "$pid")" = "$recorded_start" ]
}

owner_manifest_matches_instance() {
  [ -f "$OWNER_FILE" ] || return 1
  local recorded_hash
  [ "$(owner_value schemaVersion)" = "1" ] || return 1
  [ "$(owner_value mode)" = "$MODE" ] || return 1
  [ "$(owner_value worktreeID)" = "$WORKTREE_ID" ] || return 1
  [ "$(owner_value worktreePath)" = "$ROOT_DIR" ] || return 1
  [ "$(owner_value bundleIdentifier)" = "$BUNDLE_IDENTIFIER" ] || return 1
  [ "$(owner_value appURL)" = "$APP_DIR" ] || return 1
  [ "$(owner_value executablePath)" = "$APP_EXECUTABLE" ] || return 1
  [ "$(owner_value stateDirectory)" = "$STATE_DIR" ] || return 1
  [ "$(owner_value logDirectory)" = "$LOG_DIR" ] || return 1
  recorded_hash="$(owner_value executableSHA256)"
  [[ "$recorded_hash" =~ ^[[:xdigit:]]{64}$ ]]
}

# The full ownership proof except for the bytes currently staged at the app
# URL. A rebuild can replace those bytes while the already-launched process is
# still the exact process recorded by the manifest; no other mismatch is safe
# to reinterpret as rebuild drift.
owner_process_matches_manifest() {
  owner_manifest_matches_instance || return 1
  local pid recorded_start recorded_executable live_start live_command
  pid="$(owner_value pid)"
  recorded_start="$(owner_value processStartIdentity)"
  recorded_executable="$(owner_value executablePath)"
  [[ "$pid" =~ ^[0-9]+$ ]] || return 1
  kill -0 "$pid" 2>/dev/null || return 1
  live_start="$(process_start_identity "$pid")"
  live_command="$(process_command "$pid")"
  [ -n "$live_start" ] && [ "$live_start" = "$recorded_start" ] || return 1
  [ "$recorded_executable" = "$APP_EXECUTABLE" ] || return 1
  case "$live_command" in
    "$recorded_executable"|"$recorded_executable "*)
      ;;
    *)
      return 1
      ;;
  esac
}

owner_matches_live_process() {
  owner_process_matches_manifest || return 1
  local recorded_executable recorded_hash live_hash
  recorded_executable="$(owner_value executablePath)"
  recorded_hash="$(owner_value executableSHA256)"
  [ -x "$recorded_executable" ] || return 1
  live_hash="$(shasum -a 256 "$recorded_executable" | awk '{print $1}')"
  [ "$live_hash" = "$recorded_hash" ]
}

write_owner_manifest() {
  local pid="$1"
  local process_start="$2"
  local executable_hash owner_temp
  executable_hash="$(shasum -a 256 "$APP_EXECUTABLE" | awk '{print $1}')"
  owner_temp="$OWNER_FILE.tmp.$$"
  plutil -create xml1 "$owner_temp"
  plutil -insert schemaVersion -integer 1 "$owner_temp"
  plutil -insert mode -string "$MODE" "$owner_temp"
  plutil -insert worktreeID -string "$WORKTREE_ID" "$owner_temp"
  plutil -insert worktreePath -string "$ROOT_DIR" "$owner_temp"
  plutil -insert bundleIdentifier -string "$BUNDLE_IDENTIFIER" "$owner_temp"
  plutil -insert appURL -string "$APP_DIR" "$owner_temp"
  plutil -insert executablePath -string "$APP_EXECUTABLE" "$owner_temp"
  plutil -insert executableSHA256 -string "$executable_hash" "$owner_temp"
  plutil -insert pid -integer "$pid" "$owner_temp"
  plutil -insert processStartIdentity -string "$process_start" "$owner_temp"
  plutil -insert stateDirectory -string "$STATE_DIR" "$owner_temp"
  plutil -insert logDirectory -string "$LOG_DIR" "$owner_temp"
  plutil -convert json "$owner_temp"
  mv "$owner_temp" "$OWNER_FILE"
}

remove_expected_owner_manifest() {
  local expected_pid="$1"
  local expected_start="$2"
  [ -f "$OWNER_FILE" ] || return 0
  if [ "$(owner_value pid)" = "$expected_pid" ] &&
     [ "$(owner_value processStartIdentity)" = "$expected_start" ]; then
    rm -f "$OWNER_FILE"
  fi
}

process_identity_is_live() {
  local pid="$1"
  local process_start="$2"
  [ -n "$process_start" ] &&
    [ "$(process_start_identity "$pid")" = "$process_start" ]
}

app_instance_process_is_live() {
  local pid="$1"
  local process_start="$2"
  local command
  process_identity_is_live "$pid" "$process_start" || return 1
  command="$(process_command "$pid")"
  case "$command" in
    "$APP_EXECUTABLE"|"$APP_EXECUTABLE "*)
      return 0
      ;;
  esac
  return 1
}

# Returns 2 when the captured identity is already gone or no longer belongs to
# this app instance. That is success for cleanup, but distinct from a signal
# failure while the exact target remains live.
signal_app_instance_if_identity_matches() {
  local pid="$1"
  local process_start="$2"
  local signal_name="$3"
  app_instance_process_is_live "$pid" "$process_start" || return 2
  kill "-$signal_name" "$pid" 2>/dev/null && return 0
  app_instance_process_is_live "$pid" "$process_start" || return 2
  return 1
}

signal_process_if_identity_matches() {
  local pid="$1"
  local process_start="$2"
  local signal_name="$3"
  process_identity_is_live "$pid" "$process_start" || return 0
  kill "-$signal_name" "$pid" 2>/dev/null && return 0
  process_identity_is_live "$pid" "$process_start" || return 0
  return 1
}

# SIGTERM asks the app for an AppKit quit so it writes saved application state
# (the open Workspace windows). A quit may deliberately wait for a shared CLI
# credential refresh or be cancelled. Preserve the app instead of bypassing
# that protection when the bounded launcher wait expires.
await_owned_exit() {
  local pid="$1"
  local process_start="$2"
  local _
  for _ in {1..100}; do
    if [ "$(process_start_identity "$pid")" != "$process_start" ]; then
      return 0
    fi
    sleep 0.1
  done
  echo "PID $pid is still running. Finish or cancel its Quit dialog before relaunching; the launcher will not force termination." >&2
  return 70
}

current_process_is_descendant_of() {
  local root_pid="$1"
  local descendant_pid
  while IFS= read -r descendant_pid; do
    [ "$descendant_pid" = "$$" ] && return 0
  done < <(descendant_pids "$root_pid")
  return 1
}

stop_relaunch_process_if_requested() {
  local pid="${OTTAM_RELAUNCH_OLD_PID:-}"
  local process_start child_snapshot
  [ -n "$pid" ] || return 0
  [[ "$pid" =~ ^[0-9]+$ ]] || {
    echo "Invalid OTTAM_RELAUNCH_OLD_PID: $pid" >&2
    return 70
  }
  process_start="$(process_start_identity "$pid")"
  [ -n "$process_start" ] || return 0
  current_process_is_descendant_of "$pid" || {
    echo "Refusing relaunch because PID $pid does not own this relaunch process." >&2
    return 70
  }
  child_snapshot="$(mktemp "$INSTANCE_ROOT/relaunch-children.XXXXXX")"
  capture_owned_children "$pid" "$child_snapshot"
  if ! signal_process_if_identity_matches "$pid" "$process_start" TERM; then
    rm -f "$child_snapshot"
    echo "Could not stop relaunch PID $pid." >&2
    return 70
  fi
  if ! await_owned_exit "$pid" "$process_start"; then
    rm -f "$child_snapshot"
    echo "Relaunch PID $pid did not exit after the bounded stop request." >&2
    return 70
  fi
  if ! terminate_owned_children "$child_snapshot"; then
    echo "An exact child process owned by relaunch PID $pid survived cleanup." >&2
    rm -f "$child_snapshot"
    return 70
  fi
  rm -f "$child_snapshot"
  echo "Stopped relaunch PID: $pid"
  return 0
}

remove_foreground_owner_if_exited() {
  local expected_pid="$1"
  local expected_start="$2"
  if [ "$(process_start_identity "$expected_pid")" != "$expected_start" ]; then
    remove_expected_owner_manifest "$expected_pid" "$expected_start"
  fi
}

print_identity() {
  echo "Mode: $MODE"
  echo "Display name: ${DISPLAY_NAME:-Ottam}"
  echo "Worktree path: $ROOT_DIR"
  echo "Worktree identifier: $WORKTREE_ID"
  echo "Bundle identifier: $BUNDLE_IDENTIFIER"
  echo "State directory: $STATE_DIR"
  echo "Log directory: $LOG_DIR"
  echo "App URL: $APP_DIR"
}

status_instance() {
  print_identity
  if owner_matches_live_process; then
    echo "Status: running"
    echo "Owned PID: $(owner_value pid)"
    return 0
  fi
  if [ -f "$OWNER_FILE" ]; then
    if owner_process_identity_is_live; then
      echo "Status: ownership conflict"
    else
      echo "Status: stopped (stale owner manifest)"
    fi
    echo "Owner manifest: $OWNER_FILE"
    return 2
  fi
  if [ -n "$(running_app_records || true)" ]; then
    echo "Status: ownership conflict (exact app is live without a valid owner manifest)"
    return 2
  fi
  echo "Status: stopped"
  return 3
}

stop_instance() {
  if [ ! -f "$OWNER_FILE" ]; then
    print_identity
    echo "Status: already stopped"
    return 0
  fi
  if ! owner_matches_live_process; then
    if ! owner_process_identity_is_live; then
      rm -f "$OWNER_FILE"
      print_identity
      echo "Recovered stale owner manifest."
      echo "Status: already stopped"
      return 0
    fi
    if [ "${STRICT_OWNERSHIP:-0}" = "1" ]; then
      print_identity
      echo "Refusing cleanup because the owner manifest conflicts with a live process." >&2
      echo "Owner manifest: $OWNER_FILE" >&2
      return 70
    fi
    if ! owner_process_matches_manifest; then
      print_identity
      echo "Refusing cleanup because the owner manifest conflicts with a live process." >&2
      echo "Owner manifest: $OWNER_FILE" >&2
      return 70
    fi
    # Every manifest and live-process field matches. Only the staged executable
    # hash changed underneath it, which is what a rebuild does — not a conflict.
    echo "Owner manifest predates the staged executable; stopping the recorded PID."
  fi

  local pid process_start child_snapshot
  pid="$(owner_value pid)"
  process_start="$(owner_value processStartIdentity)"
  child_snapshot="$(mktemp "$INSTANCE_ROOT/owned-children.XXXXXX")"
  capture_owned_children "$pid" "$child_snapshot"
  if ! signal_process_if_identity_matches "$pid" "$process_start" TERM; then
    rm -f "$child_snapshot"
    echo "Could not stop owned PID $pid." >&2
    return 70
  fi
  if ! await_owned_exit "$pid" "$process_start"; then
    rm -f "$child_snapshot"
    echo "Owned PID $pid did not exit after the bounded stop request." >&2
    return 70
  fi
  if ! terminate_owned_children "$child_snapshot"; then
    echo "An exact child process owned by PID $pid survived cleanup." >&2
    rm -f "$child_snapshot"
    return 70
  fi
  rm -f "$child_snapshot"
  rm -f "$OWNER_FILE"
  print_identity
  echo "Stopped owned PID: $pid"
  return 0
}
