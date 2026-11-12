#!/usr/bin/env bash
# Contract tests for the repository's own scripts, hooks, and CI selection.
# Each suite runs in disposable directories with fake tools; nothing builds.
# A suite is a file in scripts/tests/: test_<name>.py or <name>.sh, where a
# Python name's underscores become hyphens.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

suites=()
for file in scripts/tests/test_*.py scripts/tests/*.sh; do
  name="${file#scripts/tests/}"
  name="${name#test_}"
  name="${name%.*}"
  suites+=("${name//_/-}")
done

usage() {
  echo "Usage: scripts/test-scripts.sh [suite ...]"
  echo "Runs every suite when none is named. Suites: ${suites[*]}"
}

run_suite() {
  local python="scripts/tests/test_${1//-/_}.py"
  if [ -f "$python" ]; then
    python3 "$python"
  else
    "scripts/tests/$1.sh"
  fi
}

selected=()
for argument in "$@"; do
  case "$argument" in
    -h|--help) usage; exit 0 ;;
  esac
  case " ${suites[*]} " in
    *" $argument "*) selected+=("$argument") ;;
    *) echo "test-scripts.sh: unknown suite '$argument'" >&2; usage >&2; exit 64 ;;
  esac
done
[ "${#selected[@]}" -gt 0 ] || selected=("${suites[@]}")

failed=()
for suite in "${selected[@]}"; do
  echo "== $suite"
  run_suite "$suite" || failed+=("$suite")
done
if [ "${#failed[@]}" -gt 0 ]; then
  echo "test-scripts.sh: failed: ${failed[*]}" >&2
  exit 1
fi
echo "test-scripts.sh: passed: ${selected[*]}"
