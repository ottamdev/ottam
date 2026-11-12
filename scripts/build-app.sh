#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_PACKAGE_DIR="$ROOT_DIR/app"
SWIFT_BUILD_DIR="$ROOT_DIR/.build/swift"
OUTPUT_ROOT="$ROOT_DIR/.build/app"
APP_NAME="Ottam"
DISPLAY_NAME=""
BUNDLE_IDENTIFIER="dev.ottam.Ottam"
EXECUTABLE_NAME="Ottam"
SKIP_BUILD=0
# debug (default dev loop), release (shipping), or profile.
#
# The UI-test and screenshot hooks are `#if DEBUG`, so an optimized bundle the
# scenario runner can still drive is a third thing: `profile` is `-c release`
# with `-DDEBUG` kept, which is the only way to measure optimized code on the
# same driven path the debug build takes. Its core keeps the test seams too
# (`--features test-seams`). Profile builds into its own Swift scratch path and
# Cargo profile, so `--release --skip-build` can never pick up its products.
CONFIGURATION=debug
RELEASE_VERSION=""
NO_SIGN=0

usage() {
  cat <<'USAGE'
Usage: scripts/build-app.sh [options]

Builds and stages a signed Ottam application bundle.

Options:
  --output-root <path>       Staging directory (default: .build/app)
  --app-name <name>          Bundle filename (default: Ottam)
  --display-name <name>      Visible app name (default: --app-name)
  --bundle-id <identifier>   Bundle identifier (default: dev.ottam.Ottam)
  --executable-name <name>   Main executable name (default: Ottam)
  --release                  Build optimized (no DEBUG-gated testing hooks)
  --profile                  Build optimized but keep the DEBUG testing hooks
  --skip-build               Reuse existing Swift and Rust build products
  --version <YY.M.N>         With --release: stamp a release version, require
                             Cargo.lock to be current, and strip local paths
  --no-sign                  With --version: leave signing to the release script
  -h, --help                 Show this help
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --output-root)
      OUTPUT_ROOT="${2:?--output-root requires a path}"
      shift 2
      ;;
    --app-name)
      APP_NAME="${2:?--app-name requires a name}"
      shift 2
      ;;
    --display-name)
      DISPLAY_NAME="${2:?--display-name requires a name}"
      shift 2
      ;;
    --bundle-id)
      BUNDLE_IDENTIFIER="${2:?--bundle-id requires an identifier}"
      shift 2
      ;;
    --executable-name)
      EXECUTABLE_NAME="${2:?--executable-name requires a name}"
      shift 2
      ;;
    --release)
      CONFIGURATION=release
      shift
      ;;
    --profile)
      CONFIGURATION=profile
      shift
      ;;
    --skip-build)
      SKIP_BUILD=1
      shift
      ;;
    --version)
      RELEASE_VERSION="${2:?--version requires a version}"
      shift 2
      ;;
    --no-sign)
      NO_SIGN=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 64
      ;;
  esac
done

DISPLAY_NAME="${DISPLAY_NAME:-$APP_NAME}"
# In UTF-8 locales [:cntrl:] includes format characters such as emoji joiners.
# Check ASCII controls in a subshell so Unicode display labels remain valid.
if (LC_ALL=C; [[ "$DISPLAY_NAME" =~ [[:cntrl:]] ]]) || [ -z "${DISPLAY_NAME//[[:space:]]/}" ]; then
  echo "Invalid application display name." >&2
  exit 64
fi

if [ -n "$RELEASE_VERSION" ]; then
  # shellcheck source=scripts/lib/release-version.sh
  source "$ROOT_DIR/scripts/lib/release-version.sh"
  if ! release_version_is_valid "$RELEASE_VERSION"; then
    echo "Invalid release version: $RELEASE_VERSION" >&2
    exit 64
  fi
  if [ "$CONFIGURATION" != release ]; then
    echo "--version requires --release." >&2
    exit 64
  fi
elif [ "$NO_SIGN" = 1 ]; then
  echo "--no-sign requires --version." >&2
  exit 64
fi

