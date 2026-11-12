#!/usr/bin/env bash
# Exercise destructive reset behavior only in a disposable home with mock OS tools.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fixture="$(mktemp -d "${TMPDIR:-/tmp}/ottam-reset-test.XXXXXX")"
fixture="$(cd "$fixture" && pwd -P)"
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/bin" "$fixture/home" "$fixture/outside"
cat > "$fixture/bin/ps" <<'SH'
#!/usr/bin/env bash
[ "${TEST_PS_FAILURE:-0}" = 0 ] || exit 1
printf '%s\n' "${TEST_PROCESS:-/usr/bin/unrelated}"
SH
cat > "$fixture/bin/defaults" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$HOME/defaults-calls"
SH
chmod +x "$fixture/bin/ps" "$fixture/bin/defaults"
export HOME="$fixture/home" PATH="$fixture/bin:$PATH"
unset OTTAM_HOME OTTAM_LOG_DIR
script="$root/scripts/reset-app-data.sh"
domain=dev.ottam.Ottam
mkdir -p "$HOME/.ottam/workspaces" "$HOME/.codex" \
  "$HOME/Library/Application Support/$domain" \
  "$HOME/Library/Application Support/$domain.dev.fixture" \
  "$HOME/Library/Preferences/ByHost" \
  "$HOME/Library/Saved Application State/$domain.savedState"
touch "$HOME/.ottam/workspaces/attachment" "$HOME/.codex/auth.json" \
  "$HOME/Library/Preferences/$domain.plist" \
  "$HOME/Library/Preferences/ByHost/$domain.fixture.plist" \
  "$HOME/Library/Preferences/unrelated.plist" "$fixture/outside/keep"

"$script" --dry-run > /dev/null
[ -f "$HOME/.ottam/workspaces/attachment" ]
[ ! -e "$HOME/defaults-calls" ]
if printf 'no\n' | "$script" > /dev/null; then exit 1; fi
[ -f "$HOME/.ottam/workspaces/attachment" ]
if TEST_PROCESS=/Applications/Ottam.app/Contents/MacOS/Ottam "$script" --yes > /dev/null 2>&1; then exit 1; fi
[ -f "$HOME/.ottam/workspaces/attachment" ]
if TEST_PS_FAILURE=1 "$script" --yes > /dev/null 2>&1; then exit 1; fi
[ -f "$HOME/.ottam/workspaces/attachment" ]
if OTTAM_HOME="$fixture/outside" "$script" --yes > /dev/null 2>&1; then exit 1; fi
[ ! -e "$HOME/defaults-calls" ] || {
  # The cancelled and blocked runs must not call defaults either.
  echo 'A non-destructive run changed preferences.' >&2; exit 1;
}

# Database-only resets preserve managed agents, settings, and workspace bytes.
db_files=(ottam.sqlite ottam.sqlite-wal ottam.sqlite-shm ottam.sqlite-journal payloads.sqlite payloads.sqlite-wal payloads.sqlite-shm payloads.sqlite-journal)
mkdir -p "$HOME/.ottam/agents/claude/home" "$HOME/.ottam/agents/claude/adapter"
printf 'agent config\n' > "$HOME/.ottam/agents/claude/home/config"
printf 'installed adapter\n' > "$HOME/.ottam/agents/claude/adapter/manifest.json"
printf 'model catalog\n' > "$HOME/.ottam/model-catalog.json"
printf 'terminal settings\n' > "$HOME/Library/Application Support/$domain/config.ghostty"
for name in "${db_files[@]}"; do
  printf 'original %s\n' "$name" > "$HOME/.ottam/$name"
done
"$script" --db-only --dry-run > "$fixture/db-preview"
"$script" --dry-run --db-only > /dev/null
grep -q 'backups/db-reset-' "$fixture/db-preview"
if printf 'no\n' | "$script" --db-only > /dev/null; then exit 1; fi
if "$script" --db-only --yes --dry-run > /dev/null 2>&1; then exit 1; fi
if TEST_PROCESS=ottam-core "$script" --db-only --yes > /dev/null 2>&1; then exit 1; fi
if TEST_PS_FAILURE=1 "$script" --db-only --yes > /dev/null 2>&1; then exit 1; fi
if OTTAM_HOME="$fixture/outside" "$script" --db-only --yes > /dev/null 2>&1; then exit 1; fi
if OTTAM_LOG_DIR="$fixture/outside" "$script" --db-only --yes > /dev/null 2>&1; then exit 1; fi
[ ! -e "$HOME/.ottam/backups" ]

