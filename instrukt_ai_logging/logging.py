from __future__ import annotations

import errno
import logging
import os
import re
import stat
from collections.abc import Iterable
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Protocol, cast, runtime_checkable

# Standard logging levels are: NOTSET=0, DEBUG=10, INFO=20, WARNING=30, ERROR=40, CRITICAL=50
TRACE: int = 5
logging.addLevelName(TRACE, "TRACE")
# Monkey-patch logging so our _lookup_level helper finds it automatically.
logging.TRACE = TRACE  # type: ignore[attr-defined]


def _normalize_env_prefix(name: str) -> str:
    # TELECLAUDE, MY_APP, etc.
    raw = name.strip().upper()
    if not raw:
        raise ValueError("name must be non-empty")
    raw = re.sub(r"[^A-Z0-9]+", "_", raw)
    raw = re.sub(r"_+", "_", raw).strip("_")
    if not raw:
        raise ValueError("name did not produce a valid env prefix")
    return raw


def _normalize_app_name(name: str) -> str:
    # teleclaude, my-app, etc.
    raw = name.strip().lower()
    if not raw:
        raise ValueError("name must be non-empty")
    raw = re.sub(r"[^a-z0-9]+", "-", raw)
    raw = re.sub(r"-+", "-", raw).strip("-")
    if not raw:
        raise ValueError("name did not produce a valid app name")
    return raw


def _normalize_logger_prefix(name: str) -> str:
    # teleclaude, crypto_ai, etc. (match Python package style)
    raw = name.strip().lower()
    if not raw:
        raise ValueError("name must be non-empty")
    raw = re.sub(r"[^a-z0-9]+", "_", raw)
    raw = re.sub(r"_+", "_", raw).strip("_")
    if not raw:
        raise ValueError("name did not produce a valid logger prefix")
    return raw


# Level names are matched in ASCII only, like the Rust writer: only these are
# trimmed (not `str.strip()`'s Unicode whitespace), and only ASCII letters are
# upper-cased (so `\u0131nfo`, dotless i, does not become `INFO` through Unicode
# case mapping).
_ASCII_WHITESPACE = " \t\n\r\x0b\x0c"


def _lookup_level(name: str) -> int | None:
    """Level number for a trimmed name, matched case-insensitively in ASCII."""
    if not name.isascii():
        return None
    candidate: object = getattr(logging, name.upper(), None)
    if isinstance(candidate, int):
        return candidate
    return None


def _is_utf8(value: str) -> bool:
    try:
        value.encode("utf-8")
    except UnicodeEncodeError:
        return False
    return True


def _getenv_utf8(name: str) -> str | None:
    """Read an environment variable as strict UTF-8, like the Rust writer.

    `None` when the variable is unset or its value is not valid UTF-8. On POSIX
    the raw bytes (`os.environb`) are decoded, so Python's filesystem encoding,
    which follows the locale, never decides what counts as valid UTF-8.
    """
    if os.supports_bytes_environ:
        raw = os.environb.get(os.fsencode(name))
        if raw is None:
            return None
        try:
            return raw.decode("utf-8")
        except UnicodeDecodeError:
            return None
    value = os.environ.get(name)
    if value is None or not _is_utf8(value):
        return None
    return value


def _parse_level(raw: str | None, default: int) -> tuple[int, bool]:
    """Resolve a level variable to `(level, unknown)`.

    Unset, empty and ASCII-whitespace-only values give the default silently
    (`unknown` is False), and so does a value that is not valid UTF-8 (on
    POSIX `os.environ` carries its bytes as lone surrogates, which could not
    be written to the UTF-8 log file), like the Rust writer. Any other value
    that is not a level name gives the default with `unknown` True, so
    initialization can warn about it.
    """
    if raw is None or not _is_utf8(raw):
        return default, False
    name = raw.strip(_ASCII_WHITESPACE)
    if not name:
        return default, False
    level = _lookup_level(name)
    if level is None:
        return default, True
    return level, False


def _parse_csv(value: str) -> list[str]:
    return [item.strip() for item in value.split(",") if item.strip()]


class UtcMillisFormatter(logging.Formatter):
    def formatTime(self, record: logging.LogRecord, datefmt: str | None = None) -> str:
        dt = datetime.fromtimestamp(record.created, tz=UTC)
        if datefmt:
            return dt.strftime(datefmt)
        return dt.strftime("%Y-%m-%dT%H:%M:%S") + f".{int(record.msecs):03d}Z"


