from __future__ import annotations

import errno
import logging
import os
import re
import stat
import sys
import threading
import time
from collections.abc import Callable, Iterable
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


# Escaped inside quotes (docs/contract.md, Values): `\\`, `"`, LF and CR with
# their short escapes; every other C0 control, DEL, C1 and the bidi controls as
# `\u` and four lowercase hex digits, so no value can drive the terminal a log
# is read in, or reorder what it shows. A lone surrogate (an undecodable byte
# carried by surrogateescape, as in a file name) is escaped the same way, so
# the line can always be encoded as UTF-8 and the event is never lost.
_SHORT_ESCAPES = {"\\": "\\\\", '"': '\\"', "\n": "\\n", "\r": "\\r"}
_TERMINAL_CONTROLS = "\\x00-\\x1f\\x7f-\\x9f\\u061c\\u200e\\u200f\\u202a-\\u202e\\u2066-\\u2069"
_SURROGATES = "\\ud800-\\udfff"
_ESCAPED = re.compile(f'[\\\\"{_TERMINAL_CONTROLS}{_SURROGATES}]')


def _escape_match(match: re.Match[str]) -> str:
    char = match.group()
    short = _SHORT_ESCAPES.get(char)
    return short if short is not None else f"\\u{ord(char):04x}"


_REPLACE_DISTINCT_MAX = 8


def _escape_quotes(value: str) -> str:
    # Every character escaped as `\uXXXX` is in Unicode category Cc, Cf or Cs,
    # so none is printable: printable text needs only `\\` and `"` (LF and CR are
    # not printable either).
    if value.isprintable():
        return value.replace("\\", "\\\\").replace('"', '\\"')
    # Text with controls usually holds a few distinct ones, many times over:
    # one `str.replace` per distinct character beats a Python callback per
    # occurrence. The backslash goes first, as every escape adds one.
    found = set(_ESCAPED.findall(value))
    if len(found) > _REPLACE_DISTINCT_MAX:
        return _ESCAPED.sub(_escape_match, value)
    if "\\" in found:
        value = value.replace("\\", "\\\\")
        found.discard("\\")
    for char in found:
        short = _SHORT_ESCAPES.get(char)
        value = value.replace(char, short if short is not None else f"\\u{ord(char):04x}")
    return value


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


def _process_text(text: str, max_chars: int) -> str:
    """The string rule's first two steps: redact, then cut at `max_chars`."""
    return _truncate_text(_redact_text(text), max_chars=max_chars)


def _render_text(text: str, *, force_quote: bool = False) -> str:
    """Write an already processed string bare, or quoted and escaped."""
    if not force_quote and _SAFE_BARE_VALUE.fullmatch(text):
        return text
    return f'"{_escape_quotes(text)}"'


# The line limit (docs/contract.md, Line limit): a formatted line, without its
# newline, is at most this many bytes of UTF-8.
_MAX_LINE_BYTES = 16 * 1024
_DROPPED_MARKER = " fields_dropped="
_TRUNCATION_SUFFIX = "…(truncated)"
# A cut value: its quotes and the suffix, around the kept prefix.
_CUT_BYTES = 2 + len(_TRUNCATION_SUFFIX.encode("utf-8"))


def _utf8_len(text: str) -> int:
    # str.isascii() is O(1) in CPython; only non-ASCII text needs encoding.
    return len(text) if text.isascii() else len(text.encode("utf-8"))


def _cut_quoted(text: str, avail: int) -> str:
    """`text` quoted, cut to the longest character prefix whose escaped form takes
    at most `avail` bytes, then marked with the truncation suffix.

    An escape is never split: each character escapes on its own, so the escaped
    length grows with the prefix and a binary search finds the cut.
    """
    lo, hi = 0, len(text)
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if _utf8_len(_escape_quotes(text[:mid])) <= avail:
            lo = mid
        else:
            hi = mid - 1
    return f'"{_escape_quotes(text[:lo])}{_TRUNCATION_SUFFIX}"'


