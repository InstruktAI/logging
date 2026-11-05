#!/usr/bin/env bash
# Bump every version location by <minor|patch> and stage the result; print
# `version=X.Y.Z` for $GITHUB_OUTPUT. Nothing is committed or pushed here.
# Usage: bump.sh <minor|patch>
# Needs uv, cargo and dist on PATH.
set -euo pipefail

[ "$#" -eq 1 ] || { echo "usage: $0 <minor|patch>" >&2; exit 2; }
case "$1" in minor | patch) ;; *) echo "::error::bump must be minor or patch, not '$1'" >&2; exit 2 ;; esac
BUMP="$1"

if [ -n "$(git status --porcelain)" ]; then
  echo "::error::the checkout is not clean before the bump" >&2
  exit 1
fi
python3 .github/scripts/version.py check >&2
# Fetch while Cargo.lock still matches Cargo.toml, so the workspace re-lock
# below can run offline and only touches our own crates.
cargo fetch --locked >&2
# pyproject.toml + uv.lock (re-locked, not --frozen, so uv.lock carries the
# new version too).
uv version --bump "${BUMP}" --no-sync >&2
VERSION="$(uv version --short)"
# Root Cargo.toml: [workspace.package] version + the instrukt-ai-logging pin in
# [workspace.dependencies].
python3 .github/scripts/version.py set-cargo "${VERSION}" >&2
cargo update --workspace --offline >&2
# Everything agrees, both lockfiles are consistent, and dist accepts the tag.
python3 .github/scripts/version.py check >&2
cargo metadata --locked --offline --format-version 1 > /dev/null
uv lock --check >&2
dist plan --tag="v${VERSION}" > /dev/null
CHANGED="$(git diff --name-only | LC_ALL=C sort | tr '\n' ' ')"
if [ "${CHANGED}" != "Cargo.lock Cargo.toml pyproject.toml uv.lock " ]; then
  echo "::error::unexpected files changed by the bump: ${CHANGED}" >&2
  exit 1
fi
git add Cargo.lock Cargo.toml pyproject.toml uv.lock
echo "version=${VERSION}"