_SAFE_BARE_VALUE = re.compile(r"^[A-Za-z0-9._/:+-]+$")
_SAFE_KEY = re.compile(r"^[A-Za-z_][A-Za-z0-9_.-]*$")

# Redaction (docs/contract.md, Redaction) is a set of secret spans found on the
# original text, merged, then each replaced by `<REDACTED>`. Every rule finds
# its spans by search rather than by regex substitution, so that no match
# consumes text another match could use, and in linear time on any input.
#
# Every character set is spelled out instead of using `\b`, `\w` or `\s`, whose
# meaning differs between regex engines for non-ASCII input.
# WORD: a redaction word character, ASCII only.
_REDACT_WORD = "A-Za-z0-9_"
# SPACE: Unicode White_Space. A token ends only here.
_REDACT_SPACE = "\\t-\\r \\x85\\xa0\\u1680\\u2000-\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000"
# SEP: SPACE plus U+001C-U+001F. Separates `Bearer` from its token.
_REDACT_SEP = _REDACT_SPACE + "\\x1c-\\x1f"

_REDACTED = "<REDACTED>"
_TELEGRAM = "api.telegram.org/bot"
_BEARER = "Bearer"
_SK = "sk-"
_SK_MIN_KEY = 10

# `Bearer` after a boundary, then its maximal separator run. The boundary is a
# lookbehind placed after the literal, so the search starts from the literal and
# rejects a start without returning to Python.
_BEARER_AT = re.compile(f"Bearer(?<![{_REDACT_WORD}]Bearer)[{_REDACT_SEP}]+")
# `sk-` after a boundary, a maximal run of ten or more ASCII alphanumerics, then a
# boundary. The run is possessive: it is maximal, so giving characters back can
# only put an alphanumeric after it, which is never a boundary. A key holds no
# `sk-`, so consuming it hides no other start.
_SK_AT = re.compile(f"sk-(?<![{_REDACT_WORD}]sk-)[A-Za-z0-9]{{{_SK_MIN_KEY},}}+(?![{_REDACT_WORD}])")
# The Telegram literal, with its token in a lookahead: only the literal is
# consumed, so a literal inside a token is still found.
_TELEGRAM_AT = re.compile(f"api\\.telegram\\.org/bot(?=([^/{_REDACT_SPACE}]+))")
_SPACE_AT = re.compile(f"[{_REDACT_SPACE}]")


# Each rule appends its spans to one shared list, in start order, joining a span
# to the one before it when they overlap or touch, as long as that one is the
# same rule's: the rule's spans start at index `run`.


def _add_telegram_spans(text: str, spans: list[tuple[int, int]]) -> None:
    run = len(spans)
    for m in _TELEGRAM_AT.finditer(text):
        start, end = m.span(1)
        if len(spans) > run and start <= spans[-1][1]:
            if end > spans[-1][1]:
                spans[-1] = (spans[-1][0], end)
        else:
            spans.append((start, end))


def _add_bearer_spans(text: str, spans: list[tuple[int, int]]) -> None:
    run = len(spans)
    n = len(text)
    # The end of a token depends only on where it starts. Starts only grow, so
    # while one falls inside the last answered range the answer still holds.
    asked, answer = n + 1, n
    pos = 0
    while (m := _BEARER_AT.search(text, pos)) is not None:
        start = m.end()
        if start == n:
            # The separator run reaches the end: no token, and no later
            # `Bearer` either.
            return
        # The run is maximal, so text[start] is not in SEP, hence not in SPACE:
        # the token is never empty.
        if not asked <= start <= answer:
            found = _SPACE_AT.search(text, start)
            asked, answer = start, found.start() if found else n
        end = answer
        if len(spans) > run and start <= spans[-1][1]:
            if end > spans[-1][1]:
                spans[-1] = (spans[-1][0], end)
        else:
            spans.append((start, end))
        # A `Bearer` inside this token has no SPACE before the token's end, so
        # its own token, if any, starts inside this one and ends where it does:
        # a span inside this span. Only one whose separator run reaches past
        # the end can add text, and only the last one can.
        last = text.rfind(_BEARER, start, end)
        pos = last if last >= 0 else end


