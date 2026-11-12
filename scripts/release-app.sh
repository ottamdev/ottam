#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/release-version.sh
source "$ROOT_DIR/scripts/lib/release-version.sh"
# shellcheck source=scripts/lib/release-signing.sh
source "$ROOT_DIR/scripts/lib/release-signing.sh"

CARGO_MANIFEST="$ROOT_DIR/backend/Cargo.toml"
ENTITLEMENTS="$ROOT_DIR/app/Signing/Ottam.entitlements"
MODE=release
IDENTITY="${OTTAM_RELEASE_IDENTITY:-}"
NOTARY_PROFILE="${OTTAM_NOTARY_PROFILE:-ottam-notary}"
RELEASE_OUTPUT="$ROOT_DIR/.build/release"
VERSION=""
BUNDLE_ID=""
USE_ENTITLEMENTS=1

usage() {
  cat <<'USAGE'
Usage:
  scripts/release-app.sh --cut
      Pick the next version, bump Cargo, commit, and tag locally. Never pushes.

  scripts/release-app.sh [--identity <sha1-or-name>]
      Build, sign, notarize, and staple the release tagged at HEAD, then
      verify the extracted archive. Writes .build/release/Ottam-<version>.zip
      only when every step passes.

  scripts/release-app.sh --rehearsal --version <YY.M.N> --bundle-id <id> [--no-entitlements]
      Hardened build signed with Apple Development, for testing without
      Developer ID. Skips the tag checks and notarization. Not distributable.

OTTAM_RELEASE_IDENTITY is the default for --identity. OTTAM_NOTARY_PROFILE
names the notarytool keychain profile (default: ottam-notary).
USAGE
}

refuse() {
  echo "Release refused: $*" >&2
  exit 65
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --cut)
      MODE=cut
      shift
      ;;
    --rehearsal)
      MODE=rehearsal
      shift
      ;;
    --identity)
      IDENTITY="${2:?--identity requires a value}"
      shift 2
      ;;
    --version)
      VERSION="${2:?--version requires a version}"
      shift 2
      ;;
    --bundle-id)
      BUNDLE_ID="${2:?--bundle-id requires an identifier}"
      shift 2
      ;;
    --no-entitlements)
      USE_ENTITLEMENTS=0
      shift
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

if [ "$MODE" != rehearsal ] && { [ -n "$VERSION" ] || [ -n "$BUNDLE_ID" ] || [ "$USE_ENTITLEMENTS" = 0 ]; }; then
  echo "--version, --bundle-id, and --no-entitlements are rehearsal-only; a release takes its version from the tag." >&2
  exit 64
fi

require_clean_tree() {
  [ -z "$(git -C "$ROOT_DIR" status --porcelain)" ] ||
    refuse "working tree has uncommitted changes"
}

