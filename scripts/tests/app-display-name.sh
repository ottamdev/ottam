#!/usr/bin/env bash
# Naming policy against real primary, linked, detached, and non-Git folders.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/ottam-display-name.XXXXXX")"
trap 'rm -rf "$test_root"' EXIT
helper="$repo_root/scripts/app-display-name.sh"

expect_name() {
  local expected="$1" root="$2" role="$3" actual
  actual="$(LC_ALL=C /bin/bash "$helper" --root "$root" --role "$role")"
  if [ "$actual" != "$expected" ]; then
    printf 'Expected <%s>, got <%s> for %s\n' "$expected" "$actual" "$root" >&2
    exit 1
  fi
}

primary="$test_root/primary"
git init -q -b naming-contract "$primary"
git -C "$primary" -c user.name=Test -c user.email=test@example.invalid \
  -c commit.gpgsign=false -c core.hooksPath=/dev/null commit -q --allow-empty -m fixture
expect_name "Ottam Dev" "$primary" dev
expect_name "Ottam Test" "$primary" test
# Primary is a Git relationship, not the branch name or whether HEAD is detached.
git -C "$primary" checkout -q --detach
expect_name "Ottam Dev" "$primary" dev

git -C "$primary" worktree add -q --detach "$test_root/ottam-textkit2"
expect_name "Ottam Dev · textkit2" "$test_root/ottam-textkit2" dev
expect_name "Ottam Test · textkit2" "$test_root/ottam-textkit2" test
ln -s "$test_root/ottam-textkit2" "$test_root/alias"
expect_name "Ottam Dev · textkit2" "$test_root/alias" dev

git -C "$primary" worktree add -q -b naming-linked "$test_root/worktrees/90ae/ottam"
expect_name "Ottam Dev · 90ae" "$test_root/worktrees/90ae/ottam" dev
git -C "$primary" worktree add -q --detach "$test_root/worktrees/slice6-telemetry-baseline/ottam"
expect_name "Ottam Test · slice6-tele…" "$test_root/worktrees/slice6-telemetry-baseline/ottam" test

for label in abcdefghijkl abcdefghijklm ééééééééééééé $'  two\t words\n '; do
  mkdir -p "$test_root/$label"
done
expect_name "Ottam Dev · abcdefghijkl" "$test_root/abcdefghijkl" dev
expect_name "Ottam Dev · abcdefghijk…" "$test_root/abcdefghijklm" dev
expect_name "Ottam Dev · ééééééééééé…" "$test_root/ééééééééééééé" dev
expect_name "Ottam Test · two words" "$test_root/"$'  two\t words\n ' test

# The cap applies to grapheme clusters, including decomposed e + acute and
# emoji joined by a zero-width joiner, under the caller's LC_ALL=C above.
decomposed="$(printf 'e\314\201%.0s' {1..7})"
long_decomposed="$(printf 'e\314\201%.0s' {1..13})"
shortened_decomposed="$(printf 'e\314\201%.0s' {1..11})…"
joined_emoji="1234567890👩‍💻zz"
for label in "$decomposed" "$long_decomposed" "$joined_emoji"; do
  mkdir -p "$test_root/$label"
done
expect_name "Ottam Dev · $decomposed" "$test_root/$decomposed" dev
expect_name "Ottam Dev · $shortened_decomposed" "$test_root/$long_decomposed" dev
expect_name "Ottam Test · 1234567890👩‍💻…" "$test_root/$joined_emoji" test

# Exercise the builder's argument validation without compiling or staging a
# bundle: accepted names reach its missing-build-products check (exit 66).
builder_root="$test_root/builder"
mkdir -p "$builder_root/scripts"
cp "$repo_root/scripts/build-app.sh" "$builder_root/scripts/build-app.sh"
printf '#!/bin/sh\n' > "$builder_root/scripts/fetch-ghosttykit.sh"
chmod +x "$builder_root/scripts/fetch-ghosttykit.sh"
for name in "Ottam Dev · 1234567890👩‍💻…" $'Ottam Dev\ndiagnostic'; do
  status=0
  LC_ALL=en_US.UTF-8 /bin/bash "$builder_root/scripts/build-app.sh" \
    --skip-build --display-name "$name" > "$builder_root/result.log" 2>&1 || status="$?"
  case "$name" in
    *$'\n'*) expected_status=64 ;;
    *) expected_status=66 ;;
  esac
  if [ "$status" != "$expected_status" ]; then
    cat "$builder_root/result.log" >&2
    echo "Unexpected builder display-name validation status: $status" >&2
    exit 1
  fi
done

if /bin/bash "$helper" --root "$primary" --role release >/dev/null 2>&1; then
  echo "Invalid role was accepted" >&2
  exit 1
fi
if /bin/bash "$helper" --root "$test_root/missing" --role dev >/dev/null 2>&1; then
  echo "Missing checkout was accepted" >&2
  exit 1
fi
echo "App display naming contract passed"
