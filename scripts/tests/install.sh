#!/usr/bin/env bash
# install.sh against fake curl, codesign, spctl, diskutil, and platform
# probes. The fake disk image holds a stub Ottam.app; nothing downloads,
# verifies a real signature, or touches /Applications.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/ottam-install-test.XXXXXX")"
trap 'rm -rf "$test_root"' EXIT

fail() {
  echo "FAILED: $*" >&2
  exit 1
}

stubs="$test_root/stubs"
# Parentheses and a dot: the running check must match the path literally.
apps="$test_root/Apps (test.1)"
mkdir -p "$stubs"

# --- fake tools

# curl: the feed is $FAKE_FEED with status $FAKE_FEED_STATUS; anything else
# is a download of the fake disk image, logged to $test_root/downloads.
cat > "$stubs/curl" <<STUB
#!/bin/bash
out="" format="" url=""
while [ "\$#" -gt 0 ]; do
  case "\$1" in
    -o) out="\$2"; shift 2 ;;
    -w) format="\$2"; shift 2 ;;
    --retry) shift 2 ;;
    -*) shift ;;
    *) url="\$1"; shift ;;
  esac
done
case "\$url" in
  *appcast.xml)
    [ "\${FAKE_FEED_STATUS:-200}" != 200 ] || cp "\$FAKE_FEED" "\$out"
    [ -z "\$format" ] || printf '%s' "\${FAKE_FEED_STATUS:-200}"
    ;;
  *)
    echo "\$url" >> "$test_root/downloads"
    printf '%s' "\${FAKE_DMG_BYTES:-0123456789}" > "\$out"
    ;;
esac
STUB

cat > "$stubs/codesign" <<'STUB'
#!/bin/bash
case "$1" in
  --verify) exit "${FAKE_CODESIGN:-0}" ;;
  --display) echo "TeamIdentifier=${FAKE_TEAM:-22G49YDA3D}" >&2 ;;
esac
STUB

cat > "$stubs/spctl" <<'STUB'
#!/bin/bash
exit "${FAKE_SPCTL:-0}"
STUB

# diskutil: attaching puts the stub app in the mount point, which must exist,
# as macOS 26 requires; ejecting empties it.
cat > "$stubs/diskutil" <<STUB
#!/bin/bash
if [ "\$1 \$2" = "image attach" ]; then
  while [ "\$#" -gt 0 ]; do
    [ "\$1" = --mountPoint ] && mount="\$2"
    shift
  done
  [ -d "\$mount" ] || { echo "Error: Mount point \$mount doesn't exist" >&2; exit 1; }
  cp -R "$test_root/image/Ottam.app" "\$mount/"
