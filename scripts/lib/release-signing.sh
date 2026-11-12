#!/usr/bin/env bash
# What a release bundle signs, and with which identity.

# Prints bundle-relative paths of the separately signed code, innermost first.
# The bundle itself is signed last and covers its main executable. Adding a
# binary to the bundle means adding it here, or verify-release-app.sh refuses
# the build.
release_nested_code() {
  echo "Contents/MacOS/ottam-core"
}

# Prints the SHA-1 of the one valid codesigning identity whose name starts with
# $1 ("Developer ID Application" or "Apple Development"). A non-empty $2 narrows
# the search to an exact SHA-1 or full identity name. Zero or several matches
# are an error, so signing never guesses or falls back.
release_resolve_identity() {
  local kind="$1" requested="${2:-}" identities matches count
  identities="$(
    security find-identity -v -p codesigning 2>/dev/null |
      sed -nE 's/^[[:space:]]*[0-9]+\) ([0-9A-F]{40}) "(.*)"$/\1 \2/p'
  )"
  matches="$(
    while read -r hash name; do
      [[ "$name" == "$kind:"* ]] || continue
      if [ -z "$requested" ] || [ "$requested" = "$hash" ] || [ "$requested" = "$name" ]; then
        echo "$hash"
      fi
    done <<< "$identities"
  )"
  count="$(grep -c . <<< "$matches" || true)"
  if [ "$count" != 1 ]; then
    echo "Expected exactly one \"$kind\" signing identity${requested:+ matching $requested}; found $count." >&2
    return 69
  fi
  echo "$matches"
}
