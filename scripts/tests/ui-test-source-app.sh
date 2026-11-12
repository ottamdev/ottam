#!/usr/bin/env bash
set -euo pipefail

# Contract for the `scripts/ui-test.sh --source-app` override.
#
# The override used to be applied by a function that assigned SOURCE_APP while
# running inside `run_evidence_stage`'s subshell, so the assignment never
# reached the launcher and the scenario runner kept staging the cached
# `.build/ui-testing/runtime/source-app` bundle. A run that overrides the
# source app must stage and launch the overridden bundle.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/ottam-ui-source-app-test.XXXXXX")"
test_root="$(cd "$test_root" && pwd -P)"
trap 'rm -rf "$test_root"' EXIT

fail() {
  echo "$1" >&2
  exit 1
}

make_app() {
  local bundle="$1" executable_name="$2" marker="$3"
  mkdir -p "$bundle/Contents/MacOS"
  cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>$executable_name</string>
</dict>
</plist>
PLIST
  printf '#!/bin/sh\necho %s\n' "$marker" > "$bundle/Contents/MacOS/$executable_name"
  chmod +x "$bundle/Contents/MacOS/$executable_name"
}

CACHED_APP="$test_root/runtime/source-app/OttamUISource.app"
OVERRIDE_APP="$test_root/app/Ottam.app"
make_app "$CACHED_APP" OttamUISource cached
make_app "$OVERRIDE_APP" Ottam override

CURRENT_EVIDENCE="$test_root/ui-tests/current"
mkdir -p "$CURRENT_EVIDENCE/logs"

# shellcheck source=scripts/lib/ui-test-evidence.sh
source "$repo_root/scripts/lib/ui-test-evidence.sh"
# shellcheck source=scripts/lib/ui-test-source-app.sh
source "$repo_root/scripts/lib/ui-test-source-app.sh"

# The fixtures are unsigned, and PlistBuddy only exists on macOS.
codesign() {
  return 0
}
if [ ! -x /usr/libexec/PlistBuddy ]; then
  source_app_executable_name() {
    sed -n 's:.*<key>CFBundleExecutable</key><string>\(.*\)</string>.*:\1:p' \
      "$1/Contents/Info.plist"
  }
fi

launcher_failures=""
write_launcher_failure() {
  launcher_failures="${launcher_failures}${launcher_failures:+,}$2:$3"
}

# Evidence stages run in a subshell, so the stub records itself on disk.
source_app_build_marker="$test_root/source-app-build.count"
build_isolated_source_app() {
  printf 'x' >> "$source_app_build_marker"
  echo "built cached source app"
}
source_app_builds() {
  if [ -f "$source_app_build_marker" ]; then
    wc -c < "$source_app_build_marker" | tr -d ' '
  else
    printf '0'
  fi
}

# The two bundles must genuinely differ, otherwise the assertions below cannot
# distinguish the overridden bundle from the cached one.
cached_executable="$CACHED_APP/Contents/MacOS/OttamUISource"
override_executable="$OVERRIDE_APP/Contents/MacOS/Ottam"
! cmp -s "$cached_executable" "$override_executable" ||
  fail "fixture bundles share an executable; the test cannot detect the bug"

# An overridden run stages and launches the overridden bundle.
SOURCE_APP="$CACHED_APP"
select_source_app smoke "$OVERRIDE_APP"
[ "$SOURCE_APP" = "$OVERRIDE_APP" ] ||
  fail "override was not applied: expected $OVERRIDE_APP, got $SOURCE_APP"
[ "$(source_app_builds)" = "0" ] ||
  fail "an overridden run rebuilt the cached source app"
selected_executable="$SOURCE_APP/Contents/MacOS/$(source_app_executable_name "$SOURCE_APP")"
cmp -s "$selected_executable" "$override_executable" ||
  fail "selected bundle does not carry the overriding executable"
[ "$launcher_failures" = "" ] ||
  fail "a successful selection recorded a launcher failure: $launcher_failures"

