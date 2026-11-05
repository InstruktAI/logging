import logging
import os
from logging.handlers import WatchedFileHandler
from pathlib import Path
from tempfile import TemporaryDirectory

import pytest
from instrukt_ai_logging import InstruktAILogger, configure_logging, get_logger

# Log markers shared by each test and its assertions — named
# constants. The logfmt field fragments are derived from the markers so the
# expected output stays in sync with what is logged.
_LOGGER_OURS = "teleclaude.core"
_LOGGER_HTTPCORE = "httpcore.http11"
_LOGGER_TELEGRAM = "telegram.ext.ExtBot"
_LOGGER_MUTED = "teleclaude.cli.tui"

_MSG_OURS = "hello from ours"
_MSG_THIRD_PARTY = "hello from third-party"
_MSG_HTTPCORE = "httpcore info"
_MSG_TELEGRAM = "telegram info"
_MSG_TELEGRAM_ERROR = "telegram error"
_MSG_HELLO = "hello"
_MSG_CORE_DEBUG = "core debug"
_MSG_TUI_DEBUG = "tui debug"
_MSG_TUI_WARNING = "tui warning"

_KV_SESSION = "abc123"
_KV_N = 1

_FIELD_LOGGER_OURS = f"logger={_LOGGER_OURS}"
_FIELD_LOGGER_HTTPCORE = f"logger={_LOGGER_HTTPCORE}"
_FIELD_LOGGER_TELEGRAM = f"logger={_LOGGER_TELEGRAM}"
_FIELD_LOGGER_MUTED = f"logger={_LOGGER_MUTED}"
_FIELD_MSG_OURS = f'msg="{_MSG_OURS}"'
_FIELD_MSG_HTTPCORE = f'msg="{_MSG_HTTPCORE}"'
_FIELD_MSG_HELLO = f'msg="{_MSG_HELLO}"'
_FIELD_MSG_CORE_DEBUG = f'msg="{_MSG_CORE_DEBUG}"'
_FIELD_MSG_TUI_WARNING = f'msg="{_MSG_TUI_WARNING}"'
_FIELD_SESSION = f"session={_KV_SESSION}"
_FIELD_N = f"n={_KV_N}"

_APP_NAME = "teleclaude"


@pytest.fixture()
def isolated_logging():
    previous_handlers = list(logging.root.handlers)
    previous_root_level = logging.root.level

    try:
        yield
    finally:
        for handler in logging.root.handlers:
            try:
                handler.close()
            except Exception:
                pass
        logging.root.handlers = previous_handlers
        logging.root.setLevel(previous_root_level)


