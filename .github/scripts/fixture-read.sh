#!/usr/bin/env bash
# One read through an installed instrukt-ai-logs against fixture logs.
#
# Writes a log file for app `release-check` under $XDG_STATE_HOME (the
# caller's fresh temporary state dir) with lines timestamped about a minute
# ago (plus one line outside the window), then asserts the exact stdout and exit code of
# `instrukt-ai-logs release-check --since 5m`, and that `--grep` selects the
# expected subset.
# Usage: XDG_STATE_HOME=<fresh dir> fixture-read.sh <dir holding instrukt-ai-logs>
set -euo pipefail

bin="$1/instrukt-ai-logs"
: "${XDG_STATE_HOME:?set XDG_STATE_HOME to a fresh temporary directory}"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

# Log timestamp format of docs/contract.md: UTC, milliseconds, `Z`.
ts() { python3 -c 'import datetime, sys; t = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(seconds=int(sys.argv[1])); print(t.strftime("%Y-%m-%dT%H:%M:%S.") + f"{t.microsecond // 1000:03d}Z")' "$1"; }

log_dir="${XDG_STATE_HOME}/instrukt-ai/release-check"
mkdir -p "${log_dir}"
{
  echo "$(ts 62) level=INFO logger=release_check msg=\"started\""
  echo "$(ts 61) level=WARNING logger=release_check.db msg=\"slow query\" ms=812"
  echo "$(ts 60) level=INFO logger=release_check msg=\"ready\" port=8080"
} > "${work}/expected-all.txt"
# A line older than the window, which --since must leave out.
{
  echo "$(ts 600) level=INFO logger=release_check msg=\"outside the window\""
  cat "${work}/expected-all.txt"
} > "${log_dir}/release-check.log"
grep -F 'logger=release_check.db ' "${work}/expected-all.txt" > "${work}/expected-grep.txt"

check() {
  local name="$1" expected="$2"
  shift 2
  local rc=0
  "${bin}" "$@" > "${work}/${name}.out" 2> "${work}/${name}.err" || rc=$?
  echo "\$ instrukt-ai-logs $* (exit ${rc})"
  cat "${work}/${name}.out"
  if [ "${rc}" -ne 0 ]; then
    echo "::error::instrukt-ai-logs $* exited ${rc}; stderr:"; cat "${work}/${name}.err"
    exit 1
  fi
  if ! diff -u "${expected}" "${work}/${name}.out"; then
    echo "::error::instrukt-ai-logs $* printed something other than the fixture lines above"
    exit 1
  fi
}

check all "${work}/expected-all.txt" release-check --since 5m
check grep "${work}/expected-grep.txt" release-check --since 5m --grep 'logger=release_check\.db'
echo "fixture read: ok (3 lines read, --grep selected 1)"
