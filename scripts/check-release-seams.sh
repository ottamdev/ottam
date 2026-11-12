#!/usr/bin/env bash
set -euo pipefail

# Proves a bundle's executables carry none of the test seams, UI-test hooks, or
# development overrides that let the environment choose what a release build
# runs or downloads. Each sentinel is a string literal the gated code reads;
# its absence from `strings` means that code was compiled out.
#
# Every sentinel is longer than 15 bytes: Swift stores shorter literals inline
# in the instruction stream, where `strings` cannot see them, so a short one
# would pass whether or not its code shipped.
#
# --expect-present is the negative control: a debug or profile bundle must
# contain every sentinel, or the list has gone stale and the check proves
# nothing.

usage() {
  echo "Usage: scripts/check-release-seams.sh [--expect-present] <path/to/App.app>" >&2
}

EXPECT_PRESENT=0
if [ "${1:-}" = "--expect-present" ]; then
  EXPECT_PRESENT=1
  shift
fi
if [ "$#" -ne 1 ] || [ ! -d "$1/Contents/MacOS" ]; then
  usage
  exit 64
fi
MACOS_DIR="$1/Contents/MacOS"

SENTINELS=(
  # ottam-core (`cfg(test_seams)`, see backend/crates/ottam-core/build.rs)
  OTTAM_TEST_NATIVE_STEER_ENGINE
  OTTAM_TEST_BINARY_AGENT_URL
  OTTAM_ADAPTER_INSTALLS
  OTTAM_NODE_DIST_BASE
  OTTAM_SECRET_STORE
  OTTAM_ACP_WIRE_LOG
  # the app (`#if DEBUG`)
  OTTAM_UI_TEST_PREFERRED_ENGINE
  OTTAM_UI_TEST_READINESS_DELAY_MS
  OTTAM_SCREENSHOT_EXTERNAL_DIR
  .build/ottam-core/ottam-core
)

executables=()
for candidate in "$MACOS_DIR"/*; do
  if [ -f "$candidate" ] && [ -x "$candidate" ]; then
    executables+=("$candidate")
  fi
done
if [ "${#executables[@]}" -lt 2 ]; then
  echo "check-release-seams: expected the app and ottam-core in $MACOS_DIR" >&2
  exit 66
fi

contents="$(mktemp)"
trap 'rm -f "$contents"' EXIT
for executable in "${executables[@]}"; do
  strings -a "$executable" >> "$contents"
done

failures=0
for sentinel in "${SENTINELS[@]}"; do
  if grep -qF -- "$sentinel" "$contents"; then
    if [ "$EXPECT_PRESENT" = 0 ]; then
      echo "check-release-seams: '$sentinel' is in a release executable" >&2
      failures=$((failures + 1))
    fi
  elif [ "$EXPECT_PRESENT" = 1 ]; then
    echo "check-release-seams: '$sentinel' is missing from the control bundle; the sentinel list is stale" >&2
    failures=$((failures + 1))
  fi
done

if [ "$failures" -ne 0 ]; then
  exit 1
fi
if [ "$EXPECT_PRESENT" = 1 ]; then
  echo "Seam sentinels present in control bundle $1"
else
  echo "No test seams in $1"
fi
