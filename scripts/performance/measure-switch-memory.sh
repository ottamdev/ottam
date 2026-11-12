#!/bin/bash
# Reads the app's memory footprint after chat.measure-switch-cache-memory
# streams the multi-turn corpus into ten conversations and returns to the
# first. Compare capacities on the same build:
#
#   scripts/performance/measure-switch-memory.sh 10 .build/performance/switch-memory/cap10
#   scripts/performance/measure-switch-memory.sh 1 .build/performance/switch-memory/cap1
#
# Run from any directory except .build/ui-tests/current, which each run replaces.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
if [ "$#" -ne 2 ]; then
  echo "usage: measure-switch-memory.sh <cache-capacity> <output-dir>" >&2
  exit 2
fi
CAPACITY="$1"
OUT="$2"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
touch "$OUT/run-start"

export OTTAM_UI_TEST_SWITCH_CACHE_CAPACITY="$CAPACITY"
# The wire shape does not affect the parked footprint; fast chunks keep a run
# near six minutes.
export OTTAM_UI_TEST_STREAM_CHUNK_BYTES="${OTTAM_UI_TEST_STREAM_CHUNK_BYTES:-1024}"
export OTTAM_UI_TEST_STREAM_DELAY_MS="${OTTAM_UI_TEST_STREAM_DELAY_MS:-5}"
export OTTAM_UI_TEST_MEASUREMENT_READY_FILE="$OUT/harness-ready"
export OTTAM_UI_TEST_MEASUREMENT_PHASE_START_ACK_FILE="$OUT/phase-start-ack"
export OTTAM_UI_TEST_MEASUREMENT_PHASE_END_ACK_FILE="$OUT/phase-end-ack"
env | LC_ALL=C sort | grep '^OTTAM_UI_TEST_' > "$OUT/environment.txt"
git -C "$ROOT" rev-parse HEAD > "$OUT/source-commit.txt"
git -C "$ROOT" status --porcelain=v1 > "$OUT/source-status.txt"

cd "$ROOT"
OTTAM_UI_TEST_CONFIGURATION=profile \
  "$ROOT/scripts/ui-test.sh" run chat.measure-switch-cache-memory > "$OUT/scenario.out" 2>&1 &
SCENARIO_PID=$!

# The same source-app discovery as measure-transcript.sh.
APP_PID=""
for _ in $(seq 1 4800); do
  for request in "$ROOT/.build/ui-tests/current/control/"*.request.json; do
    [ -f "$request" ] || continue
    [ "$request" -nt "$OUT/run-start" ] || continue
    candidate="$(jq -r '.parameters.pid // empty' "$request" 2>/dev/null || true)"
    if [ -n "$candidate" ] && kill -0 "$candidate" 2>/dev/null; then
      APP_PID="$candidate"
      break
    fi
  done
  [ -n "$APP_PID" ] && break
  kill -0 "$SCENARIO_PID" 2>/dev/null || break
  sleep 0.25
done
if [ -z "$APP_PID" ]; then
  echo "source app process not found" > "$OUT/error.txt"
  wait "$SCENARIO_PID"
  exit 1
fi
echo "$APP_PID" > "$OUT/app-pid.txt"
# Proves which capacity the app actually received.
ps eww -o command= -p "$APP_PID" | tr ' ' '\n' \
  | grep '^OTTAM_DEBUG_CONVERSATION_CACHE=' > "$OUT/app-cache-env.txt" \
  || echo "unset" > "$OUT/app-cache-env.txt"
touch "$OUT/harness-ready"

actions="$ROOT/.build/ui-tests/current/logs/actions.jsonl"
started=false
for _ in $(seq 1 14400); do
  if grep -q '"event":"measurement_phase_started"' "$actions" 2>/dev/null; then
    started=true
    break
  fi
  kill -0 "$APP_PID" 2>/dev/null || break
  sleep 0.25
done
if [ "$started" != true ]; then
  echo "scenario measurement phase did not start" > "$OUT/error.txt"
  wait "$SCENARIO_PID"
  exit 1
fi

for sample in 1 2 3; do
  footprint "$APP_PID" > "$OUT/footprint-$sample.txt" 2>&1
  sleep 2
done
vmmap --summary "$APP_PID" > "$OUT/vmmap-summary.txt" 2>&1
touch "$OUT/phase-start-ack"
for _ in $(seq 1 240); do
  grep -q '"event":"measurement_phase_ended"' "$actions" 2>/dev/null && break
  sleep 0.25
done
touch "$OUT/phase-end-ack"
wait "$SCENARIO_PID"
scenario_exit=$?
echo "$scenario_exit" > "$OUT/scenario-exit.txt"
jq -r '.classification // "missing"' "$ROOT/.build/ui-tests/current/result.json" \
  > "$OUT/result.txt" 2>/dev/null
grep -h 'Footprint:' "$OUT"/footprint-*.txt
cat "$OUT/app-cache-env.txt" "$OUT/result.txt"
exit "$scenario_exit"
