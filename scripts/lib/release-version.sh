#!/usr/bin/env bash
# Release version rules. A version is YY.M.N: the two-digit UTC year and month
# of the cut, then a counter of releases within that month. No part has a
# leading zero, because Cargo requires valid semver.
#
# Only the cut reads the date. Everything after it reads the tag, so a rerun
# that crosses a month boundary keeps its version.

release_version_is_valid() {
  [[ "${1:-}" =~ ^[1-9][0-9]\.([1-9]|1[0-2])\.(0|[1-9][0-9]*)$ ]]
}

# Succeeds when version $1 is strictly higher than version $2.
release_version_is_newer() {
  local a_year a_month a_count b_year b_month b_count
  IFS=. read -r a_year a_month a_count <<< "$1"
  IFS=. read -r b_year b_month b_count <<< "$2"
  [ "$a_year" -gt "$b_year" ] && return 0
  [ "$a_year" -lt "$b_year" ] && return 1
  [ "$a_month" -gt "$b_month" ] && return 0
  [ "$a_month" -lt "$b_month" ] && return 1
  [ "$a_count" -gt "$b_count" ]
}

# Prints the version a release cut on UTC date $1 (YYYY-MM-DD) gets, given the
# repository's tags as the remaining arguments. Tags that aren't release tags
# are ignored.
release_next_version() {
  local date="${1:-}"
  if ! [[ "$date" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
    echo "Invalid release date: $date" >&2
    return 64
  fi
  shift

  local year="${date:2:2}" month
  month="$((10#${date:5:2}))"
  local prefix="$year.$month." count=-1 latest="" tag version part
  for tag in "$@"; do
    version="${tag#v}"
    [ "$version" != "$tag" ] || continue
    release_version_is_valid "$version" || continue
    if [ -z "$latest" ] || release_version_is_newer "$version" "$latest"; then
      latest="$version"
    fi
    case "$version" in
      "$prefix"*)
        part="${version#"$prefix"}"
        [ "$part" -le "$count" ] || count="$part"
        ;;
    esac
  done

  local next="$year.$month.$((count + 1))"
  if ! release_version_is_valid "$next"; then
    echo "Release date $date does not produce a valid version: $next" >&2
    return 65
  fi
  if [ -n "$latest" ] && ! release_version_is_newer "$next" "$latest"; then
    echo "Next version $next is not newer than v$latest; check the system clock." >&2
    return 65
  fi
  echo "$next"
}

# Prints the version in the [workspace.package] table of the Cargo.toml at $1.
release_cargo_version() {
  awk '
    /^\[/ { in_table = ($0 == "[workspace.package]"); next }
    in_table && /^version[[:space:]]*=/ {
      value = $0
      sub(/^[^"]*"/, "", value)
      sub(/".*$/, "", value)
      print value
      exit
    }
  ' "$1"
}
