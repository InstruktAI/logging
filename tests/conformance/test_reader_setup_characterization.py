"""Characterize today's Python reader and setup against the shared conformance fixtures.

Temporary: this proves `conformance/reader` and `conformance/setup` describe
what `instrukt-ai-logs` (cli.py) and `instrukt-ai-log-setup` (install.py) do
today. It is deleted when the Python CLI leaves the package; the Rust test
suite runs the same fixtures afterwards.
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Any

import pytest

from tests.conformance._harness import Sandbox, check_reader_case, check_setup_case, load_cases

pytestmark = pytest.mark.process

_READER_COMMAND = [sys.executable, "-c", "from instrukt_ai_logging.cli import main; main()"]
_SETUP_COMMAND = [sys.executable, "-c", "from instrukt_ai_logging.install import main; main()"]

# A follow case runs for its fixture's `run_ms` (up to ~3 s) plus interpreter
# start-up and SIGINT handling, which can exceed the suite-wide 5 s per-test cap.
_FOLLOW_TIMEOUT_S = 10


def _reader_params() -> list[Any]:
    params: list[Any] = []
    for name, case in load_cases("reader"):
        marks = [pytest.mark.timeout(_FOLLOW_TIMEOUT_S)] if case.get("follow") else []
        params.append(pytest.param(case, id=name, marks=marks))
    return params


def _setup_params() -> list[Any]:
    host = "darwin" if sys.platform == "darwin" else "linux"
    params: list[Any] = []
    for name, case in load_cases("setup"):
        skip = case["platform"] != host
        marks = [pytest.mark.skip(reason=f"{case['platform']} case on a {host} host")] if skip else []
        params.append(pytest.param(case, id=name, marks=marks))
    return params


@pytest.mark.parametrize("case", _reader_params())
def test_reader_case(case: dict[str, Any], tmp_path: Path) -> None:
    check_reader_case(_READER_COMMAND, case, Sandbox.create(tmp_path / "sandbox"))


@pytest.mark.parametrize("case", _setup_params())
def test_setup_case(case: dict[str, Any], tmp_path: Path) -> None:
    check_setup_case(_SETUP_COMMAND, case, Sandbox.create(tmp_path / "sandbox"))
