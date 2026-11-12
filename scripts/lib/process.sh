# Process identity shared by the launcher and UI-test lifecycles.
#
# A process that exits between a pid listing and its lookup is the normal
# case, not an error: the lookup answers with empty output and succeeds, so a
# caller under `set -e -o pipefail` keeps sweeping instead of dying silently.
# The start time tells a live process from a later one that reused its pid.
process_start_identity() {
  ps -p "$1" -o lstart= 2>/dev/null | sed 's/^[[:space:]]*//;s/[[:space:]]*$//' || true
}
