"""Benchmark collection settings.

The repository-wide `timeout = 5` guards the unit tests. A benchmark under
CodSpeed runs many rounds (walltime) or under a CPU simulator (simulation),
both far slower than one call, so each benchmark gets a larger budget.
"""

from __future__ import annotations

import pytest

_BENCH_TIMEOUT_S = 300


def pytest_collection_modifyitems(items: list[pytest.Item]) -> None:
    for item in items:
        item.add_marker(pytest.mark.timeout(_BENCH_TIMEOUT_S))
