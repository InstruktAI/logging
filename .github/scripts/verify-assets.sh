#!/usr/bin/env bash
# Verify an assembled release asset directory against dist's plan.
#
#   - the directory holds exactly the files .github/scripts/dist-assets.sh
#     lists for the plan: no more, no less;
#   - every `.sha256` file and `sha256.sum` check, and sha256.sum covers every
#     archive;
#   - every archive holds both binaries.
#
# Writes `<name> <size> <sha256>` lines to <dir>/../asset-table.txt and, on a
# runner, a Markdown table to the job summary.
# Usage: verify-assets.sh <asset dir> <plan.json>
set -euo pipefail

dir="$1"
plan="$2"
here="$(cd "$(dirname "$0")" && pwd)"
expected_count=13
binaries=(instrukt-ai-logs instrukt-ai-log-setup)

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
sha256_check() { if command -v sha256sum >/dev/null; then sha256sum -c "$@"; else shasum -a 256 -c "$@"; fi; }

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

"${here}/dist-assets.sh" "${plan}" > "${work}/expected.txt"
find "${dir}" -mindepth 1 -maxdepth 1 -print | sed 's|.*/||' | LC_ALL=C sort > "${work}/present.txt"
if ! diff -u "${work}/expected.txt" "${work}/present.txt"; then
  echo "::error::the asset directory differs from dist's planned set (- expected, + present)"
  exit 1
fi
count="$(wc -l < "${work}/expected.txt" | tr -d ' ')"
if [ "${count}" -ne "${expected_count}" ]; then
  echo "::error::dist plans ${count} assets, not ${expected_count}; the release contract in docs/publishing.md lists them"
  exit 1
fi
while read -r name; do
  if [ ! -f "${dir}/${name}" ] || [ ! -s "${dir}/${name}" ]; then
    echo "::error::${name} is not a non-empty regular file"
    exit 1
  fi
done < "${work}/expected.txt"

(
  cd "${dir}"
  for sum in *.sha256; do sha256_check "${sum}"; done
  sha256_check sha256.sum
  for archive in *.tar.xz; do
    if ! grep -qE "^[0-9a-f]{64} [ *]${archive}\$" sha256.sum; then
      echo "::error::sha256.sum does not cover ${archive}"
      exit 1
    fi
    tar -tJf "${archive}" | sed 's|^[^/]*/||' > "${work}/members.txt"
    for bin in "${binaries[@]}"; do
      if ! grep -qxF "${bin}" "${work}/members.txt"; then
        echo "::error::${archive} does not contain ${bin}"
        exit 1
      fi
    done
    echo "${archive}: contains ${binaries[*]}"
  done
)

table="$(dirname "${dir}")/asset-table.txt"
: > "${table}"
while read -r name; do
  size="$(wc -c < "${dir}/${name}" | tr -d ' ')"
  hash="$(sha256 "${dir}/${name}" | cut -d' ' -f1)"
  echo "${name} ${size} ${hash}" >> "${table}"
done < "${work}/expected.txt"
echo "verified ${count} assets:"
cat "${table}"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### Verified release assets (${count})"
    echo
    echo "| name | size (bytes) | sha256 |"
    echo "| --- | ---: | --- |"
    while read -r name size hash; do echo "| \`${name}\` | ${size} | \`${hash}\` |"; done < "${table}"
  } >> "${GITHUB_STEP_SUMMARY}"
fi