def _add_sk_spans(text: str, spans: list[tuple[int, int]]) -> None:
    run = len(spans)
    for m in _SK_AT.finditer(text):
        start, end = m.start() + len(_SK), m.end()
        if len(spans) > run and start <= spans[-1][1]:
            if end > spans[-1][1]:
                spans[-1] = (spans[-1][0], end)
        else:
            spans.append((start, end))


def _redact_text(text: str) -> str:
    # Most text holds none of the literals: it comes back as it is.
    has_telegram = _TELEGRAM in text
    has_bearer = _BEARER in text
    has_sk = _SK in text
    if not (has_telegram or has_bearer or has_sk):
        return text

    spans: list[tuple[int, int]] = []
    runs = 0
    if has_telegram:
        _add_telegram_spans(text, spans)
        runs += len(spans) > 0
    if has_bearer:
        before = len(spans)
        _add_bearer_spans(text, spans)
        runs += len(spans) > before
    if has_sk:
        before = len(spans)
        _add_sk_spans(text, spans)
        runs += len(spans) > before
    if not spans:
        return text
    if runs > 1:
        # A few runs, each already in start order: the sort merges them in
        # linear time. Then join overlapping and touching spans across rules.
        spans.sort()
        joined: list[tuple[int, int]] = []
        for start, end in spans:
            if joined and start <= joined[-1][1]:
                if end > joined[-1][1]:
                    joined[-1] = (joined[-1][0], end)
            else:
                joined.append((start, end))
        spans = joined
    parts: list[str] = []
    copied = 0
    for start, end in spans:
        parts.append(text[copied:start])
        parts.append(_REDACTED)
        copied = end
    parts.append(text[copied:])
    return "".join(parts)


def _truncate_text(text: str, max_chars: int) -> str:
    if max_chars > 0 and len(text) > max_chars:
        return text[:max_chars] + "…(truncated)"
    return text


def _escape_quotes(value: str) -> str:
    return value.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n").replace("\r", "\\r")


def _format_logfmt_value(value: object, *, max_chars: int) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)

    text = _redact_text(str(value))
    text = _truncate_text(text, max_chars=max_chars)

    if _SAFE_BARE_VALUE.fullmatch(text):
        return text
    return f'"{_escape_quotes(text)}"'


def _format_logfmt_string(text: str, *, max_chars: int, force_quote: bool) -> str:
    redacted = _redact_text(text)
    redacted = _truncate_text(redacted, max_chars=max_chars)
    if not force_quote and _SAFE_BARE_VALUE.fullmatch(redacted):
        return redacted
    return f'"{_escape_quotes(redacted)}"'


class LogfmtFormatter(UtcMillisFormatter):
    """Single-line logfmt-ish formatter.

    Always emits key/value pairs with at least:
      level=..., logger=..., msg="..."

    Optional additional pairs can be attached via `extra={"kv": {...}}`.
    """

    def __init__(self, max_message_chars: int) -> None:
        super().__init__(datefmt=None)
        self.max_message_chars = max_message_chars

    def format(self, record: logging.LogRecord) -> str:
        ts = self.formatTime(record)
        try:
            message = record.getMessage()
        except Exception:
            message = "<unprintable>"

        parts = [
            ts,
            f"level={_format_logfmt_value(record.levelname, max_chars=self.max_message_chars)}",
            f"logger={_format_logfmt_value(record.name, max_chars=self.max_message_chars)}",
            f"msg={_format_logfmt_string(str(message), max_chars=self.max_message_chars, force_quote=True)}",
        ]

        raw_kv: object = getattr(record, "kv", None)
        if isinstance(raw_kv, dict):
            kv = cast("dict[object, object]", raw_kv)
            for key in sorted(kv.keys(), key=lambda k: str(k)):
                if key == "msg":
                    continue
                if not isinstance(key, str):
                    continue
                if not _SAFE_KEY.fullmatch(key):
                    continue
                parts.append(f"{key}={_format_logfmt_value(kv[key], max_chars=self.max_message_chars)}")

        if record.exc_info:
            try:
                exc_text = self.formatException(record.exc_info).replace("\n", "\\n")
            except Exception:
                exc_text = "<exception>"
            parts.append(f"exc={_format_logfmt_value(exc_text, max_chars=self.max_message_chars)}")

        return " ".join(parts)


