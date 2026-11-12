#!/usr/bin/env bash
set -euo pipefail

# Installs the pinned GhosttyKit and its terminal resources: upstream Ghostty
# plus Ottam's manual-IO patch, built and published from ARTIFACT_REPO.

# --verify skips the cached install and downloads the pinned artifact again,
# so a release links bytes that match ARTIFACT_SHA256 rather than whatever a
# checkout left in app/Vendor.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERIFY=0
if [ "${1:-}" = "--verify" ]; then
  VERIFY=1
elif [ "$#" -ne 0 ]; then
  echo "Usage: scripts/fetch-ghosttykit.sh [--verify]" >&2
  exit 64
fi
ARTIFACT_REPO="ottamdev/ghosttykit"
ARTIFACT_TAG="ghostty-v1.4.0-ottam.1"
ARTIFACT_SHA256="6843020a18fde22324a2851c99df1e5693fd6dda840270c3cced5b2907f0dca9"
OUTPUT_DIR="$ROOT_DIR/app/Vendor/ottam-ghostty"
MARKER="$OUTPUT_DIR/.artifact_tag"
LOCK_FILE="$ROOT_DIR/.build/fetch-ghosttykit.lock"

artifact_is_complete() {
  [ -d "$1/GhosttyKit.xcframework" ] &&
    [ -d "$1/resources/ghostty/themes" ] &&
    [ -f "$1/resources/terminfo/78/xterm-ghostty" ]
}

if [ "$VERIFY" = 0 ] && [ -f "$MARKER" ] && [ "$(cat "$MARKER")" = "$ARTIFACT_TAG" ] &&
   artifact_is_complete "$OUTPUT_DIR"; then
  exit 0
fi

# An install replaces the directory, so concurrent runs take turns. The kernel
# drops the lock if its holder dies, and the rerun under it repeats the check
# above, so a run that waited skips the download.
if [ -z "${OTTAM_FETCH_GHOSTTYKIT_LOCKED:-}" ]; then
  mkdir -p "$(dirname "$LOCK_FILE")"
  OTTAM_FETCH_GHOSTTYKIT_LOCKED=1 exec lockf -k -t 600 "$LOCK_FILE" "$ROOT_DIR/scripts/fetch-ghosttykit.sh" "$@"
fi

ARCHIVE_NAME="ottam-ghostty.tar.gz"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ottam-ghostty.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "Downloading $ARTIFACT_TAG"
curl --fail --show-error --location --retry 5 --retry-all-errors \
  -o "$TMP_DIR/$ARCHIVE_NAME" \
  "https://github.com/$ARTIFACT_REPO/releases/download/$ARTIFACT_TAG/$ARCHIVE_NAME"

ACTUAL_SHA256="$(shasum -a 256 "$TMP_DIR/$ARCHIVE_NAME" | awk '{print $1}')"
if [ "$ACTUAL_SHA256" != "$ARTIFACT_SHA256" ]; then
  echo "$ARCHIVE_NAME checksum mismatch: expected $ARTIFACT_SHA256, got $ACTUAL_SHA256" >&2
  exit 1
fi

tar -xzf "$TMP_DIR/$ARCHIVE_NAME" -C "$TMP_DIR"
if ! artifact_is_complete "$TMP_DIR/ottam-ghostty"; then
  echo "$ARCHIVE_NAME is missing GhosttyKit or its terminal resources" >&2
  exit 1
fi

# A worktree's link points at the main checkout's copy, which another pin may
# still need, so this replaces the link rather than writing through it.
if [ -L "$OUTPUT_DIR" ]; then
  rm "$OUTPUT_DIR"
else
  rm -rf "$OUTPUT_DIR"
fi
mkdir -p "$(dirname "$OUTPUT_DIR")"
mv "$TMP_DIR/ottam-ghostty" "$OUTPUT_DIR"
printf '%s\n' "$ARTIFACT_TAG" > "$MARKER"
