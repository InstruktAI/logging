#!/usr/bin/env bash
# Publish instrukt-ai-logging, then instrukt-ai-logs, at <version> from the
# checked-out tag, skipping a crate whose version crates.io already has.
# Usage: publish-crates.sh <version>
# Env: CARGO_REGISTRY_TOKEN. CRATES_IO_API overrides the API base (tests only).
set -euo pipefail

[ "$#" -eq 1 ] || { echo "usage: $0 <version>" >&2; exit 2; }
VERSION="$1"
CRATES_IO_API="${CRATES_IO_API:-https://crates.io/api/v1}"
if [ -z "${CARGO_REGISTRY_TOKEN:-}" ]; then
  echo "::error::missing GitHub Actions secret CARGO_REGISTRY_TOKEN"
  exit 1
fi
# crates.io's API requires an identifying User-Agent.
UA="InstruktAI-logging-release (https://github.com/InstruktAI/logging)"
# Dependency order: instrukt-ai-logs depends on instrukt-ai-logging.
for crate in instrukt-ai-logging instrukt-ai-logs; do
  # 200 also for a yanked version, which cannot be published again either.
  code="$(curl -sS -o /dev/null -w '%{http_code}' -A "${UA}" "${CRATES_IO_API}/crates/${crate}/${VERSION}")"
  case "${code}" in
    200) echo "${crate} ${VERSION} is already on crates.io; skipping."; continue ;;
    404) ;;
    *) echo "::error::crates.io answered HTTP ${code} for ${crate} ${VERSION}"; exit 1 ;;
  esac
  # cargo waits until the published version is in the index, so the next
  # crate resolves it.
  cargo publish --locked -p "${crate}"
done
