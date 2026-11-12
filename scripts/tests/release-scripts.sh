#!/usr/bin/env bash
# Release version rules, build flag validation, release refusals, the cut, and
# the verifier's signing-list guard. Nothing here compiles or signs.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/ottam-release-scripts.XXXXXX")"
trap 'rm -rf "$test_root"' EXIT

# shellcheck source=scripts/lib/release-version.sh
source "$repo_root/scripts/lib/release-version.sh"

fail() {
  echo "FAILED: $*" >&2
  exit 1
}

expect_valid() {
  release_version_is_valid "$1" || fail "expected valid version: $1"
}

expect_invalid() {
  if release_version_is_valid "$1"; then
    fail "expected invalid version: $1"
  fi
}

expect_next() {
  local expected="$1" date="$2" actual
  shift 2
  actual="$(release_next_version "$date" "$@")" || fail "no next version for $date"
  [ "$actual" = "$expected" ] || fail "next version on $date: expected $expected, got $actual"
}

# Runs a command and checks its exit status and that its output names the reason.
expect_status() {
  local expected="$1" reason="$2" status=0
  shift 2
  "$@" > "$test_root/output.log" 2>&1 || status="$?"
  if [ "$status" != "$expected" ]; then
    cat "$test_root/output.log" >&2
    fail "expected status $expected, got $status: $*"
  fi
  if [ -n "$reason" ] && ! grep -Fq -- "$reason" "$test_root/output.log"; then
    cat "$test_root/output.log" >&2
    fail "expected output to mention <$reason>: $*"
  fi
}

# --- version rules

for version in 26.1.0 26.11.0 26.12.14 30.10.100; do
  expect_valid "$version"
done
for version in "" 26.01.0 26.13.0 26.0.0 26.11.01 6.11.0 2026.11.0 26.11 26.11.0.1 v26.11.0 26.11.0-beta; do
  expect_invalid "$version"
done

expect_next 26.11.0 2026-11-09
expect_next 26.11.0 2026-11-09 v26.10.3 backup/plan-pre-rebase v1 ghostty-v1.4.0
expect_next 26.11.1 2026-11-30 v26.11.0
expect_next 26.11.11 2026-11-30 v26.11.2 v26.11.10 v26.11.9
expect_next 26.12.0 2026-12-01 v26.11.4
expect_next 27.1.0 2027-01-01 v26.12.2
expect_next 26.11.0 2026-11-09 v0.1.0 v0.2.0

if release_next_version 2026-10-31 v26.11.0 >/dev/null 2>&1; then
  fail "a clock behind the latest tag was accepted"
fi
if release_next_version 11/09/2026 >/dev/null 2>&1; then
  fail "a malformed date was accepted"
fi

printf '[package]\nversion = "9.9.9"\n\n[workspace.package]\nedition = "2021"\nversion = "26.11.3"\n' \
  > "$test_root/Cargo.toml"
[ "$(release_cargo_version "$test_root/Cargo.toml")" = 26.11.3 ] ||
  fail "read the wrong Cargo version"

# --- build-app.sh flags: accepted flags reach the missing-products check (66)

builder="$test_root/builder"
mkdir -p "$builder/scripts/lib"
cp "$repo_root/scripts/build-app.sh" "$builder/scripts/"
cp "$repo_root/scripts/lib/release-version.sh" "$builder/scripts/lib/"
printf '#!/bin/sh\n' > "$builder/scripts/fetch-ghosttykit.sh"
chmod +x "$builder/scripts/fetch-ghosttykit.sh"

build_app() {
  /bin/bash "$builder/scripts/build-app.sh" --skip-build "$@"
}

expect_status 66 "Missing build products" build_app --release --version 26.1.0 --no-sign
expect_status 64 "Invalid release version" build_app --release --version 26.01.0
expect_status 64 "--version requires --release" build_app --version 26.1.0
expect_status 64 "--version requires --release" build_app --profile --version 26.1.0
expect_status 64 "--no-sign requires --version" build_app --no-sign

# --- release-app.sh, in a throwaway repository whose build must not be reached

stubs="$test_root/stubs"
mkdir -p "$stubs"
printf '#!/bin/sh\necho "     0 valid identities found"\n' > "$stubs/security"
printf '#!/bin/sh\nexit 0\n' > "$stubs/cargo"
chmod +x "$stubs/security" "$stubs/cargo"

new_repo() {
  local repo="$1" cargo_version="$2"
  mkdir -p "$repo/scripts/lib" "$repo/backend"
  cp "$repo_root/scripts/release-app.sh" "$repo/scripts/"
  cp "$repo_root/scripts/lib/release-version.sh" "$repo_root/scripts/lib/release-signing.sh" "$repo/scripts/lib/"
  printf '#!/bin/sh\necho "build reached" >&2\nexit 99\n' > "$repo/scripts/build-app.sh"
  chmod +x "$repo/scripts/build-app.sh"
  printf '[workspace.package]\nedition = "2021"\nversion = "%s"\n' "$cargo_version" > "$repo/backend/Cargo.toml"
  printf 'version = 4\n' > "$repo/backend/Cargo.lock"
  printf '.build/\n' > "$repo/.gitignore"
  git init -q -b main "$repo"
  git -C "$repo" config user.name Test
  git -C "$repo" config user.email test@example.invalid
  git -C "$repo" add .
  git -C "$repo" commit -q -m initial
  git init -q --bare "$repo.origin"
  git -C "$repo" remote add origin "$repo.origin"
  git -C "$repo" push -q origin main
}

