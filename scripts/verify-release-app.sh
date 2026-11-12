#!/usr/bin/env bash
set -euo pipefail

# Checks a release-signed bundle. Runs against the signed build and again
# against the extracted archive, because that is what users receive.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/release-signing.sh
source "$ROOT_DIR/scripts/lib/release-signing.sh"

ENTITLEMENTS="$ROOT_DIR/app/Signing/Ottam.entitlements"
APP_DIR=""
VERSION=""
AUTHORITY="Developer ID Application"
CHECK_ENTITLEMENTS=1
DISTRIBUTION=0

usage() {
  cat <<'USAGE'
Usage: scripts/verify-release-app.sh <app> --version <YY.M.N> [options]

Options:
  --authority <kind>   Required certificate kind (default: Developer ID Application)
  --no-entitlements    Expect the app to carry no entitlements
  --distribution       Also require Gatekeeper acceptance and a stapled ticket
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      VERSION="${2:?--version requires a version}"
      shift 2
      ;;
    --authority)
      AUTHORITY="${2:?--authority requires a certificate kind}"
      shift 2
      ;;
    --no-entitlements)
      CHECK_ENTITLEMENTS=0
      shift
      ;;
    --distribution)
      DISTRIBUTION=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    -*)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 64
      ;;
    *)
      APP_DIR="$1"
      shift
      ;;
  esac
done

if [ -z "$APP_DIR" ] || [ -z "$VERSION" ]; then
  usage >&2
  exit 64
fi

fail() {
  echo "Release verification failed: $*" >&2
  exit 1
}

normalized_plist() {
  plutil -convert xml1 -o - -
}

[ -f "$APP_DIR/Contents/Info.plist" ] || fail "no Info.plist in $APP_DIR"

plist_value() {
  /usr/libexec/PlistBuddy -c "Print :$1" "$APP_DIR/Contents/Info.plist"
}

MAIN_EXECUTABLE="Contents/MacOS/$(plist_value CFBundleExecutable)"
NESTED_CODE="$(release_nested_code)"

# Anything Mach-O must be on the signing list, so a new binary can't ship
# signed only by accident.
expected_code="$(printf '%s\n%s\n' "$MAIN_EXECUTABLE" "$NESTED_CODE" | LC_ALL=C sort)"
actual_code="$(
  cd "$APP_DIR"
  find Contents -type f -print0 |
    xargs -0 file -F '|' |
    sed -nE 's/^(.*)\|[[:space:]]*Mach-O.*$/\1/p' |
    LC_ALL=C sort
)"
if [ "$expected_code" != "$actual_code" ]; then
  fail "bundle's Mach-O files don't match the signing list
expected:
$expected_code
found:
$actual_code"
fi

[ "$(plist_value CFBundleShortVersionString)" = "$VERSION" ] ||
  fail "CFBundleShortVersionString is not $VERSION"
[ "$(plist_value CFBundleVersion)" = "$VERSION" ] ||
  fail "CFBundleVersion is not $VERSION"

verify_signature() {
  local path="$1" details
  codesign --verify --strict "$path" || fail "invalid signature: $path"
  details="$(codesign -d --verbose=4 "$path" 2>&1)"
  grep -Eq 'flags=0x[0-9a-f]+\(.*runtime.*\)' <<< "$details" || fail "no hardened runtime: $path"
  grep -q '^Timestamp=' <<< "$details" || fail "no secure timestamp: $path"
  grep -m1 '^Authority=' <<< "$details" | grep -q "^Authority=$AUTHORITY:" ||
    fail "not signed by $AUTHORITY: $path"
}

while read -r relative_path; do
  verify_signature "$APP_DIR/$relative_path"
  if [ -n "$(codesign -d --entitlements - --xml "$APP_DIR/$relative_path" 2>/dev/null)" ]; then
    fail "unexpected entitlements on $relative_path"
  fi
done <<< "$NESTED_CODE"

verify_signature "$APP_DIR"
codesign --verify --deep --strict "$APP_DIR" || fail "bundle does not verify"

actual_entitlements="$(codesign -d --entitlements - --xml "$APP_DIR" 2>/dev/null)"
if grep -q 'get-task-allow' <<< "$actual_entitlements"; then
  fail "app carries get-task-allow"
fi
if [ "$CHECK_ENTITLEMENTS" = 1 ]; then
  [ "$(normalized_plist <<< "$actual_entitlements")" = "$(normalized_plist < "$ENTITLEMENTS")" ] ||
    fail "app entitlements don't match $ENTITLEMENTS"
elif [ -n "$actual_entitlements" ]; then
  fail "app carries entitlements but none were expected"
fi

for relative_path in "$MAIN_EXECUTABLE" $NESTED_CODE; do
  leaks="$(strings -a "$APP_DIR/$relative_path" | grep -F -- "$HOME" || true)"
  [ -z "$leaks" ] || fail "$relative_path embeds the build machine's home path:
$(head -3 <<< "$leaks")"
done

"$ROOT_DIR/scripts/check-release-seams.sh" "$APP_DIR" ||
  fail "test seams or development overrides shipped in $APP_DIR"

if [ "$DISTRIBUTION" = 1 ]; then
  spctl --assess --type execute "$APP_DIR" || fail "Gatekeeper rejected the app"
  xcrun stapler validate "$APP_DIR" >/dev/null || fail "no valid stapled ticket"
fi

echo "Verified $APP_DIR ($VERSION, $AUTHORITY)"
