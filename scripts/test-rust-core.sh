#!/usr/bin/env bash
set -euo pipefail

# No selectors runs the full CI gate. Selectors run only ottam-core tests.
selector= target= filter=
rust_only=0
while (( $# )); do
  case "$1" in
    --help|-h)
      echo 'Usage: test-rust-core.sh [--lib | --test <target>] [--filter <pattern>]'
      echo '--rust-only runs the full portable gate without Swift integration (CI/Linux).'
      echo 'Without selectors: full gate. --filter alone implies --lib.'
      echo 'Examples: --test chat_storage; --lib --filter storage'
      echo 'Set OTTAM_TEST_VERBOSE=1 to restore Cargo output.'
      exit 0 ;;
    --rust-only) rust_only=1; shift ;;
    --lib)
      [[ -z "$selector" ]] || { echo 'Conflicting test selectors' >&2; exit 64; }
      selector=lib; shift ;;
    --test|--filter)
      [[ $# -ge 2 && -n "$2" && "$2" != -* ]] || { echo "Missing value for $1" >&2; exit 64; }
      if [[ "$1" == --test ]]; then
        [[ -z "$selector" ]] || { echo 'Conflicting test selectors' >&2; exit 64; }
        selector=test; target="$2"
      else
        [[ -z "$filter" ]] || { echo 'Duplicate filter' >&2; exit 64; }
        filter="$2"
      fi
      shift 2 ;;
    *) echo "Unknown argument: $1" >&2; exit 64 ;;
  esac
done
if [[ "$rust_only" == 1 && -n "$selector$filter" ]]; then
  echo '--rust-only cannot be combined with test selectors' >&2
  exit 64
fi
# Bash 3.2 under nounset cannot expand an empty array: use conditional expansion.
quiet=()
[[ "${OTTAM_TEST_VERBOSE:-0}" == 1 ]] || quiet=(--quiet)
# Capture each test invocation so failures remain intact and successful workspace
# invocations can report one aggregate result instead of anonymous harness rows.
# Every invocation, stdout and stderr, is also appended to a log that outlives
# the run.
test_log="$(mktemp "${TMPDIR:-/tmp}/ottam-rust-tests.XXXXXX")"
last_log="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/.build/test-rust-core-last.log"
mkdir -p "${last_log%/*}"
: > "$last_log"
trap 'rm -f "$test_log"; echo "test-rust-core.sh: full log: .build/test-rust-core-last.log" >&2' EXIT
cargo_test() {
  local label="$1" status=0
  shift
  if [[ "${OTTAM_TEST_VERBOSE:-0}" == 1 ]]; then
    cargo test "$@" 2>&1 | tee "$test_log" || status=$?
  else
    cargo test --quiet "$@" > "$test_log" 2>&1 || status=$?
  fi
  if (( status == 0 )); then
    printf '== %s\n' "$label" >> "$last_log"
  else
    printf '== %s (failed)\n' "$label" >> "$last_log"
  fi
  cat "$test_log" >> "$last_log"
  if (( status != 0 )); then
    if [[ "${OTTAM_TEST_VERBOSE:-0}" != 1 ]]; then
      printf 'FAIL  %s\n' "$label" >&2
      cat "$test_log" >&2
    fi
    return "$status"
  fi
  # A filter that matches nothing is a typo, not a pass.
  if [[ -n "$filter" ]] &&
     awk '/^test result: ok[.] / { n += $4 + $8 } END { exit n > 0 }' "$test_log"; then
    printf "FAIL  %s: 0 tests matched filter '%s'\n" "$label" "$filter" >&2
    return 1
  fi
  [[ "${OTTAM_TEST_VERBOSE:-0}" != 1 ]] || return 0
  awk -v label="$label" '
    /^running [0-9]+ tests?$/ { next }
    /^[.i]+([[:space:]]+[0-9]+\/[0-9]+)?$/ { next }
    /^test result: ok[.] / {
      summaries++; passed += $4; ignored += $8; filtered += $12
      duration = $0; sub(/^.*finished in /, "", duration); seconds += duration + 0
      next
    }
    NF { print }
    END {
      if (summaries) {
        printf "PASS  %s: %d passed", label, passed
        if (ignored) printf ", %d ignored", ignored
        if (filtered) printf ", %d filtered out", filtered
        printf " (%.2fs)\n", seconds
      }
    }
  ' "$test_log"
}
cd "$(dirname "${BASH_SOURCE[0]}")/../backend"
../scripts/prune-rust-target.sh
if [[ -n "$selector$filter" ]]; then
  args=(-p ottam-core)
  # Core's MCP upstream tests spawn the fixture server from the library too.
  cargo build ${quiet[@]+"${quiet[@]}"} -p ottam-test-mcp
  if [[ "$selector" == test ]]; then
    cargo build ${quiet[@]+"${quiet[@]}"} -p ottam-test-acp
    args+=(--test "$target")
  else
    args+=(--lib)
  fi
  [[ -z "$filter" ]] || args+=("$filter")
  args+=(--)
  [[ "$target" != jsonl_protocol ]] || args+=(--test-threads=4)
  cargo_test "ottam-core / ${target:-library}${filter:+ [filter: $filter]}" "${args[@]}"
  exit 0
fi
echo "Checks"
"../scripts/check-architecture-rules.sh"
"../scripts/check-diesel-schema.sh"
cargo build ${quiet[@]+"${quiet[@]}"} -p ottam-test-acp -p ottam-test-mcp
printf "\nTests\n"
cargo_test "workspace libraries and binaries" --workspace --lib --bins
for test_file in crates/*/tests/*.rs; do
  package_dir="${test_file%/tests/*}"
  package="${package_dir##*/}"
  test_target="${test_file##*/}"
  test_target="${test_target%.rs}"
  if [[ "$package" == "ottam-core" && "$test_target" == "jsonl_protocol" ]]; then
    continue
  fi
  cargo_test "$package / $test_target" -p "$package" --test "$test_target"
done
cargo_test "ottam-core / jsonl_protocol" -p ottam-core --test jsonl_protocol -- --test-threads=4
cargo_test "workspace doctests" --workspace --doc
printf "\nIntegration and lint\n"
cargo build ${quiet[@]+"${quiet[@]}"} -p ottam-core
if [[ "$rust_only" == 0 ]]; then
  OTTAM_BACKEND_CLIENT_INTEGRATION_CORE="$PWD/target/debug/ottam-core" \
  OTTAM_REPO_ROOT="$(cd .. && pwd)" \
    swift run ${quiet[@]+"${quiet[@]}"} --package-path ../app --scratch-path ../.build/swift OttamBackendClientTests
fi
cargo clippy ${quiet[@]+"${quiet[@]}"} --all-targets -- -D warnings
echo "PASS  Clippy"
