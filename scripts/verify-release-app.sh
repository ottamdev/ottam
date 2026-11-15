#!/usr/bin/env bash
set -euo pipefail

# Checks a release-signed bundle. Runs against the signed build and again
# against the disk image, mounted, because that is what users receive.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/release-signing.sh
source "$ROOT_DIR/scripts/lib/release-signing.sh"
# shellcheck source=scripts/lib/release-disk-image.sh
source "$ROOT_DIR/scripts/lib/release-disk-image.sh"

ENTITLEMENTS="$ROOT_DIR/app/Signing/Ottam.entitlements"
APP_DIR=""
VERSION=""
AUTHORITY="Developer ID Application"
CHECK_ENTITLEMENTS=1
DISTRIBUTION=0
FEED_URL=""
UPDATE_PUBLIC_KEY_FILE="$ROOT_DIR/app/Signing/sparkle-public-key"

usage() {
  cat <<'USAGE'
Usage: scripts/verify-release-app.sh <app|dmg> --version <YY.M.N> [options]

A .dmg is checked as an image (signature, layout) and its Ottam.app as
mounted from it.

Options:
  --authority <kind>   Required certificate kind (default: Developer ID Application)
  --no-entitlements    Expect the app to carry no entitlements
  --distribution       Also require Gatekeeper acceptance and a stapled ticket
                       (on the image, when given one)
  --feed-url <url>     Require this Sparkle feed, the repository's public key,
                       and signed-feed enforcement; without it, require no feed
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
    --feed-url)
      FEED_URL="${2:?--feed-url requires a URL}"
      shift 2
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

# Requires a timestamped signature by $AUTHORITY, given codesign's details.
verify_signer() {
  local path="$1" details="$2"
  grep -m1 '^Authority=' <<< "$details" | grep -q "^Authority=$AUTHORITY:" ||
    fail "not signed by $AUTHORITY: $path"
  grep -q '^Timestamp=' <<< "$details" || fail "no secure timestamp: $path"
}

DISK_IMAGE=""
if [[ "$APP_DIR" == *.dmg ]]; then
  DISK_IMAGE="$APP_DIR"
  [ -f "$DISK_IMAGE" ] || fail "no such disk image: $DISK_IMAGE"
  codesign --verify --strict "$DISK_IMAGE" || fail "invalid signature: $DISK_IMAGE"
  verify_signer "$DISK_IMAGE" "$(codesign -d --verbose=4 "$DISK_IMAGE" 2>&1)"
  if [ "$DISTRIBUTION" = 1 ]; then
    xcrun stapler validate "$DISK_IMAGE" >/dev/null || fail "no valid stapled ticket on the disk image"
    spctl --assess --type open --context context:primary-signature "$DISK_IMAGE" ||
      fail "Gatekeeper rejected the disk image"
  fi

  mount_dir="$(mktemp -d "${TMPDIR:-/tmp}/ottam-verify.XXXXXX")"
  trap 'release_detach_disk_image "$mount_dir/volume"; rm -rf "$mount_dir"' EXIT
  release_attach_disk_image "$DISK_IMAGE" "$mount_dir/volume" || fail "couldn't mount $DISK_IMAGE"
  image_problem="$(release_check_disk_image "$mount_dir/volume" "$ROOT_DIR")" || fail "$image_problem"
  APP_DIR="$mount_dir/volume/Ottam.app"
fi

[ -f "$APP_DIR/Contents/Info.plist" ] || fail "no Info.plist in $APP_DIR"

plist_value() {
  /usr/libexec/PlistBuddy -c "Print :$1" "$APP_DIR/Contents/Info.plist"
}

MAIN_EXECUTABLE="Contents/MacOS/$(plist_value CFBundleExecutable)"
NESTED_CODE="$(release_nested_code)"

while read -r relative_path; do
  [ -e "$APP_DIR/$relative_path" ] || fail "signing list entry missing from the bundle: $relative_path"
done <<< "$NESTED_CODE"

