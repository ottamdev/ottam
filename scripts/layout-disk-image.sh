#!/usr/bin/env bash
# Renders the release disk image's background and has Finder lay out its
# window once, saving what release-app.sh copies into every image:
#   app/Packaging/DiskImage/background.tiff   1x and 2x, from
#                                             assets/brand/disk-image-background.svg
#   app/Packaging/DiskImage/DS_Store          Finder's layout, stored without
#                                             its dot so .gitignore keeps it
# Run it on a Mac with a logged-in Finder after changing the art or the layout;
# macOS asks once to let the terminal control Finder. Release builds never
# drive Finder. Needs rsvg-convert (brew install librsvg).
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/release-disk-image.sh
source "$ROOT_DIR/scripts/lib/release-disk-image.sh"

SOURCE="$ROOT_DIR/assets/brand/disk-image-background.svg"
OUTPUT="$ROOT_DIR/$RELEASE_DISK_IMAGE_LAYOUT"
# The window's frame, title bar included, matching the art. Finder draws the
# background from the top of the content area, so the art's bottom 32 points
# or so sit under nothing and stay empty.
WIDTH=660
HEIGHT=400
ICON_SIZE=128
# Icon centers, in content coordinates.
APP_POSITION="170, 180"
APPLICATIONS_POSITION="490, 180"
# Finder saves the window's origin from the bottom of the screen, so a low
# origin keeps the window on screen on short laptop displays.
LEFT=200
BOTTOM=200

command -v rsvg-convert >/dev/null || { echo "rsvg-convert is missing: brew install librsvg" >&2; exit 69; }
VOLUME="/Volumes/$RELEASE_VOLUME_NAME"
[ ! -e "$VOLUME" ] || { echo "Eject the volume at $VOLUME first." >&2; exit 75; }

work="$(mktemp -d "${TMPDIR:-/tmp}/ottam-disk-image-layout.XXXXXX")"
device=""
cleanup() {
  [ -z "$device" ] || diskutil eject force "$device" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

mkdir -p "$OUTPUT"
rsvg-convert --width "$WIDTH" --height "$HEIGHT" -o "$work/background.png" "$SOURCE"
rsvg-convert --width $((WIDTH * 2)) --height $((HEIGHT * 2)) -o "$work/background@2x.png" "$SOURCE"
tiffutil -cathidpicheck "$work/background.png" "$work/background@2x.png" \
  -out "$OUTPUT/background.tiff" 2>/dev/null

# Lay out on a RAM disk with the release volume's name: Finder records where
# the volume's backing file lives, and a RAM disk has none, so the layout
# carries nothing from this Mac. The name must match the shipped image, since
# the layout finds its background through the volume.
device="$(diskutil image attach --noMount ram://102400 | awk 'NR == 1 { print $1 }')"
release_quietly diskutil eraseDisk APFS "$RELEASE_VOLUME_NAME" GPT "$device"
[ -d "$VOLUME" ] || { echo "The RAM disk didn't mount at $VOLUME." >&2; exit 70; }
mkdir "$VOLUME/Ottam.app"
release_stage_disk_image "$VOLUME" "$ROOT_DIR"

osascript - "$RELEASE_VOLUME_NAME" "$WIDTH" "$HEIGHT" "$ICON_SIZE" \
  "$APP_POSITION" "$APPLICATIONS_POSITION" "$LEFT" "$BOTTOM" <<'APPLESCRIPT'
on run {volumeName, frameWidth, frameHeight, iconSize, appPosition, applicationsPosition, frameLeft, frameBottom}
  set {appX, appY} to my coordinates(appPosition)
  set {applicationsX, applicationsY} to my coordinates(applicationsPosition)
  tell application "Finder"
    set screenHeight to item 4 of (get bounds of window of desktop)
    set frameTop to screenHeight - frameBottom - frameHeight
    tell disk volumeName
      open
      set theWindow to container window
      set current view of theWindow to icon view
      set toolbar visible of theWindow to false
      set statusbar visible of theWindow to false
      set bounds of theWindow to {frameLeft, frameTop, frameLeft + frameWidth, frameTop + frameHeight}
      set theOptions to icon view options of theWindow
      set arrangement of theOptions to not arranged
      set icon size of theOptions to iconSize
      set text size of theOptions to 13
      set background picture of theOptions to file ".background:background.tiff"
      set position of item "Ottam.app" of theWindow to {appX, appY}
      set position of item "Applications" of theWindow to {applicationsX, applicationsY}
      close
      open
      update without registering applications
      delay 2
      close
    end tell
  end tell
end run

on coordinates(pair)
  set AppleScript's text item delimiters to ", "
  set {x, y} to text items of pair
  set AppleScript's text item delimiters to ""
  return {x as integer, y as integer}
end coordinates
APPLESCRIPT

# Finder writes .DS_Store on each close, sometimes a moment later: wait until
# it holds the background and has stopped changing.
previous=""
for _ in $(seq 1 20); do
  sleep 1
  [ -s "$VOLUME/.DS_Store" ] && grep -aq background.tiff "$VOLUME/.DS_Store" || continue
  current="$(stat -f '%m %z' "$VOLUME/.DS_Store")"
  [ "$current" != "$previous" ] || break
  previous="$current"
done
grep -aq background.tiff "$VOLUME/.DS_Store" 2>/dev/null ||
  { echo "Finder saved no layout to $VOLUME/.DS_Store." >&2; exit 70; }
cp "$VOLUME/.DS_Store" "$OUTPUT/DS_Store"
echo "Wrote ${OUTPUT#"$ROOT_DIR"/}/background.tiff and DS_Store"
