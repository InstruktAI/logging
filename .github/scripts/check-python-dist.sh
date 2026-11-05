#!/usr/bin/env bash
# Check a Python distribution directory: exactly one wheel and one sdist of
# instruktai-python-logger at <version>, and no console scripts (the package is
# the writer SDK only; the tool ships as Rust binaries).
# Usage: check-python-dist.sh <dist-dir> <version>
set -euo pipefail

[ "$#" -eq 2 ] || { echo "usage: $0 <dist-dir> <version>" >&2; exit 2; }
dir="$1" version="$2"
wheel="instruktai_python_logger-${version}-py3-none-any.whl"
sdist="instruktai_python_logger-${version}.tar.gz"

err() { echo "::error::$*" >&2; exit 1; }

have="$( (cd "${dir}" && find . -mindepth 1 -maxdepth 1 | sed 's|^\./||') | LC_ALL=C sort)"
want="$(printf '%s\n' "${sdist}" "${wheel}" | LC_ALL=C sort)"
[ "${have}" = "${want}" ] || err "python dist holds [${have//$'\n'/, }], expected [${want//$'\n'/, }]"

python3 - "${dir}/${wheel}" "${dir}/${sdist}" "${version}" <<'EOF'
import sys
import tarfile
import zipfile

wheel, sdist, version = sys.argv[1:]
with zipfile.ZipFile(wheel) as zf:
    names = zf.namelist()
    info = f"instruktai_python_logger-{version}.dist-info"
    if f"{info}/METADATA" not in names:
        sys.exit(f"::error::{wheel} has no {info}/METADATA")
    if any(n.endswith("entry_points.txt") for n in names):
        sys.exit(f"::error::{wheel} declares entry points (console scripts)")
    meta = zf.read(f"{info}/METADATA").decode()
    if f"\nVersion: {version}\n" not in meta:
        sys.exit(f"::error::{wheel} METADATA is not version {version}")
with tarfile.open(sdist) as tf:
    root = f"instruktai_python_logger-{version}"
    members = tf.getnames()
    if any(n.endswith("entry_points.txt") for n in members):
        sys.exit(f"::error::{sdist} declares entry points (console scripts)")
    pkg = tf.extractfile(f"{root}/PKG-INFO")
    if pkg is None or f"\nVersion: {version}\n" not in pkg.read().decode():
        sys.exit(f"::error::{sdist} PKG-INFO is not version {version}")
print(f"ok: {wheel} and {sdist} at {version}, no entry points")
EOF