release_app() {
  local repo="$1"
  shift
  PATH="$stubs:$PATH" /bin/bash "$repo/scripts/release-app.sh" "$@"
}

repo="$test_root/refusals"
new_repo "$repo" 26.11.0

expect_status 65 "no valid vYY.M.N tag" release_app "$repo"
git -C "$repo" tag v26.01.0
expect_status 65 "no valid vYY.M.N tag" release_app "$repo"
git -C "$repo" tag -d v26.01.0 >/dev/null

git -C "$repo" tag v26.11.1
expect_status 65 "does not match Cargo" release_app "$repo"
git -C "$repo" tag -d v26.11.1 >/dev/null

git -C "$repo" tag v26.11.0
touch "$repo/uncommitted"
expect_status 65 "uncommitted changes" release_app "$repo"
rm "$repo/uncommitted"

expect_status 69 "Developer ID Application" release_app "$repo"

mkdir -p "$repo/.build/release"
touch "$repo/.build/release/Ottam-26.11.0.zip"
expect_status 65 "already exists" release_app "$repo"
rm -r "$repo/.build/release"

# With an identity present, a notary profile that can't sign in stops the
# release before the build.
identity_stubs="$test_root/identity-stubs"
mkdir -p "$identity_stubs"
cat > "$identity_stubs/security" <<'STUB'
#!/bin/sh
echo '  1) 0123456789ABCDEF0123456789ABCDEF01234567 "Developer ID Application: Test (TEAM123456)"'
echo '     1 valid identities found'
STUB
printf '#!/bin/sh\nexit 1\n' > "$identity_stubs/xcrun"
chmod +x "$identity_stubs/security" "$identity_stubs/xcrun"
expect_status 69 "notary keychain profile" \
  env PATH="$identity_stubs:$stubs:$PATH" /bin/bash "$repo/scripts/release-app.sh"

expect_status 64 "rehearsal-only" release_app "$repo" --version 26.11.0
expect_status 65 "own --bundle-id" release_app "$repo" --rehearsal --version 26.11.0
expect_status 65 "production bundle id" release_app "$repo" \
  --rehearsal --version 26.11.0 --bundle-id dev.ottam.Ottam

# --- the cut

repo="$test_root/cut"
new_repo "$repo" 0.1.0

git -C "$repo" switch -q -c topic
expect_status 65 "cut from main" release_app "$repo" --cut
git -C "$repo" switch -q main

touch "$repo/uncommitted"
expect_status 65 "uncommitted changes" release_app "$repo" --cut
rm "$repo/uncommitted"

# A cut sits on the published main: refuse when local main is ahead or behind.
git -C "$repo" commit -q --allow-empty -m "local only"
expect_status 65 "doesn't match origin/main" release_app "$repo" --cut
git -C "$repo" push -q origin main
other="$test_root/cut-other"
git clone -q "$repo.origin" "$other"
git -C "$other" -c user.name=Test -c user.email=test@example.invalid \
  commit -q --allow-empty -m "elsewhere"
git -C "$other" push -q origin main
expect_status 65 "doesn't match origin/main" release_app "$repo" --cut
git -C "$repo" pull -q --ff-only origin main

# A release pushed from another checkout counts toward the next version.
pushed="$(release_next_version "$(date -u +%F)")"
git -C "$other" tag "v$pushed"
git -C "$other" push -q origin "v$pushed"

expected="$(release_next_version "$(date -u +%F)" "v$pushed")"
expect_status 0 "Nothing was pushed" release_app "$repo" --cut
[ "$(git -C "$repo" tag --points-at HEAD)" = "v$expected" ] || fail "cut did not tag HEAD v$expected"
[ "$(release_cargo_version "$repo/backend/Cargo.toml")" = "$expected" ] || fail "cut did not bump Cargo"
[ -z "$(git -C "$repo" status --porcelain)" ] || fail "cut left the tree dirty"
[ "$(git -C "$repo" log -1 --format=%s)" = "chore(release): v$expected" ] || fail "wrong cut commit"

# A cut leaves exactly what the release step accepts.
expect_status 69 "Developer ID Application" release_app "$repo"

# --- verifier's signing-list guard

verifier="$test_root/verifier"
mkdir -p "$verifier/scripts/lib"
cp "$repo_root/scripts/verify-release-app.sh" "$verifier/scripts/"
cp "$repo_root/scripts/lib/release-signing.sh" "$verifier/scripts/lib/"

app="$test_root/Fake.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp /usr/bin/true "$app/Contents/MacOS/Ottam"
cp /usr/bin/true "$app/Contents/MacOS/ottam-core"
plutil -create xml1 "$app/Contents/Info.plist"
plutil -insert CFBundleExecutable -string Ottam "$app/Contents/Info.plist"
plutil -insert CFBundleShortVersionString -string 26.11.0 "$app/Contents/Info.plist"
plutil -insert CFBundleVersion -string 26.11.0 "$app/Contents/Info.plist"
printf 'not code\n' > "$app/Contents/Resources/notes.txt"

verify() {
  /bin/bash "$verifier/scripts/verify-release-app.sh" "$app" "$@"
}

# The expected set passes the guard and fails later, on its signatures.
expect_status 1 "Release verification failed" verify --version 26.11.0
if grep -Fq "signing list" "$test_root/output.log"; then
  fail "the expected Mach-O set was rejected"
fi
expect_status 1 "CFBundleShortVersionString is not" verify --version 26.11.1

cp /usr/bin/true "$app/Contents/Resources/extra-helper"
expect_status 1 "don't match the signing list" verify --version 26.11.0
rm "$app/Contents/Resources/extra-helper"

echo "Release script contract passed"
