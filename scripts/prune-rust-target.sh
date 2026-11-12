#!/usr/bin/env bash
set -euo pipefail

# Cargo never deletes stale artifacts: every dependency, feature, flag, or
# toolchain change writes new hashed copies beside the old ones, so a checkout
# that switches branches grows its target dir without bound. Rust build entry
# points call this first; once the target dir passes the cap it runs
# `cargo clean`, trading one cold build for the space.
#
#     OTTAM_RUST_TARGET_MAX_GB=40 ./scripts/prune-rust-target.sh
#
# The cap defaults to 20 GB, accepts decimals, and 0 disables pruning.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
max_gb="${OTTAM_RUST_TARGET_MAX_GB:-20}"
target_dir="${CARGO_TARGET_DIR:-$ROOT_DIR/backend/target}"

if ! awk -v n="$max_gb" 'BEGIN { exit !(n ~ /^[0-9]+([.][0-9]+)?$/) }'; then
  echo "OTTAM_RUST_TARGET_MAX_GB must be a number of GB, got '$max_gb'" >&2
  exit 64
fi
max_kb="$(awk -v gb="$max_gb" 'BEGIN { printf "%d", gb * 1048576 }')"
if (( max_kb == 0 )) || [[ ! -d "$target_dir" ]]; then
  exit 0
fi

used_kb="$(du -sk "$target_dir" | awk '{ print $1 }')"
if (( used_kb <= max_kb )); then
  exit 0
fi
used_gb="$(awk -v kb="$used_kb" 'BEGIN { printf "%.1f", kb / 1048576 }')"
echo "prune-rust-target: $target_dir is $used_gb GB, over the $max_gb GB cap; running cargo clean (next build is cold)" >&2
cargo clean --quiet --manifest-path "$ROOT_DIR/backend/Cargo.toml"