def _fit_line(ts: str, level: str, logger: str, msg: str, head: str, fields: list[str]) -> str:
    """The line limit, for a line that does not fit whole.

    - When the head fits together with the marker, the fields are appended in
      order while they fit, leaving room for the marker, and
      ` fields_dropped=<n>` says how many were left out.
    - Otherwise every field is left out, and the head is cut so that it fits
      together with the marker (no marker without fields): the message, on a
      character boundary with the truncation suffix; should even an empty
      message not do, then the logger as well, then the level.

    `level`, `logger` and `msg` are processed (redacted, truncated) texts.
    """
    count = len(fields)
    marker = len(_DROPPED_MARKER) + len(str(count)) if count else 0
    used = _utf8_len(head)
    if count and used + marker <= _MAX_LINE_BYTES:
        out = [head]
        kept = 0
        for field in fields:
            size = _utf8_len(field)
            if used + size + marker > _MAX_LINE_BYTES:
                break
            out.append(field)
            used += size
            kept += 1
        out.append(f"{_DROPPED_MARKER}{count - kept}")
        return "".join(out)

    budget = _MAX_LINE_BYTES - marker
    level_r = _render_text(level)
    logger_r = _render_text(logger)
    lv, lg, m = len(" level="), len(" logger="), len(" msg=")
    avail = budget - (len(ts) + lv + _utf8_len(level_r) + lg + _utf8_len(logger_r) + m + _CUT_BYTES)
    if avail >= 0:
        line = f"{ts} level={level_r} logger={logger_r} msg={_cut_quoted(msg, avail)}"
    else:
        empty = _cut_quoted("", 0)
        avail = budget - (len(ts) + lv + _utf8_len(level_r) + lg + _CUT_BYTES + m + _CUT_BYTES)
        if avail >= 0:
            line = f"{ts} level={level_r} logger={_cut_quoted(logger, avail)} msg={empty}"
        else:
            avail = max(0, budget - (len(ts) + lv + _CUT_BYTES + lg + _CUT_BYTES + m + _CUT_BYTES))
            line = f"{ts} level={_cut_quoted(level, avail)} logger={empty} msg={empty}"
    if count:
        line += f"{_DROPPED_MARKER}{count}"
    return line


