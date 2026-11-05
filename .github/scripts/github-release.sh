#!/usr/bin/env bash
# GitHub Release for a pushed tag, through the REST API (gh api).
#
#   github-release.sh publish <tag> <assets-dir> <notes-file>
#       Create <tag> as a draft with the notes, upload every file of
#       <assets-dir>, read the draft back (asset names must equal the local
#       set, each asset's `digest` must be the local sha256), then publish it
#       as latest. Any failure before publishing deletes the draft. Idempotent:
#       a release already published with exactly these assets passes; a draft
#       left by an interrupted attempt is deleted and redone.
#   github-release.sh check-published <tag> <planned.txt>
#       Pass only when <tag> is a published release whose asset names equal
#       the planned set exactly (resume mode).
#
# Env: GH_REPO (owner/repo), GH_TOKEN. GH_UPLOAD_BASE overrides the upload host
# (tests only).
#
# REST facts this relies on (https://docs.github.com/en/rest/releases):
# GET releases/tags/{tag} returns published releases only; drafts appear only
# in GET releases, and only to a token with push access; a release asset
# carries `digest` ("sha256:<hex>", or null while not computed); assets are
# uploaded to uploads.github.com/.../releases/{id}/assets?name=.
set -euo pipefail

UPLOAD_BASE="${GH_UPLOAD_BASE:-https://uploads.github.com}"
: "${GH_REPO:?GH_REPO must be owner/repo}"
err() { echo "::error::$*" >&2; exit 1; }

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1
}

# File names directly in a directory, sorted.
list_dir() {
  (cd "$1" && find . -mindepth 1 -maxdepth 1 | sed 's|^\./||') | LC_ALL=C sort
}

# Every release of the repository whose tag is $1, drafts included.
releases_for_tag() {
  gh api --paginate "repos/${GH_REPO}/releases?per_page=100" \
    | jq -s --arg t "$1" '[add // [] | .[] | select(.tag_name == $t)]'
}

# Compare a release's assets with a local directory: same names, and each
# digest is the local sha256. $1: release JSON, $2: directory.
assets_match() {
  local release="$1" dir="$2" name want got
  local remote local_names
  remote="$(printf '%s' "${release}" | jq -r '.assets[].name' | LC_ALL=C sort)"
  local_names="$(list_dir "${dir}")"
  if [ "${remote}" != "${local_names}" ]; then
    echo "asset names differ (- local, + release):" >&2
    diff -u <(printf '%s\n' "${local_names}") <(printf '%s\n' "${remote}") >&2 || true
    return 1
  fi
  while IFS= read -r name; do
    want="sha256:$(sha256 "${dir}/${name}")"
    got="$(printf '%s' "${release}" | jq -r --arg n "${name}" '.assets[] | select(.name == $n) | "\(.state) \(.digest)"')"
    if [ "${got}" != "uploaded ${want}" ]; then
      echo "${name}: release has '${got}', local is 'uploaded ${want}'" >&2
      return 1
    fi
  done <<< "${local_names}"
}

# assets_match with a few retries: GitHub fills `digest` in after the upload.
assets_match_settled() {
  local id="$1" dir="$2" attempt release
  for attempt in 1 2 3 4 5 6; do
    release="$(gh api "repos/${GH_REPO}/releases/${id}")"
    if ! printf '%s' "${release}" | jq -e '[.assets[] | select(.digest == null)] | length > 0' >/dev/null; then
      assets_match "${release}" "${dir}"
      return
    fi
    echo "digests not computed yet (attempt ${attempt}); waiting" >&2
    sleep 5
  done
  echo "release ${id} still has assets without a digest" >&2
  return 1
}

# The draft this run created and has not published yet. Global: the EXIT trap
# runs outside cmd_publish.
DRAFT_ID=""
delete_draft() {
  if [ -n "${DRAFT_ID}" ]; then
    echo "deleting draft ${DRAFT_ID}" >&2
    gh api -X DELETE "repos/${GH_REPO}/releases/${DRAFT_ID}" >/dev/null \
      || echo "::error::could not delete draft ${DRAFT_ID}; delete it by hand" >&2
  fi
}

