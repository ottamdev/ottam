#!/usr/bin/env bash
set -euo pipefail

# Provision a fresh git worktree with the heavyweight, gitignored artifacts a
# build needs, symlinked from the main checkout instead of re-fetched: a
# worktree exists to isolate *source*, and a 500MB pinned binary is the same
# bytes wherever it lives. Run from anywhere inside the worktree:
#
#     ./scripts/setup-worktree.sh
#
# `--links-only` skips the git hooks and prints only what it links. The
# SessionStart hook in .claude/settings.json runs it that way, so a session
# never changes git config on its own.
#
# Safe to re-run; each item is skipped once present. Add new entries at the
# bottom as builds grow new heavyweight dependencies.

links_only=0
case "${1:-}" in
  "") ;;
  --links-only) links_only=1 ;;
  *) echo "usage: $0 [--links-only]" >&2; exit 64 ;;
esac

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[ "$links_only" -eq 1 ] || "$ROOT_DIR/scripts/install-git-hooks.sh"
MAIN_DIR="$(git -C "$ROOT_DIR" worktree list --porcelain | head -1 | sed 's/^worktree //')"

if [ "$ROOT_DIR" = "$MAIN_DIR" ]; then
  [ "$links_only" -eq 1 ] || echo "already in the main checkout; nothing to link"
  exit 0
fi

link() {
  local rel="$1"
  local src="$MAIN_DIR/$rel"
  local dst="$ROOT_DIR/$rel"
  if [ -L "$dst" ] && [ ! -e "$dst" ]; then
    # Dangling link — the main checkout moved or lost the artifact; relink.
    rm "$dst"
  elif [ -e "$dst" ]; then
    [ "$links_only" -eq 1 ] || echo "exists:  $rel"
    return 0
  fi
  mkdir -p "$(dirname "$dst")"
  ln -s "$src" "$dst"
  echo "linked:  $rel"
}

# Pinned Ghostty build and resources. Without a copy to share, the build
# scripts run scripts/fetch-ghosttykit.sh, which installs one in this worktree.
if [ -e "$MAIN_DIR/app/Vendor/ottam-ghostty" ]; then
  link app/Vendor/ottam-ghostty
fi

# Maintainers' private checkout, when the main checkout has one.
if [ -e "$MAIN_DIR/private" ]; then
  link private
fi
