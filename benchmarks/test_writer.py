"""Writer benchmarks: one log call per iteration.

Run with `uv run pytest benchmarks --codspeed` (CodSpeed measures; locally it
reports walltime). Without `--codspeed` each case runs once, as a smoke check.
The plain `uv run pytest` never collects this directory (`testpaths`).
"""

from __future__ import annotations

from collections.abc import Callable, Iterator
from pathlib import Path

import pytest
from pytest_codspeed import BenchmarkFixture

from benchmarks.cases import CASES, ENV_KEYS, Case, install, uninstall


@pytest.fixture
def writer(
    request: pytest.FixtureRequest, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> Iterator[Callable[[], None]]:
    case: Case = request.param
    state = tmp_path / "xdg-state"
    home = tmp_path / "home"
    home.mkdir()
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("XDG_STATE_HOME", str(state))
    for key in ENV_KEYS:
        monkeypatch.delenv(key, raising=False)
    for key, value in case.env.items():
        monkeypatch.setenv(key, value)
    try:
        yield install(case)
    finally:
        uninstall()


@pytest.mark.parametrize(
    "writer",
    [pytest.param(c, id=c.name, marks=[pytest.mark.slow_bench] if c.slow else []) for c in CASES],
    indirect=True,
)
def test_writer(benchmark: BenchmarkFixture, writer: Callable[[], None]) -> None:
    benchmark(writer)