class LogfmtFormatter(UtcMillisFormatter):
    """Single-line logfmt-ish formatter.

    Always emits key/value pairs with at least:
      level=..., logger=..., msg="..."

    Optional additional pairs can be attached via `extra={"kv": {...}}`.
    A line is at most 16 KiB of UTF-8 (see `_fit_line`).
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

        max_chars = self.max_message_chars
        level = _process_text(str(record.levelname), max_chars)
        logger = _process_text(str(record.name), max_chars)
        msg = _process_text(str(message), max_chars)
        head = (
            f"{ts} level={_render_text(level)} logger={_render_text(logger)} msg={_render_text(msg, force_quote=True)}"
        )

        fields: list[str] = []
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
                fields.append(f" {key}={_format_logfmt_value(kv[key], max_chars=max_chars)}")

        if record.exc_info:
            try:
                exc_text = self.formatException(record.exc_info).replace("\n", "\\n")
            except Exception:
                exc_text = "<exception>"
            fields.append(f" exc={_format_logfmt_value(exc_text, max_chars=max_chars)}")

        line = head + "".join(fields)
        size = len(line)
        # At most 4 bytes per character: a short line fits without measuring.
        if size <= _MAX_LINE_BYTES // 4 or (
            size <= _MAX_LINE_BYTES and (line.isascii() or len(line.encode("utf-8")) <= _MAX_LINE_BYTES)
        ):
            return line
        return _fit_line(ts, level, logger, msg, head, fields)


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


def _under(name: str, prefix: str) -> bool:
    """`name` is `prefix` or lies under it (separator `.`)."""
    return name == prefix or name.startswith(prefix + ".")


class _Selection:
    """The resolved selection policy (docs/contract.md, Selection).

    A logger's threshold comes from the longest matching prefix that has a level
    of its own (a muted prefix wins a tie, a `NOTSET` prefix is skipped), else
    from the root threshold. This is what the logger levels `configure_logging`
    sets express, computed from the prefixes alone, so that no level set
    elsewhere can make a logger louder than the policy.
    """

    def __init__(
        self,
        app_prefix: str,
        app_level: int,
        third_party_level: int,
        spotlight: list[str],
        muted: list[str],
    ) -> None:
        rules: list[tuple[str, int, bool]] = [(app_prefix, app_level, False)]
        rules.extend((p, third_party_level, False) for p in spotlight if not _under(p, app_prefix))
        rules.extend((p, logging.WARNING, True) for p in muted)
        self.rules = tuple(rule for rule in rules if rule[1] != logging.NOTSET)
        self.root = third_party_level if not spotlight else logging.WARNING

    def threshold(self, name: str) -> int:
        best: tuple[str, int, bool] | None = None
        for rule in self.rules:
            prefix, _, muted = rule
            if not _under(name, prefix):
                continue
            if best is None or len(prefix) > len(best[0]) or (len(prefix) == len(best[0]) and muted and not best[2]):
                best = rule
        return self.root if best is None else best[1]


_THRESHOLD_CACHE_MAX = 4096


class _SelectionFilter(logging.Filter):
    """Enforce the selection policy on the handler, whatever the logger levels.

    Python's propagation hands a record to the root handler without checking
    ancestor levels, so a library that sets its own child logger to DEBUG
    would get DEBUG past a WARNING third-party threshold. Here every record
    must meet the policy's threshold for its logger: a logger's own level can
    make it quieter, never louder. Thresholds are cached per logger name.
    """

    def __init__(self, selection: _Selection) -> None:
        super().__init__()
        self._selection = selection
        self._thresholds: dict[str, int] = {}

    def filter(self, record: logging.LogRecord) -> bool:
        name = record.name
        threshold = self._thresholds.get(name)
        if threshold is None:
            threshold = self._selection.threshold(name)
            if len(self._thresholds) >= _THRESHOLD_CACHE_MAX:
                self._thresholds.clear()
            self._thresholds[name] = threshold
        return record.levelno >= threshold


_SELF_LOGGER = "instrukt_ai_logging"

# The loggers whose level the last `configure_logging` call set.
_CONFIGURED_LOGGERS: set[str] = set()
_UNKNOWN_LEVEL_MSG = "unknown log level; using the default"


_REPORT_INTERVAL_S = 60.0


class _FailureReporter:
    """Count dropped lines and report them on stderr, at most once a minute.

    A report names the exception type, errno, the log path and the counts. It
    never holds the record's message, its args or its fields, so a secret that
    redaction would have hidden in the file cannot reach stderr instead.
    """

    def __init__(self, clock: Callable[[], float] = time.monotonic) -> None:
        self._clock = clock
        self._lock = threading.Lock()
        self._last: float | None = None
        self._suppressed = 0
        self.failures = 0

    def admit(self) -> tuple[int, int] | None:
        """Count one failure; `(failures, suppressed)` when a report is due."""
        with self._lock:
            self.failures += 1
            now = self._clock()
            if self._last is not None and now - self._last < _REPORT_INTERVAL_S:
                self._suppressed += 1
                return None
            self._last = now
            suppressed, self._suppressed = self._suppressed, 0
            return self.failures, suppressed

    def report(self, path: str, exc: BaseException | None) -> None:
        """Count the failure and, when one is due, write the report. Never raises.

        Only builtin values are rendered: the exception type's `__qualname__`,
        an `errno` that is exactly an `int` (via `int.__repr__`), `path` and the
        counts. Nothing from the record or the exception is passed to `str()`
        or `repr()`, so neither a failing nor a secret-printing `__str__` can
        reach stderr or raise from here.
        """
        try:
            admitted = self.admit()
            if admitted is None:
                return
            failures, suppressed = admitted
            shown_path = path if type(path) is str else "<unknown path>"
            line = (
                f"instrukt-ai-logging: dropped a log line: cannot write {shown_path}: "
                f"{_exception_kind(exc)} {_errno_text(exc)}; "
                f"{failures} dropped since start, {suppressed} reports suppressed\n"
            )
            stream = sys.stderr
            if stream is not None:
                stream.write(line)
                stream.flush()
        except Exception:
            pass  # stderr is broken too: the count is all that is left.


def _exception_kind(exc: BaseException | None) -> str:
    """The exception type's `__qualname__`, only when it is exactly a `str`."""
    if exc is None:
        return "unknown"
    try:
        name: object = type(exc).__qualname__
    except Exception:
        return "unknown"
    return name if type(name) is str else "unknown"


