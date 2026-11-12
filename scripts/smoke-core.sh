#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_PACKAGE_DIR="$ROOT_DIR/app"
SWIFT_BUILD_DIR="$ROOT_DIR/.build/swift"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ottam-smoke.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

cd "$ROOT_DIR"

swift run --package-path "$APP_PACKAGE_DIR" --scratch-path "$SWIFT_BUILD_DIR" ottam version | grep -q "ottam 0.1.0"
swift run --package-path "$APP_PACKAGE_DIR" --scratch-path "$SWIFT_BUILD_DIR" ottam init "$TMP_DIR"

test -d "$TMP_DIR/.ottam"
test ! -e "$TMP_DIR/.ottam/workflows"
test ! -e "$TMP_DIR/.ottam/attempts"

# One real JSONL round trip against a scratch core (no adapters are started).
agents="$("$ROOT_DIR/scripts/inspect-core.sh" call agent.list '{}' --spawn)"
grep -q '"agents"' <<<"$agents"
echo "agent.list: $(grep -c '"agentID"' <<<"$agents") agents"

echo "Core smoke check passed"
