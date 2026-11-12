#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
UI_DIR="$ROOT_DIR/ui-testing"
PROJECT="$UI_DIR/OttamUITesting.xcodeproj"
UI_TESTING_CACHE="$ROOT_DIR/.build/ui-testing/runtime"
DERIVED_DATA="$UI_TESTING_CACHE/DerivedData"
PRODUCTS="$DERIVED_DATA/Build/Products/Debug"
STABLE_DRIVER_ROOT="$UI_TESTING_CACHE/driver"
STABLE_DRIVER="$STABLE_DRIVER_ROOT/OttamUITestDriver.app"
STABLE_DRIVER_FINGERPRINT="$STABLE_DRIVER_ROOT/fingerprint.sha256"
UI_TESTING_INPUT_FINGERPRINT="$UI_TESTING_CACHE/inputs.sha256"
# debug (default) or profile: an optimized source app that keeps the DEBUG-gated
# testing hooks, for measuring the app the way it actually ships-fast. Cached
# per configuration so switching does not thrash the other one's build.
SOURCE_APP_CONFIGURATION="${OTTAM_UI_TEST_CONFIGURATION:-debug}"
case "$SOURCE_APP_CONFIGURATION" in
  debug) SOURCE_APP_ROOT="$UI_TESTING_CACHE/source-app" ;;
  profile) SOURCE_APP_ROOT="$UI_TESTING_CACHE/source-app-profile" ;;
  *)
    echo "OTTAM_UI_TEST_CONFIGURATION must be debug or profile" >&2
    exit 64
    ;;
esac
SOURCE_APP="$SOURCE_APP_ROOT/OttamUISource.app"
SOURCE_APP_FINGERPRINT="$SOURCE_APP_ROOT/fingerprint.sha256"
RUNNER="$PRODUCTS/OttamUIScenarioRunner"
COORDINATOR="$PRODUCTS/OttamUITestCoordinator"
UI_TEST_ROOT="$ROOT_DIR/.build/ui-tests"
CURRENT_EVIDENCE="$UI_TEST_ROOT/current"
ARCHIVE_ROOT="$UI_TEST_ROOT/archive"
RUN_LOCK="$UI_TEST_ROOT/.run-lock"
DEVELOPER_DIR_VALUE="/Applications/Xcode.app/Contents/Developer"
DRIVER_TOPOLOGY="${OTTAM_UI_TEST_DRIVER_TOPOLOGY:-launchServices}"
SIGNING_IDENTITY=""
DEVELOPMENT_TEAM=""
RUN_LOCK_TOKEN=""
LAUNCHER_RUN_ID=""
LAUNCHER_STARTED_AT=""
PUBLIC_COMMAND_JSON="[]"
EVIDENCE_FAILURE_GUARD=""
ORIGINAL_ARGUMENTS=("$@")

export DEVELOPER_DIR="$DEVELOPER_DIR_VALUE"

usage() {
  cat <<'USAGE'
Usage:
  scripts/ui-test.sh list
  scripts/ui-test.sh native
  scripts/ui-test.sh run <scenario-id>
  scripts/ui-test.sh archive <descriptive-name>
  scripts/ui-test.sh queue [--json]
  scripts/ui-test.sh preflight
  scripts/ui-test.sh request-permissions

Environment:
  OTTAM_UI_TEST_SIGNING_IDENTITY  Select an Apple Development certificate
  OTTAM_UI_TEST_DEVELOPMENT_TEAM Cross-check the selected certificate team
  OTTAM_UI_TEST_DRIVER_TOPOLOGY  launchServices (local) or direct (hosted)
  OTTAM_UI_TEST_CONFIGURATION    debug (default) or profile (optimized app)
USAGE
}


# shellcheck source=scripts/lib/ui-test-evidence.sh
source "$ROOT_DIR/scripts/lib/ui-test-evidence.sh"
# shellcheck source=scripts/lib/ui-test-source-app.sh
source "$ROOT_DIR/scripts/lib/ui-test-source-app.sh"
environment_unavailable() {
  echo "environment_unavailable: $1" >&2
  echo "Open Xcode to create an Apple Development certificate if needed." >&2
  echo "For driver permissions, enable $STABLE_DRIVER in:" >&2
  echo "  System Settings > Privacy & Security > Accessibility" >&2
  echo "  System Settings > Privacy & Security > Screen & System Audio Recording" >&2
  exit 69
}

