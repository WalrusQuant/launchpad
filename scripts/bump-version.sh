#!/usr/bin/env bash
#
# Bump the app version in the three places it lives, sync Cargo.lock, commit,
# and create the release tag. Pushing that tag is what triggers the signed
# release build (see .github/workflows/release.yml) — this script does NOT push,
# so you stay in control of when it ships.
#
# Usage:
#   ./scripts/bump-version.sh 0.3.2
#   git push origin main --follow-tags   # pushes the commit AND the v0.3.2 tag
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

NEW="${1:-}"
if [[ -z "$NEW" ]]; then
  echo "Usage: $0 <version>   e.g. $0 0.3.2" >&2
  exit 1
fi
if [[ ! "$NEW" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "✗ Version must be semver X.Y.Z (got: $NEW)" >&2
  exit 1
fi

TAG="v$NEW"
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  echo "✗ Tag $TAG already exists." >&2
  exit 1
fi
if [[ -n "$(git status --porcelain)" ]]; then
  echo "✗ Working tree not clean — commit or stash first." >&2
  exit 1
fi

CUR="$(grep -m1 '"version"' package.json | sed -E 's/.*"version": *"([^"]+)".*/\1/')"
echo "▸ $CUR → $NEW"

# package.json + package-lock.json — npm owns both and is macOS/Linux portable.
npm version "$NEW" --no-git-tag-version --allow-same-version >/dev/null

# tauri.conf.json — replace the FIRST "version": "..." only (perl, not GNU-only
# sed `0,/re/`, so this works with macOS BSD sed too).
perl -pi -e "if (!\$done && s/(\"version\"\\s*:\\s*)\"[^\"]+\"/\${1}\"$NEW\"/) { \$done = 1 }" src-tauri/tauri.conf.json
# Cargo.toml — the [package] version line (anchored at column 0).
perl -pi -e "if (!\$done && s/^version = \"[^\"]+\"/version = \"$NEW\"/) { \$done = 1 }" src-tauri/Cargo.toml

# Keep Cargo.lock's launchpad entry in sync.
( cd src-tauri && cargo update -p launchpad >/dev/null 2>&1 || true )

# Sanity: every file now reads the new version.
grep -q "\"version\": \"$NEW\"" package.json || { echo "✗ package.json did not update" >&2; exit 1; }
grep -q "\"version\": \"$NEW\"" src-tauri/tauri.conf.json || { echo "✗ tauri.conf.json did not update" >&2; exit 1; }
grep -q "^version = \"$NEW\"" src-tauri/Cargo.toml || { echo "✗ Cargo.toml did not update" >&2; exit 1; }

git add package.json package-lock.json src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "release: $TAG"
git tag -a "$TAG" -m "$TAG"

echo
echo "✓ Committed and tagged $TAG."
echo "  Push to ship:  git push origin main --follow-tags"
