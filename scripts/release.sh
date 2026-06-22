#!/usr/bin/env bash
#
# Cut a release whose version matches the auto-version scheme in build.rs:
# 0.1.<commit-count>. major.minor (0.1) is the deliberate knob in Cargo.toml;
# the patch is the commit count, so releases never need a hand-picked number.
#
# What it does:
#   1. Refuse to release a dirty tree.
#   2. version = commit-count AFTER the release commit, so the tagged commit's
#      build.rs count == the tag == Cargo.toml (git builds and no-git Homebrew/
#      crates.io builds all agree).
#   3. Bake the version into Cargo.toml (so a no-.git install reads it via the
#      CARGO_PKG_VERSION fallback), commit, tag, push, and open a GitHub release.
#
# Usage:  scripts/release.sh
# Needs:  a clean tree, push access, and `gh` authenticated.

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

CARGO_TOML="crates/reproit/Cargo.toml"

# 1. Clean tree only.
if [ -n "$(git status --porcelain)" ]; then
  echo "release: working tree is dirty; commit or stash first." >&2
  exit 1
fi

# 2. major.minor from Cargo.toml, patch = count AFTER this release's commit.
MM="$(grep -m1 '^version = ' "$CARGO_TOML" | sed -E 's/version = "([0-9]+\.[0-9]+)\..*/\1/')"
NEXT=$(( $(git rev-list --count HEAD) + 1 ))
VERSION="${MM}.${NEXT}"
TAG="v${VERSION}"

if git rev-parse "$TAG" >/dev/null 2>&1; then
  echo "release: tag $TAG already exists." >&2
  exit 1
fi

echo "release: cutting $TAG"

# 3. Bake into Cargo.toml (BSD/macOS sed in-place), commit, tag, push, GH release.
sed -i '' -E "s/^version = \"[^\"]*\"/version = \"${VERSION}\"/" "$CARGO_TOML"
git commit -aqm "release ${TAG}"
git tag -a "$TAG" -m "$TAG"
git push
git push origin "$TAG"
gh release create "$TAG" --title "$TAG" --generate-notes

echo "release: ${TAG} pushed and published."
echo "note: update the Homebrew formula to point at ${TAG} (brew bump-formula-pr or edit the tap)."