@runtime_checkable
class InstruktAILoggerProtocol(Protocol):
    """Type surface for loggers that accept arbitrary `**kv` fields."""

    name: str

    def trace(self, msg: object, *args: object, **kwargs: object) -> None: ...

    def debug(self, msg: object, *args: object, **kwargs: object) -> None: ...

    def info(self, msg: object, *args: object, **kwargs: object) -> None: ...

    def warning(self, msg: object, *args: object, **kwargs: object) -> None: ...

    def error(self, msg: object, *args: object, **kwargs: object) -> None: ...

    def critical(self, msg: object, *args: object, **kwargs: object) -> None: ...

    def exception(self, msg: object, *args: object, **kwargs: object) -> None: ...

    def log(self, level: int, msg: object, *args: object, **kwargs: object) -> None: ...


class InstruktAILogger(logging.Logger):
    """Logger that accepts arbitrary `**kv` fields.

    This lets callers use normal logger methods with named key/value pairs:
      logging.getLogger("...").info("event_name", job_id=..., user_id=...)

    All `**kv` values are serialized to text by the formatter.
    """

    def _log_with_kv(self, level: int, msg: object, args: tuple[object, ...], **kwargs: Any) -> None:
        exc_info = kwargs.pop("exc_info", None)
        stack_info = kwargs.pop("stack_info", False)
        stacklevel = kwargs.pop("stacklevel", 1)
        extra = kwargs.pop("extra", None)

        # Remaining kwargs are treated as `kv`.
        kv: dict[str, object] = {k: v for k, v in kwargs.items() if _SAFE_KEY.fullmatch(k)}

        merged_extra: dict[str, object] = {}
        if isinstance(extra, dict):
            merged_extra.update(cast("dict[str, object]", extra))

        existing_kv = merged_extra.get("kv")
        if isinstance(existing_kv, dict):
            merged_extra["kv"] = {**existing_kv, **kv}
        else:
            merged_extra["kv"] = kv

        super()._log(
            level,
            msg,
            args,
            exc_info=exc_info,
            extra=merged_extra,
            stack_info=stack_info,
            stacklevel=stacklevel,
        )

    def trace(self, msg: object, *args: object, **kwargs: Any) -> None:
        if self.isEnabledFor(TRACE):
            self._log_with_kv(TRACE, msg, args, **kwargs)

    def debug(self, msg: object, *args: object, **kwargs: Any) -> None:
        if self.isEnabledFor(logging.DEBUG):
            self._log_with_kv(logging.DEBUG, msg, args, **kwargs)

    def info(self, msg: object, *args: object, **kwargs: Any) -> None:
        if self.isEnabledFor(logging.INFO):
            self._log_with_kv(logging.INFO, msg, args, **kwargs)

    def warning(self, msg: object, *args: object, **kwargs: Any) -> None:
        if self.isEnabledFor(logging.WARNING):
            self._log_with_kv(logging.WARNING, msg, args, **kwargs)

    def error(self, msg: object, *args: object, **kwargs: Any) -> None:
        if self.isEnabledFor(logging.ERROR):
            self._log_with_kv(logging.ERROR, msg, args, **kwargs)

    def critical(self, msg: object, *args: object, **kwargs: Any) -> None:
        if self.isEnabledFor(logging.CRITICAL):
            self._log_with_kv(logging.CRITICAL, msg, args, **kwargs)

    def exception(self, msg: object, *args: object, **kwargs: Any) -> None:
        kwargs.setdefault("exc_info", True)
        if self.isEnabledFor(logging.ERROR):
            self._log_with_kv(logging.ERROR, msg, args, **kwargs)

    def log(self, level: int, msg: object, *args: object, **kwargs: Any) -> None:
        if not isinstance(level, int):  # pyright: ignore[reportUnnecessaryIsInstance]
            raise TypeError("level must be an int")
        if self.isEnabledFor(level):
            self._log_with_kv(level, msg, args, **kwargs)


def _coerce_instrukt_logger(logger: logging.Logger) -> InstruktAILogger:
    if isinstance(logger, InstruktAILogger):
        return logger
    try:
        logger.__class__ = InstruktAILogger
    except TypeError:
        pass
    return cast(InstruktAILogger, logger)


def get_logger(name: str) -> InstruktAILogger:
    """Return a logger that accepts arbitrary `**kv` fields."""
    logging.setLoggerClass(InstruktAILogger)
    return _coerce_instrukt_logger(logging.getLogger(name))


