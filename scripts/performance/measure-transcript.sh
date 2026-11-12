#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DEFAULT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

usage() {
  cat <<'USAGE'
Usage:
  measure-transcript.sh --run NAME --variant LABEL --scenario ID [options]

Required:
  --run NAME          Unique immutable kebab-case run name
  --variant LABEL     Comparison variant, such as baseline or feature
  --scenario ID       Registered transcript UI scenario

Options:
  --root PATH         Ottam checkout to build and measure (default: this checkout)
  --pair LABEL        Matched-pair label, such as 1, 2, or 3
  --focus MODE        active or inactive (default: active)
  --template NAME     Logging, Time Profiler, or CPU Counters (default: Logging)
  --output-root PATH  Run parent directory (default: <root>/.build/performance/runs)
  -h, --help          Show this help

Streaming fixture knobs are passed through from the environment, including:
  OTTAM_UI_TEST_STREAM_CHUNK_BYTES
  OTTAM_UI_TEST_STREAM_DELAY_MS
  OTTAM_UI_TEST_STREAM_SAMPLE_SECONDS
  OTTAM_UI_TEST_STREAM_DETACH_VIEWPORTS
USAGE
}

ROOT="$DEFAULT_ROOT"
RUN=""
VARIANT=""
SCENARIO=""
PAIR=""
FOCUS_MODE="active"
TRACE_TEMPLATE="Logging"
OUTPUT_ROOT=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --root) ROOT="${2:-}"; shift 2 ;;
    --run) RUN="${2:-}"; shift 2 ;;
    --variant) VARIANT="${2:-}"; shift 2 ;;
    --scenario) SCENARIO="${2:-}"; shift 2 ;;
    --pair) PAIR="${2:-}"; shift 2 ;;
    --focus) FOCUS_MODE="${2:-}"; shift 2 ;;
    --template) TRACE_TEMPLATE="${2:-}"; shift 2 ;;
    --output-root) OUTPUT_ROOT="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [ -z "$RUN" ] || [ -z "$VARIANT" ] || [ -z "$SCENARIO" ]; then
  echo "error: --run, --variant, and --scenario are required" >&2
  usage >&2
  exit 2
fi
case "$RUN" in
  ''|*[!a-z0-9-]*|-*|*-|*--*)
    echo "error: run name must be descriptive kebab-case: $RUN" >&2
    exit 2
    ;;
esac
case "$VARIANT" in
  *[!A-Za-z0-9._-]*|'') echo "error: invalid variant: $VARIANT" >&2; exit 2 ;;
esac
case "$PAIR" in
  *[!A-Za-z0-9._-]*) echo "error: invalid pair label: $PAIR" >&2; exit 2 ;;
esac
case "$FOCUS_MODE" in
  active|inactive) ;;
  *) echo "error: --focus must be active or inactive" >&2; exit 2 ;;
esac
case "$TRACE_TEMPLATE" in
  Logging|'Time Profiler'|'CPU Counters') ;;
  *) echo "error: unsupported trace template: $TRACE_TEMPLATE" >&2; exit 2 ;;
esac

ROOT="$(cd "$ROOT" && pwd)"
if [ ! -x "$ROOT/scripts/ui-test.sh" ]; then
  echo "error: Ottam checkout not found at $ROOT" >&2
  exit 2
fi
if [ -z "$OUTPUT_ROOT" ]; then
  OUTPUT_ROOT="$ROOT/.build/performance/runs"
fi
OUT="$OUTPUT_ROOT/$RUN"
if [ -e "$OUT" ]; then
  echo "error: run directory already exists: $OUT" >&2
  exit 2
fi
mkdir -p "$OUT"
touch "$OUT/run-start"

export OTTAM_UI_TEST_STREAM_CHUNK_BYTES="${OTTAM_UI_TEST_STREAM_CHUNK_BYTES:-64}"
export OTTAM_UI_TEST_STREAM_DELAY_MS="${OTTAM_UI_TEST_STREAM_DELAY_MS:-20}"
export OTTAM_UI_TEST_STREAM_SAMPLE_SECONDS="${OTTAM_UI_TEST_STREAM_SAMPLE_SECONDS:-12}"
export OTTAM_UI_TEST_STREAM_DETACH_VIEWPORTS="${OTTAM_UI_TEST_STREAM_DETACH_VIEWPORTS:-3}"
export OTTAM_UI_TEST_MEASUREMENT_READY_FILE="$OUT/harness-ready"
export OTTAM_UI_TEST_MEASUREMENT_PHASE_START_ACK_FILE="$OUT/phase-start-ack"
export OTTAM_UI_TEST_MEASUREMENT_PHASE_END_ACK_FILE="$OUT/phase-end-ack"

