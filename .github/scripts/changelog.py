"""CHANGELOG.md checks used by CI and the release chain.

Usage:
    python .github/scripts/changelog.py masked-text
    python .github/scripts/changelog.py notes <version>

`masked-text` fails when CHANGELOG.md holds text the GitHub Actions runner
masks as a credential (the word "Bearer" followed by a word). A masked value
is withheld wherever the runner prints or passes it, so release text must not
carry it.

`notes` prints the body of the `## <version>` section (up to the next `## `
heading) and fails when the section is missing or empty. It is the release
notes of that version.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

CHANGELOG = Path(__file__).resolve().parents[2] / "CHANGELOG.md"
MASKED = re.compile(r"bearer\s+\S", re.IGNORECASE)
SEMVER = re.compile(r"^\d+\.\d+\.\d+$")


def masked_text(lines: list[str]) -> int:
    hits = [(number, line) for number, line in enumerate(lines, start=1) if MASKED.search(line)]
    for number, line in hits:
        print(
            f"::error file=CHANGELOG.md,line={number}::the Actions runner masks 'Bearer <word>' as a credential: {line}"
        )
    return 1 if hits else 0


def notes(lines: list[str], version: str) -> int:
    if not SEMVER.match(version):
        print(f"error: {version!r} is not a plain X.Y.Z version", file=sys.stderr)
        return 2
    heading = f"## {version}"
    try:
        start = next(index for index, line in enumerate(lines) if line.rstrip() == heading) + 1
    except StopIteration:
        print(f"error: CHANGELOG.md has no '{heading}' section", file=sys.stderr)
        return 1
    end = next((index for index in range(start, len(lines)) if lines[index].startswith("## ")), len(lines))
    body = "\n".join(lines[start:end]).strip()
    if not body:
        print(f"error: the '{heading}' section of CHANGELOG.md is empty", file=sys.stderr)
        return 1
    print(body)
    return 0


def main(argv: list[str]) -> int:
    lines = CHANGELOG.read_text(encoding="utf-8").splitlines()
    if argv == ["masked-text"]:
        return masked_text(lines)
    if len(argv) == 2 and argv[0] == "notes":
        return notes(lines, argv[1])
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