class _ThirdPartySelectorFilter(logging.Filter):
    """Enforce third-party spotlight semantics on a handler.

    - Always allow records from `app_logger_prefix`.
    - For non-app loggers:
      - If `spotlight_prefixes` is empty: allow (level gating happens via logger levels).
      - If `spotlight_prefixes` is set: allow records whose logger name starts with one of them;
        every other third-party record is allowed only at WARNING and above.
    """

    def __init__(self, app_logger_prefix: str, spotlight_prefixes: tuple[str, ...]) -> None:
        super().__init__()
        self.app_logger_prefix = app_logger_prefix
        self.spotlight_prefixes = spotlight_prefixes

    def filter(self, record: logging.LogRecord) -> bool:
        name = record.name
        if name == self.app_logger_prefix or name.startswith(self.app_logger_prefix + "."):
            return True
        if not self.spotlight_prefixes:
            return True
        if record.levelno >= logging.WARNING:
            return True
        return any(name == p or name.startswith(p + ".") for p in self.spotlight_prefixes)


_SELF_LOGGER = "instrukt_ai_logging"
_UNKNOWN_LEVEL_MSG = "unknown log level; using the default"


_DIR_MODE = 0o700
_FILE_MODE = 0o600
_OPEN_FLAGS = os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC


def _open_log_fd(path: str) -> tuple[int, int, int]:
    """Open `path` for append and return `(fd, st_dev, st_ino)`.

    The file is created `0600`, an existing file is set to `0600`, and a
    symbolic link at `path` is refused (`O_NOFOLLOW`): it raises `OSError`
    with `ELOOP`.
    """
    try:
        fd = os.open(path, _OPEN_FLAGS, _FILE_MODE)
    except OSError as exc:
        if exc.errno == errno.ELOOP:
            raise OSError(errno.ELOOP, "the log path is a symbolic link, which is refused", path) from None
        raise
    try:
        st = os.fstat(fd)
        if stat.S_IMODE(st.st_mode) != _FILE_MODE:
            os.fchmod(fd, _FILE_MODE)
    except BaseException:
        os.close(fd)
        raise
    return fd, st.st_dev, st.st_ino


class LogFileHandler(logging.Handler):
    """Append each record as one line to the log file, following rotation.

    Like `logging.handlers.WatchedFileHandler`, before each write it checks that
    the path still names the open file (device and inode) and reopens it when
    rotation moved or removed it. Unlike it, the file is opened with `os.open`
    (`O_APPEND`, `O_NOFOLLOW`, mode `0600`) and each line goes out in one
    `os.write`, with no text-layer buffering.
    """

    terminator = "\n"

    def __init__(self, path: Path) -> None:
        super().__init__()
        self.path = path
        self._path = os.fspath(path)
        self._fd, self._dev, self._ino = _open_log_fd(self._path)

    def _reopen_if_needed(self) -> None:
        # `follow_symlinks=False`: a symbolic link planted at the path is a new
        # identity, so the reopen sees it, and refuses it.
        try:
            st: os.stat_result | None = os.stat(self._path, follow_symlinks=False)
        except FileNotFoundError:
            st = None
        if self._fd >= 0 and st is not None and st.st_dev == self._dev and st.st_ino == self._ino:
            return
        if self._fd >= 0:
            fd, self._fd = self._fd, -1
            os.close(fd)
        self._fd, self._dev, self._ino = _open_log_fd(self._path)

    def write_line(self, data: bytes) -> None:
        """Write one encoded line (terminator included). Raises on failure.

        The caller holds the handler lock.
        """
        self._reopen_if_needed()
        os.write(self._fd, data)

    def emit(self, record: logging.LogRecord) -> None:
        try:
            self.write_line((self.format(record) + self.terminator).encode("utf-8"))
        except RecursionError:
            raise
        except Exception:
            self.handleError(record)

    def close(self) -> None:
        self.acquire()
        try:
            if self._fd >= 0:
                fd, self._fd = self._fd, -1
                os.close(fd)
        finally:
            self.release()
        super().close()


