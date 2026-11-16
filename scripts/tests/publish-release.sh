#!/usr/bin/env bash
# publish-release.sh against fake gh, curl, and Sparkle tools, a local bare
# repository standing in for ottamdev/updates, and a fake release archive.
# Nothing signs, uploads, or leaves the machine.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/ottam-publish-release.XXXXXX")"
trap 'rm -rf "$test_root"' EXIT

fail() {
  echo "FAILED: $*" >&2
  exit 1
}

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

test_feed="https://updates.ottam.dev/test/appcast.xml"
production_feed="https://updates.ottam.dev/appcast.xml"
public="$test_root/public"
stubs="$test_root/stubs"
sparkle="$test_root/sparkle"
mkdir -p "$public" "$stubs" "$sparkle"

# --- fake tools

# gh: a release exists once created; creating one publishes its asset. The
# runs API answers with STUB_PASSED_RUNS passing full-tests runs.
cat > "$stubs/gh" <<STUB
#!/bin/bash
set -e
if [ "\$1" = api ]; then
  echo "\$2" >> "$public/api-calls"
  [ -z "\${STUB_API_FAIL:-}" ] || exit 1
  echo "\${STUB_PASSED_RUNS:-0}"
  exit 0
fi
[ "\$1" = release ] || exit 2
case "\$2" in
  view) [ -e "$public/release-\$3" ] ;;
  create)
    tag="\$3" asset="\$4"
    echo "\$*" > "$public/release-\$tag"
    cp "\$asset" "$public/\$(basename "\$asset")"
    ;;
  *) exit 2 ;;
esac
STUB

# curl: release assets come from the fake release store; feeds come from the
# bare updates repository's main, as Cloudflare would serve them.
cat > "$stubs/curl" <<STUB
#!/bin/bash
out="" url=""
while [ "\$#" -gt 0 ]; do
  case "\$1" in
    -o) out="\$2"; shift 2 ;;
    -H|--retry|--retry-delay) shift 2 ;;
    -*) shift ;;
    *) url="\$1"; shift ;;
  esac
done
case "\$url" in
  https://github.com/*/releases/download/*) cp "$public/\$(basename "\$url")" "\$out" 2>/dev/null || exit 22 ;;
  $test_feed) git --git-dir "$test_root/updates.git" show "\${STUB_FEED_REV:-main}:public/test/appcast.xml" > "\$out" 2>/dev/null || exit 22 ;;
  $production_feed) git --git-dir "$test_root/updates.git" show "\${STUB_FEED_REV:-main}:public/appcast.xml" > "\$out" 2>/dev/null || exit 22 ;;
  *) exit 6 ;;
esac
STUB

# generate_appcast: keeps existing items, adds one per archive in the folder.
# Like the real tool, it reads the version from the app inside the image and
# embeds only the notes whose base name matches the archive's.
cat > "$sparkle/generate_appcast" <<'STUB'
#!/bin/bash
prefix="" dir=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --download-url-prefix) prefix="$2"; shift 2 ;;
    --ed-key-file) shift 2 ;;
    --*) shift ;;
    *) dir="$1"; shift ;;
  esac
