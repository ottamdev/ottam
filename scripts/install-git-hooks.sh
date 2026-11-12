#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
configured="$(git config --type=path --get core.hooksPath || true)"
if [[ -n "$configured" && "$configured" != .githooks ]]; then
  # Worktrees share the main checkout's config, which may hold an absolute path
  # to its hooks. Compare resolved directories so that counts as installed.
  resolve() { (cd "$1" 2>/dev/null && pwd -P) || true; }
  main_dir="$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")"
  resolved="$(resolve "$configured")"
  if [[ -n "$resolved" && ("$resolved" == "$(resolve .githooks)" || "$resolved" == "$(resolve "$main_dir/.githooks")") ]]; then
    echo "Lightweight pre-commit checks already installed ($configured)."
    exit 0
  fi
  echo "Git hooks not installed: core.hooksPath is already '$configured'. Reconcile it explicitly first." >&2
  exit 1
fi
git config --local core.hooksPath .githooks
echo 'Installed lightweight pre-commit checks (.githooks).'
