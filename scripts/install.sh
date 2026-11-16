#!/bin/sh
# Installs the latest Ottam release:
#
#   curl -fsSL https://ottam.dev/install.sh | sh
#
# Reads the stable update feed, downloads the disk image it names, checks that
# the image and the app are signed by Ottam's Developer ID and notarized, and
# copies Ottam.app into /Applications. A download with curl carries no
# quarantine flag, so Gatekeeper never checks the app on first launch; this
# script runs those checks itself before anything is copied. Updates after
# that come from Sparkle inside the app.
#
# Environment:
#   OTTAM_INSTALL_DIR  where Ottam.app goes (default: /Applications)
#   OTTAM_FEED_URL     the appcast to read (default: the stable feed)
#
# POSIX sh: macOS runs this with bash 3.2 in sh mode, or whatever /bin/sh is.
# Everything sits in main, so a download cut short runs nothing.

main() {
  set -eu

  feed_url="${OTTAM_FEED_URL:-https://updates.ottam.dev/appcast.xml}"
  install_dir="${OTTAM_INSTALL_DIR:-/Applications}"
  dest="$install_dir/Ottam.app"
  team_id="22G49YDA3D"
  bundle_id="dev.ottam.Ottam"
  source_url="https://github.com/ottamdev/ottam"

  check_platform

  [ -d "$install_dir" ] || fail "$install_dir doesn't exist."
  [ -w "$install_dir" ] ||
    fail "can't write to $install_dir. Rerun with OTTAM_INSTALL_DIR=\"\$HOME/Applications\", or as an administrator."

  work="$(mktemp -d "${TMPDIR:-/tmp}/ottam-install.XXXXXX")"
  mount="$work/volume"
  trap cleanup EXIT
  trap 'exit 130' INT TERM

  say "Reading $feed_url"
  # HTTPS only, redirects included, so no hop can be read or rewritten.
  status="$(curl -sSL --proto '=https' --proto-redir '=https' --retry 3 -o "$work/appcast.xml" -w '%{http_code}' "$feed_url")" ||
    fail "couldn't reach $feed_url."
  case "$status" in
    200) ;;
    404) no_release ;;
    *) fail "$feed_url answered HTTP $status." ;;
  esac
  pick_latest "$work/appcast.xml"
  [ -n "$version" ] || no_release

  if [ -d "$dest" ]; then
    installed="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$dest/Contents/Info.plist" 2>/dev/null || true)"
    if [ -n "$installed" ] && ! version_newer "$version" "$installed"; then
      say "Ottam $installed is already installed at $dest; nothing to do."
      exit 0
    fi
  fi
  # Checked again just before the swap; this one saves a wasted download.
  ! running || fail "Ottam is running. Quit it and run this again."

  say "Downloading Ottam $version"
  curl -fL --proto '=https' --proto-redir '=https' --retry 3 --progress-bar -o "$work/Ottam.dmg" "$url" ||
    fail "couldn't download $url."
  if [ -n "$length" ]; then
    size="$(wc -c < "$work/Ottam.dmg" | tr -d ' ')"
    [ "$size" = "$length" ] || fail "the download is $size bytes; the feed says $length."
  fi

  say "Checking signatures"
  check_signed "$work/Ottam.dmg" "the disk image"
  spctl --assess --type open --context context:primary-signature "$work/Ottam.dmg" 2>/dev/null ||
    fail "Gatekeeper rejected the disk image."
  # macOS 26's diskutil refuses a mount point that doesn't exist; 27 makes it.
  mkdir "$mount" || fail "couldn't make a mount point at $mount."
  diskutil image attach --readOnly --nobrowse --mountPoint "$mount" "$work/Ottam.dmg" >/dev/null ||
    fail "couldn't mount the disk image."
  app="$mount/Ottam.app"
  [ -d "$app" ] || fail "the disk image holds no Ottam.app."
  [ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist" 2>/dev/null)" = "$bundle_id" ] ||
    fail "the disk image's app isn't $bundle_id."
  # The feed's version decided this is an upgrade, so the app must be that
  # version; otherwise a feed could serve an older, still-signed release.
  app_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$app/Contents/Info.plist" 2>/dev/null || true)"
  [ "$app_version" = "$version" ] ||
    fail "the disk image holds Ottam ${app_version:-of no version}; the feed says $version."
  check_signed "$app" "Ottam.app" --deep
  spctl --assess --type execute "$app" 2>/dev/null || fail "Gatekeeper rejected Ottam.app."

  say "Installing to $dest"
  # A fresh private folder on the install volume, so the copy can't merge
  # into anything already there and the final mv is a rename.
  stage="$(mktemp -d "$install_dir/.Ottam.app.installing.XXXXXX")" ||
    fail "couldn't make a staging folder in $install_dir."
  staged="$stage/Ottam.app"
  ditto "$app" "$staged" || fail "couldn't copy Ottam.app into $install_dir."
  # The copy is what lands, so it gets the same checks as the original.
  check_signed "$staged" "the copied Ottam.app" --deep
  spctl --assess --type execute "$staged" 2>/dev/null || fail "Gatekeeper rejected the copied Ottam.app."
  ! running || fail "Ottam is running. Quit it and run this again."
  if [ -d "$dest" ]; then
    mv "$dest" "$work/previous.app" || fail "couldn't move the old Ottam.app aside."
  fi
  # On failure, cleanup puts the previous app back.
  mv "$staged" "$dest" || fail "couldn't put Ottam.app in place."

  say "Installed Ottam $version. Open it from Applications, or run: open \"$dest\""
}