# The ownership lock must block a reset even when ps reports no Core process.
python3 - "$HOME/.ottam/core.lock" "$script" <<'PY'
import fcntl, subprocess, sys
with open(sys.argv[1], "w") as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    result = subprocess.run([sys.argv[2], "--db-only", "--yes"],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if result.returncode == 0:
        sys.exit("Database reset ignored the core ownership lock.")
PY
[ ! -e "$HOME/.ottam/backups" ]
[ -f "$HOME/.ottam/ottam.sqlite" ]

# Reject redirected database files, the lock, and the backup parent before moving anything.
for name in "${db_files[@]}" core.lock; do
  mv "$HOME/.ottam/$name" "$fixture/original-file"
  ln -s "$fixture/outside/keep" "$HOME/.ottam/$name"
  if "$script" --db-only --yes > /dev/null 2>&1; then exit 1; fi
  [ -f "$fixture/outside/keep" ]
  [ ! -e "$HOME/.ottam/backups" ]
  rm "$HOME/.ottam/$name"
  mv "$fixture/original-file" "$HOME/.ottam/$name"
done
ln -s "$fixture/outside" "$HOME/.ottam/backups"
if "$script" --db-only --yes > /dev/null 2>&1; then exit 1; fi
[ -f "$HOME/.ottam/ottam.sqlite" ]
rm "$HOME/.ottam/backups"

# A filesystem failure after moving the main DB must restore the whole set.
# Python imports sitecustomize from PYTHONPATH at startup, so the fault is
# injected without a test seam in the reset itself.
mkdir "$fixture/fail-rename"
cat > "$fixture/fail-rename/sitecustomize.py" <<'PY'
import errno, os
original_rename = os.rename
def rename(source, destination, *args, **kwargs):
    if str(source).endswith("ottam.sqlite-wal"):
        raise OSError(errno.EIO, "injected WAL move failure")
    return original_rename(source, destination, *args, **kwargs)
os.rename = rename
PY
if PYTHONPATH="$fixture/fail-rename" "$script" --db-only --yes > "$fixture/failed-reset" 2>&1; then exit 1; fi
grep -q 'injected WAL move failure' "$fixture/failed-reset"
for name in "${db_files[@]}"; do
  printf 'original %s\n' "$name" > "$fixture/expected"
  cmp "$fixture/expected" "$HOME/.ottam/$name"
done

# Confirming the prompt backs up every SQLite file byte-for-byte.
printf 'reset\n' | "$script" --db-only > "$fixture/db-reset-output"
backups=("$HOME/.ottam/backups/"db-reset-*)
[ "${#backups[@]}" -eq 1 ]
for name in "${db_files[@]}"; do
  [ ! -e "$HOME/.ottam/$name" ]
  printf 'original %s\n' "$name" > "$fixture/expected"
  cmp "$fixture/expected" "${backups[0]}/$name"
done
grep -Fq "Database backup: ${backups[0]}" "$fixture/db-reset-output"
[ -f "$HOME/.ottam/workspaces/attachment" ]
[ "$(cat "$HOME/.ottam/agents/claude/home/config")" = 'agent config' ]
[ "$(cat "$HOME/.ottam/agents/claude/adapter/manifest.json")" = 'installed adapter' ]
[ "$(cat "$HOME/.ottam/model-catalog.json")" = 'model catalog' ]
[ "$(cat "$HOME/Library/Application Support/$domain/config.ghostty")" = 'terminal settings' ]
[ -f "$HOME/Library/Preferences/$domain.plist" ]
[ -f "$HOME/Library/Preferences/ByHost/$domain.fixture.plist" ]
[ -d "$HOME/Library/Saved Application State/$domain.savedState" ]
[ ! -e "$HOME/defaults-calls" ]

# An empty reset is a no-op; a later reset creates a separate backup.
"$script" --yes --db-only > /dev/null
backups=("$HOME/.ottam/backups/"db-reset-*)
[ "${#backups[@]}" -eq 1 ]
printf 'new database\n' > "$HOME/.ottam/ottam.sqlite"
"$script" --db-only --yes > /dev/null
backups=("$HOME/.ottam/backups/"db-reset-*)
[ "${#backups[@]}" -eq 2 ]
[ ! -e "$HOME/.ottam/ottam.sqlite" ]
[ ! -e "$HOME/defaults-calls" ]

"$script" --yes > /dev/null
[ ! -e "$HOME/.ottam" ]
[ ! -e "$HOME/Library/Application Support/$domain" ]
[ ! -e "$HOME/Library/Preferences/$domain.plist" ]
[ ! -e "$HOME/Library/Preferences/ByHost/$domain.fixture.plist" ]
[ ! -e "$HOME/Library/Saved Application State/$domain.savedState" ]
[ -d "$HOME/Library/Application Support/$domain.dev.fixture" ]
[ -f "$HOME/.codex/auth.json" ]
[ -f "$HOME/Library/Preferences/unrelated.plist" ]
grep -q '^delete dev.ottam.Ottam$' "$HOME/defaults-calls"

# A data leaf symlink is removed, never followed to external files.
ln -s "$fixture/outside" "$HOME/.ottam"
if "$script" --db-only --yes > /dev/null 2>&1; then exit 1; fi
[ -L "$HOME/.ottam" ]
"$script" --yes > /dev/null
[ ! -L "$HOME/.ottam" ]
[ -f "$fixture/outside/keep" ]
"$script" --yes > /dev/null
"$script" --db-only --yes > /dev/null
[ ! -e "$HOME/.ottam" ]

# A redirected Library parent is rejected before any deletion.
mv "$HOME/Library" "$HOME/Library-original"
ln -s "$HOME/Library-original" "$HOME/Library"
mkdir "$HOME/.ottam"
if "$script" --yes > /dev/null 2>&1; then exit 1; fi
[ -d "$HOME/.ottam" ]
# Database-only mode never accesses Library, including redirected parents.
touch "$HOME/.ottam/ottam.sqlite"
"$script" --db-only --yes > /dev/null
[ ! -e "$HOME/.ottam/ottam.sqlite" ]
echo 'Reset tests passed (full and database-only reset, previews, confirmation, live processes, core lock, backups, preservation, idempotence, symlinks).'
