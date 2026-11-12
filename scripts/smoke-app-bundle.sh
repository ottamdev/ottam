#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CURRENT_EVIDENCE="$ROOT_DIR/.build/ui-tests/current"

usage() {
  cat <<'USAGE'
Usage: scripts/smoke-app-bundle.sh [--release]

Builds the development bundle, checks its layout and signature, and drives the
UI smoke run.

  --release  Instead build a --release bundle (.build/app-release) and a
             --profile control (.build/app-profile), check both layouts, and
             prove the release executables carry no test seams. No UI run:
             a release bundle has no UI-test hooks to drive.
USAGE
}

RELEASE=0
case "${1:-}" in
  "") ;;
  --release) RELEASE=1 ;;
  -h|--help) usage; exit 0 ;;
  *) usage >&2; exit 64 ;;
esac

check_bundle_layout() {
  local app_dir="$1"
  local contents_dir="$app_dir/Contents"
  local resources_dir="$contents_dir/Resources"
  test -x "$contents_dir/MacOS/Ottam"
  test -x "$contents_dir/MacOS/ottam-core"
  test -f "$contents_dir/Info.plist"
  test -f "$resources_dir/Ottam.icns"
  # CFBundleIconName resolves here; without it macOS redraws the .icns flat.
  test -f "$resources_dir/Assets.car"
  test -f "$resources_dir/THIRD_PARTY_NOTICES.md"
  cmp -s "$ROOT_DIR/LICENSE" "$resources_dir/LICENSE"
  /usr/libexec/PlistBuddy -c "Print :NSHumanReadableCopyright" "$contents_dir/Info.plist" >/dev/null
  # ATSApplicationFontsPath points here; without these the whole UI silently
  # falls back to the system font.
  test -f "$resources_dir/Fonts/HackNerdFontMono-Regular.ttf"
  test -f "$resources_dir/Fonts/HackNerdFontMono-Bold.ttf"
  test -f "$resources_dir/Fonts/HackNerdFontMono-Italic.ttf"
  test -f "$resources_dir/Fonts/HackNerdFontMono-BoldItalic.ttf"
  test -d "$resources_dir/ghostty"
  test -d "$resources_dir/ghostty/themes"
  test -f "$resources_dir/terminfo/78/xterm-ghostty"
  # Ghostty's shell-integration scripts are GPL and unused (shell-integration = none).
  test ! -e "$resources_dir/shell-integration"
  test ! -e "$resources_dir/ghostty/shell-integration"

  codesign --verify --deep --strict --verbose=2 "$app_dir"
}

if [ "$RELEASE" = 1 ]; then
  "$ROOT_DIR/scripts/build-app.sh" --release --output-root "$ROOT_DIR/.build/app-release"
  "$ROOT_DIR/scripts/build-app.sh" --profile --output-root "$ROOT_DIR/.build/app-profile"
  check_bundle_layout "$ROOT_DIR/.build/app-release/Ottam.app"
  check_bundle_layout "$ROOT_DIR/.build/app-profile/Ottam.app"
  "$ROOT_DIR/scripts/check-release-seams.sh" "$ROOT_DIR/.build/app-release/Ottam.app"
  "$ROOT_DIR/scripts/check-release-seams.sh" --expect-present "$ROOT_DIR/.build/app-profile/Ottam.app"
  echo "Release smoke check passed for $ROOT_DIR/.build/app-release/Ottam.app"
  exit 0
fi

APP_DIR="$ROOT_DIR/.build/app/Ottam.app"

# shellcheck source=scripts/lib/ui-test-evidence.sh
source "$ROOT_DIR/scripts/lib/ui-test-evidence.sh"

# The bundle checks below run before ui-test.sh gets a chance to reset the
# evidence directory, so a failure here would otherwise leave the previous
# run's screenshots looking like this run's result.
on_smoke_exit() {
  local status="$?"
  [ "$status" -eq 0 ] ||
    mark_evidence_stale "scripts/smoke-app-bundle.sh exited with status $status"
  return "$status"
}
trap on_smoke_exit EXIT

"$ROOT_DIR/scripts/build-app.sh"
check_bundle_layout "$APP_DIR"

"$ROOT_DIR/scripts/ui-test.sh" smoke --source-app "$APP_DIR"

echo "Smoke check passed for $APP_DIR"
