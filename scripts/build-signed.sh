#!/usr/bin/env bash
#
# Build a signed + notarized macOS release of Launchpad.
#
# Prereqs (one-time — see docs/macos-signing.md):
#   1. A "Developer ID Application" cert installed in your login Keychain.
#   2. An App Store Connect API key (.p8) + its Key ID and Issuer ID.
#   3. A gitignored secrets file at src-tauri/.env.signing (copy the template
#      from docs/macos-signing.md and fill in your values).
#
# Tauri auto-signs (hardened runtime), notarizes, and staples the .app/.dmg
# when these env vars are present at build time. This script loads them, builds,
# and then verifies the result.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="$REPO_ROOT/src-tauri/.env.signing"

# Local runs load secrets from the gitignored env file. In CI the same vars are
# injected from repo secrets (see .github/workflows/release.yml), so a missing
# file is fine as long as the required vars are already exported.
if [[ -f "$ENV_FILE" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
else
  echo "▸ No $ENV_FILE — expecting signing vars from the environment (CI)."
fi

# Validate required vars.
missing=()
for v in APPLE_SIGNING_IDENTITY APPLE_API_ISSUER APPLE_API_KEY APPLE_API_KEY_PATH; do
  [[ -n "${!v:-}" ]] || missing+=("$v")
done
if (( ${#missing[@]} )); then
  echo "✗ Missing required vars in $ENV_FILE: ${missing[*]}" >&2
  exit 1
fi
if [[ ! -f "$APPLE_API_KEY_PATH" ]]; then
  echo "✗ APPLE_API_KEY_PATH does not point to a file: $APPLE_API_KEY_PATH" >&2
  exit 1
fi

# Confirm the signing identity is actually in the Keychain.
if ! security find-identity -v -p codesigning | grep -qF "$APPLE_SIGNING_IDENTITY"; then
  echo "✗ Signing identity not found in Keychain: $APPLE_SIGNING_IDENTITY" >&2
  echo "  Run: security find-identity -v -p codesigning" >&2
  exit 1
fi

echo "▸ Signing identity: $APPLE_SIGNING_IDENTITY"
echo "▸ Notarizing via App Store Connect API key: $APPLE_API_KEY"
echo "▸ Building (this signs + notarizes + staples; can take several minutes)…"
cd "$REPO_ROOT"
npx tauri build

# Locate the artifacts.
APP="$REPO_ROOT/src-tauri/target/release/bundle/macos/Launchpad.app"
DMG="$(ls -t "$REPO_ROOT"/src-tauri/target/release/bundle/dmg/Launchpad_*_aarch64.dmg 2>/dev/null | head -1)"

# Tauri notarizes + staples the .app but NOT the .dmg. Notarize + staple the
# disk image too so the downloaded .dmg passes Gatekeeper on its own. The dmg
# is already signed by the tauri build above; we only submit + staple here.
echo
echo "▸ Notarizing the .dmg (Tauri only does the .app)…"
xcrun notarytool submit "$DMG" \
  --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER" \
  --wait
xcrun stapler staple "$DMG"

echo
echo "▸ Verifying signature + notarization…"
codesign --verify --deep --strict --verbose=2 "$APP"
xcrun stapler validate "$APP"
# .dmg is assessed with --type install (it's a disk image, not an app to open).
xcrun stapler validate "$DMG"
spctl --assess --type install --verbose=2 "$DMG"

echo
echo "✓ Signed + notarized build ready:"
echo "  $DMG"
