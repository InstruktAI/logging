#!/usr/bin/env bash
# Verify an assembled release directory before anything is published.
# Usage: verify-release-assets.sh <assets-dir> <planned.txt> <version>
#   <planned.txt>: the planned file names, one per line (dist-assets.sh output).
# Checks: the directory holds exactly the planned set (13 files), every
# `.sha256` and `sha256.sum` verify, each archive holds both binaries, and the
# installer downloads from the vX release. Prints the asset table (markdown)
# on stdout.
set -euo pipefail

[ "$#" -eq 3 ] || { echo "usage: $0 <assets-dir> <planned.txt> <version>" >&2; exit 2; }
dir="$1" planned="$2" version="$3"
EXPECTED_COUNT=13
BINARIES=(instrukt-ai-logs instrukt-ai-log-setup)

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}

err() { echo "::error::$*" >&2; exit 1; }

want="$(LC_ALL=C sort -u "${planned}")"
have="$( (cd "${dir}" && find . -mindepth 1 -maxdepth 1 | sed 's|^\./||') | LC_ALL=C sort -u)"
if [ "${want}" != "${have}" ]; then
  diff -u <(printf '%s\n' "${want}") <(printf '%s\n' "${have}") >&2 || true
  err "release assets differ from the planned set (- planned, + present)"
fi
count="$(printf '%s\n' "${have}" | grep -c .)"
[ "${count}" -eq "${EXPECTED_COUNT}" ] || err "expected ${EXPECTED_COUNT} release assets, got ${count}"
find "${dir}" -mindepth 1 -maxdepth 1 ! -type f | grep -q . && err "release assets must be plain files"

(
  cd "${dir}"
  # Each `<file>.sha256` names its file; sha256.sum names every archive.
  for sum in *.sha256; do
    target="${sum%.sha256}"
    [ -s "${target}" ] || err "${sum} has no ${target}"
    grep -qE "^[0-9a-f]{64} [ *]${target}\$" "${sum}" || err "${sum} does not hold one sha256 line for ${target}"
    sha256 -c "${sum}" >/dev/null || err "${sum} does not verify"
  done
  sha256 -c sha256.sum >/dev/null || err "sha256.sum does not verify"
  for archive in instrukt-ai-logs-*.tar.xz; do
    grep -qE "^[0-9a-f]{64} [ *]${archive}\$" sha256.sum || err "sha256.sum does not list ${archive}"
    stem="${archive%.tar.xz}"
    listing="$(tar -tJf "${archive}")"
    for bin in "${BINARIES[@]}"; do
      printf '%s\n' "${listing}" | grep -qxF "${stem}/${bin}" || err "${archive} does not hold ${bin}"
    done
  done
  grep -qF "/releases/download/v${version}" instrukt-ai-logs-installer.sh \
    || err "the installer does not download from the v${version} release"
)

echo "| asset | bytes | sha256 |"
echo "| --- | ---: | --- |"
while IFS= read -r name; do
  size="$(wc -c < "${dir}/${name}" | tr -d ' ')"
  sum="$(sha256 "${dir}/${name}" | cut -d' ' -f1)"
  echo "| \`${name}\` | ${size} | \`${sum}\` |"
done <<< "${have}"
