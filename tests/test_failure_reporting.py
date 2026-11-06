"""Runtime write failures: dropped, counted and reported, never raised and never leaking the line."""

from __future__ import annotations

import errno
import io
import logging
import os
from collections.abc import Callable, Iterator
from pathlib import Path

import instrukt_ai_logging.logging as logging_module
import pytest
from instrukt_ai_logging import configure_logging, failure_count

_APP = "teleclaude"
_TOKEN = "eyJhbGciOiJIUzI1NiJ9.secret-token-value"
_PREFIX = "instrukt-ai-logging: dropped a log line: cannot write "


@pytest.fixture()
def isolated_logging() -> Iterator[None]:
    previous_handlers = list(logging.root.handlers)
    previous_root_level = logging.root.level
    try:
        yield
    finally:
        for handler in logging.root.handlers:
            handler.close()
        logging.root.handlers = previous_handlers
        logging.root.setLevel(previous_root_level)


@pytest.fixture()
def fresh_reporter(monkeypatch: pytest.MonkeyPatch) -> Iterator[None]:
    """A reporter of its own, so an earlier report's rate limit cannot hold this one back."""
    monkeypatch.setattr(logging_module, "_REPORTER", logging_module._FailureReporter())
    yield


def _reports(text: str) -> list[str]:
    return [line for line in text.splitlines() if line.startswith("instrukt-ai-logging:")]


def test_enospc_is_reported_without_the_message_or_args(
    isolated_logging: None,
    fresh_reporter: None,
    fail_log_writes: Callable[[int], None],
    capsys: pytest.CaptureFixture[str],
) -> None:
    fail_log_writes(errno.ENOSPC)
    log_path = configure_logging(_APP)
    log = logging.getLogger("teleclaude.core")

    # The token is in the args, the formatted message and a field: none may reach stderr.
    log.warning("auth with %s", f"Bearer {_TOKEN}", extra={"kv": {"token": _TOKEN}})
    log.error("again Bearer %s", _TOKEN)

    err = capsys.readouterr().err
    assert _TOKEN not in err
    assert "auth with" not in err
    assert _reports(err) == [
        f"{_PREFIX}{log_path}: OSError errno={errno.ENOSPC} ({os.strerror(errno.ENOSPC)}); "
        "1 dropped since start, 0 reports suppressed"
    ]
    assert err == _reports(err)[0] + "\n"
    assert failure_count() == 2


def test_failure_count_is_the_public_counter(fresh_reporter: None) -> None:
    import instrukt_ai_logging

    assert instrukt_ai_logging.failure_count is failure_count
    assert failure_count() == 0


def test_reports_are_rate_limited_to_one_a_minute() -> None:
    now = [100.0]
    reporter = logging_module._FailureReporter(clock=lambda: now[0])
    assert reporter.admit() == (1, 0)
    now[0] = 130.0
    assert reporter.admit() is None
    now[0] = 159.9
    assert reporter.admit() is None
    now[0] = 160.0
    assert reporter.admit() == (4, 2)
    now[0] = 500.0
    assert reporter.admit() == (5, 0)
    assert reporter.failures == 5


@pytest.mark.skipif(os.geteuid() == 0, reason="root can open a mode-000 file")
def test_an_unwritable_file_after_rotation_never_raises(
    isolated_logging: None, fresh_reporter: None, capsys: pytest.CaptureFixture[str]
) -> None:
    # Python 3.12's WatchedFileHandler reopened outside its try: the
    # PermissionError escaped from the logging call.
    log_path = configure_logging(_APP)
    log_path.rename(log_path.with_name(log_path.name + ".0"))
    log_path.write_text("", encoding="utf-8")
    os.chmod(log_path, 0)
    try:
        logging.getLogger("teleclaude.core").warning("after rotation")
    finally:
        os.chmod(log_path, 0o600)

    assert _reports(capsys.readouterr().err) == [
        f"{_PREFIX}{log_path}: PermissionError errno={errno.EACCES} ({os.strerror(errno.EACCES)}); "
        "1 dropped since start, 0 reports suppressed"
    ]
    assert failure_count() == 1
    # The next write, once the file is writable again, goes through.
    logging.getLogger("teleclaude.core").warning("recovered")
    assert 'msg="recovered"' in log_path.read_text(encoding="utf-8")