LOG_PID=""
TRACE_PID=""
SCENARIO_PID=""
SAMPLE_PID=""

cleanup() {
  [ -z "$SAMPLE_PID" ] || kill "$SAMPLE_PID" 2>/dev/null || true
  [ -z "$LOG_PID" ] || kill "$LOG_PID" 2>/dev/null || true
  [ -z "$TRACE_PID" ] || kill -INT "$TRACE_PID" 2>/dev/null || true
  [ -z "$SCENARIO_PID" ] || kill "$SCENARIO_PID" 2>/dev/null || true
}
trap cleanup EXIT

frontmost_asn() {
  lsappinfo front 2>/dev/null || true
}

application_name() {
  local asn="$1"
  [ -n "$asn" ] || { echo unavailable; return; }
  lsappinfo info -only name "$asn" 2>/dev/null \
    | sed -E 's/.*="([^"]*)".*/\1/'
}

monotonic_timestamp() {
  python3 -c 'import time; print(time.clock_gettime(time.CLOCK_MONOTONIC))'
}

wall_timestamp() {
  python3 -c 'import datetime; print(datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ"))'
}

cputime_seconds() {
  awk -F: '
    NF == 3 { print ($1 * 3600) + ($2 * 60) + $3; next }
    NF == 2 { print ($1 * 60) + $2; next }
    { print $1 }
  ' "$1"
}

git -C "$ROOT" rev-parse HEAD > "$OUT/source-commit.txt"
git -C "$ROOT" symbolic-ref --short -q HEAD > "$OUT/source-branch.txt" || true
git -C "$ROOT" status --porcelain=v1 > "$OUT/source-status.txt"
env | LC_ALL=C sort | grep '^OTTAM_UI_TEST_STREAM_' > "$OUT/fixture-environment.txt" || true
shasum -a 256 \
  "$ROOT/app/Sources/OttamTranscriptFixtures/MarkdownCorpus.swift" \
  "$ROOT/app/Sources/OttamTranscriptFixtures/SeededRandom.swift" \
  "$ROOT/app/Sources/OttamTranscriptFixtures/StreamChunking.swift" \
  "$ROOT/app/Sources/OttamTranscriptFixtures/TranscriptFixtureCatalog.swift" \
  > "$OUT/fixture-source-sha256.txt"
fixture_source_sha256="$(awk '{ print $1 }' "$OUT/fixture-source-sha256.txt" | shasum -a 256 | awk '{ print $1 }')"
sw_vers > "$OUT/os.txt"
xcodebuild -version > "$OUT/xcode.txt"
system_profiler SPHardwareDataType > "$OUT/hardware.txt"
system_profiler SPDisplaysDataType > "$OUT/displays.txt"
pmset -g batt > "$OUT/power.txt" 2>&1 || true
pmset -g therm > "$OUT/thermal.txt" 2>&1 || true
machine_identity_sha256="$(shasum -a 256 "$OUT/os.txt" "$OUT/xcode.txt" "$OUT/hardware.txt" | awk '{ print $1 }' | shasum -a 256 | awk '{ print $1 }')"

/usr/bin/log stream --style compact --signpost \
  --predicate 'subsystem == "dev.ottam.Ottam"' \
  > "$OUT/signposts.log" 2>&1 &
LOG_PID=$!

OTTAM_UI_TEST_CONFIGURATION=profile \
  "$ROOT/scripts/ui-test.sh" run "$SCENARIO" > "$OUT/scenario.out" 2>&1 &
SCENARIO_PID=$!

APP_PID=""
for _ in $(seq 1 2400); do
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
  sleep 0.25
done
if [ -z "$APP_PID" ]; then
  echo "source app process not found" > "$OUT/error.txt"
  wait "$SCENARIO_PID" || true
  exit 1
fi

APP_ASN="$(lsappinfo find pid="$APP_PID" 2>/dev/null || true)"
if [ -z "$APP_ASN" ]; then
  echo "source app application identity not found" > "$OUT/error.txt"
  wait "$SCENARIO_PID" || true
  exit 1
fi
APP_NAME="$(application_name "$APP_ASN")"

