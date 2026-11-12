#!/usr/bin/env bash
# Migrations are append-only once committed. Diesel records only a migration's
# version, so a home that already ran one never sees a later edit to it, and
# its schema silently drifts from schema.rs. Add a new migration instead.
#
#   check-migrations.sh --staged      staged changes against HEAD (pre-commit)
#   check-migrations.sh BASE HEAD     HEAD against its merge base with BASE (CI)
#
# Pre-commit is strict because this checkout's own home may already have run a
# migration it committed. CI compares the whole range, so a pull request may
# keep fixing a migration it adds, while a hook bypass that edits one the base
# already has is still caught.
#
# OTTAM_ALLOW_MIGRATION_EDIT=1 skips the check for an approved reset.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

DIRECTORIES=(
  backend/crates/ottam-core/migrations
  backend/crates/ottam-core/src/storage/payload/migrations
)

usage() {
  echo "usage: $0 --staged | $0 BASE HEAD" >&2
  exit 2
}

violations=()

# A change is allowed only when it adds a file to a migration directory that
# did not exist in the parent.
check_change() {
  local parent="$1" status="$2" path="$3" where="$4"
  local directory="${path%/*}"
  if [[ "$status" == A ]] && ! git cat-file -e "$parent:$directory" 2>/dev/null; then
    return
  fi
  violations+=("$where: $status $path")
}

case "${1:-}" in
  --staged)
    [[ $# -eq 1 ]] || usage
    git rev-parse --verify -q HEAD >/dev/null || exit 0
    while IFS=$'\t' read -r status path; do
      check_change HEAD "$status" "$path" "staged"
    done < <(git diff --cached --name-status --no-renames -- "${DIRECTORIES[@]}")
    ;;
  "")
    usage
    ;;
  *)
    [[ $# -eq 2 ]] || usage
    base="$(git merge-base "$1" "$2")"
    changes="$(git diff --name-status --no-renames "$base" "$2" -- "${DIRECTORIES[@]}")"
    while IFS=$'\t' read -r status path; do
      [[ -n "$status" ]] || continue
      check_change "$base" "$status" "$path" "$(git rev-parse --short "$base")..$(git rev-parse --short "$2")"
    done <<<"$changes"
    ;;
esac

if (( ${#violations[@]} == 0 )); then
  exit 0
fi
if [[ "${OTTAM_ALLOW_MIGRATION_EDIT:-}" == 1 ]]; then
  echo "check-migrations: OTTAM_ALLOW_MIGRATION_EDIT=1, allowing:" >&2
  printf '  %s\n' "${violations[@]}" >&2
  exit 0
fi
echo "check-migrations: committed migrations are append-only." >&2
printf '  %s\n' "${violations[@]}" >&2
cat >&2 <<'EOF'
A home that already ran a migration never runs it again, so an edit leaves
that home's schema behind schema.rs. Add a new migration instead. For an
explicitly approved reset, set OTTAM_ALLOW_MIGRATION_EDIT=1.
EOF
exit 1
