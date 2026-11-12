#!/usr/bin/env bash
set -euo pipefail

# Regenerates the Rust dependency section of THIRD_PARTY_NOTICES.md with
# cargo-about, leaving the hand-written sections outside the markers intact.
# Rerun after Cargo.lock changes. Requires:
#   cargo install cargo-about --locked --features cli

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NOTICES="$ROOT_DIR/THIRD_PARTY_NOTICES.md"
BEGIN_MARKER="<!-- BEGIN GENERATED RUST NOTICES -->"
END_MARKER="<!-- END GENERATED RUST NOTICES -->"

if ! cargo about --version >/dev/null 2>&1; then
  echo "cargo-about is missing; install it with:" >&2
  echo "  cargo install cargo-about --locked --features cli" >&2
  exit 1
fi

generated="$(mktemp)"
trap 'rm -f "$generated" "$generated.notices"' EXIT

cargo about generate \
  --config "$ROOT_DIR/backend/about.toml" \
  --manifest-path "$ROOT_DIR/backend/crates/ottam-core/Cargo.toml" \
  --locked \
  --fail \
  "$ROOT_DIR/backend/about.hbs" |
  # Upstream license files carry CRLF and trailing spaces; the whitespace
  # pre-commit check rejects both.
  sed -e $'s/\r$//' -e 's/[[:space:]]*$//' > "$generated"

awk -v begin="$BEGIN_MARKER" -v end="$END_MARKER" -v generated="$generated" '
  $0 == begin {
    print
    while ((getline line < generated) > 0) print line
    skipping = 1
    found_begin = 1
    next
  }
  $0 == end { skipping = 0; found_end = 1 }
  !skipping { print }
  END { if (!found_begin || !found_end) exit 1 }
' "$NOTICES" > "$generated.notices" || {
  echo "Missing generated-section markers in $NOTICES" >&2
  exit 1
}
mv "$generated.notices" "$NOTICES"
echo "Updated $NOTICES"
