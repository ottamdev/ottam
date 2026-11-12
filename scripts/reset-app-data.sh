#!/usr/bin/env bash
# Reset the canonical install without following data-directory symlinks.
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/reset-app-data.sh [--db-only] [--dry-run | --yes]

Delete ~/.ottam (including chats, attachments, managed workspace files,
and installed runtimes/agents), plus the canonical app's macOS preferences,
terminal configuration, caches, saved window state, and logs.

Quit Ottam first. By default, show the paths and ask for confirmation.
  --db-only  Back up and reset SQLite only; preserve agents and app settings
  --dry-run  Show paths without changing anything
  --yes      Reset without prompting

With --db-only, move ottam.sqlite, payloads.sqlite, and their WAL/SHM/journal files
into ~/.ottam/backups/db-reset-<timestamp>-<unique suffix>/ (requires python3).
The next launch creates an empty database with no saved projects, workspaces,
or chats. Agent installations, sign-ins, configuration, attachment/workspace
files, and macOS preferences stay in place.

Keeps the app bundle, source repositories outside ~/.ottam, shared
Codex/Claude sign-ins, and isolated development/test instances. Custom
OTTAM_HOME or OTTAM_LOG_DIR locations are not reset; unset these
variables to explicitly reset the standard install.
USAGE
}

mode=prompt
scope=all
while [ "$#" -gt 0 ]; do
  case "$1" in
    --db-only) scope=database ;;
    --dry-run|--yes)
      [ "$mode" = prompt ] || { usage >&2; exit 64; }
      if [ "$1" = --dry-run ]; then mode=preview; else mode=delete; fi
      ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 64 ;;
  esac
  shift
done
[ -z "${OTTAM_HOME:-}${OTTAM_LOG_DIR:-}" ] || {
  echo 'Unset OTTAM_HOME and OTTAM_LOG_DIR to reset the standard install.' >&2
  exit 64
}
[ -n "${HOME:-}" ] && [ -d "$HOME" ] && [ "$HOME" != / ] || {
  echo 'Refusing reset: HOME must be an existing user home directory.' >&2
  exit 64
}
user_home="$(cd "$HOME" && pwd -P)"
[ "$user_home" != / ] || exit 64
domain=dev.ottam.Ottam
library="$user_home/Library"
if [ "$scope" = database ]; then
  [ ! -L "$user_home/.ottam" ] || {
    echo "Refusing symlinked parent: $user_home/.ottam" >&2; exit 1;
  }
  targets=()
  for name in ottam.sqlite ottam.sqlite-wal ottam.sqlite-shm ottam.sqlite-journal payloads.sqlite payloads.sqlite-wal payloads.sqlite-shm payloads.sqlite-journal; do
    targets+=("$user_home/.ottam/$name")
  done
  echo 'Back up and reset only these SQLite files if present:'
  printf '  %s\n' "${targets[@]}"
  echo "Backup destination: $user_home/.ottam/backups/db-reset-<timestamp>-<unique suffix>/"
  echo 'Agent installations, sign-ins, settings, and attachment/workspace files will be preserved.'
else
  targets=("$user_home/.ottam")
  for directory in 'Application Support' Caches 'Saved Application State' HTTPStorages WebKit Logs Preferences Preferences/ByHost; do
    # A leaf symlink can be unlinked safely, but never traverse a redirected parent.
    for parent in "$library" "$library/${directory%%/*}" "$library/$directory"; do
      [ ! -L "$parent" ] || { echo "Refusing symlinked parent: $parent" >&2; exit 1; }
    done
  done
  for directory in 'Application Support' Caches HTTPStorages WebKit Logs; do
    targets+=("$library/$directory/$domain")
  done
  targets+=("$library/Saved Application State/$domain.savedState")
  targets+=("$library/Preferences/$domain.plist")
  shopt -s nullglob
  for path in "$library/Preferences/ByHost/$domain."*.plist; do
    targets+=("$path")
  done

  echo 'Reset canonical Ottam preferences and remove these paths if present:'
  printf '  %s\n' "${targets[@]}"
fi
[ "$mode" != preview ] || exit 0

# Fail closed if process inspection is unavailable. Refuse all Ottam
# instances because a core may use the canonical home through an override.
processes="$(ps -axo comm=)"
running="$(printf '%s\n' "$processes" | awk '
  { sub(/^.*\//, ""); sub(/^[[:space:]]+/, "") }
  /^(Ottam|OttamApp|ottam-core|OttamDev-.*|OttamTest-.*)$/ { print }
')"
if [ -n "$running" ]; then
  echo 'Quit Ottam and stop its ottam-core processes before resetting.' >&2
  exit 1
fi
if [ "$mode" = prompt ]; then
  if [ "$scope" = database ]; then
    printf 'Back up SQLite and start with an empty database? Type reset to continue: '
  else
    printf 'Permanently delete this data? Type reset to continue: '
  fi
  IFS= read -r answer || exit 1
  [ "$answer" = reset ] || { echo 'Cancelled.'; exit 1; }
fi

if [ "$scope" = database ]; then
  exec python3 "$(dirname "${BASH_SOURCE[0]}")/lib/reset_database.py" "$user_home/.ottam"
fi

# Use defaults first so cfprefsd cannot restore the old preference values.
if defaults read "$domain" >/dev/null 2>&1; then
  defaults delete "$domain"
fi
if defaults -currentHost read "$domain" >/dev/null 2>&1; then
  defaults -currentHost delete "$domain"
fi
for path in "${targets[@]}"; do
  rm -rf -- "$path"
  [ ! -e "$path" ] && [ ! -L "$path" ] || { echo "Could not remove: $path" >&2; exit 1; }
done
echo 'Ottam data reset. Launch the app to start fresh.'
