#!/usr/bin/env bash
# The update feeds, served from the ottamdev/updates repository's public/
# directory. The production URL is baked into every shipped build, so it must
# outlive any host. The test feed serves update walkthroughs only; a build
# that points at it is never published to production.

RELEASE_FEED_URL="https://updates.ottam.dev/appcast.xml"
RELEASE_TEST_FEED_URL="https://updates.ottam.dev/test/appcast.xml"

# Prints "<feed URL> <path in the updates repository>" for a feed name, or
# fails for an unknown one.
release_feed() {
  case "${1:-}" in
    production) echo "$RELEASE_FEED_URL public/appcast.xml" ;;
    test) echo "$RELEASE_TEST_FEED_URL public/test/appcast.xml" ;;
    *) return 64 ;;
  esac
}
