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

# package.json — first top-level "version" key.
sed -i '' -E "0,/\"version\": *\"[^\"]+\"/s//\"version\": \"$NEW\"/" package.json
# tauri.conf.json — its top-level "version" key.
sed -i '' -E "0,/\"version\": *\"[^\"]+\"/s//\"version\": \"$NEW\"/" src-tauri/tauri.conf.json
# Cargo.toml — the [package] version line.
sed -i '' -E "0,/^version = \"[^\"]+\"/s//version = \"$NEW\"/" src-tauri/Cargo.toml

# Keep Cargo.lock's launchpad entry in sync (no network).
( cd src-tauri && cargo update --offline -p launchpad --precise "$NEW" >/dev/null 2>&1 || cargo update -p launchpad >/dev/null )

# Sanity: every file now reads the new version.
for f in 'package.json' 'src-tauri/tauri.conf.json'; do
  grep -q "\"version\": \"$NEW\"" "$f" || { echo "✗ $f did not update" >&2; exit 1; }
done
grep -q "^version = \"$NEW\"" src-tauri/Cargo.toml || { echo "✗ Cargo.toml did not update" >&2; exit 1; }

git add package.json src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "release: $TAG"
git tag -a "$TAG" -m "$TAG"

echo
echo "✓ Committed and tagged $TAG."
echo "  Push to ship:  git push origin main --follow-tags"