xcrun xctrace record --template "$TRACE_TEMPLATE" --attach "$APP_PID" \
  --output "$OUT/capture.trace" --time-limit 10m --no-prompt \
  > "$OUT/xctrace.out" 2>&1 &
TRACE_PID=$!

trace_ready=false
for _ in $(seq 1 240); do
  if grep -q 'Starting recording' "$OUT/xctrace.out" 2>/dev/null; then
    trace_ready=true
    break
  fi
  kill -0 "$TRACE_PID" 2>/dev/null || break
  sleep 0.05
done
if [ "$trace_ready" != true ]; then
  echo "xctrace did not begin recording" > "$OUT/error.txt"
  wait "$SCENARIO_PID" || true
  exit 1
fi

# Establish the requested focus regime before the scenario-owned phase marker,
# then verify it throughout that phase.
if [ "$FOCUS_MODE" = "inactive" ]; then
  expected_asn=""
  for _ in $(seq 1 20); do
    osascript -e 'tell application "Finder" to activate' >/dev/null
    sleep 0.1
    candidate_asn="$(frontmost_asn)"
    candidate_name="$(application_name "$candidate_asn")"
    if [ "$candidate_name" = "Finder" ]; then
      expected_asn="$candidate_asn"
      break
    fi
  done
  if [ -z "$expected_asn" ]; then
    echo "could not establish inactive focus regime" > "$OUT/error.txt"
    wait "$SCENARIO_PID" || true
    exit 1
  fi
else
  expected_asn=""
fi
touch "$OUT/harness-ready"

actions_log="$ROOT/.build/ui-tests/current/logs/actions.jsonl"
phase_started=false
for _ in $(seq 1 2400); do
  if grep -q '"event":"measurement_phase_started"' "$actions_log" 2>/dev/null; then
    phase_started=true
    break
  fi
  kill -0 "$APP_PID" 2>/dev/null || break
  sleep 0.05
done
if [ "$phase_started" != true ]; then
  echo "scenario measurement phase did not start" > "$OUT/error.txt"
  wait "$SCENARIO_PID" || true
  exit 1
fi

phase_workload="$(jq -r 'select(.event == "measurement_phase_started") | .workload' "$actions_log" | tail -1)"
accepted_count="$(grep -c 'stream.admission.*disposition=accepted' "$OUT/signposts.log" || true)"
if [ "$FOCUS_MODE" = "active" ]; then
  expected_asn="$(frontmost_asn)"
  # The isolated app can adopt its worktree display name after launch. Focus
  # belongs to a process, not that mutable human-readable label.
  front_pid="$(lsappinfo info -only pid "$expected_asn" 2>/dev/null | awk -F= '/"pid"/ { gsub(/[^0-9]/, "", $2); print $2 }')"
  if [ "$front_pid" != "$APP_PID" ]; then
    echo "source app was not frontmost at active phase start" > "$OUT/error.txt"
    wait "$SCENARIO_PID" || true
    exit 1
  fi
  APP_NAME="$(application_name "$expected_asn")"
fi
printf '%s\n' "$expected_asn" > "$OUT/expected-frontmost-asn.txt"
scroll_mode="following"
scroll_stable=true
if [ "$SCENARIO" = "transcript.measure-detached" ]; then
  scroll_mode="detached"
  phase_definition="scenario-marker-after-detach-and-${FOCUS_MODE}-focus-stable"
else
  phase_definition="scenario-marker-after-${FOCUS_MODE}-focus-stable"
fi

front_asn="$(frontmost_asn)"
front_name="$(application_name "$front_asn")"
printf '%s\n' "$front_asn" > "$OUT/frontmost-start-asn.txt"
printf '%s\n' "$front_name" > "$OUT/frontmost-start.txt"
printf '%s\t%s\t%s\n' "$accepted_count" "$front_asn" "$front_name" > "$OUT/frontmost-phase.tsv"

taskpolicy -p "$APP_PID" > "$OUT/taskpolicy-start.txt" 2>&1 || true
ps -o pid=,ppid=,state=,pri=,nice=,pcpu=,time=,etime=,command= -p "$APP_PID" \
  > "$OUT/process-start.txt" 2>&1 || true
if [ -n "$APP_ASN" ]; then
  lsappinfo info "$APP_ASN" > "$OUT/lsappinfo-start.txt" 2>&1 || true
fi
pmset -g assertions > "$OUT/power-assertions-start.txt" 2>&1 || true

