#!/usr/bin/env bash
# Publishes a release that scripts/release-app.sh built: signs the archive and
# a candidate appcast with Sparkle's generate_appcast, creates the GitHub
# Release, checks the public asset, and pushes the appcast to the feed
# repository last. That push is the commit point: until it lands, no client
# can see the release.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/release-version.sh
source "$ROOT_DIR/scripts/lib/release-version.sh"
# shellcheck source=scripts/lib/release-feed.sh
source "$ROOT_DIR/scripts/lib/release-feed.sh"

REPO="${OTTAM_RELEASE_REPO:-ottamdev/ottam}"
# Where release-app.sh's build leaves Sparkle's tools.
SPARKLE_BIN="${OTTAM_SPARKLE_BIN:-$ROOT_DIR/.build/swift-release/artifacts/sparkle/Sparkle/bin}"
POLL_SECONDS="${OTTAM_PUBLISH_POLL_SECONDS:-15}"
POLL_ATTEMPTS="${OTTAM_PUBLISH_POLL_ATTEMPTS:-40}"
VERSION=""
FEED=""
UPDATES=""
KEY_FILE=""
ARCHIVE=""
PREFLIGHT=0

usage() {
  cat <<'USAGE'
Usage:
  scripts/publish-release.sh --version <YY.M.N> --feed <production|test>
      --updates-checkout <dir> --ed-key-file <file> [--archive <zip>]
  scripts/publish-release.sh --preflight --version <YY.M.N> --feed <production|test>
      --updates-checkout <dir>

Publishes the release tagged vYY.M.N at HEAD. The archive defaults to
.build/release/Ottam-<version>.zip, and its bundle must update from the named
feed. Release notes come from release-notes/<version>.md. The updates checkout
is a clone of ottamdev/updates whose main can be pushed; the Sparkle private
key file is what `generate_keys -x` exports. A test-feed release is marked as
a pre-release.

--preflight runs only the refusals that need no build (tag, notes, feed
version, existing release), so a doomed run stops before building.

OTTAM_RELEASE_REPO overrides the GitHub repository (default ottamdev/ottam).
USAGE
}

refuse() {
  echo "Publish refused: $*" >&2
  exit 65
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version) VERSION="${2:?--version requires a version}"; shift 2 ;;
    --feed) FEED="${2:?--feed requires production or test}"; shift 2 ;;
    --updates-checkout) UPDATES="${2:?--updates-checkout requires a directory}"; shift 2 ;;
    --ed-key-file) KEY_FILE="${2:?--ed-key-file requires a file}"; shift 2 ;;
    --archive) ARCHIVE="${2:?--archive requires a file}"; shift 2 ;;
    --preflight) PREFLIGHT=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown option: $1" >&2; usage >&2; exit 64 ;;
  esac
done
[ -n "$VERSION" ] && [ -n "$FEED" ] && [ -n "$UPDATES" ] && { [ "$PREFLIGHT" = 1 ] || [ -n "$KEY_FILE" ]; } || {
  usage >&2
  exit 64
}

release_version_is_valid "$VERSION" || refuse "invalid version: $VERSION"
feed_entry="$(release_feed "$FEED")" || refuse "unknown feed '$FEED'; use production or test"
FEED_URL="${feed_entry% *}"
FEED_PATH="${feed_entry#* }"
TAG="v$VERSION"
ARCHIVE_NAME="Ottam-$VERSION.zip"
ARCHIVE="${ARCHIVE:-$ROOT_DIR/.build/release/$ARCHIVE_NAME}"
NOTES="$ROOT_DIR/release-notes/$VERSION.md"
DOWNLOAD_PREFIX="https://github.com/$REPO/releases/download/$TAG/"

git -C "$ROOT_DIR" tag --points-at HEAD | grep -qx "$TAG" || refuse "$TAG is not tagged at HEAD"
[ -s "$NOTES" ] || refuse "no release notes at release-notes/$VERSION.md"

# The candidate must build on the feed as published, so the push below can
# only fast-forward it.
[ -z "$(git -C "$UPDATES" status --porcelain)" ] || refuse "the updates checkout has local changes"
git -C "$UPDATES" fetch --quiet origin main || refuse "couldn't fetch the updates repository"
[ "$(git -C "$UPDATES" rev-parse HEAD)" = "$(git -C "$UPDATES" rev-parse origin/main)" ] ||
  refuse "the updates checkout isn't at origin/main"
if [ -f "$UPDATES/$FEED_PATH" ]; then
  published_versions="$(python3 "$ROOT_DIR/scripts/lib/appcast_items.py" "$UPDATES/$FEED_PATH" | cut -f 1)" ||
    refuse "the published $FEED feed isn't a readable appcast"
  for published in $published_versions; do
    release_version_is_newer "$VERSION" "$published" ||
      refuse "$VERSION is not newer than $published in the $FEED feed"
  done
fi
if gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
  refuse "GitHub Release $TAG already exists; delete it first if it was never in a feed"
fi
if [ "$PREFLIGHT" = 1 ]; then
  echo "Preflight passed: $TAG can publish to the $FEED feed"
  exit 0
fi

[ -f "$ARCHIVE" ] || refuse "no archive at $ARCHIVE"
[ "$(basename "$ARCHIVE")" = "$ARCHIVE_NAME" ] || refuse "the archive must be named $ARCHIVE_NAME"
[ -s "$KEY_FILE" ] || refuse "no Sparkle key at $KEY_FILE"
[ -x "$SPARKLE_BIN/generate_appcast" ] && [ -x "$SPARKLE_BIN/sign_update" ] ||
  refuse "Sparkle tools missing from $SPARKLE_BIN; build the app first"