# Anything Mach-O must be on the signing list, or inside a bundle on it, so a
# new binary can't ship signed only by accident.
uncovered="$(
  cd "$APP_DIR"
  find Contents -type f -print0 |
    xargs -0 file -F '|' |
    sed -nE 's/^(.*)\|[[:space:]]*Mach-O.*$/\1/p' |
    LC_ALL=C sort |
    while read -r binary; do
      covered=0
      while read -r unit; do
        if [ "$binary" = "$unit" ] || [[ "$binary" == "$unit/"* ]]; then
          covered=1
          break
        fi
      done <<< "$(printf '%s\n%s\n' "$MAIN_EXECUTABLE" "$NESTED_CODE")"
      [ "$covered" = 1 ] || echo "$binary"
    done
)"
[ -z "$uncovered" ] || fail "Mach-O files outside the signing list:
$uncovered"

[ "$(plist_value CFBundleShortVersionString)" = "$VERSION" ] ||
  fail "CFBundleShortVersionString is not $VERSION"
[ "$(plist_value CFBundleVersion)" = "$VERSION" ] ||
  fail "CFBundleVersion is not $VERSION"

# Empty when the key is absent.
plist_optional() {
  plutil -extract "$1" raw -o - "$APP_DIR/Contents/Info.plist" 2>/dev/null || true
}
actual_feed="$(plist_optional SUFeedURL)"
if [ -n "$FEED_URL" ]; then
  [ "$actual_feed" = "$FEED_URL" ] || fail "SUFeedURL is '$actual_feed', not $FEED_URL"
  [ -s "$UPDATE_PUBLIC_KEY_FILE" ] || fail "no Sparkle public key to compare against"
  [ "$(plist_optional SUPublicEDKey)" = "$(tr -d '[:space:]' < "$UPDATE_PUBLIC_KEY_FILE")" ] ||
    fail "SUPublicEDKey doesn't match ${UPDATE_PUBLIC_KEY_FILE#"$ROOT_DIR"/}"
  [ "$(plist_optional SURequireSignedFeed)" = true ] || fail "SURequireSignedFeed is not set"
  [ "$(plist_optional SUVerifyUpdateBeforeExtraction)" = true ] ||
    fail "SUVerifyUpdateBeforeExtraction is not set"
  [ -z "$(plist_optional SUEnableAutomaticChecks)" ] ||
    fail "SUEnableAutomaticChecks is set; Sparkle must ask for consent"
  echo "Update feed: $actual_feed"
elif [ -n "$actual_feed$(plist_optional SUPublicEDKey)" ]; then
  fail "bundle carries an update feed or key but none was expected"
fi

verify_signature() {
  local path="$1" details
  codesign --verify --strict "$path" || fail "invalid signature: $path"
  details="$(codesign -d --verbose=4 "$path" 2>&1)"
  grep -Eq 'flags=0x[0-9a-f]+\(.*runtime.*\)' <<< "$details" || fail "no hardened runtime: $path"
  verify_signer "$path" "$details"
}

# Prints the entitlements as JSON, or nothing when there are none. Sparkle's
# Downloader ships an empty <dict/> that --preserve-metadata keeps; it grants
# nothing. Fails when they can't be read, so unreadable never passes as none.
entitlement_keys() {
  local xml json
  xml="$(codesign -d --entitlements - --xml "$1" 2>/dev/null)" || return 1
  [ -n "$xml" ] || return 0
  json="$(plutil -convert json -o - - <<< "$xml")" || return 1
  [ "$json" = "{}" ] || echo "$json"
}

while read -r relative_path; do
  verify_signature "$APP_DIR/$relative_path"
  keys="$(entitlement_keys "$APP_DIR/$relative_path")" ||
    fail "couldn't read the entitlements of $relative_path"
  [ -z "$keys" ] || fail "unexpected entitlements on $relative_path"
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

for relative_path in "$MAIN_EXECUTABLE" $(release_own_binaries); do
  leaks="$(strings -a "$APP_DIR/$relative_path" | grep -F -- "$HOME" || true)"
  [ -z "$leaks" ] || fail "$relative_path embeds the build machine's home path:
$(head -3 <<< "$leaks")"
done

"$ROOT_DIR/scripts/check-release-seams.sh" "$APP_DIR" ||
  fail "test seams or development overrides shipped in $APP_DIR"

if [ "$DISTRIBUTION" = 1 ]; then
  spctl --assess --type execute "$APP_DIR" || fail "Gatekeeper rejected the app"
  xcrun stapler validate "$APP_DIR" >/dev/null || fail "no valid stapled ticket on the app"
fi

echo "Verified ${DISK_IMAGE:-$APP_DIR} ($VERSION, $AUTHORITY)"
