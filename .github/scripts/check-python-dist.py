"""Check the built Python distributions before they can be published.

Usage:
    python -I .github/scripts/check-python-dist.py <dist dir> <version>

The directory must hold exactly one wheel and one sdist of
instruktai-python-logger, both at <version> (file name and metadata), and the
wheel must carry no `entry_points.txt`: the package is the writer SDK only and
ships no console scripts.
"""

from __future__ import annotations

import email.parser
import sys
import tarfile
import zipfile
from pathlib import Path

DIST_NAME = "instruktai_python_logger"


def _metadata_version(raw: bytes) -> str:
    return str(email.parser.BytesParser().parsebytes(raw, headersonly=True)["Version"])


def check(directory: Path, version: str) -> list[str]:
    errors: list[str] = []
    files = sorted(path.name for path in directory.iterdir())
    wheel = f"{DIST_NAME}-{version}-py3-none-any.whl"
    sdist = f"{DIST_NAME}-{version}.tar.gz"
    if files != sorted([wheel, sdist]):
        return [f"expected exactly {wheel} and {sdist}, found {files}"]

    with zipfile.ZipFile(directory / wheel) as archive:
        names = archive.namelist()
        dist_info = f"{DIST_NAME}-{version}.dist-info"
        if f"{dist_info}/METADATA" not in names:
            errors.append(f"{wheel} has no {dist_info}/METADATA")
        elif (found := _metadata_version(archive.read(f"{dist_info}/METADATA"))) != version:
            errors.append(f"{wheel} METADATA carries version {found}")
        errors.extend(f"{wheel} carries {name}" for name in names if name.endswith(".dist-info/entry_points.txt"))

    with tarfile.open(directory / sdist) as archive:
        member = archive.extractfile(f"{DIST_NAME}-{version}/PKG-INFO")
        if member is None:
            errors.append(f"{sdist} has no PKG-INFO")
        elif (found := _metadata_version(member.read())) != version:
            errors.append(f"{sdist} PKG-INFO carries version {found}")
    return errors


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    errors = check(Path(argv[0]), argv[1])
    for error in errors:
        print(f"::error::{error}")
    if not errors:
        print(f"ok: wheel and sdist at {argv[1]}, no entry_points.txt")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
