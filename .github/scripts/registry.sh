#!/usr/bin/env bash
# Registry lookups and the crates.io actions of the release chain.
#
#   registry.sh crate-state <crate> <version>   prints absent | published | yanked
#   registry.sh pypi-state <project> <version>  prints absent | published | yanked
#   registry.sh crates-token-check              CARGO_REGISTRY_TOKEN is a live token
#   registry.sh crate-yank <crate> <version>    yanks with CARGO_REGISTRY_TOKEN
#
# Any answer other than the documented ones is an error (exit 1): the caller
# fails closed. CRATES_IO_API and PYPI_JSON_API override the endpoints, so the
# script can run against a local stub.
set -euo pipefail

crates_api="${CRATES_IO_API:-https://crates.io/api/v1}"
pypi_api="${PYPI_JSON_API:-https://pypi.org/pypi}"
# crates.io's data-access policy requires an identifying User-Agent:
# https://crates.io/data-access
ua="InstruktAI-logging-release-chain (https://github.com/InstruktAI/logging)"

body="$(mktemp)"
trap 'rm -f "${body}"' EXIT

# get <url> [curl args...]: HTTP status on stdout, body in ${body}.
get() {
  local url="$1"
  shift
  curl --proto '=https,http' -sS -o "${body}" -w '%{http_code}' -A "${ua}" "$@" "${url}"
}

fail() { echo "::error::$*" >&2; exit 1; }

crate_state() {
  local crate="$1" version="$2" code
  # GET /api/v1/crates/{name}/{version}: 404 when the version does not
  # exist, 200 with `version.yanked` otherwise (a yanked version stays
  # published). Source: rust-lang/crates.io src/controllers/version/metadata.rs.
  code="$(get "${crates_api}/crates/${crate}/${version}")"
  case "${code}" in
    404) echo absent ;;
    200)
      case "$(jq -r --arg v "${version}" 'if .version.num == $v then (.version.yanked | tostring) else "mismatch" end' "${body}")" in
        false) echo published ;;
        true) echo yanked ;;
        *) fail "crates.io answered 200 for ${crate} ${version} without a matching version.num/yanked" ;;
      esac
      ;;
    *) fail "crates.io answered HTTP ${code} for ${crate} ${version}: $(head -c 300 "${body}")" ;;
  esac
}

pypi_state() {
  local project="$1" version="$2" code
  # GET /pypi/<project>/<version>/json: 404 when the release does not exist,
  # 200 with `info.yanked` otherwise (a yanked release still exists and its
  # version can never be uploaded again).
  # Source: https://docs.pypi.org/api/json/
  code="$(get "${pypi_api}/${project}/${version}/json")"
  case "${code}" in
    404) echo absent ;;
    200)
      case "$(jq -r --arg v "${version}" 'if .info.version == $v then (.info.yanked | tostring) else "mismatch" end' "${body}")" in
        false) echo published ;;
        true) echo yanked ;;
        *) fail "PyPI answered 200 for ${project} ${version} without a matching info.version/yanked" ;;
      esac
      ;;
    *) fail "PyPI answered HTTP ${code} for ${project} ${version}: $(head -c 300 "${body}")" ;;
  esac
}

crates_token_check() {
  : "${CARGO_REGISTRY_TOKEN:?missing GitHub Actions secret CARGO_REGISTRY_TOKEN}"
  local code detail
  # crates.io has no read-only endpoint that tells an API token its account
  # or email state: GET /api/v1/me is cookie-only (AuthCheck::only_cookie in
  # src/controllers/user/me.rs). GET /api/v1/me/tokens/{id} takes token auth
  # (AuthCheck::default in src/controllers/token.rs) and authenticates before
  # anything else (src/auth.rs), so it tells a live token from a dead one:
  #   401/403 "authentication failed" or a format error: unknown, revoked or
  #       expired token;
  #   403 "...does not have the required permissions...": a live scoped
  #       token (this endpoint accepts only legacy tokens past auth);
  #   404: a live legacy token (id 1 is not one of its account's tokens);
  #   200: a live legacy token that owns token id 1.
  code="$(get "${crates_api}/me/tokens/1" -H "Authorization: ${CARGO_REGISTRY_TOKEN}")"
  detail="$(jq -r '[.errors[]?.detail] | join("; ")' "${body}" 2>/dev/null || true)"
  case "${code}" in
    200 | 404) echo "crates.io: CARGO_REGISTRY_TOKEN authenticates (legacy token)" ;;
    403)
      case "${detail}" in
        *"does not have the required permissions"*) echo "crates.io: CARGO_REGISTRY_TOKEN authenticates (scoped token)" ;;
        *) fail "crates.io rejected CARGO_REGISTRY_TOKEN (HTTP 403: ${detail})" ;;
      esac
      ;;
    *) fail "crates.io rejected CARGO_REGISTRY_TOKEN (HTTP ${code}: ${detail})" ;;
  esac
}

crate_yank() {
  : "${CARGO_REGISTRY_TOKEN:?missing GitHub Actions secret CARGO_REGISTRY_TOKEN}"
  local crate="$1" version="$2" code
  # DELETE /api/v1/crates/{name}/{version}/yank answers {"ok":true}.
  # Source: rust-lang/crates.io src/controllers/version/yank.rs.
  code="$(get "${crates_api}/crates/${crate}/${version}/yank" -X DELETE -H "Authorization: ${CARGO_REGISTRY_TOKEN}")"
  if [ "${code}" != 200 ] || [ "$(jq -r '.ok' "${body}")" != true ]; then
    fail "crates.io did not yank ${crate} ${version} (HTTP ${code}: $(head -c 300 "${body}"))"
  fi
  echo "yanked ${crate} ${version} on crates.io"
}

case "${1:-}" in
  crate-state) [ $# -eq 3 ] || fail "usage: $0 crate-state <crate> <version>"; crate_state "$2" "$3" ;;
  pypi-state) [ $# -eq 3 ] || fail "usage: $0 pypi-state <project> <version>"; pypi_state "$2" "$3" ;;
  crates-token-check) [ $# -eq 1 ] || fail "usage: $0 crates-token-check"; crates_token_check ;;
  crate-yank) [ $# -eq 3 ] || fail "usage: $0 crate-yank <crate> <version>"; crate_yank "$2" "$3" ;;
  *) fail "usage: $0 crate-state|pypi-state|crates-token-check|crate-yank ..." ;;
esac
