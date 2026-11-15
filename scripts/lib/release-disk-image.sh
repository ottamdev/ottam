#!/usr/bin/env bash
# The release disk image: what it holds, and how scripts look inside it.
# Sparkle installs updates from the same image people download, copying the
# app out and skipping hidden items and top-level symlinks such as the
# Applications link.

RELEASE_VOLUME_NAME="Ottam"
# Finder's window layout and its background, made by
# scripts/layout-disk-image.sh; relative to the repository root.
RELEASE_DISK_IMAGE_LAYOUT="app/Packaging/DiskImage"

# Runs a command, showing its output only when it fails; diskutil prints
# progress on stderr.
release_quietly() {
  local output
  output="$("$@" 2>&1)" || { echo "$output" >&2; return 1; }
}

# Adds what sits beside Ottam.app in every image to the folder $1, from the
# repository at $2. The layout itself (.DS_Store) is added separately.
release_stage_disk_image() {
  ln -s /Applications "$1/Applications"
  mkdir "$1/.background"
  cp "$2/$RELEASE_DISK_IMAGE_LAYOUT/background.tiff" "$1/.background/background.tiff"
}

# Builds the release image at $2 from a folder ($1) holding Ottam.app, with the
# layout committed in the repository at $3. diskutil leaves .DS_Store out of
# an image made from a folder, so the layout goes into a writable image that
# is then compressed (APFS, lzfse, as Sparkle recommends). fseventsd may log
# to the writable mount; that log is removed before the eject.
release_create_disk_image() {
  local root="$1" output="$2" repo="$3" raw="$2.raw.dmg" mount="$2.mount"
  release_stage_disk_image "$root" "$repo" || return 1
  release_quietly diskutil image create from --format RAW --volumeName "$RELEASE_VOLUME_NAME" \
    "$root" "$raw" || return 1
  release_quietly diskutil image attach --nobrowse --mountPoint "$mount" "$raw" || return 1
  if ! cp "$repo/$RELEASE_DISK_IMAGE_LAYOUT/DS_Store" "$mount/.DS_Store" ||
    ! rm -rf "$mount/.fseventsd"; then
    release_detach_disk_image "$mount"
    return 1
  fi
  release_detach_disk_image "$mount"
  [ ! -d "$mount" ] || { echo "Couldn't eject $mount." >&2; return 1; }
  release_quietly diskutil image create from --format ULFO "$raw" "$output" || return 1
  rm -f "$raw"
}

# Prints the image's top level as `name` or `name -> target`, sorted.
release_disk_image_layout() {
  local entry
  for entry in "$1"/* "$1"/.[!.]* "$1"/..?*; do
    [ -e "$entry" ] || [ -L "$entry" ] || continue
    if [ -L "$entry" ]; then
      echo "$(basename "$entry") -> $(readlink "$entry")"
    else
      basename "$entry"
    fi
  done | LC_ALL=C sort
}

# Checks a mounted image ($1) holds exactly Ottam.app, the Applications link,
# and the layout committed in the repository at $2; prints what's wrong.
release_check_disk_image() {
  local layout expected
  layout="$(release_disk_image_layout "$1")"
  expected="$(printf '%s\n' .DS_Store .background "Applications -> /Applications" Ottam.app)"
  if [ "$layout" != "$expected" ]; then
    echo "the disk image should hold Ottam.app, an Applications link, and its layout, not:"
    echo "$layout"
    return 1
  fi
  if ! cmp -s "$1/.DS_Store" "$2/$RELEASE_DISK_IMAGE_LAYOUT/DS_Store" ||
      ! cmp -s "$1/.background/background.tiff" "$2/$RELEASE_DISK_IMAGE_LAYOUT/background.tiff"; then
    echo "the disk image's window layout isn't the one in $RELEASE_DISK_IMAGE_LAYOUT"
    return 1
  fi
}

# Mounts the image read-only at the given path, out of Finder's sight.
release_attach_disk_image() {
  diskutil image attach --readOnly --nobrowse --mountPoint "$2" "$1" >/dev/null
}

# Ejects the image mounted at $1, forcing it if needed; never fails.
release_detach_disk_image() {
  [ -d "$1" ] || return 0
  diskutil eject "$1" >/dev/null 2>&1 || diskutil eject force "$1" >/dev/null 2>&1 || true
}