case "$OUTPUT_ROOT" in
  /*) ;;
  *) OUTPUT_ROOT="$ROOT_DIR/$OUTPUT_ROOT" ;;
esac
mkdir -p "$(dirname "$OUTPUT_ROOT")"
OUTPUT_ROOT="$(cd "$(dirname "$OUTPUT_ROOT")" && pwd -P)/$(basename "$OUTPUT_ROOT")"

if [ "$OUTPUT_ROOT" = "/" ] || [ "$OUTPUT_ROOT" = "$ROOT_DIR" ]; then
  echo "Refusing unsafe output root: $OUTPUT_ROOT" >&2
  exit 64
fi
if ! [[ "$APP_NAME" =~ ^[A-Za-z0-9._\ -]+$ ]] ||
   ! [[ "$EXECUTABLE_NAME" =~ ^[A-Za-z0-9._-]+$ ]] ||
   ! [[ "$BUNDLE_IDENTIFIER" =~ ^[A-Za-z0-9.-]+$ ]]; then
  echo "Invalid application staging identity." >&2
  exit 64
fi

APP_BUNDLE_DIR="$OUTPUT_ROOT/$APP_NAME.app"
CONTENTS_DIR="$APP_BUNDLE_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"
GHOSTTY_RESOURCES_SOURCE="$ROOT_DIR/app/Vendor/ottam-ghostty/resources"

cd "$ROOT_DIR"

case "$CONFIGURATION" in
  debug)
    SWIFT_CONFIGURATION=debug
    SWIFT_EXTRA_ARGUMENTS=()
    CARGO_EXTRA_ARGUMENTS=()
    CARGO_PROFILE_DIR=debug
    ;;
  release)
    SWIFT_CONFIGURATION=release
    SWIFT_EXTRA_ARGUMENTS=()
    CARGO_EXTRA_ARGUMENTS=(--release)
    CARGO_PROFILE_DIR=release
    ;;
  profile)
    SWIFT_BUILD_DIR="$ROOT_DIR/.build/swift-profile"
    SWIFT_CONFIGURATION=release
    SWIFT_EXTRA_ARGUMENTS=(-Xswiftc -DDEBUG)
    CARGO_EXTRA_ARGUMENTS=(--profile profiling --features test-seams)
    CARGO_PROFILE_DIR=profiling
    ;;
esac

CARGO_TARGET_ROOT="$ROOT_DIR/backend/target"
if [ -n "$RELEASE_VERSION" ]; then
  # Separate output dirs: these flags would otherwise invalidate the dev caches.
  SWIFT_BUILD_DIR="$ROOT_DIR/.build/swift-release"
  CARGO_TARGET_ROOT="$ROOT_DIR/.build/rust-release"
  SWIFT_EXTRA_ARGUMENTS=(-Xswiftc -file-prefix-map -Xswiftc "$ROOT_DIR=.")
  CARGO_EXTRA_ARGUMENTS=(--release --locked --target-dir "$CARGO_TARGET_ROOT")
  RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$ROOT_DIR=. --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=${RUSTUP_HOME:-$HOME/.rustup}=/rustup"
  export RUSTFLAGS
fi

if [ -n "$RELEASE_VERSION" ]; then
  # A clean tree can't see the gitignored vendor copy, so a release refetches it.
  "$ROOT_DIR/scripts/fetch-ghosttykit.sh" --verify
else
  "$ROOT_DIR/scripts/fetch-ghosttykit.sh"
fi

if [ "$SKIP_BUILD" = "0" ]; then
  swift build \
    --package-path "$APP_PACKAGE_DIR" \
    --scratch-path "$SWIFT_BUILD_DIR" \
    --product OttamApp \
    -Xswiftc -warnings-as-errors \
    -c "$SWIFT_CONFIGURATION" \
    "${SWIFT_EXTRA_ARGUMENTS[@]+"${SWIFT_EXTRA_ARGUMENTS[@]}"}"
  "$ROOT_DIR/scripts/prune-rust-target.sh"
  cargo build --manifest-path "$ROOT_DIR/backend/Cargo.toml" -p ottam-core \
    "${CARGO_EXTRA_ARGUMENTS[@]+"${CARGO_EXTRA_ARGUMENTS[@]}"}"
fi

SWIFT_EXECUTABLE="$SWIFT_BUILD_DIR/$SWIFT_CONFIGURATION/OttamApp"
CORE_EXECUTABLE="$CARGO_TARGET_ROOT/$CARGO_PROFILE_DIR/ottam-core"
if [ ! -x "$SWIFT_EXECUTABLE" ] || [ ! -x "$CORE_EXECUTABLE" ]; then
  echo "Missing build products; run without --skip-build first." >&2
  exit 66
fi

rm -rf "$APP_BUNDLE_DIR"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"

cp "$SWIFT_EXECUTABLE" "$MACOS_DIR/$EXECUTABLE_NAME"
cp "$CORE_EXECUTABLE" "$MACOS_DIR/ottam-core"
cp "$APP_PACKAGE_DIR/Resources/Info.plist" "$CONTENTS_DIR/Info.plist"
plutil -replace CFBundleIdentifier -string "$BUNDLE_IDENTIFIER" "$CONTENTS_DIR/Info.plist"
plutil -replace CFBundleName -string "$DISPLAY_NAME" "$CONTENTS_DIR/Info.plist"
plutil -replace CFBundleDisplayName -string "$DISPLAY_NAME" "$CONTENTS_DIR/Info.plist" 2>/dev/null ||
  plutil -insert CFBundleDisplayName -string "$DISPLAY_NAME" "$CONTENTS_DIR/Info.plist"
plutil -replace CFBundleExecutable -string "$EXECUTABLE_NAME" "$CONTENTS_DIR/Info.plist"
if [ -n "$RELEASE_VERSION" ]; then
  plutil -replace CFBundleShortVersionString -string "$RELEASE_VERSION" "$CONTENTS_DIR/Info.plist"
  plutil -replace CFBundleVersion -string "$RELEASE_VERSION" "$CONTENTS_DIR/Info.plist"
fi

if [ ! -d "$APP_PACKAGE_DIR/Resources/AppResources" ]; then
  echo "Missing app resources at $APP_PACKAGE_DIR/Resources/AppResources" >&2
  echo "Run scripts/generate-brand-assets.py before building the app bundle." >&2
  exit 1
fi
cp -R "$APP_PACKAGE_DIR/Resources/AppResources/." "$RESOURCES_DIR/"
cp "$ROOT_DIR/LICENSE" "$RESOURCES_DIR/LICENSE"
cp "$ROOT_DIR/THIRD_PARTY_NOTICES.md" "$RESOURCES_DIR/THIRD_PARTY_NOTICES.md"

rsync -a --delete "$GHOSTTY_RESOURCES_SOURCE/ghostty/" "$RESOURCES_DIR/ghostty/"
rsync -a --delete "$GHOSTTY_RESOURCES_SOURCE/terminfo/" "$RESOURCES_DIR/terminfo/"

if [ "$NO_SIGN" = 1 ]; then
  echo "Built $APP_BUNDLE_DIR (unsigned)"
  exit 0
fi

# macOS ties an ad-hoc app's privacy grants (Screen Recording, Accessibility)
# to that exact build, so every rebuild silently loses them. A lone Apple
# Development identity keeps them; OTTAM_CODESIGN_IDENTITY picks one, and
# "-" forces ad hoc. Machines without one, such as CI, stay ad hoc.
SIGNING_IDENTITY="${OTTAM_CODESIGN_IDENTITY:-}"
if [ -z "$SIGNING_IDENTITY" ]; then
  development_identities="$(
    security find-identity -v -p codesigning 2>/dev/null |
      sed -nE 's/^[[:space:]]*[0-9]+\) ([0-9A-F]{40}) "Apple Development:[^"]+"$/\1/p'
  )"
  if [ -n "$development_identities" ] && [ "$(wc -l <<< "$development_identities" | tr -d ' ')" = 1 ]; then
    SIGNING_IDENTITY="$development_identities"
  else
    SIGNING_IDENTITY="-"
  fi
fi
codesign --force --deep --timestamp=none --sign "$SIGNING_IDENTITY" "$APP_BUNDLE_DIR" >/dev/null
codesign --verify --deep --strict "$APP_BUNDLE_DIR"

echo "Built $APP_BUNDLE_DIR"
echo "Signing identity: $([ "$SIGNING_IDENTITY" = "-" ] && echo "ad hoc" || echo "$SIGNING_IDENTITY")"
echo "Display name: $DISPLAY_NAME"
echo "Bundle identifier: $BUNDLE_IDENTIFIER"
echo "Executable: $MACOS_DIR/$EXECUTABLE_NAME"
