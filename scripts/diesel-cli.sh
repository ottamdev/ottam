#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXPECTED_VERSION="2.3.10"
LOCAL_DIESEL="$ROOT_DIR/.build/tools/bin/diesel"

if [ -x "$LOCAL_DIESEL" ]; then
  DIESEL="$LOCAL_DIESEL"
elif command -v diesel >/dev/null 2>&1; then
  DIESEL="$(command -v diesel)"
else
  echo "Diesel CLI $EXPECTED_VERSION is required." >&2
  echo "Install it with:" >&2
  echo "  cargo install diesel_cli --version $EXPECTED_VERSION --locked --root .build/tools --no-default-features --features sqlite-bundled" >&2
  exit 1
fi

version="$("$DIESEL" --version)"
if [[ "$version" != *"Version: $EXPECTED_VERSION"* ]]; then
  echo "Expected Diesel CLI $EXPECTED_VERSION, found: $version" >&2
  exit 1
fi

exec "$DIESEL" "$@"
