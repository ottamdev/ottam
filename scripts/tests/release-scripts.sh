#!/usr/bin/env bash
# Release version rules, build flag validation, release refusals, the cut, the
# verifier's signing-list guard, and the release disk image. Nothing here
# compiles or signs with a real identity.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/ottam-release-scripts.XXXXXX")"

# shellcheck source=scripts/lib/release-version.sh
source "$repo_root/scripts/lib/release-version.sh"
# shellcheck source=scripts/lib/release-disk-image.sh
source "$repo_root/scripts/lib/release-disk-image.sh"
trap 'release_detach_disk_image "$test_root/mounted"; rm -rf "$test_root"' EXIT

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
  cp "$repo_root/scripts/lib/release-version.sh" "$repo_root/scripts/lib/release-signing.sh" \
    "$repo_root/scripts/lib/release-feed.sh" "$repo_root/scripts/lib/release-disk-image.sh" \
    "$repo/scripts/lib/"
  printf '#!/bin/sh\necho "build reached" >&2\nexit 99\n' > "$repo/scripts/build-app.sh"
  chmod +x "$repo/scripts/build-app.sh"
  printf '[workspace.package]\nedition = "2021"\nversion = "%s"\n' "$cargo_version" > "$repo/backend/Cargo.toml"
  printf 'version = 4\n' > "$repo/backend/Cargo.lock"
  mkdir -p "$repo/app/Signing"
  printf 'cHVibGljLWtleQ==\n' > "$repo/app/Signing/sparkle-public-key"
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

# A release always gets a feed, so it needs the key and an https URL first.
expect_status 65 "must be an https URL" release_app "$repo" --feed-url http://feed.invalid/appcast.xml
expect_status 69 "Never publish it" release_app "$repo" --feed-url https://feed.invalid/appcast.xml
git -C "$repo" rm -q app/Signing/sparkle-public-key
git -C "$repo" commit -q -m "no key"
git -C "$repo" tag -f v26.11.0 >/dev/null
expect_status 65 "no Sparkle public key" release_app "$repo"
git -C "$repo" reset -q --hard HEAD~1
git -C "$repo" tag -f v26.11.0 >/dev/null
expect_status 64 "not a cut" release_app "$repo" --cut --feed-url https://feed.invalid/appcast.xml

mkdir -p "$repo/.build/release"
touch "$repo/.build/release/Ottam-26.11.0.dmg"
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
cp "$repo_root/scripts/lib/release-signing.sh" "$repo_root/scripts/lib/release-disk-image.sh" \
  "$verifier/scripts/lib/"

app="$test_root/Fake.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp /usr/bin/true "$app/Contents/MacOS/Ottam"
cp /usr/bin/true "$app/Contents/MacOS/ottam-core"
sparkle="$app/Contents/Frameworks/Sparkle.framework/Versions/B"
for helper in XPCServices/Installer.xpc/Contents/MacOS/Installer \
  XPCServices/Downloader.xpc/Contents/MacOS/Downloader \
  Updater.app/Contents/MacOS/Updater Autoupdate Sparkle; do
  mkdir -p "$(dirname "$sparkle/$helper")"
  cp /usr/bin/true "$sparkle/$helper"
done
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
expect_status 1 "outside the signing list" verify --version 26.11.0
rm "$app/Contents/Resources/extra-helper"

# Code inside a listed bundle is covered by that bundle's signature.
cp /usr/bin/true "$sparkle/Updater.app/Contents/MacOS/extra"
expect_status 1 "Release verification failed" verify --version 26.11.0
if grep -Fq "signing list" "$test_root/output.log"; then
  fail "code inside a signed bundle was rejected"
fi
rm "$sparkle/Updater.app/Contents/MacOS/extra"

mv "$sparkle/Autoupdate" "$test_root/Autoupdate"
expect_status 1 "missing from the bundle: Contents/Frameworks/Sparkle.framework/Versions/B/Autoupdate" \
  verify --version 26.11.0
mv "$test_root/Autoupdate" "$sparkle/Autoupdate"

