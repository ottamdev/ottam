#!/usr/bin/env bash
# Loads Ottam's release credentials into the GitHub `release` environment.
# The maintainer runs it on the Mac that holds them; values go straight from
# the keychain or the given files into `gh secret set` and never touch the
# repository. Rerun it to rotate any of them.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="ottamdev/ottam"
UPDATES_REPO="ottamdev/updates"
ENVIRONMENT="release"
SPARKLE_ACCOUNT="ottam"
SPARKLE_BIN="$ROOT_DIR/.build/swift/artifacts/sparkle/Sparkle/bin"
DEPLOY_KEY_TITLE="ottam release workflow"
P12=""
API_KEY=""
KEY_ID=""
ISSUER=""

usage() {
  cat <<'USAGE'
Usage:
  scripts/setup-release-secrets.sh --p12 <DeveloperID.p12> \
      --api-key <AuthKey_XXXX.p8> --key-id <key id> --issuer <issuer id>

Creates or updates the ottamdev/ottam `release` environment (you as required
reviewer, `v*` tags only) and sets its secrets:

  DEVELOPER_ID_P12, DEVELOPER_ID_P12_PASSWORD   Developer ID Application identity
  NOTARY_API_KEY, NOTARY_API_KEY_ID, NOTARY_API_ISSUER
                                                App Store Connect key for notarytool
  SPARKLE_ED_PRIVATE_KEY                        exported from the `ottam` keychain item
  UPDATES_DEPLOY_KEY                            new write deploy key on ottamdev/updates

Each credential is checked before upload. Needs gh signed in with repo admin,
and a prior build so Sparkle's tools exist under .build/swift.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --p12) P12="${2:?--p12 requires a file}"; shift 2 ;;
    --api-key) API_KEY="${2:?--api-key requires a file}"; shift 2 ;;
    --key-id) KEY_ID="${2:?--key-id requires a value}"; shift 2 ;;
    --issuer) ISSUER="${2:?--issuer requires a value}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 64 ;;
  esac
done

fail() {
  echo "setup-release-secrets: $*" >&2
  exit 1
}

[ -n "$P12" ] && [ -n "$API_KEY" ] && [ -n "$KEY_ID" ] && [ -n "$ISSUER" ] || { usage >&2; exit 64; }
[ -f "$P12" ] || fail "no such file: $P12"
[ -f "$API_KEY" ] || fail "no such file: $API_KEY"
[ -x "$SPARKLE_BIN/generate_keys" ] || fail "Sparkle tools missing; run a swift build first"
gh auth status >/dev/null 2>&1 || fail "gh is not signed in"

WORK="$(mktemp -d)"
TEMP_KEYCHAIN="$WORK/check.keychain-db"
cleanup() {
  security delete-keychain "$TEMP_KEYCHAIN" >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

set_secret() {
  gh secret set "$1" --repo "$REPO" --env "$ENVIRONMENT" >/dev/null
  echo "  set $1"
}

echo "Environment '$ENVIRONMENT' on $REPO"
reviewer_id="$(gh api user -q .id)"
printf '{"reviewers":[{"type":"User","id":%s}],"deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":true}}' \
  "$reviewer_id" | gh api --method PUT "repos/$REPO/environments/$ENVIRONMENT" --input - >/dev/null
if ! gh api "repos/$REPO/environments/$ENVIRONMENT/deployment-branch-policies" \
    -q '.branch_policies[] | select(.type == "tag") | .name' | grep -qx 'v\*'; then
  gh api --method POST "repos/$REPO/environments/$ENVIRONMENT/deployment-branch-policies" \
    -f name='v*' -f type=tag >/dev/null
fi
echo "  required reviewer: $(gh api user -q .login); deploys: v* tags"

echo "Developer ID"
read -rsp "  .p12 password: " p12_password
echo
security create-keychain -p check "$TEMP_KEYCHAIN"
security import "$P12" -k "$TEMP_KEYCHAIN" -P "$p12_password" -T /usr/bin/codesign >/dev/null \
  || fail "the .p12 did not import with that password"
security find-identity -v -p codesigning "$TEMP_KEYCHAIN" | grep -q '"Developer ID Application: ' \
  || fail "the .p12 holds no Developer ID Application identity with its private key"
base64 -i "$P12" | set_secret DEVELOPER_ID_P12
printf '%s' "$p12_password" | set_secret DEVELOPER_ID_P12_PASSWORD
unset p12_password

echo "Notarization"
xcrun notarytool history --key "$API_KEY" --key-id "$KEY_ID" --issuer "$ISSUER" >/dev/null \
  || fail "notarytool rejected the API key, key id, or issuer"
set_secret NOTARY_API_KEY < "$API_KEY"
printf '%s' "$KEY_ID" | set_secret NOTARY_API_KEY_ID
printf '%s' "$ISSUER" | set_secret NOTARY_API_ISSUER

echo "Sparkle (macOS may ask to allow keychain access)"
"$SPARKLE_BIN/generate_keys" --account "$SPARKLE_ACCOUNT" -x "$WORK/sparkle" >/dev/null
keychain_public="$("$SPARKLE_BIN/generate_keys" --account "$SPARKLE_ACCOUNT" -p)"
[ "$keychain_public" = "$(tr -d '[:space:]' < "$ROOT_DIR/app/Signing/sparkle-public-key")" ] \
  || fail "the keychain's Sparkle key does not match app/Signing/sparkle-public-key"
set_secret SPARKLE_ED_PRIVATE_KEY < "$WORK/sparkle"

echo "Feed deploy key on $UPDATES_REPO"
gh repo deploy-key list --repo "$UPDATES_REPO" --json id,title \
  -q ".[] | select(.title == \"$DEPLOY_KEY_TITLE\") | .id" |
  while read -r old_id; do
    gh repo deploy-key delete "$old_id" --repo "$UPDATES_REPO" >/dev/null
    echo "  removed the previous key"
  done
ssh-keygen -q -t ed25519 -N "" -C "$DEPLOY_KEY_TITLE" -f "$WORK/deploy"
gh repo deploy-key add "$WORK/deploy.pub" --repo "$UPDATES_REPO" --allow-write --title "$DEPLOY_KEY_TITLE" >/dev/null
set_secret UPDATES_DEPLOY_KEY < "$WORK/deploy"

echo "Done. Secrets in '$ENVIRONMENT':"
gh secret list --repo "$REPO" --env "$ENVIRONMENT"
