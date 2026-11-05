"""One repository version, kept in agreement across Python and Cargo metadata.

Usage:
    python .github/scripts/version.py check
    python .github/scripts/version.py set-cargo <version>

`check` exits non-zero unless every place that carries the release version
agrees: pyproject.toml, uv.lock (the project's own entry), the root Cargo.toml
(`[workspace.package] version` and the `instrukt-ai-logging` pin in
`[workspace.dependencies]`), every workspace crate inheriting that version, and
Cargo.lock's entries for the workspace crates.

`set-cargo` rewrites the two version strings in the root Cargo.toml. The
pyproject.toml/uv.lock side is bumped by `uv version`; Cargo.lock is refreshed
by `cargo update --workspace` afterwards.
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
PYTHON_PACKAGE = "instruktai-python-logger"
WRITER_CRATE = "instrukt-ai-logging"
SEMVER = re.compile(r"^\d+\.\d+\.\d+$")


def _load(name: str) -> dict[str, Any]:
    with (ROOT / name).open("rb") as handle:
        return tomllib.load(handle)


def _lock_versions(lock: dict[str, Any], names: set[str]) -> dict[str, list[str]]:
    found: dict[str, list[str]] = {name: [] for name in names}
    for package in lock.get("package", []):
        if package.get("name") in names:
            found[package["name"]].append(str(package.get("version")))
    return found


def collect() -> dict[str, str]:
    """Every version-carrying location, mapped to the version it holds."""
    versions: dict[str, str] = {}
    versions["pyproject.toml [project].version"] = str(_load("pyproject.toml")["project"]["version"])

    for name, found in _lock_versions(_load("uv.lock"), {PYTHON_PACKAGE}).items():
        versions[f"uv.lock {name}"] = ",".join(found) or "<missing>"

    cargo = _load("Cargo.toml")
    workspace = cargo["workspace"]
    versions["Cargo.toml [workspace.package].version"] = str(workspace["package"]["version"])
    versions[f"Cargo.toml [workspace.dependencies].{WRITER_CRATE}.version"] = str(
        workspace["dependencies"][WRITER_CRATE].get("version", "<missing>")
    )

    members: set[str] = set()
    for member in workspace["members"]:
        crate = _load(f"{member}/Cargo.toml")["package"]
        members.add(crate["name"])
        if crate.get("version") != {"workspace": True}:
            versions[f"{member}/Cargo.toml version (must be `version.workspace = true`)"] = str(crate.get("version"))

    for name, found in _lock_versions(_load("Cargo.lock"), members).items():
        versions[f"Cargo.lock {name}"] = ",".join(found) or "<missing>"
    return versions


def check() -> int:
    versions = collect()
    distinct = set(versions.values())
    width = max(len(key) for key in versions)
    for key, value in versions.items():
        print(f"{key:<{width}}  {value}")
    if len(distinct) != 1 or not SEMVER.match(next(iter(distinct))):
        print("error: repository versions disagree (or are not plain X.Y.Z)", file=sys.stderr)
        return 1
    print(f"ok: every location carries {next(iter(distinct))}")
    return 0


def _replace_in_section(text: str, section: str, pattern: str, replacement: str) -> str:
    """Replace exactly one match of `pattern` inside the TOML table `section`."""
    header = re.search(rf"^\[{re.escape(section)}\]\s*$", text, flags=re.MULTILINE)
    if header is None:
        raise SystemExit(f"error: [{section}] not found in Cargo.toml")
    start = header.end()
    following = re.search(r"^\[", text[start:], flags=re.MULTILINE)
    end = start + following.start() if following else len(text)
    body, count = re.subn(pattern, replacement, text[start:end], count=1, flags=re.MULTILINE)
    if count != 1:
        raise SystemExit(f"error: version not found in [{section}] of Cargo.toml")
    return text[:start] + body + text[end:]


def set_cargo(version: str) -> int:
    if not SEMVER.match(version):
        raise SystemExit(f"error: {version!r} is not a plain X.Y.Z version")
    path = ROOT / "Cargo.toml"
    text = path.read_text(encoding="utf-8")
    text = _replace_in_section(text, "workspace.package", r'^(version\s*=\s*)"[^"]*"', rf'\g<1>"{version}"')
    text = _replace_in_section(
        text,
        "workspace.dependencies",
        rf'^({re.escape(WRITER_CRATE)}\s*=\s*\{{[^}}\n]*\bversion\s*=\s*)"[^"]*"',
        rf'\g<1>"{version}"',
    )
    path.write_text(text, encoding="utf-8")
    workspace = tomllib.loads(text)["workspace"]
    if workspace["package"]["version"] != version or workspace["dependencies"][WRITER_CRATE].get("version") != version:
        raise SystemExit("error: Cargo.toml did not take the new version")
    print(f"Cargo.toml set to {version}")
    return 0


def main(argv: list[str]) -> int:
    if argv == ["check"]:
        return check()
    if len(argv) == 2 and argv[0] == "set-cargo":
        return set_cargo(argv[1])
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
