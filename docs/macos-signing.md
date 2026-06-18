# macOS code-signing & notarization

How Launchpad's `.dmg` is signed with a **Developer ID Application** certificate and
notarized by Apple, so users don't get a Gatekeeper "unidentified developer" warning.

Tauri does the heavy lifting: when the env vars below are present during
`tauri build`, it signs the app with the hardened runtime, submits it to Apple's
notary service, and staples the ticket. The wrapper `scripts/build-signed.sh`
loads the secrets, runs the build, and verifies the result.

## One-time setup

### 1. Developer ID Application certificate
This is the cert type required to distribute outside the Mac App Store.

1. Keychain Access → **Certificate Assistant → Request a Certificate from a
   Certificate Authority** → your email, leave "CA Email" blank, **Saved to disk**.
2. [developer.apple.com/account](https://developer.apple.com/account) → Certificates →
   ➕ → **Developer ID Application** → upload the CSR → download the `.cer`.
3. Double-click the `.cer` to install it into your login Keychain.
4. Verify and note your identity string + Team ID:
   ```
   security find-identity -v -p codesigning
   # → "Developer ID Application: Your Name (TEAMID)"
   ```

### 2. App Store Connect API key (for notarization)
1. [appstoreconnect.apple.com](https://appstoreconnect.apple.com) → Users and Access →
   **Integrations → App Store Connect API** → ➕ → role **Developer**.
2. Download the `.p8` (**one-time download**). Store it outside the repo, e.g.
   `~/.appstoreconnect/private_keys/AuthKey_XXXXXXXXXX.p8`.
3. Note the **Key ID** and the **Issuer ID**.

### 3. Secrets file
Create `src-tauri/.env.signing` (gitignored) from this template:

```sh
# The exact string from `security find-identity -v -p codesigning`
APPLE_SIGNING_IDENTITY="Developer ID Application: Your Name (TEAMID)"

# App Store Connect API key (notarization)
APPLE_API_ISSUER="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"   # Issuer ID
APPLE_API_KEY="XXXXXXXXXX"                                 # Key ID
APPLE_API_KEY_PATH="$HOME/.appstoreconnect/private_keys/AuthKey_XXXXXXXXXX.p8"
```

> The `.p8`, the `.env.signing`, and any `*.p8`/`*.p12` are gitignored — never commit them.

## Building a signed release

```sh
./scripts/build-signed.sh
```

It validates the secrets, builds, and verifies with `codesign --verify`,
`spctl --assess`, and `stapler validate`. The signed `.dmg` lands at
`src-tauri/target/release/bundle/dmg/Launchpad_<version>_aarch64.dmg` — attach
that to the GitHub release.

## Troubleshooting

- **`spctl` rejects the app** → the cert is the wrong type (must be *Developer ID
  Application*, not *Apple Development*), or notarization didn't run.
- **Notarization fails on entitlements / hardened runtime** → the app may need a
  custom entitlements plist referenced from `tauri.conf.json`
  (`bundle.macOS.entitlements`). Start without one; only add if Apple's notary log
  flags a specific requirement (fetch it with `xcrun notarytool log <submission-id>`).
- **`The binary is not signed with a valid Developer ID certificate`** → confirm
  `APPLE_SIGNING_IDENTITY` matches `security find-identity` exactly.

## Later: CI

A `.github/workflows/release.yml` can build/sign/notarize on `v*` tag push using
`tauri-apps/tauri-action`, with repo secrets: `APPLE_CERTIFICATE` (base64 of an
exported `.p12`), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_API_ISSUER`,
`APPLE_API_KEY` (Key ID), `APPLE_API_KEY` contents, and `KEYCHAIN_PASSWORD`.
Not set up yet — see the "Local + CI" path when ready.
