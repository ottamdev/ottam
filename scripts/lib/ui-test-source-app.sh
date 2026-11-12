# Source-app selection for scripts/ui-test.sh.
#
# `run_evidence_stage` runs each stage in a subshell so its output can be
# captured and classified. A stage therefore cannot publish a value to the
# launcher by assigning a shell variable: the assignment dies with the
# subshell. The selection stage writes the resolved bundle path to a file and
# the launcher reads it back, so an explicit `--source-app` override actually
# reaches the scenario runner instead of silently falling back to the cached
# `.build/ui-testing/runtime/source-app` bundle.

source_app_executable_name() {
  /usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' \
    "$1/Contents/Info.plist" 2>/dev/null || true
}

# Validates an overriding source app and prints its absolute path.
verify_source_app() {
  local candidate="$1"
  local executable_name
  [ -d "$candidate" ] ||
    { echo "UI test source app does not exist: $candidate" >&2; return 66; }
  candidate="$(cd "$(dirname "$candidate")" && pwd -P)/$(basename "$candidate")"
  executable_name="$(source_app_executable_name "$candidate")"
  [ -n "$executable_name" ] &&
    [ -x "$candidate/Contents/MacOS/$executable_name" ] ||
    { echo "UI test source app has no executable: $candidate" >&2; return 65; }
  # This function's stdout carries the resolved path, so keep every
  # diagnostic on stderr where the stage log still captures it.
  codesign --verify --deep --strict "$candidate" >&2 || return "$?"
  printf '%s\n' "$candidate"
}

# Evidence stage body. Runs in a subshell, so it publishes through a file.
use_source_app() {
  local candidate="$1"
  local selection_file="$2"
  local resolved
  resolved="$(verify_source_app "$candidate")" || return "$?"
  printf '%s\n' "$resolved" > "$selection_file"
  echo "Using overridden UI test source app: $resolved"
}

# Launcher-side counterpart: reads back what the selection stage published.
read_selected_source_app() {
  local selection_file="$1"
  local selected
  selected="$(cat "$selection_file" 2>/dev/null || true)"
  [ -n "$selected" ] && [ -d "$selected" ] ||
    { echo "UI test source app selection was not published: $selection_file" >&2; return 70; }
  SOURCE_APP="$selected"
}

# Sets SOURCE_APP to the bundle the scenario runner must stage and launch:
# the caller's `--source-app` override when given, otherwise the cached
# isolated source app built by the launcher.
select_source_app() {
  local scenario_id="$1"
  local override="$2"
  local selection_file
  if [ -z "$override" ]; then
    run_evidence_stage "$scenario_id" build_failed source-app-build \
      build-source-app.log build_isolated_source_app || return "$?"
    return 0
  fi
  selection_file="$CURRENT_EVIDENCE/logs/source-app.path"
  rm -f "$selection_file"
  run_evidence_stage "$scenario_id" build_failed source-app-selection \
    select-source-app.log use_source_app "$override" "$selection_file" || return "$?"
  read_selected_source_app "$selection_file"
}
