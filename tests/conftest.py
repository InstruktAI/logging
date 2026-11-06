from __future__ import annotations

import os
import threading
from collections.abc import Callable
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


@pytest.fixture()
def fail_log_writes(monkeypatch: pytest.MonkeyPatch) -> Callable[[int], None]:
    """Return a switch that makes every write to a log file fail with an errno.

    Only the descriptors the writer opens for its log files fail, as on a full
    disk (`ENOSPC`); every other write, pytest's own capture included, goes
    through.
    """
    import instrukt_ai_logging.logging as logging_module

    def enable(error: int) -> None:
        log_fds: set[int] = set()
        real_open = logging_module._open_log_fd  # pyright: ignore[reportPrivateUsage]
        real_write = os.write

        def open_log_fd(path: str) -> tuple[int, int, int]:
            opened = real_open(path)
            log_fds.add(opened[0])
            return opened

        def write(fd: int, data: bytes) -> int:
            if fd in log_fds:
                raise OSError(error, os.strerror(error))
            return real_write(fd, data)

        monkeypatch.setattr(logging_module, "_open_log_fd", open_log_fd)
        monkeypatch.setattr(logging_module.os, "write", write)

    return enable
