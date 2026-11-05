#!/usr/bin/env bash
# Undo what one release-chain run created. Every action is logged.
#
#   rollback.sh github <tag>     delete each GitHub Release with this tag (draft
#                                or published), then the tag itself
#   rollback.sh crates <version> yank each crate of $CRATES at this version
#                                that crates.io serves unyanked
#
# The caller guarantees that whatever it finds is this run's: the release
# chain proves the tag, release and crate versions absent before it creates
# any, and runs one chain at a time. Needs GH_TOKEN/GH_REPO (github) or
# CARGO_REGISTRY_TOKEN and CRATES (crates).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
log() { echo "rollback: $*"; [ -z "${GITHUB_STEP_SUMMARY:-}" ] || echo "- rollback: $*" >> "${GITHUB_STEP_SUMMARY}"; }

github() {
  local tag="$1" ids id
  : "${GH_REPO:?set GH_REPO to owner/name}"
  ids="$(gh api --paginate "repos/${GH_REPO}/releases" --jq ".[] | select(.tag_name == \"${tag}\") | .id")"
  if [ -z "${ids}" ]; then
    log "no GitHub Release ${tag} to delete"
  fi
  for id in ${ids}; do
    gh api -X DELETE "repos/${GH_REPO}/releases/${id}"
    log "deleted GitHub Release ${tag} (id ${id})"
  done
  # A draft creates no tag; a release that finalize published did.
  if gh api "repos/${GH_REPO}/git/matching-refs/tags/${tag}" --jq '.[].ref' | grep -qxF "refs/tags/${tag}"; then
    gh api -X DELETE "repos/${GH_REPO}/git/refs/tags/${tag}"
    log "deleted tag ${tag}"
  else
    log "no tag ${tag} to delete"
  fi
}

crates() {
  local version="$1" crate state status=0
  : "${CRATES:?set CRATES to the crate names}"
  for crate in ${CRATES}; do
    if ! state="$("${here}/registry.sh" crate-state "${crate}" "${version}")"; then
      log "FAILED to read ${crate} ${version} on crates.io: check it and yank it by hand (docs/publishing.md)"
      status=1
      continue
    fi
    case "${state}" in
      published)
        if "${here}/registry.sh" crate-yank "${crate}" "${version}"; then
          log "yanked ${crate} ${version} on crates.io (the version number is burnt)"
        else
          log "FAILED to yank ${crate} ${version}: yank it by hand (docs/publishing.md)"
          status=1
        fi
        ;;
      absent) log "crates.io has no ${crate} ${version}; nothing to yank" ;;
      yanked) log "${crate} ${version} is already yanked" ;;
    esac
  done
  return "${status}"
}

case "${1:-}" in
  github) [ $# -eq 2 ] || { echo "usage: $0 github <tag>" >&2; exit 2; }; github "$2" ;;
  crates) [ $# -eq 2 ] || { echo "usage: $0 crates <version>" >&2; exit 2; }; crates "$2" ;;
  *) echo "usage: $0 github <tag> | crates <version>" >&2; exit 2 ;;
esac
