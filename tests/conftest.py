from __future__ import annotations

import os
import threading
from pathlib import Path

import pytest

_SESSION_TIMEOUT_KILL_CODE = 124  # matches the shell `timeout(1)` convention
_session_timeout_timer: threading.Timer | None = None


def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addini(
        "session_timeout",
        help="Hard wall-clock budget (seconds) for the whole test session; force-exits past it.",
        default="90",
    )


def pytest_sessionstart(session: pytest.Session) -> None:
    global _session_timeout_timer
    limit = float(session.config.getini("session_timeout"))
    # A per-test timeout (see [tool.pytest.ini_options] timeout) cannot catch a
    # hang outside a test body (fixture teardown, collection). This backstop
    # guarantees the whole run cannot silently sit past its budget either.
    _session_timeout_timer = threading.Timer(limit, lambda: os._exit(_SESSION_TIMEOUT_KILL_CODE))
    _session_timeout_timer.daemon = True
    _session_timeout_timer.start()


def pytest_sessionfinish() -> None:
    if _session_timeout_timer is not None:
        _session_timeout_timer.cancel()


@pytest.fixture(autouse=True)
def _isolate_home(  # pyright: ignore[reportUnusedFunction]
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Keep every test off the real home and XDG trees.

    Autouse and unconditional: no test may rely on the ambient HOME or XDG
    variables, so nothing a test writes can reach the real `~/.local/state`.
    """
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path / "xdg-state"))
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "xdg-config"))