require_toolchain() {
  [ -d "$DEVELOPER_DIR_VALUE" ] ||
    environment_unavailable "Xcode is not installed at $DEVELOPER_DIR_VALUE"
  command -v xcodebuild >/dev/null ||
    environment_unavailable "xcodebuild is unavailable"
  command -v xcodegen >/dev/null ||
    environment_unavailable "XcodeGen 2.46.0 is required"
  if [[ "$(xcodegen version)" != *"2.46.0"* ]]; then
    environment_unavailable "XcodeGen 2.46.0 is required"
  fi
}

ensure_package_inputs() {
  "$ROOT_DIR/scripts/fetch-ghosttykit.sh"
}

resolve_signing() {
  local requested_identity="${OTTAM_UI_TEST_SIGNING_IDENTITY:-}"
  local requested_team="${OTTAM_UI_TEST_DEVELOPMENT_TEAM:-}"
  local identities identity_count common_name certificate_subject discovered_team
  identities="$(
    security find-identity -v -p codesigning |
      sed -nE 's/^[[:space:]]*[0-9]+\) ([0-9A-F]{40}) "(Apple Development:[^"]+)"$/\1|\2/p'
  )"
  if [ -n "$requested_identity" ]; then
    identities="$(
      awk -F '|' -v requested="$requested_identity" '
        $1 == requested ||
        $2 == requested ||
        (requested == "Apple Development" && $2 ~ /^Apple Development:/)
      ' <<< "$identities"
    )"
  fi
  identity_count="$(sed '/^[[:space:]]*$/d' <<< "$identities" | wc -l | tr -d ' ')"
  [ "$identity_count" = "1" ] ||
    environment_unavailable \
      "Select exactly one Apple Development identity with OTTAM_UI_TEST_SIGNING_IDENTITY"
  SIGNING_IDENTITY="${identities%%|*}"
  common_name="${identities#*|}"
  certificate_subject="$(
    security find-certificate -c "$common_name" -p |
      openssl x509 -noout -subject -nameopt RFC2253
  )"
  discovered_team="$(sed -nE 's/.*OU=([^,]+).*/\1/p' <<< "$certificate_subject")"
  [ -n "$discovered_team" ] ||
    environment_unavailable "Could not derive a team from the certificate OU"
  if [ -n "$requested_team" ] && [ "$requested_team" != "$discovered_team" ]; then
    environment_unavailable \
      "OTTAM_UI_TEST_DEVELOPMENT_TEAM does not match the selected certificate"
  fi
  DEVELOPMENT_TEAM="$discovered_team"
}

build_tools() {
  require_toolchain
  ensure_package_inputs
  mkdir -p "$UI_TESTING_CACHE"
  xcodebuild \
    -project "$PROJECT" \
    -scheme OttamUITestTools \
    -configuration Debug \
    -derivedDataPath "$DERIVED_DATA" \
    -quiet \
    CODE_SIGNING_ALLOWED=NO \
    build
}

fingerprint_tree() {
  local root
  root="$1"
  shift
  while [ "$#" -gt 0 ]; do
    if [ -d "$1" ]; then
      find "$1" -type f -print
    elif [ -f "$1" ]; then
      printf '%s\n' "$1"
    fi
    shift
  done |
    LC_ALL=C sort |
    while IFS= read -r file; do
      printf '%s  %s\n' \
        "$(shasum -a 256 "$file" | awk '{print $1}')" \
        "${file#"$root"/}"
    done |
    shasum -a 256 |
    awk '{print $1}'
}

fingerprint_metadata() {
  while [ "$#" -gt 0 ]; do
    if [ -d "$1" ]; then
      find "$1" -type f -print0
    elif [ -f "$1" ]; then
      printf '%s\0' "$1"
    fi
    shift
  done |
    xargs -0 stat -f '%N|%z|%Fm|%Fc' |
    LC_ALL=C sort |
    shasum -a 256 |
    awk '{print $1}'
}