done
[ -z "${STUB_KEY_MISMATCH:-}" ] || echo "Warning: SUPublicEDKey in the app does not match key EdDSA"
old=""
[ ! -f "$dir/appcast.xml" ] || old="$(sed -n '/<item>/,/<\/item>/p' "$dir/appcast.xml")"
{
  echo '<?xml version="1.0"?>'
  echo '<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle"><channel>'
  for dmg in "$dir"/*.dmg; do
    name="$(basename "$dmg")"
    mount="$(mktemp -d)"
    diskutil image attach --readOnly --nobrowse --mountPoint "$mount" "$dmg" >/dev/null 2>&1 || exit 1
    version="$(plutil -extract CFBundleVersion raw -o - "$mount/Ottam.app/Contents/Info.plist")"
    diskutil eject "$mount" >/dev/null 2>&1
    notes=""
    [ ! -f "$dir/${name%.dmg}.md" ] || notes="<description>$(cat "$dir/${name%.dmg}.md")</description>"
    echo "<item><sparkle:version>$version</sparkle:version>$notes"
    echo "<enclosure url=\"$prefix$name\" length=\"$(stat -f %z "$dmg")\" sparkle:edSignature=\"c2ln\"/></item>"
  done
  [ -z "$old" ] || echo "$old"
  echo '</channel></rss>'
} > "$dir/appcast.xml.new"
mv "$dir/appcast.xml.new" "$dir/appcast.xml"
echo "Wrote 1 new update"
STUB

printf '#!/bin/bash\n[ -z "${STUB_BAD_SIGNATURE:-}" ]\n' > "$sparkle/sign_update"
chmod +x "$stubs/gh" "$stubs/curl" "$sparkle/generate_appcast" "$sparkle/sign_update"

# --- a source checkout tagged for release, and the feed repository

source_repo="$test_root/source"
mkdir -p "$source_repo/scripts/lib" "$source_repo/release-notes" "$source_repo/.build/release"
cp "$repo_root/scripts/publish-release.sh" "$source_repo/scripts/"
cp "$repo_root/scripts/lib/release-version.sh" "$repo_root/scripts/lib/release-feed.sh" \
  "$repo_root/scripts/lib/release-disk-image.sh" "$repo_root/scripts/lib/appcast_items.py" \
  "$source_repo/scripts/lib/"
printf -- '- Walkthrough build.\n' > "$source_repo/release-notes/26.11.901.md"
printf '.build/\n' > "$source_repo/.gitignore"
git init -q -b main "$source_repo"
git -C "$source_repo" config user.name Test
git -C "$source_repo" config user.email test@example.invalid
git -C "$source_repo" add -A
git -C "$source_repo" commit -q -m source
git -C "$source_repo" tag -a v26.11.901 -m "Ottam 26.11.901"

make_archive() {
  local version="$1" feed="$2" app="$test_root/app-$1/Ottam.app"
  rm -rf "$test_root/app-$1"
  mkdir -p "$app/Contents"
  plutil -create xml1 "$app/Contents/Info.plist"
  plutil -insert CFBundleVersion -string "$version" "$app/Contents/Info.plist"
  [ -z "$feed" ] || plutil -insert SUFeedURL -string "$feed" "$app/Contents/Info.plist"
  rm -f "$source_repo/.build/release/Ottam-$version.dmg"
  diskutil image create from --format ULFO --volumeName Ottam "$test_root/app-$1" \
    "$source_repo/.build/release/Ottam-$version.dmg" >/dev/null 2>&1
}

git init -q --bare -b main "$test_root/updates.git"
git clone -q "$test_root/updates.git" "$test_root/seed" 2>/dev/null
git -C "$test_root/seed" config user.name Test
git -C "$test_root/seed" config user.email test@example.invalid
mkdir -p "$test_root/seed/public"
printf '/appcast.xml\n' > "$test_root/seed/public/_headers"
git -C "$test_root/seed" add -A
git -C "$test_root/seed" commit -q -m seed
git -C "$test_root/seed" push -q origin main

fresh_updates() {
  rm -rf "$test_root/updates"
  git clone -q "$test_root/updates.git" "$test_root/updates"
  git -C "$test_root/updates" config user.name Test
  git -C "$test_root/updates" config user.email test@example.invalid
}

printf 'a2V5\n' > "$test_root/sparkle.key"

publish() {
  (cd "$source_repo" && PATH="$stubs:$PATH" OTTAM_SPARKLE_BIN="$sparkle" \
    OTTAM_PUBLISH_POLL_SECONDS=0 OTTAM_PUBLISH_POLL_ATTEMPTS=2 \
    /bin/bash scripts/publish-release.sh --ed-key-file "$test_root/sparkle.key" \
      --updates-checkout "$test_root/updates" "$@")
}

# As the workflow runs it, before the build: no key file and no Sparkle tools.
preflight() {
  (cd "$source_repo" && PATH="$stubs:$PATH" OTTAM_SPARKLE_BIN="$test_root/no-sparkle" \
    /bin/bash scripts/publish-release.sh --preflight \
      --updates-checkout "$test_root/updates" "$@")
}

# --- refusals before anything is published

fresh_updates
make_archive 26.11.901 "$test_feed"
expect_status 64 "" publish --version 26.11.901
expect_status 65 "unknown feed 'beta'" publish --version 26.11.901 --feed beta
expect_status 65 "v26.11.902 is not tagged at HEAD" publish --version 26.11.902 --feed test

# --- production needs the tagged commit tested on main

head="$(git -C "$source_repo" rev-parse HEAD)"
expect_status 65 "no passing full-tests run on main for $head" preflight --version 26.11.901 --feed production
grep -Fq "head_sha=$head&branch=main&event=push&status=success" "$public/api-calls" ||
  fail "the tests query didn't ask for a passing push to main at the tagged commit"
STUB_API_FAIL=1 expect_status 65 "couldn't read full-tests runs" preflight --version 26.11.901 --feed production
STUB_PASSED_RUNS=1 expect_status 0 "Preflight passed: v26.11.901 can publish to the production feed" \
  preflight --version 26.11.901 --feed production
STUB_PASSED_RUNS=1 expect_status 65 "not the production feed" publish --version 26.11.901 --feed production
rm "$public/api-calls"

make_archive 26.11.901 ""
expect_status 65 "updates from 'nothing'" publish --version 26.11.901 --feed test
make_archive 26.11.901 "$test_feed"

mv "$source_repo/release-notes/26.11.901.md" "$test_root/notes.md"
expect_status 65 "no release notes" publish --version 26.11.901 --feed test
mv "$test_root/notes.md" "$source_repo/release-notes/26.11.901.md"

STUB_KEY_MISMATCH=1 expect_status 65 "doesn't match the bundle's SUPublicEDKey" \
  publish --version 26.11.901 --feed test
STUB_BAD_SIGNATURE=1 expect_status 65 "archive signature doesn't verify" \
  publish --version 26.11.901 --feed test
[ ! -e "$public/release-v26.11.901" ] || fail "a refused publish created a release"

git -C "$test_root/seed" commit -q --allow-empty -m "moved"
git -C "$test_root/seed" push -q origin main
expect_status 65 "isn't at origin/main" publish --version 26.11.901 --feed test
git -C "$test_root/updates" pull -q --ff-only

echo dirty > "$test_root/updates/stray"
expect_status 65 "has local changes" publish --version 26.11.901 --feed test
rm "$test_root/updates/stray"

# A release left behind by a run that never reached the feed.
touch "$public/release-v26.11.901"
expect_status 65 "GitHub Release v26.11.901 already exists" publish --version 26.11.901 --feed test
rm "$public/release-v26.11.901"

# --- preflight needs no archive, key, or Sparkle tools

mv "$source_repo/.build/release/Ottam-26.11.901.dmg" "$test_root/held.dmg"
expect_status 0 "Preflight passed: v26.11.901 can publish to the test feed" \
  preflight --version 26.11.901 --feed test
mv "$test_root/held.dmg" "$source_repo/.build/release/Ottam-26.11.901.dmg"

# --- a test-feed release publishes a pre-release, then the feed

expect_status 0 "Published 26.11.901 to the test feed" publish --version 26.11.901 --feed test
grep -q -- '--prerelease' "$public/release-v26.11.901" || fail "a test-feed release wasn't a pre-release"
[ -f "$public/Ottam.dmg" ] || fail "the release asset isn't named Ottam.dmg"
git --git-dir "$test_root/updates.git" show main:public/test/appcast.xml > "$test_root/published.xml"
grep -q 'releases/download/v26.11.901/Ottam.dmg' "$test_root/published.xml" ||
  fail "the test feed doesn't point at the release"
grep -q '<description>- Walkthrough build.' "$test_root/published.xml" ||
  fail "the release notes weren't embedded in the feed"
if git --git-dir "$test_root/updates.git" show main:public/appcast.xml >/dev/null 2>&1; then
  fail "a test-feed release touched the production feed"
fi
[ ! -e "$public/api-calls" ] || fail "a test-feed release asked whether its commit was tested"

# --- a rerun refuses on the feed's version

fresh_updates
expect_status 65 "26.11.901 is not newer than 26.11.901 in the test feed" \
  preflight --version 26.11.901 --feed test
expect_status 65 "26.11.901 is not newer than 26.11.901 in the test feed" \
  publish --version 26.11.901 --feed test

# --- the public feed never catching up is a failure, after the push

git -C "$source_repo" commit -q --allow-empty -m next
git -C "$source_repo" tag -a v26.11.902 -m "Ottam 26.11.902"
cp "$source_repo/release-notes/26.11.901.md" "$source_repo/release-notes/26.11.902.md"
make_archive 26.11.902 "$test_feed"
STUB_FEED_REV=main~1 expect_status 70 "still serves something else" publish --version 26.11.902 --feed test

echo "publish-release contract passed"
