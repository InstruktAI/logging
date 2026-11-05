#!/usr/bin/env bash
# Pass only when crates.io has both crates and PyPI has the package at
# <version> (resume mode). Resume never uploads: the verified files live in
# the run that pushed the tag, so anything missing is recovered there.
# Usage: registries-complete.sh <version>
# Env: CRATES_IO_API / PYPI_API override the registry base URLs (tests only).
#
# 200 counts as present (a yanked version too: it cannot be published again),
# 404 as missing (crates.io GET /crates/{name}/{version}; PyPI GET
# /pypi/{project}/{version}/json, https://docs.pypi.org/api/json/). Any other
# answer fails closed.
set -euo pipefail

[ "$#" -eq 1 ] || { echo "usage: $0 <version>" >&2; exit 2; }
VERSION="$1"
CRATES_IO_API="${CRATES_IO_API:-https://crates.io/api/v1}"
PYPI_API="${PYPI_API:-https://pypi.org/pypi}"
# crates.io's API requires an identifying User-Agent.
UA="InstruktAI-logging-release (https://github.com/InstruktAI/logging)"

missing=()
check() { # check <label> <url>
  local code
  if ! code="$(curl -sS -o /dev/null -w '%{http_code}' -A "${UA}" "$2")"; then
    echo "::error::could not reach the registry for $1" >&2
    exit 1
  fi
  case "${code}" in
    200) echo "present: $1" ;;
    404) echo "missing: $1"; missing+=("$1") ;;
    *) echo "::error::HTTP ${code} for $1" >&2; exit 1 ;;
  esac
}

check "crates.io instrukt-ai-logging ${VERSION}" "${CRATES_IO_API}/crates/instrukt-ai-logging/${VERSION}"
check "crates.io instrukt-ai-logs ${VERSION}" "${CRATES_IO_API}/crates/instrukt-ai-logs/${VERSION}"
check "PyPI instruktai-python-logger ${VERSION}" "${PYPI_API}/instruktai-python-logger/${VERSION}/json"

if [ "${#missing[@]}" -ne 0 ]; then
  exit 1
fi
echo "every registry has ${VERSION}"
