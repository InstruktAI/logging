#!/usr/bin/env bash
# Release preflight: prove vX can be released before anything is visible.
# Usage: preflight.sh <version>
# Env: GH_TOKEN, GH_REPO (owner/repo), CARGO_REGISTRY_TOKEN, PYPI_TOKEN.
#      CRATES_IO_API / PYPI_API override the registry base URLs (tests only).
#
# Every check fails closed: an answer other than the one that proves absence
# (or presence, for the tokens) stops the release.
set -euo pipefail

[ "$#" -eq 1 ] || { echo "usage: $0 <version>" >&2; exit 2; }
VERSION="$1"
: "${GH_REPO:?GH_REPO must be owner/repo}"
TAG="v${VERSION}"
CRATES_IO_API="${CRATES_IO_API:-https://crates.io/api/v1}"
PYPI_API="${PYPI_API:-https://pypi.org/pypi}"
# crates.io's crawler policy requires an identifying User-Agent.
UA="InstruktAI-logging-release (https://github.com/${GH_REPO})"

failures=0
fail() { echo "::error::preflight: $*"; failures=$((failures + 1)); }
ok() { echo "ok: $*"; }

# HTTP status of a GET, with the body discarded. curl failing to connect at
# all is a failure of the check, not "absent".
http_status() {
  curl -sS -o /dev/null -w '%{http_code}' -A "${UA}" "$1"
}

# 1. Tag absent on the remote.
if remote_tag="$(git ls-remote --tags origin "refs/tags/${TAG}")"; then
  if [ -n "${remote_tag}" ]; then fail "tag ${TAG} already exists on origin"; else ok "tag ${TAG} absent on origin"; fi
else
  fail "could not list tags on origin"
fi

# 2. No GitHub Release for the tag, drafts included. GET releases/tags/{tag}
# returns published releases only, and drafts are listed only to a token with
# push access (https://docs.github.com/en/rest/releases/releases), so this
# lists every release with a contents: write token.
if releases="$(gh api --paginate "repos/${GH_REPO}/releases?per_page=100")"; then
  found="$(printf '%s' "${releases}" | jq -s --arg t "${TAG}" '[add // [] | .[] | select(.tag_name == $t) | {id, draft}]')"
  if [ "${found}" = "[]" ]; then
    ok "no GitHub Release ${TAG} (drafts included)"
  else
    fail "a GitHub Release for ${TAG} already exists: ${found}"
  fi
else
  fail "could not list the GitHub Releases"
fi

# 3. crates.io has neither crate at this version. GET /crates/{name}/{version}
# answers 200 for a yanked version too (its `yanked` is true), and 404 only
# when the version was never published; a yanked number cannot be reused.
for crate in instrukt-ai-logging instrukt-ai-logs; do
  if code="$(http_status "${CRATES_IO_API}/crates/${crate}/${VERSION}")"; then
    case "${code}" in
      404) ok "crates.io has no ${crate} ${VERSION}" ;;
      200) fail "crates.io already has ${crate} ${VERSION} (a yanked version counts)" ;;
      *) fail "crates.io answered HTTP ${code} for ${crate} ${VERSION}" ;;
    esac
  else
    fail "could not reach crates.io for ${crate} ${VERSION}"
  fi
done

# 4. PyPI has no such release. GET /pypi/{project}/{version}/json answers 200
# for a yanked release too (`info.yanked` is true), 404 when it never existed
# (https://docs.pypi.org/api/json/).
if code="$(http_status "${PYPI_API}/instruktai-python-logger/${VERSION}/json")"; then
  case "${code}" in
    404) ok "PyPI has no instruktai-python-logger ${VERSION}" ;;
    200) fail "PyPI already has instruktai-python-logger ${VERSION} (a yanked release counts)" ;;
    *) fail "PyPI answered HTTP ${code} for instruktai-python-logger ${VERSION}" ;;
  esac
else
  fail "could not reach PyPI for instruktai-python-logger ${VERSION}"
fi

# 5. Publishing credentials are present. crates.io exposes the account's email
# verification only through GET /api/v1/me, which refuses API tokens (it is
# cookie-only: AuthCheck::only_cookie() in crates.io's
# src/controllers/user/me.rs), so a verified email cannot be checked here;
# docs/publishing.md lists it as an account precondition.
if [ -n "${CARGO_REGISTRY_TOKEN:-}" ]; then ok "CARGO_REGISTRY_TOKEN is set"; else fail "secret CARGO_REGISTRY_TOKEN is empty"; fi
if [ -n "${PYPI_TOKEN:-}" ]; then ok "PYPI_TOKEN is set"; else fail "secret PYPI_TOKEN is empty"; fi

if [ "${failures}" -ne 0 ]; then
  echo "::error::preflight failed ${failures} check(s); nothing was tagged, released or uploaded."
  exit 1
fi
echo "preflight passed for ${TAG}"
