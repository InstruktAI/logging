#!/usr/bin/env bash
# Land a verified release: tag the release commit and push main and the tag
# atomically, fast-forward only, never forced.
# Usage: land.sh <version> <base-sha> <release-sha>
# Run in a checkout whose HEAD is the release commit (release-commit.sh apply)
# with push credentials for origin.
#
# If main moved since the run started, the push is not a fast-forward and is
# refused; nothing becomes visible, and the next push to main releases.
# `git push --atomic` moves both refs or neither (git-push(1)).
set -euo pipefail

[ "$#" -eq 3 ] || { echo "usage: $0 <version> <base-sha> <release-sha>" >&2; exit 2; }
VERSION="$1" BASE="$2" RELEASE="$3"
TAG="v${VERSION}"

[ "$(git rev-parse HEAD)" = "${RELEASE}" ] || { echo "::error::HEAD is not the release commit ${RELEASE}" >&2; exit 1; }
[ "$(git rev-parse HEAD~1)" = "${BASE}" ] || { echo "::error::the release commit is not on ${BASE}" >&2; exit 1; }

remote_main="$(git ls-remote origin refs/heads/main | cut -f1)"
if [ "${remote_main}" != "${BASE}" ]; then
  echo "::error::main moved from ${BASE} to ${remote_main:-<none>} since this run started; nothing was pushed. The next push to main releases." >&2
  exit 1
fi
if [ -n "$(git ls-remote --tags origin "refs/tags/${TAG}")" ]; then
  echo "::error::tag ${TAG} already exists on origin; nothing was pushed." >&2
  exit 1
fi

git tag "${TAG}" "${RELEASE}"
git push --atomic origin "${RELEASE}:refs/heads/main" "refs/tags/${TAG}"
echo "landed ${TAG} at ${RELEASE} on main"
