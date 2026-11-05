#!/usr/bin/env bash
# Install the release with its own shell installer from a local HTTP mirror of
# the assembled assets, into a fresh HOME and fresh XDG dirs, then run the
# installed tool.
# Usage: installer-smoke.sh <assets-dir> <version>
#
# Installer env vars, as read by the instrukt-ai-logs-installer.sh that dist
# 0.33.1 generates for this config:
#   INSTRUKT_AI_LOGS_DOWNLOAD_URL  base URL the archives are fetched from
#   INSTRUKT_AI_LOGS_NO_MODIFY_PATH=1  leave shell rc files alone
#   XDG_BIN_HOME   first entry of install-path in dist-workspace.toml
#   XDG_CONFIG_HOME  where the install receipt goes
# INSTRUKT_AI_LOGS_INSTALL_DIR is not set, so the configured install-path is
# what gets exercised.
set -euo pipefail

[ "$#" -eq 2 ] || { echo "usage: $0 <assets-dir> <version>" >&2; exit 2; }
assets="$(cd "$1" && pwd)" version="$2"
APP="smoke"

root="$(mktemp -d "${TMPDIR:-/tmp}/installer-smoke.XXXXXX")"
server=""
cleanup() {
  if [ -n "${server}" ]; then kill "${server}" 2>/dev/null || true; fi
  rm -rf "${root}"
}
trap cleanup EXIT
home="${root}/home"
mkdir -p "${home}" "${root}/config" "${root}/state" "${root}/data" "${root}/cache" "${root}/bin"

# A free port, then a mirror of the assets directory on it.
port="$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
python3 -m http.server "${port}" --bind 127.0.0.1 --directory "${assets}" >"${root}/http.log" 2>&1 &
server=$!
for _ in $(seq 1 100); do
  curl -fsS -o /dev/null "http://127.0.0.1:${port}/sha256.sum" 2>/dev/null && break
  sleep 0.1
done

clean_env=(
  env -i
  PATH="/usr/bin:/bin:/usr/sbin:/sbin"
  HOME="${home}"
  TMPDIR="${root}"
  XDG_CONFIG_HOME="${root}/config"
  XDG_STATE_HOME="${root}/state"
  XDG_DATA_HOME="${root}/data"
  XDG_CACHE_HOME="${root}/cache"
  XDG_BIN_HOME="${root}/bin"
)

"${clean_env[@]}" \
  INSTRUKT_AI_LOGS_DOWNLOAD_URL="http://127.0.0.1:${port}" \
  INSTRUKT_AI_LOGS_NO_MODIFY_PATH=1 \
  sh "${assets}/instrukt-ai-logs-installer.sh"

for bin in instrukt-ai-logs instrukt-ai-log-setup; do
  [ -x "${root}/bin/${bin}" ] || { echo "::error::the installer did not put ${bin} in \$XDG_BIN_HOME"; exit 1; }
done

got="$("${clean_env[@]}" "${root}/bin/instrukt-ai-logs" --version)"
echo "instrukt-ai-logs --version: ${got}"
if [ "${got}" != "instrukt-ai-logs ${version}" ]; then
  echo "::error::expected 'instrukt-ai-logs ${version}', got '${got}'"
  exit 1
fi
# --version only: setup without arguments would write rotation config and
# register a service.
setup_version="$("${clean_env[@]}" "${root}/bin/instrukt-ai-log-setup" --version)"
echo "instrukt-ai-log-setup --version: ${setup_version}"

# One read against fixture logs under the temp XDG_STATE_HOME: two lines from
# about a minute ago are inside --since 5m, one from 15 minutes ago is not.
ts() { python3 -c 'import datetime as d, sys; t = d.datetime.now(d.timezone.utc) - d.timedelta(seconds=int(sys.argv[1])); print(t.strftime("%Y-%m-%dT%H:%M:%S.") + f"{t.microsecond // 1000:03d}Z")' "$1"; }
logdir="${root}/state/instrukt-ai/${APP}"
mkdir -p "${logdir}"
old="$(ts 900) level=INFO logger=${APP} msg=\"outside the window\""
first="$(ts 61) level=INFO logger=${APP} msg=\"first\""
second="$(ts 60) level=WARNING logger=${APP}.db msg=\"second\" attempt=2"
printf '%s\n%s\n%s\n' "${old}" "${first}" "${second}" > "${logdir}/${APP}.log"
printf '%s\n%s\n' "${first}" "${second}" > "${root}/expected.out"

set +e
"${clean_env[@]}" "${root}/bin/instrukt-ai-logs" "${APP}" --since 5m >"${root}/read.out" 2>"${root}/read.err"
code=$?
set -e
echo "instrukt-ai-logs ${APP} --since 5m: exit ${code}"
sed 's/^/  | /' "${root}/read.out"
if [ "${code}" -ne 0 ] || ! cmp -s "${root}/expected.out" "${root}/read.out"; then
  echo "::error::fixture read: expected exit 0 and exactly the two in-window lines"
  echo "expected:"; sed 's/^/  | /' "${root}/expected.out"
  echo "stderr:"; sed 's/^/  | /' "${root}/read.err"
  exit 1
fi
echo "fixture read: exact stdout (byte-identical), exit 0"
