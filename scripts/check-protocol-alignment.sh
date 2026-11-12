#!/usr/bin/env bash
# Proves the Rust protocol crate and the Swift app agree. CI's macOS gate.
#   1. Protocol crate tests: byte-guards assert the committed JSON artifacts
#      and ProtocolTypes.generated.swift match what the Rust types regenerate.
#   2. App build: the real consumer compiles against the generated public API.
#   3. Swift fixture gate: the generated Swift types decode and round-trip the
#      Rust fixtures, and talk JSONL to a real debug core.
#   4. App unit tests: the pure logic targets that build on those types.
# Fails whenever either side changes without running:
#   cargo run --manifest-path backend/Cargo.toml -p ottam-protocol --bin generate-protocol-artifacts
# Narrower than test-rust-core.sh: no clippy and no workspace tests.
# --skip-protocol-tests skips step 1, for CI, whose Linux job already runs it.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
protocol_tests=1
for argument in "$@"; do
  case "$argument" in
    --skip-protocol-tests) protocol_tests=0 ;;
    *) echo "Usage: scripts/check-protocol-alignment.sh [--skip-protocol-tests]" >&2; exit 64 ;;
  esac
done
# One Xcode toolchain and one scratch path for every Swift command, shared
# with test-app.sh.
source scripts/lib/xcode-developer-dir.sh
scratch=.build/swift-tests
if (( protocol_tests )); then
  cargo test --manifest-path backend/Cargo.toml --locked --quiet -p ottam-protocol
fi
cargo build --manifest-path backend/Cargo.toml --locked --quiet -p ottam-core
swift build --package-path app --scratch-path "$scratch" --product OttamApp -Xswiftc -warnings-as-errors --quiet
OTTAM_BACKEND_CLIENT_INTEGRATION_CORE="$PWD/backend/target/debug/ottam-core" \
OTTAM_REPO_ROOT="$PWD" \
  swift run --package-path app --scratch-path "$scratch" -Xswiftc -warnings-as-errors --quiet OttamBackendClientTests
./scripts/test-app.sh