build_ui_testing() {
  local input_fingerprint cached_input_fingerprint
  local product_driver product_fingerprint cached_fingerprint
  require_toolchain
  ensure_package_inputs
  mkdir -p "$UI_TESTING_CACHE" "$STABLE_DRIVER_ROOT"
  if [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then
    SIGNING_IDENTITY="-"
    DEVELOPMENT_TEAM=""
    DRIVER_TOPOLOGY="direct"
  else
    resolve_signing
  fi
  input_fingerprint="$(
    {
      fingerprint_tree "$ROOT_DIR" \
        "$ROOT_DIR/ui-testing/project.yml" \
        "$ROOT_DIR/ui-testing/OttamUITesting.xcodeproj/project.pbxproj" \
        "$ROOT_DIR/ui-testing/Configuration" \
        "$ROOT_DIR/ui-testing/Sources" \
        "$ROOT_DIR/ui-testing/Supporting" \
        "$ROOT_DIR/app/Package.swift" \
        "$ROOT_DIR/app/Package.resolved" \
        "$ROOT_DIR/app/Sources/OttamAccessibility" \
        "$ROOT_DIR/app/Sources/OttamTranscriptFixtures"
      xcodebuild -version
      printf '%s\n%s\n' "$SIGNING_IDENTITY" "$DEVELOPMENT_TEAM"
    } | shasum -a 256 | awk '{print $1}'
  )"
  cached_input_fingerprint="$(cat "$UI_TESTING_INPUT_FINGERPRINT" 2>/dev/null || true)"
  if [ "$input_fingerprint" = "$cached_input_fingerprint" ] &&
     [ -x "$RUNNER" ] &&
     [ -x "$COORDINATOR" ] &&
     [ -d "$STABLE_DRIVER" ] &&
     codesign --verify --deep --strict "$STABLE_DRIVER" >/dev/null 2>&1; then
    echo "Reusing cached UI test tools and driver: $UI_TESTING_CACHE"
    return
  fi
  xcodebuild \
    -project "$PROJECT" \
    -scheme OttamUITesting \
    -configuration Debug \
    -derivedDataPath "$DERIVED_DATA" \
    -quiet \
    "OTTAM_UI_TEST_SIGNING_IDENTITY=$SIGNING_IDENTITY" \
    "OTTAM_UI_TEST_DEVELOPMENT_TEAM=$DEVELOPMENT_TEAM" \
    build
  product_driver="$PRODUCTS/OttamUITestDriver.app"
  product_fingerprint="$(
    {
      fingerprint_tree "$product_driver" \
        "$product_driver/Contents/Info.plist" \
        "$product_driver/Contents/MacOS"
      printf '%s\n' "$SIGNING_IDENTITY"
    } | shasum -a 256 | awk '{print $1}'
  )"
  cached_fingerprint="$(cat "$STABLE_DRIVER_FINGERPRINT" 2>/dev/null || true)"
  if [ "$product_fingerprint" = "$cached_fingerprint" ] &&
     [ -d "$STABLE_DRIVER" ] &&
     codesign --verify --deep --strict "$STABLE_DRIVER" >/dev/null 2>&1; then
    echo "Reusing cached UI test driver: $STABLE_DRIVER"
    printf '%s\n' "$input_fingerprint" > "$UI_TESTING_INPUT_FINGERPRINT"
    return
  fi
  rm -rf "$STABLE_DRIVER"
  ditto "$product_driver" "$STABLE_DRIVER"
  codesign --force --deep --sign "$SIGNING_IDENTITY" "$STABLE_DRIVER" >/dev/null
  codesign --verify --deep --strict "$STABLE_DRIVER"
  printf '%s\n' "$product_fingerprint" > "$STABLE_DRIVER_FINGERPRINT"
  printf '%s\n' "$input_fingerprint" > "$UI_TESTING_INPUT_FINGERPRINT"
}

build_isolated_source_app() {
  local source_fingerprint cached_fingerprint
  source_fingerprint="$(
    {
      fingerprint_tree "$ROOT_DIR" \
        "$ROOT_DIR/app/Package.swift" \
        "$ROOT_DIR/app/Package.resolved" \
        "$ROOT_DIR/app/Sources" \
        "$ROOT_DIR/app/Resources" \
        "$ROOT_DIR/backend/Cargo.toml" \
        "$ROOT_DIR/backend/Cargo.lock" \
        "$ROOT_DIR/backend/crates" \
        "$ROOT_DIR/scripts/build-app.sh" \
        "$ROOT_DIR/THIRD_PARTY_NOTICES.md"
      fingerprint_metadata \
        "$ROOT_DIR/app/Vendor/ottam-ghostty/.artifact_tag"
      swift --version 2>&1
      rustc --version
      printf '%s\n' "$SOURCE_APP_CONFIGURATION"
    } | shasum -a 256 | awk '{print $1}'
  )"
  cached_fingerprint="$(cat "$SOURCE_APP_FINGERPRINT" 2>/dev/null || true)"
  if [ "$source_fingerprint" = "$cached_fingerprint" ] &&
     [ -d "$SOURCE_APP" ] &&
     codesign --verify --deep --strict "$SOURCE_APP" >/dev/null 2>&1; then
    echo "Reusing cached UI test source app: $SOURCE_APP"
    return
  fi
  local build_app_arguments=()
  if [ "$SOURCE_APP_CONFIGURATION" = "profile" ]; then
    build_app_arguments=(--profile)
  fi
  # The fixture adapter always stays debug: the scenario runtime resolves it at
  # backend/target/debug, and its own speed is not what any scenario measures.
  cargo build \
    --manifest-path "$ROOT_DIR/backend/Cargo.toml" \
    -p ottam-test-acp
  mkdir -p "$SOURCE_APP_ROOT"
  "$ROOT_DIR/scripts/build-app.sh" \
    --output-root "$SOURCE_APP_ROOT" \
    --app-name OttamUISource \
    --bundle-id dev.ottam.Ottam.ui-test-source \
    --executable-name OttamUISource \
    "${build_app_arguments[@]+"${build_app_arguments[@]}"}"
  printf '%s\n' "$source_fingerprint" > "$SOURCE_APP_FINGERPRINT"
}

