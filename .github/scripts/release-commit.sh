#!/usr/bin/env bash
# Hand the unpushed `chore(release): vX` commit from the `version` job to every
# later job of the same run, byte-identical.
#
#   release-commit.sh make <version> <out-dir>
#       Commit the staged version bump on top of HEAD and write
#       <out-dir>/release.patch (git format-patch). Prints `base_sha=`,
#       `release_sha=` lines for $GITHUB_OUTPUT.
#   release-commit.sh apply <patch-dir> <base-sha> <release-sha>
#       In a checkout of <base-sha>, apply the patch with `git am` and fail
#       unless the result is exactly <release-sha>.
#
# The SHA is reproducible because everything that goes into a commit object is
# fixed: the tree and parent come from the patch and the base, the author and
# committer are the same bot identity, and both dates are the author date the
# patch carries (`git am --committer-date-is-author-date`, see git-am(1)).
set -euo pipefail

BOT_NAME="github-actions[bot]"
BOT_EMAIL="41898282+github-actions[bot]@users.noreply.github.com"

identity() {
  git config user.name "${BOT_NAME}"
  git config user.email "${BOT_EMAIL}"
  # A signature would differ per job and change the SHA.
  git config commit.gpgsign false
}

case "${1:-}" in
  make)
    [ "$#" -eq 3 ] || { echo "usage: $0 make <version> <out-dir>" >&2; exit 2; }
    version="$2" out="$3"
    identity
    base="$(git rev-parse HEAD)"
    # Whole seconds: the date a patch carries has no sub-second part.
    now="$(date -u +%s)"
    GIT_AUTHOR_DATE="@${now} +0000" GIT_COMMITTER_DATE="@${now} +0000" \
      git commit --quiet -m "chore(release): v${version}"
    mkdir -p "${out}"
    git format-patch --quiet -1 --stdout HEAD > "${out}/release.patch"
    echo "base_sha=${base}"
    echo "release_sha=$(git rev-parse HEAD)"
    ;;
  apply)
    [ "$#" -eq 4 ] || { echo "usage: $0 apply <patch-dir> <base-sha> <release-sha>" >&2; exit 2; }
    dir="$2" base="$3" want="$4"
    head="$(git rev-parse HEAD)"
    if [ "${head}" != "${base}" ]; then
      echo "::error::checkout is ${head}, expected the release base ${base}" >&2
      exit 1
    fi
    identity
    git am --quiet --committer-date-is-author-date "${dir}/release.patch"
    got="$(git rev-parse HEAD)"
    if [ "${got}" != "${want}" ]; then
      echo "::error::applying the release patch gave ${got}, expected ${want}" >&2
      exit 1
    fi
    echo "release commit ${got} on ${base}: $(git log -1 --format=%s)"
    ;;
  *)
    echo "usage: $0 make <version> <out-dir> | apply <patch-dir> <base-sha> <release-sha>" >&2
    exit 2
    ;;
esac
