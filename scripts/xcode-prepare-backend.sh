#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CORE_BINARY="$ROOT_DIR/backend/target/debug/ottam-core"
LOG_FILE="${TMPDIR:-/tmp}/ottam-xcode-prepare-backend.log"

log() {
  printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*" >>"$LOG_FILE"
}

log "starting xcode backend preparation"

if [ -z "${TARGET_BUILD_DIR:-}" ]; then
  log "failed: TARGET_BUILD_DIR is missing"
  echo "TARGET_BUILD_DIR is required. Run this as an Xcode scheme pre-action for OttamApp." >&2
  exit 64
fi

if [ ! -d "$TARGET_BUILD_DIR" ]; then
  log "failed: TARGET_BUILD_DIR does not exist: $TARGET_BUILD_DIR"
  echo "TARGET_BUILD_DIR does not exist: $TARGET_BUILD_DIR" >&2
  exit 66
fi

"$ROOT_DIR/scripts/prune-rust-target.sh"
cargo build --manifest-path "$ROOT_DIR/backend/Cargo.toml" -p ottam-core

executable_path=""
if [ -n "${EXECUTABLE_PATH:-}" ]; then
  executable_path="$TARGET_BUILD_DIR/$EXECUTABLE_PATH"
elif [ -n "${EXECUTABLE_NAME:-}" ]; then
  executable_path="$TARGET_BUILD_DIR/$EXECUTABLE_NAME"
else
  log "failed: EXECUTABLE_PATH and EXECUTABLE_NAME are missing"
  echo "EXECUTABLE_PATH or EXECUTABLE_NAME is required to locate the Xcode-built SwiftPM executable." >&2
  exit 64
fi

executable_dir="$(dirname "$executable_path")"
mkdir -p "$executable_dir"

destination="$executable_dir/ottam-core"
cp "$CORE_BINARY" "$destination"
chmod 755 "$destination"

fallback_dir="$TARGET_BUILD_DIR/.build/ottam-core"
mkdir -p "$fallback_dir"
fallback_destination="$fallback_dir/ottam-core"
cp "$CORE_BINARY" "$fallback_destination"
chmod 755 "$fallback_destination"

echo "Prepared $destination"
echo "Prepared $fallback_destination"
log "prepared $destination"
log "prepared $fallback_destination"