# codesign diagnostics must not leak into the resolved path.
codesign() {
  echo "$OVERRIDE_APP: valid on disk"
  return 0
}
SOURCE_APP="$CACHED_APP"
select_source_app smoke "$OVERRIDE_APP"
[ "$SOURCE_APP" = "$OVERRIDE_APP" ] ||
  fail "codesign output corrupted the resolved path: got $SOURCE_APP"
codesign() {
  return 0
}

# A relative override resolves to an absolute path the runner can stage.
SOURCE_APP="$CACHED_APP"
(
  cd "$(dirname "$OVERRIDE_APP")"
  select_source_app smoke "./$(basename "$OVERRIDE_APP")"
  [ "$SOURCE_APP" = "$OVERRIDE_APP" ] ||
    fail "relative override did not resolve: got $SOURCE_APP"
)

# The launcher passes exactly what selection resolved to the scenario runner.
grep -q -- '--source-app "\$SOURCE_APP"' "$repo_root/scripts/ui-test.sh" ||
  fail "ui-test.sh no longer passes SOURCE_APP to the scenario runner"

# Without an override the launcher builds and keeps the cached source app.
SOURCE_APP="$CACHED_APP"
select_source_app smoke ""
[ "$SOURCE_APP" = "$CACHED_APP" ] ||
  fail "the cached source app was not used without an override"
[ "$(source_app_builds)" = "1" ] ||
  fail "the cached source app was not built without an override"

# A missing or unusable override fails the run instead of falling back.
SOURCE_APP="$CACHED_APP"
status=0
select_source_app smoke "$test_root/absent.app" || status="$?"
[ "$status" = "66" ] || fail "a missing override exited with $status, expected 66"
[ "$SOURCE_APP" = "$CACHED_APP" ] ||
  fail "a missing override mutated the source app selection"

mkdir -p "$test_root/empty.app/Contents/MacOS"
cp "$OVERRIDE_APP/Contents/Info.plist" "$test_root/empty.app/Contents/Info.plist"
status=0
select_source_app smoke "$test_root/empty.app" || status="$?"
[ "$status" = "65" ] || fail "an override without an executable exited with $status, expected 65"
[ "$launcher_failures" = "build_failed:source-app-selection,build_failed:source-app-selection" ] ||
  fail "selection failures were not classified: $launcher_failures"

# A failed run marks its evidence so stale screenshots cannot look fresh.
mark_evidence_stale "test failure"
[ -f "$CURRENT_EVIDENCE/STALE.txt" ] ||
  fail "a failed run left its evidence unmarked"
grep -q "test failure" "$CURRENT_EVIDENCE/STALE.txt" ||
  fail "the stale marker does not record the failure reason"

# A wrapper script's exit trap must not overwrite the specific inner reason.
mark_evidence_stale "outer wrapper failure"
grep -q "test failure" "$CURRENT_EVIDENCE/STALE.txt" ||
  fail "a later marker overwrote the first failure reason"

RUN_LOCK_TOKEN=""

finalize_after() {
  local guard="$1" exit_status="$2"
  EVIDENCE_FAILURE_GUARD="$guard"
  rm -f "$CURRENT_EVIDENCE/STALE.txt"
  set +e
  ( exit "$exit_status" )
  finalize_run
  local observed="$?"
  set -e
  [ "$observed" = "$exit_status" ] ||
    fail "finalize_run changed the exit status from $exit_status to $observed"
}

finalize_after 1 3
[ -f "$CURRENT_EVIDENCE/STALE.txt" ] ||
  fail "a failing run did not mark its evidence"

finalize_after "" 3
[ ! -f "$CURRENT_EVIDENCE/STALE.txt" ] ||
  fail "a failure outside a scenario run marked evidence stale"

finalize_after 1 0
[ ! -f "$CURRENT_EVIDENCE/STALE.txt" ] ||
  fail "a passing run marked its own evidence stale"

echo "ui-test source app selection contract passed"