work="$(mktemp -d "${TMPDIR:-/tmp}/ottam-publish.XXXXXX")"
trap 'rm -rf "$work"' EXIT

# What ships must update from this feed, at this version.
mkdir "$work/extracted"
ditto -x -k "$ARCHIVE" "$work/extracted"
plist="$work/extracted/Ottam.app/Contents/Info.plist"
[ -f "$plist" ] || refuse "the archive holds no Ottam.app"
bundle_feed="$(plutil -extract SUFeedURL raw -o - "$plist" 2>/dev/null || true)"
[ "$bundle_feed" = "$FEED_URL" ] ||
  refuse "the archive updates from '${bundle_feed:-nothing}', not the $FEED feed $FEED_URL"
bundle_version="$(plutil -extract CFBundleVersion raw -o - "$plist")"
[ "$bundle_version" = "$VERSION" ] || refuse "the archive is version $bundle_version, not $VERSION"

mkdir "$work/feed"
if [ -f "$UPDATES/$FEED_PATH" ]; then
  "$SPARKLE_BIN/sign_update" --verify --ed-key-file "$KEY_FILE" "$UPDATES/$FEED_PATH" >/dev/null 2>&1 ||
    refuse "the published $FEED feed doesn't verify with this key"
  cp "$UPDATES/$FEED_PATH" "$work/feed/appcast.xml"
fi

echo "Signing the archive and the candidate $FEED appcast"
cp "$ARCHIVE" "$work/feed/$ARCHIVE_NAME"
cp "$NOTES" "$work/feed/Ottam-$VERSION.md"
"$SPARKLE_BIN/generate_appcast" --ed-key-file "$KEY_FILE" --download-url-prefix "$DOWNLOAD_PREFIX" \
  --embed-release-notes "$work/feed" > "$work/generate.log" 2>&1 || {
  cat "$work/generate.log" >&2
  exit 70
}
if grep -q 'does not match' "$work/generate.log"; then
  cat "$work/generate.log" >&2
  refuse "the Sparkle key doesn't match the bundle's SUPublicEDKey"
fi

item="$(python3 "$ROOT_DIR/scripts/lib/appcast_items.py" "$work/feed/appcast.xml" |
  awk -F '\t' -v version="$VERSION" '$1 == version')"
IFS="$(printf '\t')" read -r _ item_url item_length item_signature <<< "$item"
[ "${item_url:-}" = "$DOWNLOAD_PREFIX$ARCHIVE_NAME" ] ||
  refuse "the candidate appcast points $VERSION at '${item_url:-nothing}'"
[ "${item_length:-}" = "$(stat -f %z "$ARCHIVE")" ] || refuse "the candidate appcast has the wrong archive length"
[ -n "${item_signature:-}" ] || refuse "the candidate appcast has no archive signature"
"$SPARKLE_BIN/sign_update" --verify --ed-key-file "$KEY_FILE" "$ARCHIVE" "$item_signature" >/dev/null ||
  refuse "the archive signature doesn't verify"
"$SPARKLE_BIN/sign_update" --verify --ed-key-file "$KEY_FILE" "$work/feed/appcast.xml" >/dev/null ||
  refuse "the candidate appcast signature doesn't verify"

echo "Creating GitHub Release $TAG"
release_args=(--repo "$REPO" --verify-tag --title "Ottam $VERSION" --notes-file "$NOTES")
[ "$FEED" = production ] || release_args+=(--prerelease)
gh release create "$TAG" "$ARCHIVE" "${release_args[@]}" >/dev/null

# Clients download without credentials, so check exactly that.
curl -fsSL --retry 5 --retry-delay 5 -o "$work/public.zip" "$DOWNLOAD_PREFIX$ARCHIVE_NAME" ||
  refuse "the public asset didn't download; the feed is unchanged. Delete the release and rerun"
cmp -s "$ARCHIVE" "$work/public.zip" ||
  refuse "the public asset differs from the archive; the feed is unchanged. Delete the release and rerun"

echo "Publishing the $FEED feed"
mkdir -p "$UPDATES/$(dirname "$FEED_PATH")"
cp "$work/feed/appcast.xml" "$UPDATES/$FEED_PATH"
git -C "$UPDATES" add "$FEED_PATH"
git -C "$UPDATES" commit --quiet -m "Ottam $VERSION ($FEED feed)"
git -C "$UPDATES" push --quiet origin HEAD:main ||
  refuse "the feed repository moved since the candidate was built; nothing was published. Delete the release and rerun"

echo "Waiting for $FEED_URL to serve it"
attempt=1
until curl -fsS -H 'Cache-Control: no-cache' -o "$work/public-feed.xml" "$FEED_URL" 2>/dev/null &&
    cmp -s "$work/feed/appcast.xml" "$work/public-feed.xml"; do
  [ "$attempt" -lt "$POLL_ATTEMPTS" ] || {
    echo "The feed was pushed but $FEED_URL still serves something else; check the Cloudflare deploy." >&2
    exit 70
  }
  attempt=$((attempt + 1))
  sleep "$POLL_SECONDS"
done
"$SPARKLE_BIN/sign_update" --verify --ed-key-file "$KEY_FILE" "$work/public-feed.xml" >/dev/null ||
  refuse "the public feed doesn't verify"
echo "Published $VERSION to the $FEED feed: $FEED_URL"