def _warn_unknown_levels(handler: LogFileHandler, unknown_levels: list[tuple[str, str, str]]) -> None:
    """Write one WARNING line per unknown level value straight to the file.

    The line reports on the configuration of selection itself, so it bypasses
    every logger level and handler filter: it is formatted and written through
    the handler directly. Like an ordinary write, each line first reopens the
    path if rotation moved or removed the file. A failure raises (fail fast).
    """
    for variable, raw, default_name in unknown_levels:
        record = logging.LogRecord(_SELF_LOGGER, logging.WARNING, __file__, 0, _UNKNOWN_LEVEL_MSG, None, None)
        record.kv = {"default": default_name, "value": raw, "variable": variable}
        data = (handler.format(record) + handler.terminator).encode("utf-8")
        handler.acquire()
        try:
            handler.write_line(data)
        finally:
            handler.release()


@dataclass(frozen=True)
class LoggingContract:
    env_prefix: str
    app_logger_prefix: str
    app_name: str

    @property
    def env_log_level(self) -> str:
        return f"{self.env_prefix}_LOG_LEVEL"

    @property
    def env_third_party_level(self) -> str:
        return f"{self.env_prefix}_THIRD_PARTY_LOG_LEVEL"

    @property
    def env_third_party_loggers(self) -> str:
        return f"{self.env_prefix}_THIRD_PARTY_LOGGERS"

    @property
    def env_muted_loggers(self) -> str:
        return f"{self.env_prefix}_MUTED_LOGGERS"


def _resolve_log_root(app_name: str) -> Path:
    fs_app_name = _normalize_app_name(app_name)
    xdg_state_home = os.getenv("XDG_STATE_HOME")
    state_home = Path(xdg_state_home).expanduser() if xdg_state_home else Path("~/.local/state").expanduser()
    return state_home / "instrukt-ai" / fs_app_name


def _tighten(path: Path, mode: int) -> None:
    if stat.S_IMODE(os.stat(path).st_mode) != mode:
        os.chmod(path, mode)


def _ensure_log_dir(log_dir: Path) -> None:
    """Create `log_dir` and its missing ancestors `0700` whatever the umask, and
    set `log_dir` (the app's log directory) to `0700` if it existed otherwise."""
    missing: list[Path] = []
    for candidate in (log_dir, *log_dir.parents):
        if os.path.lexists(candidate):
            break
        missing.append(candidate)
    for directory in reversed(missing):
        try:
            os.mkdir(directory, _DIR_MODE)
        except FileExistsError:
            # Created concurrently by another process: its mode is its own.
            if not directory.is_dir():
                raise
            continue
        _tighten(directory, _DIR_MODE)
    _tighten(log_dir, _DIR_MODE)


_SOURCE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*")


def _check_source(source: str) -> None:
    """`source` is a bare file stem: `^[A-Za-z0-9][A-Za-z0-9._-]*$` without `..`.

    It can neither leave the log directory nor be hidden.
    """
    if not _SOURCE.fullmatch(source) or ".." in source:
        raise ValueError(
            f"log source {source!r} is not a bare file stem (A-Z, a-z, 0-9, then also '.', '_', '-'; no '..')"
        )


def _log_stem(fs_app_name: str, source: str | None) -> str:
    if source:
        _check_source(source)
        return source
    return fs_app_name


def resolve_log_file(app_name: str, *, source: str | None = None) -> Path:
    """Resolve the canonical log file path for `app_name`.

    `app_name` is normalized for filesystem layout, so callers may pass either:
    - distribution/service name (e.g. "crypto-ai")
    - Python module prefix (e.g. "crypto_ai")

    `source` is the logical identity of the producing process within the app.
    When omitted, the file is `<app_name>.log` (the canonical/default file).
    When provided (e.g. "cron", "docs-watch"), the file is `<source>.log`.
    The `.log` extension is always appended; callers pass the bare stem.
    A source that is not a bare stem (`^[A-Za-z0-9][A-Za-z0-9._-]*$`, no `..`)
    raises `ValueError`.
    """
    fs_app_name = _normalize_app_name(app_name)
    log_dir = _resolve_log_root(fs_app_name)
    return log_dir / f"{_log_stem(fs_app_name, source)}.log"


