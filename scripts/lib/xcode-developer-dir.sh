# Export an Xcode DEVELOPER_DIR for this run: swift-testing's Testing.framework
# ships in Xcode's MacOSX platform, not the Command Line Tools, and a global
# `xcode-select -s` needs sudo. Builds under this toolchain use
# .build/swift-tests, so they never thrash the default toolchain's .build/swift.
if [ -z "${DEVELOPER_DIR:-}" ]; then
  selected="$(xcode-select -p 2>/dev/null || true)"
  case "$selected" in
    */Xcode*.app/Contents/Developer)
      DEVELOPER_DIR="$selected"
      ;;
    *)
      for candidate in /Applications/Xcode*.app/Contents/Developer; do
        if [ -d "$candidate" ]; then
          DEVELOPER_DIR="$candidate"
          break
        fi
      done
      ;;
  esac
fi

if [ -z "${DEVELOPER_DIR:-}" ] || [ ! -d "$DEVELOPER_DIR" ]; then
  echo "$(basename "$0"): no Xcode developer directory found." >&2
  echo "swift-testing lives in Xcode's MacOSX platform, not the Command Line Tools." >&2
  echo "Install Xcode, or set DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer" >&2
  exit 69
fi
export DEVELOPER_DIR

if [ ! -d "$DEVELOPER_DIR/Platforms/MacOSX.platform/Developer/Library/Frameworks/Testing.framework" ]; then
  echo "$(basename "$0"): $DEVELOPER_DIR has no Testing.framework; Xcode may be too old." >&2
  exit 69
fi
