#!/usr/bin/env bash
# Print, one per line and sorted, every file name release.yml uploads to the
# GitHub Release for a tag: each artifact dist plans that has a file (`name`),
# plus the dist-manifest.json its host job adds.
# Usage: dist-assets.sh <plan.json>   (from `dist plan --output-format=json`)
set -euo pipefail
jq -r '. as $m
  | [$m.releases[].artifacts[] | ($m.artifacts[.].name // empty)]
  + ["dist-manifest.json"] | unique | .[]' "$1" | LC_ALL=C sort -u