phase_start_ordinal=$((accepted_count + 1))
normalized_start_count="$(grep -c 'normalized.publication.*succeeded=true' "$OUT/signposts.log" || true)"
display_start_count="$(grep -c 'display.tick:' "$OUT/signposts.log" || true)"
normalized_end_count="$normalized_start_count"
display_end_count="$display_start_count"
ps -o cputime= -p "$APP_PID" | tr -d ' ' > "$OUT/cputime-start.txt"
monotonic_timestamp > "$OUT/wall-start.txt"
wall_timestamp > "$OUT/phase-start-timestamp.txt"
cp "$OUT/cputime-start.txt" "$OUT/cputime-end.txt"
cp "$OUT/wall-start.txt" "$OUT/wall-end.txt"
cp "$OUT/phase-start-timestamp.txt" "$OUT/phase-end-timestamp.txt"

/usr/bin/sample "$APP_PID" 8 -f "$OUT/sample.txt" >/dev/null 2>&1 &
SAMPLE_PID=$!
touch "$OUT/phase-start-ack"

last_count="$accepted_count"
focus_poll_iteration=0
phase_marker_complete=false
while kill -0 "$APP_PID" 2>/dev/null; do
  accepted_count="$(grep -c 'stream.admission.*disposition=accepted' "$OUT/signposts.log" || true)"
  if [ "$accepted_count" -gt "$last_count" ]; then
    last_count="$accepted_count"
    ps -o pid=,state=,pri=,nice=,pcpu=,time=,etime= -p "$APP_PID" \
      >> "$OUT/process-phase.txt" 2>&1 || true
  fi
  focus_poll_iteration=$((focus_poll_iteration + 1))
  if [ $((focus_poll_iteration % 10)) -eq 0 ]; then
    front_asn="$(frontmost_asn)"
    front_name="$(application_name "$front_asn")"
    printf '%s\t%s\t%s\n' "$accepted_count" "$front_asn" "$front_name" \
      >> "$OUT/frontmost-phase.tsv"
  fi
  if grep -q '"event":"measurement_phase_ended"' "$actions_log" 2>/dev/null; then
    ps -o cputime= -p "$APP_PID" | tr -d ' ' > "$OUT/cputime-end.txt"
    monotonic_timestamp > "$OUT/wall-end.txt"
    wall_timestamp > "$OUT/phase-end-timestamp.txt"
    phase_marker_complete=true
    normalized_end_count="$(grep -c 'normalized.publication.*succeeded=true' "$OUT/signposts.log" || true)"
    display_end_count="$(grep -c 'display.tick:' "$OUT/signposts.log" || true)"
    touch "$OUT/phase-end-ack"
    break
  fi
  sleep 0.1
done

wait "$SAMPLE_PID" 2>/dev/null || true
SAMPLE_PID=""
set +e
wait "$SCENARIO_PID"
scenario_exit=$?
set -e
SCENARIO_PID=""
kill "$LOG_PID" 2>/dev/null || true
LOG_PID=""
if kill -0 "$TRACE_PID" 2>/dev/null; then kill -INT "$TRACE_PID" 2>/dev/null || true; fi
wait "$TRACE_PID" 2>/dev/null || true
TRACE_PID=""

xcrun xctrace export --input "$OUT/capture.trace" --toc \
  --output "$OUT/trace-toc.xml" >/dev/null
case "$TRACE_TEMPLATE" in
  Logging)
    xcrun xctrace export --input "$OUT/capture.trace" \
      --xpath '/trace-toc/run[@number="1"]/data/table[@schema="os-signpost"]' \
      --output "$OUT/signpost-events.xml" >/dev/null
    xcrun xctrace export --input "$OUT/capture.trace" \
      --xpath '/trace-toc/run[@number="1"]/data/table[@schema="os-signpost-interval"]' \
      --output "$OUT/signpost-intervals.xml" >/dev/null
    ;;
  'Time Profiler')
    xcrun xctrace export --input "$OUT/capture.trace" \
      --xpath '/trace-toc/run[@number="1"]/data/table[@schema="time-profile"]' \
      --output "$OUT/time-profile.xml" >/dev/null
    ;;
  'CPU Counters')
    xcrun xctrace export --input "$OUT/capture.trace" \
      --xpath '/trace-toc/run[@number="1"]/data/table[@schema="thread-state"]' \
      --output "$OUT/thread-state.xml" >/dev/null
    ;;
esac

archive_succeeded=true
if ! "$ROOT/scripts/ui-test.sh" archive "$RUN" > "$OUT/archive.out" 2>&1; then
  archive_succeeded=false