def resolve_log_files(app_name: str, *, stems: Iterable[str] | None = None) -> list[Path]:
    """Resolve all log files for `app_name`, optionally filtered by stem.

    Returns every `<something>.log[.suffix]` file in the app's log directory,
    including compressed rotation siblings (`.gz`). When `stems` is provided,
    only files whose stem (basename before the first `.log`) matches an entry
    are returned. Files are deduplicated and sorted by path.

    `stems` selects by exact stem, not substring — so `stems=["cron"]` matches
    `cron.log`, `cron.log.0`, `cron.log.1` and never `cron-backup.log`.
    """
    fs_app_name = _normalize_app_name(app_name)
    log_dir = _resolve_log_root(fs_app_name)
    if not log_dir.exists():
        return []

    if stems is None:
        candidates: list[Path] = list(log_dir.glob("*.log*"))
    else:
        candidates = []
        for stem in stems:
            candidates.extend(log_dir.glob(f"{stem}.log*"))

    seen: set[Path] = set()
    result: list[Path] = []
    for path in candidates:
        if path in seen:
            continue
        if not path.is_file():
            continue
        seen.add(path)
        result.append(path)
    return sorted(result)


def configure_logging(
    name: str,
    *,
    source: str | None = None,
    max_message_chars: int = 4000,
) -> Path:
    """Configure logging according to the InstruktAI contract.

    `source` is the logical identity of the producing process within the app.
    When omitted, the file is `<app>.log` (canonical/default). When provided
    (e.g. `source="cron"`), the file is `<source>.log`. The `.log` extension
    is always appended by the library; callers pass the bare stem. A source
    that is not a bare stem (`^[A-Za-z0-9][A-Za-z0-9._-]*$`, no `..`) raises
    `ValueError`.

    The log directory is created `0700` and the file `0600`; a symbolic link
    at the log path is refused.

    Returns the resolved log file path in use.
    """
    env_prefix = _normalize_env_prefix(name)
    app_logger_prefix = _normalize_logger_prefix(name)
    app_name = _normalize_app_name(name)
    log_filename = f"{_log_stem(app_name, source)}.log"

    contract = LoggingContract(
        env_prefix=env_prefix,
        app_logger_prefix=app_logger_prefix,
        app_name=app_name,
    )

    # Ensure all loggers accept arbitrary `**kv` (no wrapper at call sites).
    logging.setLoggerClass(InstruktAILogger)
    for obj in logging.root.manager.loggerDict.values():
        if isinstance(obj, logging.Logger) and not isinstance(obj, InstruktAILogger):
            obj.__class__ = InstruktAILogger

    unknown_levels: list[tuple[str, str, str]] = []
    levels: list[int] = []
    for variable, default, default_name in (
        (contract.env_log_level, logging.INFO, "INFO"),
        (contract.env_third_party_level, logging.WARNING, "WARNING"),
    ):
        raw = _getenv_utf8(variable)
        level, unknown = _parse_level(raw, default)
        if unknown and raw is not None:
            unknown_levels.append((variable, raw, default_name))
        levels.append(level)
    our_level, third_party_level = levels
    spotlight = _parse_csv(os.getenv(contract.env_third_party_loggers, ""))
    muted = _parse_csv(os.getenv(contract.env_muted_loggers, ""))

    # Root logger governs all loggers that don't explicitly set a level.
    root_level = third_party_level if not spotlight else logging.WARNING

    log_dir = _resolve_log_root(app_name)
    log_file = log_dir / log_filename

    formatter = LogfmtFormatter(max_message_chars=max_message_chars)
    selector = _ThirdPartySelectorFilter(app_logger_prefix=app_logger_prefix, spotlight_prefixes=tuple(spotlight))

    # Logging is an essential subsystem: an unwritable log dir/file raises here
    # and the host process must not start blind. No degraded/fallback mode.
    _ensure_log_dir(log_dir)
    file_handler = LogFileHandler(log_file)
    file_handler.setFormatter(formatter)
    try:
        _warn_unknown_levels(file_handler, unknown_levels)
    except BaseException:
        file_handler.close()
        raise
    handler: logging.Handler = file_handler

    handler.setLevel(logging.NOTSET)
    handler.addFilter(selector)

    # Configure root.
    logging.root.handlers = [handler]
    logging.root.setLevel(root_level)

    # Configure our logs.
    logging.getLogger(app_logger_prefix).setLevel(our_level)

    # Configure spotlight third-party prefixes (only affects non-app loggers).
    for prefix in spotlight:
        if prefix == app_logger_prefix or prefix.startswith(app_logger_prefix + "."):
            continue
        logging.getLogger(prefix).setLevel(third_party_level)

    # Muted loggers are forced to WARNING+ (applies to both app and third-party).
    for prefix in muted:
        logging.getLogger(prefix).setLevel(logging.WARNING)

    return log_file