# A feed ships only when the release asked for one, and only fully enforced.
plutil -insert SUFeedURL -string https://feed.invalid/appcast.xml "$app/Contents/Info.plist"
expect_status 1 "carries an update feed" verify --version 26.11.0
mkdir -p "$verifier/app/Signing"
printf 'cHVibGljLWtleQ==\n' > "$verifier/app/Signing/sparkle-public-key"
expect_status 1 "not https://other.invalid/appcast.xml" \
  verify --version 26.11.0 --feed-url https://other.invalid/appcast.xml
expect_status 1 "SUPublicEDKey doesn't match" \
  verify --version 26.11.0 --feed-url https://feed.invalid/appcast.xml
plutil -insert SUPublicEDKey -string cHVibGljLWtleQ== "$app/Contents/Info.plist"
expect_status 1 "SURequireSignedFeed is not set" \
  verify --version 26.11.0 --feed-url https://feed.invalid/appcast.xml
plutil -insert SURequireSignedFeed -bool YES "$app/Contents/Info.plist"
plutil -insert SUVerifyUpdateBeforeExtraction -bool YES "$app/Contents/Info.plist"
plutil -insert SUEnableAutomaticChecks -bool YES "$app/Contents/Info.plist"
expect_status 1 "must ask for consent" \
  verify --version 26.11.0 --feed-url https://feed.invalid/appcast.xml
plutil -remove SUEnableAutomaticChecks "$app/Contents/Info.plist"
expect_status 1 "Update feed: https://feed.invalid/appcast.xml" \
  verify --version 26.11.0 --feed-url https://feed.invalid/appcast.xml

# --- the release image holds the app, the Applications link, and the layout

image_root="$test_root/image"
mkdir "$image_root"
cp -R "$app" "$image_root/Ottam.app"
release_create_disk_image "$image_root" "$test_root/Ottam.dmg" "$repo_root" ||
  fail "release_create_disk_image failed"
[ ! -e "$test_root/Ottam.dmg.raw.dmg" ] || fail "the writable image was left behind"
release_attach_disk_image "$test_root/Ottam.dmg" "$test_root/mounted" || fail "couldn't mount the image"
problem="$(release_check_disk_image "$test_root/mounted" "$repo_root")" ||
  { release_detach_disk_image "$test_root/mounted"; fail "a fresh image failed its check: $problem"; }
# A stale layout and an extra item are both refused.
stale_repo="$test_root/stale-repo"
mkdir -p "$stale_repo/$RELEASE_DISK_IMAGE_LAYOUT"
cp "$repo_root/$RELEASE_DISK_IMAGE_LAYOUT/background.tiff" "$stale_repo/$RELEASE_DISK_IMAGE_LAYOUT/"
printf 'stale' > "$stale_repo/$RELEASE_DISK_IMAGE_LAYOUT/DS_Store"
problem="$(release_check_disk_image "$test_root/mounted" "$stale_repo")" &&
  { release_detach_disk_image "$test_root/mounted"; fail "a stale layout passed"; }
grep -q "isn't the one in" <<< "$problem" || fail "wrong stale-layout message: $problem"
release_detach_disk_image "$test_root/mounted"
[ ! -d "$test_root/mounted" ] || fail "the image didn't eject"

extra_root="$test_root/extra"
mkdir -p "$extra_root/Ottam.app" "$extra_root/.fseventsd"
release_stage_disk_image "$extra_root" "$repo_root"
cp "$repo_root/$RELEASE_DISK_IMAGE_LAYOUT/DS_Store" "$extra_root/.DS_Store"
problem="$(release_check_disk_image "$extra_root" "$repo_root")" && fail "an extra item passed"
grep -q ".fseventsd" <<< "$problem" || fail "the extra item wasn't named: $problem"

# --- the verifier requires the release identity on the image

verify_image() {
  /bin/bash "$verifier/scripts/verify-release-app.sh" "$test_root/Ottam.dmg" "$@"
}
expect_status 1 "invalid signature: $test_root/Ottam.dmg" verify_image --version 26.11.0
codesign --sign - "$test_root/Ottam.dmg"
expect_status 1 "not signed by Developer ID Application: $test_root/Ottam.dmg" \
  verify_image --version 26.11.0

echo "Release script contract passed"
