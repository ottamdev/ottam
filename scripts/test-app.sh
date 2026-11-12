#!/usr/bin/env bash
# Swift unit tests for the app's pure logic targets (swift-testing), on the
# Xcode toolchain that lib/xcode-developer-dir.sh selects.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

verbose=0
args=()
for arg in "$@"; do
  if [[ "$arg" == --verbose ]]; then verbose=1; else args+=("$arg"); fi
done

source scripts/lib/xcode-developer-dir.sh

# Don't `exec`: the exit status alone is not a verdict. A test that tears the
# process down mid-run (see docs, `userScrollControlsPinning`) leaves `swift
# test` reporting 0 with the run unfinished and the summary line missing, so a
# suite with real failures looks green. Keep the output and insist on a summary.
log="$(mktemp "${TMPDIR:-/tmp}/ottam-test-app.XXXXXX")"
trap 'rm -f "$log"' EXIT

set +e
swift test \
  --package-path app \
  --scratch-path .build/swift-tests \
  -Xswiftc -warnings-as-errors \
  ${args[@]+"${args[@]}"} 2>&1 | tee "$log" | awk -v verbose="$verbose" '
  function emit(line) { print line; fflush() }
  verbose { emit($0); next }
  # Parameterized test starts can contain newlines inside argument values.
  /^◇ Test case / { pending = $0; if ($0 ~ /started[.]$/) pending = ""; next }
  pending != "" {
    if ($0 ~ /^(◇|✔|✘|warning:|error:)/) { emit(pending); pending = "" }
    else { pending = pending "\n" $0; if ($0 ~ /started[.]$/) pending = ""; next }
  }
  /^(◇|✔) (Test|Suite) / && /(started[.]|passed.*[.])$/ && !/Test run / { next }
  { emit($0) }
  END { if (pending != "") emit(pending) }
'
status=${PIPESTATUS[0]}
set -e

if grep -q 'Test run started' "$log" && ! grep -q 'Test run with .* \(passed\|failed\)' "$log"; then
  echo >&2
  echo "test-app.sh: the run started but never reported a summary — it died partway" >&2
  echo "through. Whatever status swift test returned ($status) is meaningless; the" >&2
  echo "tests after the last one listed above did not run." >&2
  status=70
fi

# `swift test --filter` that matches nothing exits 0 with only a warning; a
# filter typo must not look green.
if (( status == 0 )) && [[ " ${args[*]-} " == *" --filter "* ]] &&
   grep -q 'No matching test cases were run' "$log"; then
  echo "test-app.sh: 0 tests matched the filter" >&2
  status=65
fi

mkdir -p .build
cp "$log" .build/test-app-last.log
echo 'test-app.sh: full log: .build/test-app-last.log' >&2
if (( status != 0 )); then
  tail -n 60 "$log" >&2
fi
exit "$status"
