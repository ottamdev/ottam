#!/usr/bin/env bash
# Read-only display naming shared by the dev launcher and UI scenario runner.
set -euo pipefail

# Keep child processes in UTF-8 even when invoked with LC_ALL=C.
export LC_ALL=en_US.UTF-8
CHECKOUT_ROOT=""
ROLE=""

usage() {
  echo "Usage: scripts/app-display-name.sh --root <checkout> --role dev|test" >&2
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --root|--role)
      [ "$#" -ge 2 ] || { usage; exit 64; }
      if [ "$1" = "--root" ]; then
        CHECKOUT_ROOT="$2"
      else
        ROLE="$2"
      fi
      shift 2
      ;;
    *) usage; exit 64 ;;
  esac
done

case "$ROLE" in
  dev) DISPLAY_NAME="Ottam Dev" ;;
  test) DISPLAY_NAME="Ottam Test" ;;
  *) usage; exit 64 ;;
esac
[ -d "$CHECKOUT_ROOT" ] || { usage; exit 64; }
CHECKOUT_ROOT="$(cd "$CHECKOUT_ROOT" && pwd -P)"

GIT_DIR="$(git -C "$CHECKOUT_ROOT" rev-parse --path-format=absolute --git-dir 2>/dev/null || true)"
GIT_COMMON_DIR="$(git -C "$CHECKOUT_ROOT" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
if [ -n "$GIT_DIR" ] && [ -n "$GIT_COMMON_DIR" ]; then
  GIT_DIR="$(cd "$GIT_DIR" && pwd -P)"
  GIT_COMMON_DIR="$(cd "$GIT_COMMON_DIR" && pwd -P)"
  if [ "$GIT_DIR" = "$GIT_COMMON_DIR" ]; then
    printf '%s\n' "$DISPLAY_NAME"
    exit 0
  fi
fi

LABEL="$(basename "$CHECKOUT_ROOT")"
PARENT="$(dirname "$CHECKOUT_ROOT")"
if [ "$LABEL" = "ottam" ] && [ "$(basename "$(dirname "$PARENT")")" = "worktrees" ]; then
  LABEL="$(basename "$PARENT")"
fi
LABEL="${LABEL#ottam-}"
# The system interpreter, so naming never depends on the caller's PATH.
LABEL="$(printf '%s' "$LABEL" | /usr/bin/python3 "$(dirname "${BASH_SOURCE[0]}")/lib/display_label.py")"
printf '%s · %s\n' "$DISPLAY_NAME" "$LABEL"