cmd_publish() {
  local tag="$1" dir="$2" notes="$3" existing count id published
  [ -s "${notes}" ] || err "no release notes in ${notes}"
  # The tag must already be on the remote: publishing a draft whose tag is
  # missing would create the tag from the default branch.
  gh api "repos/${GH_REPO}/git/ref/tags/${tag}" >/dev/null || err "tag ${tag} is not on ${GH_REPO}"

  existing="$(releases_for_tag "${tag}")"
  count="$(printf '%s' "${existing}" | jq 'length')"
  [ "${count}" -le 1 ] || err "${count} releases carry ${tag}; resolve by hand"
  if [ "${count}" -eq 1 ]; then
    id="$(printf '%s' "${existing}" | jq -r '.[0].id')"
    if [ "$(printf '%s' "${existing}" | jq -r '.[0].draft')" = "false" ]; then
      if assets_match "$(gh api "repos/${GH_REPO}/releases/${id}")" "${dir}"; then
        echo "${tag} is already published with exactly the verified assets."
        return 0
      fi
      err "${tag} is published but its assets are not the verified set; this needs a person (see docs/publishing.md)"
    fi
    echo "deleting draft ${id} left by an interrupted attempt"
    gh api -X DELETE "repos/${GH_REPO}/releases/${id}" >/dev/null
  fi

  id="$(gh api -X POST "repos/${GH_REPO}/releases" \
    -f tag_name="${tag}" -f name="${tag}" -F body=@"${notes}" -F draft=true --jq '.id')"
  DRAFT_ID="${id}"
  # From here until it is published, any failure removes the draft again.
  trap delete_draft EXIT
  echo "created draft ${id} for ${tag}"

  while IFS= read -r name; do
    gh api -X POST -H "Content-Type: application/octet-stream" \
      "${UPLOAD_BASE}/repos/${GH_REPO}/releases/${id}/assets?name=${name}" \
      --input "${dir}/${name}" --jq '"uploaded \(.name) (\(.size) bytes)"'
  done < <(list_dir "${dir}")

  assets_match_settled "${id}" "${dir}" || err "the draft's assets are not the verified files"
  echo "draft ${id}: asset names and digests equal the verified files"

  gh api -X PATCH "repos/${GH_REPO}/releases/${id}" -F draft=false -f make_latest=true >/dev/null
  DRAFT_ID=""
  trap - EXIT

  published="$(gh api "repos/${GH_REPO}/releases/tags/${tag}")"
  [ "$(printf '%s' "${published}" | jq -r '.draft')" = "false" ] || err "${tag} did not publish"
  assets_match "${published}" "${dir}" || err "published ${tag} does not carry the verified assets"
  echo "published $(printf '%s' "${published}" | jq -r '.html_url')"
}

cmd_check_published() {
  local tag="$1" planned="$2" release remote want
  release="$(gh api "repos/${GH_REPO}/releases/tags/${tag}")" \
    || err "${tag} has no published GitHub Release"
  remote="$(printf '%s' "${release}" | jq -r '.assets[].name' | LC_ALL=C sort)"
  want="$(LC_ALL=C sort -u "${planned}")"
  if [ "${remote}" != "${want}" ]; then
    diff -u <(printf '%s\n' "${want}") <(printf '%s\n' "${remote}") >&2 || true
    err "${tag} is published without exactly the planned assets (- planned, + release)"
  fi
  echo "${tag} is published with exactly the planned assets."
}

case "${1:-}" in
  publish) [ "$#" -eq 4 ] || err "usage: $0 publish <tag> <assets-dir> <notes-file>"; cmd_publish "$2" "$3" "$4" ;;
  check-published) [ "$#" -eq 3 ] || err "usage: $0 check-published <tag> <planned.txt>"; cmd_check_published "$2" "$3" ;;
  *) err "usage: $0 publish <tag> <assets-dir> <notes-file> | check-published <tag> <planned.txt>" ;;
esac