fi

front_asn="$(frontmost_asn)"
application_name "$front_asn" > "$OUT/frontmost-end.txt" || true
taskpolicy -p "$APP_PID" > "$OUT/taskpolicy-end.txt" 2>&1 || true
if [ -n "$APP_ASN" ]; then
  lsappinfo info "$APP_ASN" > "$OUT/lsappinfo-end.txt" 2>&1 || true
fi
pmset -g assertions > "$OUT/power-assertions-end.txt" 2>&1 || true

cpu_start="$(cputime_seconds "$OUT/cputime-start.txt")"
cpu_end="$(cputime_seconds "$OUT/cputime-end.txt")"
wall_start="$(cat "$OUT/wall-start.txt")"
wall_end="$(cat "$OUT/wall-end.txt")"
cpu_seconds="$(awk -v start="$cpu_start" -v end="$cpu_end" 'BEGIN { printf "%.3f", end - start }')"
wall_seconds="$(awk -v start="$wall_start" -v end="$wall_end" 'BEGIN { printf "%.3f", end - start }')"
cpu_percent="$(awk -v cpu="$cpu_seconds" -v wall="$wall_seconds" 'BEGIN { if (wall > 0) printf "%.1f", cpu / wall * 100; else print "0.0" }')"
if [ -f "$ROOT/.build/ui-tests/current/result.json" ]; then
  scenario_result="$(jq -r '.classification // "missing"' "$ROOT/.build/ui-tests/current/result.json")"
  scenario_duration="$(jq -r '(.durationMilliseconds // 0) / 1000' "$ROOT/.build/ui-tests/current/result.json")"
else
  scenario_result="runner_exit_$scenario_exit"
  scenario_duration=0
fi
source_commit="$(cat "$OUT/source-commit.txt")"
source_dirty=false
if [ -s "$OUT/source-status.txt" ]; then source_dirty=true; fi
phase_admissions=$((last_count - phase_start_ordinal + 1))
if [ "$phase_admissions" -lt 0 ]; then phase_admissions=0; fi
phase_valid=false
phase_ended_workload="$(jq -r 'select(.event == "measurement_phase_ended") | .workload' "$actions_log" | tail -1)"
if [ "$phase_marker_complete" = true ] &&
   [ -n "$phase_workload" ] && [ "$phase_workload" = "$phase_ended_workload" ] &&
   awk -v wall="$wall_seconds" 'BEGIN { exit !(wall > 0) }'; then
  phase_valid=true
fi
# A completed final snapshot alone does not demonstrate live streaming. Keep
# legacy relay runs comparable, but reject normalized single-reply runs whose
# native clock never advanced beyond its initial tick (for example a mixed
# app/Core build that silently drops incompatible live notifications).
streaming_delivery_valid=null
if [ "$SCENARIO" = "transcript.measure-single-reply" ] &&
   [ "$normalized_end_count" -gt "$normalized_start_count" ]; then
  streaming_delivery_valid=true
  if [ "$((display_end_count - display_start_count))" -le 1 ]; then
    streaming_delivery_valid=false
    phase_valid=false
  fi
fi
body_requests=null
if [ -f "$ROOT/.build/ui-tests/current/logs/backend.log" ]; then
  body_requests="$(grep -c 'request completed.*method=conversation.readTranscriptBody' "$ROOT/.build/ui-tests/current/logs/backend.log" || true)"
fi
focus_deviations="$(awk -F '\t' -v expected="$expected_asn" -v final="$last_count" '
  $1 <= final && $2 != expected { deviations++ }
  END { print deviations + 0 }
' "$OUT/frontmost-phase.tsv")"