def test_a_symlink_planted_after_rotation_is_not_written_through(
    isolated_logging: None, fresh_reporter: None, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    log_path = configure_logging(_APP)
    target = tmp_path / "elsewhere"
    target.write_text("", encoding="utf-8")
    log_path.rename(log_path.with_name(log_path.name + ".0"))
    log_path.symlink_to(target)

    logging.getLogger("teleclaude.core").warning("must not leave the log dir")

    assert target.read_text(encoding="utf-8") == ""
    assert _reports(capsys.readouterr().err) == [
        f"{_PREFIX}{log_path}: OSError errno={errno.ELOOP} ({os.strerror(errno.ELOOP)}); "
        "1 dropped since start, 0 reports suppressed"
    ]


class _BrokenStream(io.StringIO):
    def write(self, s: str) -> int:
        raise BrokenPipeError(errno.EPIPE, "Broken pipe")


@pytest.mark.parametrize("stderr", [None, _BrokenStream()])
def test_a_broken_stderr_never_raises(
    isolated_logging: None,
    fresh_reporter: None,
    fail_log_writes: Callable[[int], None],
    monkeypatch: pytest.MonkeyPatch,
    stderr: io.StringIO | None,
) -> None:
    fail_log_writes(errno.ENOSPC)
    configure_logging(_APP)
    monkeypatch.setattr("sys.stderr", stderr)
    logging.getLogger("teleclaude.core").warning("dropped")
    assert failure_count() == 1


def test_a_formatting_failure_is_reported_too(
    isolated_logging: None, fresh_reporter: None, capsys: pytest.CaptureFixture[str]
) -> None:
    class Unprintable:
        def __str__(self) -> str:
            raise RuntimeError(f"secret {_TOKEN}")

    configure_logging(_APP)
    logging.getLogger("teleclaude.core").warning("m", extra={"kv": {"value": Unprintable()}})
    err = capsys.readouterr().err
    assert _TOKEN not in err
    assert _reports(err)[0].endswith(": RuntimeError errno=none; 1 dropped since start, 0 reports suppressed")


class _RaisingErrno(RuntimeError):
    """An exception whose `errno` property raises in turn."""

    @property
    def errno(self) -> int:
        raise ValueError(f"errno lookup {_TOKEN}")


def test_an_exception_whose_errno_raises_never_escapes(
    isolated_logging: None, fresh_reporter: None, capsys: pytest.CaptureFixture[str]
) -> None:
    class Unprintable:
        def __str__(self) -> str:
            raise _RaisingErrno(f"secret {_TOKEN}")

    configure_logging(_APP)
    # Before: the errno lookup ran outside the reporter's `try`, so the
    # ValueError escaped from the logging call.
    logging.getLogger("teleclaude.core").warning("m", extra={"kv": {"value": Unprintable()}})

    err = capsys.readouterr().err
    assert _TOKEN not in err
    assert _reports(err)[0].endswith(": _RaisingErrno errno=none; 1 dropped since start, 0 reports suppressed")
    assert failure_count() == 1


class _SecretInt(int):
    """An `int` whose every text form is a secret."""

    def __str__(self) -> str:
        return f"Bearer {_TOKEN}"

    __repr__ = __str__

    def __format__(self, spec: str) -> str:
        return f"Bearer {_TOKEN}"


def test_an_int_subclass_errno_is_never_rendered(
    isolated_logging: None,
    fresh_reporter: None,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    configure_logging(_APP)

    def write(fd: int, data: bytes) -> int:
        raise OSError(_SecretInt(errno.ENOSPC), "No space left on device")

    monkeypatch.setattr(logging_module.os, "write", write)
    logging.getLogger("teleclaude.core").warning("m")

    err = capsys.readouterr().err
    assert _TOKEN not in err
    assert _reports(err)[0].endswith(": OSError errno=none; 1 dropped since start, 0 reports suppressed")


def test_a_type_name_that_is_not_a_str_is_never_rendered(
    isolated_logging: None, fresh_reporter: None, capsys: pytest.CaptureFixture[str]
) -> None:
    class SecretName(str):
        def __format__(self, spec: str) -> str:
            return f"Bearer {_TOKEN}"

    class Failure(RuntimeError):
        pass

    Failure.__qualname__ = SecretName("Failure")

    class Unprintable:
        def __str__(self) -> str:
            raise Failure("x")

    configure_logging(_APP)
    logging.getLogger("teleclaude.core").warning("m", extra={"kv": {"value": Unprintable()}})

    err = capsys.readouterr().err
    assert _TOKEN not in err
    assert _reports(err)[0].endswith(": unknown errno=none; 1 dropped since start, 0 reports suppressed")


class _LoggingStream(io.StringIO):
    """A stderr routed into logging, as some hosts do."""

    def write(self, s: str) -> int:
        logging.getLogger("teleclaude.stderr").error(s)
        return super().write(s)


def test_the_report_never_logs_through_the_failing_handler(
    isolated_logging: None,
    fresh_reporter: None,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class Unprintable:
        def __str__(self) -> str:
            raise RuntimeError("x")

    log_path = configure_logging(_APP)
    stream = _LoggingStream()
    monkeypatch.setattr("sys.stderr", stream)

    logging.getLogger("teleclaude.core").warning("m", extra={"kv": {"value": Unprintable()}})

    # The report reached stderr once, and its re-logged copy never reached the
    # handler: not written to the file, and not a second failure.
    assert len(_reports(stream.getvalue())) == 1
    assert "dropped a log line" not in log_path.read_text(encoding="utf-8")
    assert failure_count() == 1


def test_the_report_never_logs_through_a_handler_whose_writes_fail(
    isolated_logging: None,
    fresh_reporter: None,
    fail_log_writes: Callable[[int], None],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fail_log_writes(errno.ENOSPC)
    configure_logging(_APP)
    stream = _LoggingStream()
    monkeypatch.setattr("sys.stderr", stream)

    logging.getLogger("teleclaude.core").warning("m")

    assert len(_reports(stream.getvalue())) == 1
    # Only the original line failed: the re-logged report was never tried.
    assert failure_count() == 1