def _read_text(path: Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")


def test_our_logs_respect_app_level_and_third_party_baseline(isolated_logging, monkeypatch):
    with TemporaryDirectory() as tmp:
        monkeypatch.setenv("XDG_STATE_HOME", tmp)
        monkeypatch.setenv("TELECLAUDE_LOG_LEVEL", "DEBUG")
        monkeypatch.setenv("TELECLAUDE_THIRD_PARTY_LOG_LEVEL", "WARNING")
        monkeypatch.delenv("TELECLAUDE_THIRD_PARTY_LOGGERS", raising=False)

        log_path = configure_logging("teleclaude")

        logging.getLogger(_LOGGER_OURS).debug(_MSG_OURS)
        logging.getLogger(_LOGGER_HTTPCORE).info(_MSG_THIRD_PARTY)

        content = _read_text(log_path)
        assert _FIELD_LOGGER_OURS in content
        assert _FIELD_MSG_OURS in content
        assert _FIELD_LOGGER_HTTPCORE not in content


def test_configure_logging_uses_single_file_handler(isolated_logging, monkeypatch):
    with TemporaryDirectory() as tmp:
        monkeypatch.setenv("XDG_STATE_HOME", tmp)
        monkeypatch.setenv("TELECLAUDE_LOG_LEVEL", "INFO")
        monkeypatch.setenv("TELECLAUDE_THIRD_PARTY_LOG_LEVEL", "WARNING")

        configure_logging("teleclaude")

        assert len(logging.root.handlers) == 1
        assert isinstance(logging.root.handlers[0], WatchedFileHandler)


def test_configure_logging_fails_fast_when_file_unwritable(isolated_logging, monkeypatch):
    with TemporaryDirectory() as tmp:
        monkeypatch.setenv("XDG_STATE_HOME", tmp)
        monkeypatch.setenv("TELECLAUDE_LOG_LEVEL", "INFO")
        monkeypatch.setenv("TELECLAUDE_THIRD_PARTY_LOG_LEVEL", "WARNING")

        # Pre-create the target log file at the path configure_logging resolves,
        # then strip all permissions to simulate a root-owned/unwritable file
        # left behind by a botched log rotation.
        app_dir = Path(tmp) / "instrukt-ai" / _APP_NAME
        app_dir.mkdir(parents=True, exist_ok=True)
        log_path = app_dir / f"{_APP_NAME}.log"
        log_path.write_text("", encoding="utf-8")
        os.chmod(log_path, 0)

        try:
            # Logging is an essential subsystem: an unopenable log file must
            # raise, never silently degrade.
            with pytest.raises(PermissionError):
                configure_logging(_APP_NAME)
        finally:
            # Restore permissions so the TemporaryDirectory cleanup can unlink it.
            os.chmod(log_path, 0o644)


def test_configure_logging_resolves_xdg_state_home_location(isolated_logging, monkeypatch):
    with TemporaryDirectory() as tmp:
        monkeypatch.setenv("XDG_STATE_HOME", tmp)
        monkeypatch.setenv("TELECLAUDE_LOG_LEVEL", "INFO")
        monkeypatch.setenv("TELECLAUDE_THIRD_PARTY_LOG_LEVEL", "WARNING")

        log_path = configure_logging(_APP_NAME)

        assert log_path == Path(tmp) / "instrukt-ai" / _APP_NAME / f"{_APP_NAME}.log"


def test_configure_logging_falls_back_to_local_state_home_when_xdg_unset(isolated_logging, monkeypatch):
    with TemporaryDirectory() as tmp:
        monkeypatch.delenv("XDG_STATE_HOME", raising=False)
        monkeypatch.setenv("HOME", tmp)
        monkeypatch.setenv("TELECLAUDE_LOG_LEVEL", "INFO")
        monkeypatch.setenv("TELECLAUDE_THIRD_PARTY_LOG_LEVEL", "WARNING")

        log_path = configure_logging(_APP_NAME)

        assert log_path == Path(tmp) / ".local" / "state" / "instrukt-ai" / _APP_NAME / f"{_APP_NAME}.log"


def test_get_logger_returns_instrukt_logger(isolated_logging):
    logger = get_logger(_LOGGER_OURS)
    assert isinstance(logger, InstruktAILogger)
    logger.info(_MSG_HELLO, session=_KV_SESSION, n=_KV_N)


def test_spotlight_selects_third_party_and_keeps_others_at_warning(isolated_logging, monkeypatch):
    with TemporaryDirectory() as tmp:
        monkeypatch.setenv("XDG_STATE_HOME", tmp)
        monkeypatch.setenv("TELECLAUDE_LOG_LEVEL", "INFO")
        monkeypatch.setenv("TELECLAUDE_THIRD_PARTY_LOG_LEVEL", "INFO")
        monkeypatch.setenv("TELECLAUDE_THIRD_PARTY_LOGGERS", "httpcore")

        log_path = configure_logging("teleclaude")

        # Ensure records are actually created even though root is WARNING in spotlight mode.
        httpcore_logger = logging.getLogger("httpcore")
        telegram_logger = logging.getLogger("telegram")
        previous_httpcore_level = httpcore_logger.level
        previous_telegram_level = telegram_logger.level
        httpcore_logger.setLevel(logging.INFO)
        telegram_logger.setLevel(logging.INFO)

        try:
            logging.getLogger(_LOGGER_HTTPCORE).info(_MSG_HTTPCORE)
            logging.getLogger(_LOGGER_TELEGRAM).info(_MSG_TELEGRAM)
            logging.getLogger(_LOGGER_TELEGRAM).error(_MSG_TELEGRAM_ERROR)
        finally:
            httpcore_logger.setLevel(previous_httpcore_level)
            telegram_logger.setLevel(previous_telegram_level)

        content = _read_text(log_path)
        assert _FIELD_LOGGER_HTTPCORE in content
        assert _FIELD_MSG_HTTPCORE in content
        # Non-spotlight third-party records are held to WARNING+, not dropped: the
        # INFO record is filtered out even though its logger level allows it, the
        # ERROR record passes.
        assert _MSG_TELEGRAM not in content
        assert f'{_FIELD_LOGGER_TELEGRAM} msg="{_MSG_TELEGRAM_ERROR}"' in content


def test_named_kv_logger_emits_pairs(isolated_logging, monkeypatch):
    with TemporaryDirectory() as tmp:
        monkeypatch.setenv("XDG_STATE_HOME", tmp)
        monkeypatch.setenv("TELECLAUDE_LOG_LEVEL", "INFO")
        monkeypatch.setenv("TELECLAUDE_THIRD_PARTY_LOG_LEVEL", "WARNING")

        log_path = configure_logging("teleclaude")

        logging.getLogger(_LOGGER_OURS).info(_MSG_HELLO, session=_KV_SESSION, n=_KV_N)

        content = _read_text(log_path)
        assert _FIELD_MSG_HELLO in content
        assert _FIELD_SESSION in content
        assert _FIELD_N in content


def test_muted_loggers_forced_to_warning(isolated_logging, monkeypatch):
    with TemporaryDirectory() as tmp:
        monkeypatch.setenv("XDG_STATE_HOME", tmp)
        monkeypatch.setenv("TELECLAUDE_LOG_LEVEL", "DEBUG")
        monkeypatch.setenv("TELECLAUDE_THIRD_PARTY_LOG_LEVEL", "WARNING")
        monkeypatch.setenv("TELECLAUDE_MUTED_LOGGERS", _LOGGER_MUTED)

        log_path = configure_logging("teleclaude")

        logging.getLogger(_LOGGER_OURS).debug(_MSG_CORE_DEBUG)
        logging.getLogger(_LOGGER_MUTED).debug(_MSG_TUI_DEBUG)
        logging.getLogger(_LOGGER_MUTED).warning(_MSG_TUI_WARNING)

        content = _read_text(log_path)
        # Core debug should appear (app level is DEBUG)
        assert _FIELD_LOGGER_OURS in content
        assert _FIELD_MSG_CORE_DEBUG in content
        # Muted logger's debug should NOT appear (forced to WARNING)
        assert _MSG_TUI_DEBUG not in content
        # Muted logger's warning SHOULD appear
        assert _FIELD_LOGGER_MUTED in content
        assert _FIELD_MSG_TUI_WARNING in content