say() {
  printf 'ottam: %s\n' "$*"
}

fail() {
  printf 'ottam: %s\n' "$*" >&2
  exit 1
}

no_release() {
  fail "no release has been published yet. Build Ottam from source: $source_url"
}

# Runs on every exit, including a signal mid-swap: an old app moved aside
# goes back into place, and is kept where it is if that fails.
cleanup() {
  keep=""
  if [ -d "$work/previous.app" ] && [ ! -e "$dest" ]; then
    mv "$work/previous.app" "$dest" 2>/dev/null || {
      keep=1
      printf 'ottam: your previous Ottam.app is at %s\n' "$work/previous.app" >&2
    }
  fi
  rm -rf "${stage:-}"
  if [ -d "$mount" ]; then
    diskutil eject "$mount" >/dev/null 2>&1 || diskutil eject force "$mount" >/dev/null 2>&1 || true
  fi
  [ -n "$keep" ] || rm -rf "$work"
}

# Succeeds when the app at $dest is running. ps prints each executable's full
# path, and grep -F matches $dest literally, whatever characters it holds.
running() {
  ps -axo comm= | grep -Fq "$dest/Contents/MacOS/"
}

check_platform() {
  [ "$(uname -s)" = Darwin ] || fail "Ottam runs only on macOS."
  # uname -m says x86_64 under Rosetta, so ask the hardware.
  [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = 1 ] ||
    fail "Ottam needs a Mac with Apple silicon."
  os="$(sw_vers -productVersion)"
  [ "${os%%.*}" -ge 26 ] 2>/dev/null || fail "Ottam needs macOS 26 or later; this Mac runs $os."
}

# Sets version, url, and length from the feed item with the highest
# sparkle:version, or leaves version empty when the feed has no items.
pick_latest() {
  version="" url="" length=""
  count="$(xmllint --xpath 'count(//item)' "$1" 2>/dev/null)" || fail "the feed isn't valid XML."
  i=1
  while [ "$i" -le "$count" ]; do
    item_version="$(xmllint --xpath "string(//item[$i]/*[local-name()='version'])" "$1")"
    item_url="$(xmllint --xpath "string(//item[$i]/enclosure/@url)" "$1")"
    if [ -n "$item_url" ] && { [ -z "$version" ] || version_newer "$item_version" "$version"; }; then
      version="$item_version"
      url="$item_url"
      length="$(xmllint --xpath "string(//item[$i]/enclosure/@length)" "$1")"
    fi
    i=$((i + 1))
  done
}

# Succeeds when dotted version $1 is newer than $2 (26.11.10 > 26.11.9).
version_newer() {
  a="$1" b="$2"
  while [ -n "$a" ] || [ -n "$b" ]; do
    x="${a%%.*}" y="${b%%.*}"
    [ "${x:-0}" -gt "${y:-0}" ] && return 0
    [ "${x:-0}" -lt "${y:-0}" ] && return 1
    case "$a" in *.*) a="${a#*.}" ;; *) a="" ;; esac
    case "$b" in *.*) b="${b#*.}" ;; *) b="" ;; esac
  done
  return 1
}

# Requires a valid Developer ID signature on $1 from Ottam's team, chained to
# Apple's root; $3 = --deep checks nested code too.
check_signed() {
  requirement="=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"$team_id\""
  if [ "${3:-}" = --deep ]; then
    codesign --verify --strict --deep -R "$requirement" "$1" 2>/dev/null || fail "$2 has an invalid signature."
  else
    codesign --verify --strict -R "$requirement" "$1" 2>/dev/null || fail "$2 has an invalid signature."
  fi
  signer="$(codesign --display --verbose=2 "$1" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
  [ "$signer" = "$team_id" ] || fail "$2 is signed by team ${signer:-none}, not Ottam's ($team_id)."
}

main "$@"