set_cargo_version() {
  local updated="$CARGO_MANIFEST.release-tmp"
  awk -v version="$1" '
    /^\[/ { in_table = ($0 == "[workspace.package]") }
    in_table && /^version[[:space:]]*=/ { print "version = \"" version "\""; next }
    { print }
  ' "$CARGO_MANIFEST" > "$updated"
  mv "$updated" "$CARGO_MANIFEST"
}

cut_release() {
  require_clean_tree
  [ "$(git -C "$ROOT_DIR" branch --show-current)" = main ] ||
    refuse "a release is cut from main"
  # Versions come from tags, so another checkout's pushed release must count,
  # and the cut must sit on the published main.
  git -C "$ROOT_DIR" fetch --quiet --tags origin main ||
    refuse "couldn't fetch origin; a cut needs its tags and main"
  [ "$(git -C "$ROOT_DIR" rev-parse HEAD)" = "$(git -C "$ROOT_DIR" rev-parse origin/main)" ] ||
    refuse "local main doesn't match origin/main; pull or push first"

  local tags=() tag next
  while IFS= read -r tag; do
    tags+=("$tag")
  done < <(git -C "$ROOT_DIR" tag --list 'v*')
  next="$(release_next_version "$(date -u +%F)" "${tags[@]+"${tags[@]}"}")"

  set_cargo_version "$next"
  cargo update --workspace --offline --manifest-path "$CARGO_MANIFEST" --quiet
  git -C "$ROOT_DIR" add backend/Cargo.toml backend/Cargo.lock
  git -C "$ROOT_DIR" commit --quiet -m "chore(release): v$next"
  git -C "$ROOT_DIR" tag -a "v$next" -m "Ottam $next"
  echo "Cut v$next locally. Nothing was pushed."
}

head_release_version() {
  local tag version found=""
  while IFS= read -r tag; do
    version="${tag#v}"
    if [ "$version" != "$tag" ] && release_version_is_valid "$version"; then
      [ -z "$found" ] || refuse "more than one release tag at HEAD"
      found="$version"
    fi
  done < <(git -C "$ROOT_DIR" tag --points-at HEAD)
  [ -n "$found" ] || refuse "no valid vYY.M.N tag at HEAD"
  echo "$found"
}

build_and_sign() {
  local version="$1" identity="$2" authority="$3" output="$4" app_name="$5" bundle_id="$6"
  local app="$output/$app_name.app" relative_path
  local entitlement_args=() verify_args=(--version "$version" --authority "$authority")
  if [ "$USE_ENTITLEMENTS" = 1 ]; then
    entitlement_args=(--entitlements "$ENTITLEMENTS")
  else
    verify_args+=(--no-entitlements)
  fi

  "$ROOT_DIR/scripts/build-app.sh" --release --version "$version" --no-sign \
    --output-root "$output" --app-name "$app_name" --bundle-id "$bundle_id"

  xattr -cr "$app"
  while read -r relative_path; do
    codesign --force --options runtime --timestamp --sign "$identity" "$app/$relative_path"
  done < <(release_nested_code)
  codesign --force --options runtime --timestamp \
    "${entitlement_args[@]+"${entitlement_args[@]}"}" --sign "$identity" "$app"

  "$ROOT_DIR/scripts/verify-release-app.sh" "$app" "${verify_args[@]}"
}

rehearse() {
  release_version_is_valid "$VERSION" || refuse "invalid version: $VERSION"
  [ -n "$BUNDLE_ID" ] || refuse "a rehearsal needs its own --bundle-id"
  [ "$BUNDLE_ID" != "dev.ottam.Ottam" ] || refuse "a rehearsal can't use the production bundle id"

  local identity
  identity="$(release_resolve_identity "Apple Development" "$IDENTITY")"
  build_and_sign "$VERSION" "$identity" "Apple Development" \
    "$ROOT_DIR/.build/release-rehearsal" "Ottam Rehearsal" "$BUNDLE_ID"
  echo "Rehearsal build: $ROOT_DIR/.build/release-rehearsal/Ottam Rehearsal.app"
}

# Submits the app and waits for Apple's verdict; anything but Accepted prints
# the notary log and stops the release.
notarize() {
  local app="$1" work_dir="$2" upload="$2/notarize.zip" result="$2/notary.json"
  local status submission_id

  ditto -c -k --keepParent "$app" "$upload"
  echo "Submitting to the notary service (profile $NOTARY_PROFILE); this usually takes a few minutes."
  xcrun notarytool submit "$upload" --keychain-profile "$NOTARY_PROFILE" \
    --wait --output-format json > "$result" || true
  status="$(plutil -extract status raw -o - "$result" 2>/dev/null || true)"
  submission_id="$(plutil -extract id raw -o - "$result" 2>/dev/null || true)"
  if [ -z "$status" ]; then
    echo "notarytool returned no status (interrupted, offline, or an upload error); rerun to resubmit." >&2
    cat "$result" >&2 || true
    exit 70
  fi
  if [ "$status" != Accepted ]; then
    echo "Notarization ended with status '$status' (submission ${submission_id:-none})." >&2
    cat "$result" >&2 || true
    [ -z "$submission_id" ] ||
      xcrun notarytool log "$submission_id" --keychain-profile "$NOTARY_PROFILE" >&2 || true
    exit 70
  fi
  echo "Notarized: submission $submission_id"
}

# The ticket can take a little while to reach Apple's CDN after Accepted, and
# a failure here would throw away the notarization, so retry before giving up.
staple() {
  local attempt
  for attempt in 1 2 3 4 5; do
    xcrun stapler staple -q "$1" && return 0
    [ "$attempt" = 5 ] || {
      echo "Stapling failed (attempt $attempt of 5); retrying in 30 seconds." >&2
      sleep 30
    }
  done
  echo "Stapling failed after 5 attempts." >&2
  exit 70
}

release() {
  require_clean_tree
  local version identity cargo_version work_dir app archive_name
  version="$(head_release_version)"
  cargo_version="$(release_cargo_version "$CARGO_MANIFEST")"
  [ "$cargo_version" = "$version" ] ||
    refuse "tag v$version does not match Cargo workspace version $cargo_version"
  archive_name="Ottam-$version.zip"
  [ ! -e "$RELEASE_OUTPUT/$archive_name" ] ||
    refuse "$RELEASE_OUTPUT/$archive_name already exists; a version is built once"
  identity="$(release_resolve_identity "Developer ID Application" "$IDENTITY")"
  # Fail before the build, not after it, when the notary credentials don't work.
  xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null 2>&1 || {
    echo "The notary keychain profile '$NOTARY_PROFILE' is missing or can't sign in." >&2
    exit 69
  }

  mkdir -p "$ROOT_DIR/.build"
  work_dir="$(mktemp -d "$ROOT_DIR/.build/release-work.XXXXXX")"
  # shellcheck disable=SC2064 # The path is fixed now.
  trap "rm -rf '$work_dir'" EXIT
  app="$work_dir/build/Ottam.app"
  build_and_sign "$version" "$identity" "Developer ID Application" \
    "$work_dir/build" "Ottam" "dev.ottam.Ottam"

  notarize "$app" "$work_dir"
  staple "$app"
  ditto -c -k --sequesterRsrc --keepParent "$app" "$work_dir/$archive_name"

  # Verify what ships: the archive's extracted contents, not the build.
  mkdir "$work_dir/extracted"
  ditto -x -k "$work_dir/$archive_name" "$work_dir/extracted"
  "$ROOT_DIR/scripts/verify-release-app.sh" "$work_dir/extracted/Ottam.app" \
    --version "$version" --distribution

  mkdir -p "$RELEASE_OUTPUT"
  mv "$work_dir/$archive_name" "$RELEASE_OUTPUT/$archive_name"
  echo "Released $version: $RELEASE_OUTPUT/$archive_name"
  shasum -a 256 "$RELEASE_OUTPUT/$archive_name"
}

case "$MODE" in
  cut) cut_release ;;
  rehearsal) rehearse ;;
  release) release ;;
esac