jq -n \
  --arg run "$RUN" \
  --arg variant "$VARIANT" \
  --arg pair "$PAIR" \
  --arg root "$ROOT" \
  --arg scenario "$SCENARIO" \
  --arg sourceCommit "$source_commit" \
  --arg focusMode "$FOCUS_MODE" \
  --arg traceTemplate "$TRACE_TEMPLATE" \
  --arg fixtureSourceSHA256 "$fixture_source_sha256" \
  --arg machineIdentitySHA256 "$machine_identity_sha256" \
  --arg phaseDefinition "$phase_definition" \
  --arg phaseWorkload "$phase_workload" \
  --arg scrollMode "$scroll_mode" \
  --arg scenarioResult "$scenario_result" \
  --arg phaseStartTimestamp "$(cat "$OUT/phase-start-timestamp.txt")" \
  --arg phaseEndTimestamp "$(cat "$OUT/phase-end-timestamp.txt")" \
  --arg frontmostStart "$(cat "$OUT/frontmost-start.txt")" \
  --arg frontmostEnd "$(cat "$OUT/frontmost-end.txt")" \
  --arg applicationName "$APP_NAME" \
  --arg streamChunkBytes "${OTTAM_UI_TEST_STREAM_CHUNK_BYTES:-}" \
  --arg streamDelayMilliseconds "${OTTAM_UI_TEST_STREAM_DELAY_MS:-}" \
  --arg streamSampleSeconds "${OTTAM_UI_TEST_STREAM_SAMPLE_SECONDS:-}" \
  --arg streamDetachViewports "${OTTAM_UI_TEST_STREAM_DETACH_VIEWPORTS:-}" \
  --argjson sourceDirty "$source_dirty" \
  --argjson phaseValid "$phase_valid" \
  --argjson archiveSucceeded "$archive_succeeded" \
  --argjson scenarioDurationSeconds "$scenario_duration" \
  --argjson scenarioExitCode "$scenario_exit" \
  --argjson cpuSeconds "$cpu_seconds" \
  --argjson wallSeconds "$wall_seconds" \
  --argjson cpuPercent "$cpu_percent" \
  --argjson phaseAdmissionStartOrdinal "$phase_start_ordinal" \
  --argjson phaseAdmissionEndOrdinal "$last_count" \
  --argjson phaseAdmissions "$phase_admissions" \
  --argjson normalizedPublications "$((normalized_end_count - normalized_start_count))" \
  --argjson displayTicks "$((display_end_count - display_start_count))" \
  --argjson streamingDeliveryValid "$streaming_delivery_valid" \
  --argjson transcriptBodyRequestsInRun "$body_requests" \
  --argjson focusDeviations "$focus_deviations" \
  --argjson scrollStable "$scroll_stable" \
  '{
    run: $run,
    variant: $variant,
    pair: $pair,
    root: $root,
    scenario: $scenario,
    sourceCommit: $sourceCommit,
    sourceDirty: $sourceDirty,
    focusMode: $focusMode,
    traceTemplate: $traceTemplate,
    buildConfiguration: "profile",
    fixtureSourceSHA256: $fixtureSourceSHA256,
    machineIdentitySHA256: $machineIdentitySHA256,
    phaseDefinition: $phaseDefinition,
    phaseWorkload: $phaseWorkload,
    admissionObservationSource: "log-stream-diagnostic-only",
    fixtureKnobs: {
      chunkBytes: $streamChunkBytes,
      delayMilliseconds: $streamDelayMilliseconds,
      sampleSeconds: $streamSampleSeconds,
      detachViewports: $streamDetachViewports
    },
    scenarioResult: $scenarioResult,
    scenarioExitCode: $scenarioExitCode,
    scenarioDurationSeconds: $scenarioDurationSeconds,
    phaseStartTimestamp: $phaseStartTimestamp,
    phaseEndTimestamp: $phaseEndTimestamp,
    phaseValid: $phaseValid,
    archiveSucceeded: $archiveSucceeded,
    cpuSeconds: $cpuSeconds,
    phaseWallSeconds: $wallSeconds,
    cpuPercentOfPhase: $cpuPercent,
    phaseAdmissionStartOrdinal: $phaseAdmissionStartOrdinal,
    phaseAdmissionEndOrdinal: $phaseAdmissionEndOrdinal,
    phaseAdmissions: $phaseAdmissions,
    phaseNormalizedPublications: $normalizedPublications,
    phaseDisplayTicks: $displayTicks,
    streamingDeliveryValid: $streamingDeliveryValid,
    transcriptBodyRequestsInRun: $transcriptBodyRequestsInRun,
    focusDeviations: $focusDeviations,
    focusStable: ($focusDeviations == 0),
    scrollMode: $scrollMode,
    scrollStable: $scrollStable,
    applicationName: $applicationName,
    frontmostStart: $frontmostStart,
    frontmostEnd: $frontmostEnd
  }' > "$OUT/summary.json"

jq -c . "$OUT/summary.json"

if [ "$scenario_result" != "passed" ] || [ "$scenario_exit" -ne 0 ] ||
   [ "$phase_valid" != true ] || [ "$focus_deviations" -ne 0 ] ||
   [ "$scroll_stable" != true ] ||
   [ "$archive_succeeded" != true ]; then
  exit 1
fi