elif [ "\$1" = eject ]; then
  rm -rf "\${@: -1}"/*
fi
STUB

cat > "$stubs/sysctl" <<'STUB'
#!/bin/bash
echo "${FAKE_ARM64:-1}"
STUB

cat > "$stubs/sw_vers" <<'STUB'
#!/bin/bash
echo "${FAKE_OS:-26.1}"
STUB

# ps: lists the installed Ottam's executable while it "runs". FAKE_RUNNING=1
# runs it throughout; =late opens it after the installer's first look.
cat > "$stubs/ps" <<STUB
#!/bin/bash
echo /usr/libexec/somed
calls="\$(cat "$test_root/ps-calls" 2>/dev/null || echo 0)"
echo \$((calls + 1)) > "$test_root/ps-calls"
case "\${FAKE_RUNNING:-}" in
  1) echo "\$OTTAM_INSTALL_DIR/Ottam.app/Contents/MacOS/Ottam" ;;
  late) [ "\$calls" = 0 ] || echo "\$OTTAM_INSTALL_DIR/Ottam.app/Contents/MacOS/Ottam" ;;
esac
STUB

# mv: real, except FAKE_SIGNAL_SWAP=1 signals the installer as it moves the
# staged app into place, after the old one is already aside.
cat > "$stubs/mv" <<'STUB'
#!/bin/bash
if [ -n "${FAKE_SIGNAL_SWAP:-}" ] && [[ "$1" == *.installing.* ]]; then
  kill -TERM "$PPID"
  exit 1
fi
exec /bin/mv "$@"
STUB

chmod +x "$stubs"/*

# --- fixtures

# Writes a stub Ottam.app at $1 with bundle version $2 and identifier $3.
make_app() {
  mkdir -p "$1/Contents/MacOS"
  cat > "$1/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>${3:-dev.ottam.Ottam}</string>
  <key>CFBundleVersion</key><string>$2</string>
</dict></plist>
PLIST
}

# Writes a feed at $1 with one item per version argument, in order.
make_feed() {
  local feed="$1" version
  shift
  {
    echo '<?xml version="1.0" standalone="yes"?>'
    echo '<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" version="2.0"><channel><title>Ottam</title>'
    for version in "$@"; do
      echo "<item><title>$version</title><sparkle:version>$version</sparkle:version>"
      echo "<enclosure url=\"https://github.com/ottamdev/ottam/releases/download/v$version/Ottam-$version.dmg\" length=\"10\" type=\"application/octet-stream\"/></item>"
    done
    echo '</channel></rss>'
  } > "$feed"
}

reset() {
  rm -rf "$apps" "$test_root/image" "$test_root/downloads" "$test_root/ps-calls"
  mkdir -p "$apps"
  make_app "$test_root/image/Ottam.app" 26.11.10
}

# Runs the installer with /bin/sh, as `curl … | sh` does, and checks its
# status and that its output mentions $2.
expect() {
  local expected="$1" reason="$2" status=0
  shift 2
  env PATH="$stubs:$PATH" OTTAM_INSTALL_DIR="$apps" FAKE_FEED="$test_root/feed.xml" "$@" \
    /bin/sh "$repo_root/scripts/install.sh" > "$test_root/output.log" 2>&1 || status="$?"
  rm -f "$test_root/ps-calls"
  if [ "$status" != "$expected" ] || ! grep -Fq -- "$reason" "$test_root/output.log"; then
    cat "$test_root/output.log" >&2
    fail "expected status $expected mentioning <$reason>, got $status: $*"
  fi
}

installed_version() {
  /usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$apps/Ottam.app/Contents/Info.plist"
}

assert_clean() {
  ! ls -d "$apps"/.Ottam.app.installing.* >/dev/null 2>&1 || fail "left the staged copy behind"
  ! ls -d "${TMPDIR:-/tmp}"/ottam-install.?????? >/dev/null 2>&1 || fail "left its work folder behind"
}

# --- cases

reset
expect 1 "no release has been published yet" FAKE_FEED_STATUS=404
make_feed "$test_root/feed.xml"
expect 1 "no release has been published yet"
expect 1 "answered HTTP 503" FAKE_FEED_STATUS=503

expect 1 "needs a Mac with Apple silicon" FAKE_ARM64=0
expect 1 "needs macOS 26 or later; this Mac runs 15.5" FAKE_OS=15.5
chmod a-w "$apps"
expect 1 "can't write to $apps"
chmod u+w "$apps"

# The newest item wins whatever the feed's order, compared numerically.
make_feed "$test_root/feed.xml" 26.11.9 26.11.10 26.10.20
expect 0 "Installed Ottam 26.11.10"
grep -Fxq "https://github.com/ottamdev/ottam/releases/download/v26.11.10/Ottam-26.11.10.dmg" \
  "$test_root/downloads" || fail "downloaded the wrong release: $(cat "$test_root/downloads")"
[ "$(installed_version)" = 26.11.10 ] || fail "installed the wrong app"
assert_clean

rm "$test_root/downloads"
expect 0 "Ottam 26.11.10 is already installed"
[ ! -e "$test_root/downloads" ] || fail "downloaded a release that is already installed"

# An older install is replaced; a running one is left alone.
reset
make_app "$apps/Ottam.app" 26.11.2
expect 1 "Ottam is running" FAKE_RUNNING=1
[ "$(installed_version)" = 26.11.2 ] || fail "replaced a running app"
[ ! -e "$test_root/downloads" ] || fail "downloaded although Ottam was running"
# Opened while the installer staged its copy: the check before the swap
# catches it.
expect 1 "Ottam is running" FAKE_RUNNING=late
[ -e "$test_root/downloads" ] || fail "refused before staging; the late check is untested"
[ "$(installed_version)" = 26.11.2 ] || fail "replaced an app opened during staging"
assert_clean
# A signal after the old app is moved aside puts it back.
expect 130 "Installing to" FAKE_SIGNAL_SWAP=1
[ "$(installed_version)" = 26.11.2 ] || fail "an interrupted swap lost the old app"
assert_clean
expect 0 "Installed Ottam 26.11.10"
[ "$(installed_version)" = 26.11.10 ] || fail "didn't replace the older app"
assert_clean

# Every check refuses before anything lands in the install folder.
reset
expect 1 "the download is 3 bytes; the feed says 10" FAKE_DMG_BYTES=abc
expect 1 "the disk image has an invalid signature" FAKE_CODESIGN=1
expect 1 "signed by team ABCDE12345, not Ottam's" FAKE_TEAM=ABCDE12345
expect 1 "Gatekeeper rejected the disk image" FAKE_SPCTL=3
rm -rf "$test_root/image"
make_app "$test_root/image/Ottam.app" 26.11.10 com.example.Other
expect 1 "isn't dev.ottam.Ottam"
[ -z "$(ls -A "$apps")" ] || fail "a refused install left files: $(ls -A "$apps")"
assert_clean

# A download cut short runs nothing.
reset
sed '$d' "$repo_root/scripts/install.sh" > "$test_root/truncated.sh"
output="$(env PATH="$stubs:$PATH" OTTAM_INSTALL_DIR="$apps" /bin/sh "$test_root/truncated.sh" 2>&1)" ||
  fail "a truncated script failed: $output"
[ -z "$output" ] || fail "a truncated script ran: $output"

echo "install: passed"
