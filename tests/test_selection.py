"""Python enforces the selection policy itself: a logger's own level can make it quieter, never louder."""

from __future__ import annotations

import logging
from collections.abc import Iterator
from pathlib import Path

import pytest
from instrukt_ai_logging import configure_logging
from instrukt_ai_logging.logging import LogFileHandler

_APP = "teleclaude"


@pytest.fixture()
def clean_levels() -> Iterator[None]:
    """Restore the logging state, and clear every level a test or a call set."""
    previous_handlers = list(logging.root.handlers)
    previous_root_level = logging.root.level

    def reset() -> None:
        for obj in logging.root.manager.loggerDict.values():
            if isinstance(obj, logging.Logger):
                obj.setLevel(logging.NOTSET)

    reset()
    try:
        yield
    finally:
        for handler in logging.root.handlers:
            handler.close()
        reset()
        logging.root.handlers = previous_handlers
        logging.root.setLevel(previous_root_level)


def _lines(path: Path) -> list[str]:
    return [line.split(" ", 1)[1] for line in path.read_text(encoding="utf-8").splitlines()]


def test_a_library_child_set_to_debug_stays_at_the_third_party_threshold(
    clean_levels: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    log_path = configure_logging(_APP)
    # A library makes its own child logger chatty; propagation skips ancestor levels.
    child = logging.getLogger("httpx._client")
    child.setLevel(logging.DEBUG)
    child.debug("debug")
    child.info("info")
    child.warning("warning")
    assert _lines(log_path) == ['level=WARNING logger=httpx._client msg="warning"']


def test_an_app_child_set_to_debug_stays_at_the_app_level(clean_levels: None) -> None:
    log_path = configure_logging(_APP)
    child = logging.getLogger("teleclaude.core.db")
    child.setLevel(logging.DEBUG)
    child.debug("debug")
    child.info("info")
    assert _lines(log_path) == ['level=INFO logger=teleclaude.core.db msg="info"']


def test_a_muted_child_set_to_debug_stays_muted(clean_levels: None, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("TELECLAUDE_LOG_LEVEL", "DEBUG")
    monkeypatch.setenv("TELECLAUDE_MUTED_LOGGERS", "teleclaude.cli")
    log_path = configure_logging(_APP)
    child = logging.getLogger("teleclaude.cli.tui")
    child.setLevel(logging.DEBUG)
    child.info("info")
    child.warning("warning")
    logging.getLogger("teleclaude.core").debug("app debug")
    assert _lines(log_path) == [
        'level=WARNING logger=teleclaude.cli.tui msg="warning"',
        'level=DEBUG logger=teleclaude.core msg="app debug"',
    ]


def test_spotlight_children_are_held_to_the_spotlight_level(
    clean_levels: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("TELECLAUDE_THIRD_PARTY_LOGGERS", "httpx")
    monkeypatch.setenv("TELECLAUDE_THIRD_PARTY_LOG_LEVEL", "INFO")
    log_path = configure_logging(_APP)
    for name in ("httpx.pool", "telegram.ext"):
        child = logging.getLogger(name)
        child.setLevel(logging.DEBUG)
        child.debug("debug")
        child.info("info")
        child.warning("warning")
    assert _lines(log_path) == [
        'level=INFO logger=httpx.pool msg="info"',
        'level=WARNING logger=httpx.pool msg="warning"',
        'level=WARNING logger=telegram.ext msg="warning"',
    ]


def test_a_logger_can_still_be_made_quieter(clean_levels: None) -> None:
    log_path = configure_logging(_APP)
    child = logging.getLogger("teleclaude.noisy")
    child.setLevel(logging.ERROR)
    child.warning("warning")
    child.error("error")
    assert _lines(log_path) == ['level=ERROR logger=teleclaude.noisy msg="error"']


def test_a_second_call_starts_clean(clean_levels: None, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("TELECLAUDE_MUTED_LOGGERS", "teleclaude.cli")
    monkeypatch.setenv("TELECLAUDE_THIRD_PARTY_LOGGERS", "httpx")
    monkeypatch.setenv("TELECLAUDE_THIRD_PARTY_LOG_LEVEL", "DEBUG")
    configure_logging(_APP)
    first = logging.root.handlers[0]
    assert isinstance(first, LogFileHandler)
    assert logging.getLogger("teleclaude.cli").level == logging.WARNING
    assert logging.getLogger("httpx").level == logging.DEBUG

    monkeypatch.delenv("TELECLAUDE_MUTED_LOGGERS")
    monkeypatch.delenv("TELECLAUDE_THIRD_PARTY_LOGGERS")
    monkeypatch.delenv("TELECLAUDE_THIRD_PARTY_LOG_LEVEL")
    log_path = configure_logging(_APP)

    # The earlier call's levels are gone, its handler is closed and replaced.
    assert logging.getLogger("teleclaude.cli").level == logging.NOTSET
    assert logging.getLogger("httpx").level == logging.NOTSET
    assert logging.root.handlers != [first]
    assert first._fd == -1
    logging.getLogger("teleclaude.cli").info("cli info")
    logging.getLogger("httpx").info("httpx info dropped")
    assert _lines(log_path) == ['level=INFO logger=teleclaude.cli msg="cli info"']


def test_a_failed_second_call_keeps_the_first(clean_levels: None, monkeypatch: pytest.MonkeyPatch) -> None:
    log_path = configure_logging(_APP)
    first = logging.root.handlers[0]
    with pytest.raises(ValueError, match="not a bare file stem"):
        configure_logging(_APP, source="../x")
    assert logging.root.handlers == [first]
    logging.getLogger("teleclaude").info("still here")
    assert _lines(log_path) == ['level=INFO logger=teleclaude msg="still here"']