def _errno_text(exc: BaseException | None) -> str:
    """`errno=<n> (<strerror>)` for an `errno` that is exactly an `int`, else `errno=none`."""
    try:
        code: object = getattr(exc, "errno", None)
    except Exception:
        return "errno=none"  # an `errno` property that raises
    if type(code) is not int:
        return "errno=none"  # also an `int` subclass, whose `__str__` is its own
    number = int.__repr__(code)
    try:
        return f"errno={number} ({os.strerror(code)})"
    except (OverflowError, ValueError):
        return f"errno={number} (unknown)"


_REPORTER = _FailureReporter()


def failure_count() -> int:
    """How many log lines this process dropped because they could not be written.

    Counts every runtime failure since the process started, across every
    `configure_logging` call. Initialization failures are not counted: they
    raise from `configure_logging`.
    """
    return _REPORTER.failures


class ShortWriteError(OSError):
    """`os.write` wrote only part of a line; the rest is never written."""


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

    A line that cannot be written is dropped and reported (see
    `failure_count`); the logging call never raises.

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
        self._closed = False
        # Set while this handler reports a failure. Records are emitted under
        # the handler lock, so only the reporting thread can see it set.
        self._reporting = False
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
        if self._closed:
            raise ValueError("the log handler is closed")
        self._reopen_if_needed()
        # One write(2) on the O_APPEND descriptor: lines from several processes
        # appending to one file never interleave. A short write is an error and
        # is never followed by a second write, whose bytes would land after
        # another process's line. (os.write retries EINTR itself, PEP 475.)
        written = os.write(self._fd, data)
        if written != len(data):
            raise ShortWriteError(f"short write: {written} of {len(data)} bytes")

    def emit(self, record: logging.LogRecord) -> None:
        if self._reporting:
            # Logged by the failure report itself (stderr routed into
            # logging): never written through the handler that is failing.
            return
        # Every failure, the reopen included, is reported and the line is
        # dropped: logging never raises into the caller.
        try:
            self.write_line((self.format(record) + self.terminator).encode("utf-8"))
        except Exception:
            self.handleError(record)

    def handleError(self, record: logging.LogRecord) -> None:
        """Report the failure without the record.

        `logging.Handler.handleError` prints the record's message and args to
        stderr, unredacted; this prints only the exception type, errno, the
        log path and the counts, at most once a minute. It never raises.
        """
        self._reporting = True
        try:
            _REPORTER.report(self._path, sys.exc_info()[1])
        finally:
            self._reporting = False

    def close(self) -> None:
        self.acquire()
        try:
            self._closed = True
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
    selection = _Selection(app_logger_prefix, our_level, third_party_level, spotlight, muted)

    # Logging is an essential subsystem: an unwritable log dir/file raises here
    # and the host process must not start blind. No degraded/fallback mode. A
    # failure leaves the previous configuration as it was.
    _ensure_log_dir(log_dir)
    handler = LogFileHandler(log_file)
    handler.setFormatter(formatter)
    try:
        _warn_unknown_levels(handler, unknown_levels)
    except BaseException:
        handler.close()
        raise

    handler.setLevel(logging.NOTSET)
    handler.addFilter(_SelectionFilter(selection))

    # Start clean: undo the levels an earlier call set, so a prefix that was
    # muted or spotlighted then is not still held now.
    for configured in _CONFIGURED_LOGGERS:
        logging.getLogger(configured).setLevel(logging.NOTSET)
    _CONFIGURED_LOGGERS.clear()

    def set_level(logger_name: str, level: int) -> None:
        logging.getLogger(logger_name).setLevel(level)
        _CONFIGURED_LOGGERS.add(logger_name)

    # Configure root.
    previous = logging.root.handlers
    logging.root.handlers = [handler]
    logging.root.setLevel(root_level)
    for old in previous:
        if isinstance(old, LogFileHandler):
            old.close()

    # The logger levels let records the policy passes be created at all (the
    # app at DEBUG below a WARNING root); the handler filter enforces the
    # policy itself.
    set_level(app_logger_prefix, our_level)

    # Configure spotlight third-party prefixes (only affects non-app loggers).
    for prefix in spotlight:
        if _under(prefix, app_logger_prefix):
            continue
        set_level(prefix, third_party_level)

    # Muted loggers are forced to WARNING+ (applies to both app and third-party).
    for prefix in muted:
        set_level(prefix, logging.WARNING)

    return log_file