run_native_tests() {
  build_tools
  xcodebuild \
    -project "$PROJECT" \
    -scheme OttamUINativeTests \
    -configuration Debug \
    -derivedDataPath "$DERIVED_DATA" \
    -destination 'platform=macOS' \
    -quiet \
    test
}

preflight_driver() {
  "$RUNNER" preflight \
    --root "$ROOT_DIR" \
    --driver-app "$STABLE_DRIVER" \
    --driver-topology "$DRIVER_TOPOLOGY"
}

run_scenario_runner() {
  "$RUNNER" "$@" \
    --root "$ROOT_DIR" \
    --driver-app "$STABLE_DRIVER" \
    --source-app "$SOURCE_APP" \
    --coordinator "$COORDINATOR" \
    --driver-topology "$DRIVER_TOPOLOGY"
}

run_registry_command() {
  local scenario_id="$1"
  shift
  acquire_run_lock
  prepare_current_evidence
  EVIDENCE_FAILURE_GUARD="1"
  set_public_command
  initialize_launcher_evidence
  run_evidence_stage "$scenario_id" build_failed tool-build build-ui-testing.log \
    build_ui_testing || return "$?"
  run_evidence_stage "$scenario_id" environment_unavailable preflight preflight.log \
    preflight_driver || return "$?"
  select_source_app "$scenario_id" "${SOURCE_APP_OVERRIDE:-}" || return "$?"
  run_scenario_runner "$@"
}

command="${1:-}"
case "$command" in
  list)
    shift
    [ "$#" = "0" ] || { usage >&2; exit 64; }
    build_tools
    "$RUNNER" list --root "$ROOT_DIR"
    ;;
  native)
    shift
    [ "$#" = "0" ] || { usage >&2; exit 64; }
    run_native_tests
    ;;
  smoke)
    shift
    SOURCE_APP_OVERRIDE=""
    if [ "${1:-}" = "--source-app" ]; then
      [ -n "${2:-}" ] || { usage >&2; exit 64; }
      SOURCE_APP_OVERRIDE="$2"
      shift 2
    fi
    [ "$#" = "0" ] || { usage >&2; exit 64; }
    run_registry_command smoke smoke
    ;;
  run)
    scenario_id="${2:-}"
    [ -n "$scenario_id" ] || { usage >&2; exit 64; }
    shift 2
    [ "$#" = "0" ] || { usage >&2; exit 64; }
    run_registry_command "$scenario_id" run "$scenario_id"
    ;;
  queue)
    shift
    build_tools
    if [ "${1:-}" = "--json" ]; then
      shift
      [ "$#" = "0" ] || { usage >&2; exit 64; }
      "$COORDINATOR" status --json
    else
      [ "$#" = "0" ] || { usage >&2; exit 64; }
      "$COORDINATOR" status
    fi
    ;;
  preflight|request-permissions)
    driver_command="$command"
    shift
    [ "$#" = "0" ] || { usage >&2; exit 64; }
    build_ui_testing
    "$RUNNER" "$driver_command" \
      --root "$ROOT_DIR" \
      --driver-app "$STABLE_DRIVER" \
      --driver-topology "$DRIVER_TOPOLOGY"
    ;;
  archive)
    archive_name="${2:-}"
    [ -n "$archive_name" ] || { usage >&2; exit 64; }
    shift 2
    [ "$#" = "0" ] || { usage >&2; exit 64; }
    acquire_run_lock
    archive_current_evidence "$archive_name"
    ;;
  -h|--help|"")
    usage
    ;;
  *)
    echo "Unknown UI-test command: $command" >&2
    usage >&2
    exit 64
    ;;
esac
