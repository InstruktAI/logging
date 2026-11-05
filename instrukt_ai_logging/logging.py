from __future__ import annotations

import gzip
import heapq
import logging
import os
import re
from collections.abc import Iterable, Iterator
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from logging.handlers import WatchedFileHandler
from pathlib import Path
from typing import Any, Literal, Protocol, cast, runtime_checkable

# Standard logging levels are: NOTSET=0, DEBUG=10, INFO=20, WARNING=30, ERROR=40, CRITICAL=50
TRACE: int = 5
logging.addLevelName(TRACE, "TRACE")
# Monkey-patch logging so our _level_name_to_int helper finds it automatically.
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


def _level_name_to_int(level_name: str, default: int) -> int:
    name = level_name.strip().upper()
    if not name:
        return default
    candidate: object = getattr(logging, name, None)
    if isinstance(candidate, int):
        return candidate
    return default


def _parse_csv(value: str) -> list[str]:
    return [item.strip() for item in value.split(",") if item.strip()]


def _now_utc() -> datetime:
    return datetime.now(tz=UTC)


class UtcMillisFormatter(logging.Formatter):
    def formatTime(self, record: logging.LogRecord, datefmt: str | None = None) -> str:
        dt = datetime.fromtimestamp(record.created, tz=UTC)
        if datefmt:
            return dt.strftime(datefmt)
        return dt.strftime("%Y-%m-%dT%H:%M:%S") + f".{int(record.msecs):03d}Z"


_SAFE_BARE_VALUE = re.compile(r"^[A-Za-z0-9._/:+-]+$")
_SAFE_KEY = re.compile(r"^[A-Za-z_][A-Za-z0-9_.-]*$")

# Keep this small and high-signal; expand only with strong justification.
_REDACTION_PATTERNS: list[tuple[re.Pattern[str], str]] = [
    # Telegram bot token in API URLs: https://api.telegram.org/bot<token>/...
    (re.compile(r"(api\.telegram\.org/bot)([^/\s]+)"), r"\1<REDACTED>"),
    # Generic Bearer token
    (re.compile(r"\bBearer\s+[^\s]+"), "Bearer <REDACTED>"),
    # Common OpenAI-style key prefix (avoid leaking)
    (re.compile(r"\bsk-[A-Za-z0-9]{10,}\b"), "sk-<REDACTED>"),
]


def _redact_text(text: str) -> str:
    redacted = text
    for pattern, replacement in _REDACTION_PATTERNS:
        redacted = pattern.sub(replacement, redacted)
    return redacted


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
      - If `spotlight_prefixes` is set: only allow records whose logger name starts with one of them.
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
        return any(name == p or name.startswith(p + ".") for p in self.spotlight_prefixes)


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


def _ensure_log_dir(log_dir: Path) -> None:
    log_dir.mkdir(parents=True, exist_ok=True)


def resolve_log_file(app_name: str, *, source: str | None = None) -> Path:
    """Resolve the canonical log file path for `app_name`.

    `app_name` is normalized for filesystem layout, so callers may pass either:
    - distribution/service name (e.g. "crypto-ai")
    - Python module prefix (e.g. "crypto_ai")

    `source` is the logical identity of the producing process within the app.
    When omitted, the file is `<app_name>.log` (the canonical/default file).
    When provided (e.g. "cron", "docs-watch"), the file is `<source>.log`.
    The `.log` extension is always appended; callers pass the bare stem.
    """
    fs_app_name = _normalize_app_name(app_name)
    log_dir = _resolve_log_root(fs_app_name)
    stem = source or fs_app_name
    return log_dir / f"{stem}.log"


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
    is always appended by the library; callers pass the bare stem.

    Returns the resolved log file path in use.
    """
    env_prefix = _normalize_env_prefix(name)
    app_logger_prefix = _normalize_logger_prefix(name)
    app_name = _normalize_app_name(name)
    log_filename = f"{source or app_name}.log"

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

    our_level_name = (os.getenv(contract.env_log_level) or "INFO").upper()
    third_party_level_name = (os.getenv(contract.env_third_party_level) or "WARNING").upper()
    spotlight = _parse_csv(os.getenv(contract.env_third_party_loggers, ""))
    muted = _parse_csv(os.getenv(contract.env_muted_loggers, ""))

    our_level = _level_name_to_int(our_level_name, logging.INFO)
    third_party_level = _level_name_to_int(third_party_level_name, logging.WARNING)

    # Root logger governs all loggers that don't explicitly set a level.
    root_level = third_party_level if not spotlight else logging.WARNING

    log_dir = _resolve_log_root(app_name)
    log_file = log_dir / log_filename

    formatter = LogfmtFormatter(max_message_chars=max_message_chars)
    selector = _ThirdPartySelectorFilter(app_logger_prefix=app_logger_prefix, spotlight_prefixes=tuple(spotlight))

    # Logging is an essential subsystem: an unwritable log dir/file raises here
    # and the host process must not start blind. No degraded/fallback mode.
    _ensure_log_dir(log_dir)
    handler: logging.Handler = WatchedFileHandler(log_file, encoding="utf-8")

    handler.setLevel(logging.NOTSET)
    handler.setFormatter(formatter)
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


def parse_since(value: str) -> timedelta:
    """Parse durations like '10m', '2h', '1d', '30s'."""
    raw = value.strip().lower()
    if not raw:
        raise ValueError("Empty duration")
    unit = raw[-1]
    number = raw[:-1]
    if not number.isdigit():
        raise ValueError(f"Invalid duration: {value}")
    n = int(number)
    if unit == "s":
        return timedelta(seconds=n)
    if unit == "m":
        return timedelta(minutes=n)
    if unit == "h":
        return timedelta(hours=n)
    if unit == "d":
        return timedelta(days=n)
    raise ValueError(f"Invalid duration unit: {unit}")


def parse_log_timestamp(line: str) -> datetime | None:
    """Parse the timestamp at the start of a standard log line.

    Expected: YYYY-MM-DDTHH:MM:SS.mmmZ ...
    """
    token = line.split(" ", 1)[0].strip()
    if not token.endswith("Z"):
        return None
    try:
        # Python doesn't accept 'Z' directly in fromisoformat.
        return datetime.fromisoformat(token.replace("Z", "+00:00"))
    except ValueError:
        return None


def iter_recent_log_lines(log_file: Path, since: timedelta) -> list[str]:
    """Return log lines newer than now-`since`, reading rotated siblings when present."""
    cutoff = _now_utc() - since

    candidates = sorted(
        [p for p in log_file.parent.glob(log_file.name + "*") if p.is_file() and not p.name.endswith(".gz")],
        key=lambda p: p.stat().st_mtime,
    )

    lines: list[str] = []
    for path in candidates:
        try:
            with path.open("r", encoding="utf-8", errors="replace") as f:
                for line in f:
                    ts = parse_log_timestamp(line)
                    if ts is None:
                        continue
                    if ts >= cutoff:
                        lines.append(line)
        except FileNotFoundError:
            continue

    return lines


# Initial backward-probe size for locating the start of the --since window in a
# large file. A typical multi-minute window fits in one or two probes; the probe
# doubles when the window is larger, so worst case is still O(window), not O(file).
_WINDOW_PROBE_BYTES = 262144  # 256 KiB


def _window_start_offset(path: Path, cutoff: datetime) -> int:
    """Return a byte offset at or before the first line newer than `cutoff`.

    A log file is appended in timestamp order, so the window's start can be found
    by probing backward from EOF in exponentially growing chunks instead of
    scanning from byte 0: once a probe's earliest timestamped line is already
    older than the cutoff, the window begins within that probe and everything
    before it is skippable. Returns 0 when the window spans the whole file (or the
    file is smaller than a single probe), which reproduces a full forward scan.
    """
    try:
        size = path.stat().st_size
    except OSError:
        return 0
    if size <= _WINDOW_PROBE_BYTES:
        return 0

    probe = _WINDOW_PROBE_BYTES
    with path.open("rb") as fb:
        while probe < size:
            start = size - probe
            fb.seek(start)
            fb.readline()  # discard the partial line spanning the probe boundary
            first_ts: datetime | None = None
            for raw in fb:
                first_ts = parse_log_timestamp(raw.decode("utf-8", errors="replace"))
                if first_ts is not None:
                    break
            if first_ts is not None and first_ts < cutoff:
                return start
            probe *= 2
    return 0


_GZIP_SUFFIX = ".gz"
_BINARY_READ_MODE: Literal["rb"] = "rb"
_TEXT_DECODE_ENCODING = "utf-8"
_TEXT_DECODE_ERRORS = "replace"


def _tail_timestamp(path: Path) -> datetime | None:
    """Return the newest parsable log timestamp near the tail of `path`, or `None`.

    A log file is appended in timestamp order, so its newest entry is always at
    the tail: probe backward in exponentially growing chunks (the same
    `_WINDOW_PROBE_BYTES` doubling idiom `_window_start_offset` uses) until the
    probed span holds at least one parsable timestamp, then return the last one
    seen while scanning that span forward to EOF — ascending order makes that the
    maximum. Falls back to a full scan only when no probe finds a timestamp at
    all. Gzip archives have no seek and are streamed from the start; the caller's
    mtime bound already governs whether this is called on one.

    Only a vanished file (`FileNotFoundError`, the same race every sibling probe
    in this module already tolerates) is treated as absence. A file that exists
    but cannot be read or decompressed — permission denied, a truncated or
    corrupt archive — propagates its exception rather than being reported as
    holding no timestamp.
    """
    if path.name.endswith(_GZIP_SUFFIX):
        newest: datetime | None = None
        try:
            with gzip.open(path, _BINARY_READ_MODE) as fb:
                for raw in fb:
                    ts = parse_log_timestamp(raw.decode(_TEXT_DECODE_ENCODING, errors=_TEXT_DECODE_ERRORS))
                    if ts is not None:
                        newest = ts
        except FileNotFoundError:
            return None
        return newest

    try:
        size = path.stat().st_size
    except FileNotFoundError:
        return None

    probe = _WINDOW_PROBE_BYTES
    with path.open(_BINARY_READ_MODE) as fb:
        while True:
            start = max(0, size - probe)
            fb.seek(start)
            if start:
                fb.readline()  # discard the partial line spanning the probe boundary
            newest = None
            for raw in fb:
                ts = parse_log_timestamp(raw.decode(_TEXT_DECODE_ENCODING, errors=_TEXT_DECODE_ERRORS))
                if ts is not None:
                    newest = ts
            if newest is not None or start == 0:
                return newest
            probe *= 2


def newest_log_timestamp(files: Iterable[Path]) -> datetime | None:
    """Return the newest parsable log timestamp across `files`, or `None`.

    Examines files in descending mtime order. A file's entries never postdate
    its own mtime — the bound `iter_recent_log_lines_merged` already relies on
    to skip whole files — so once a remaining candidate's mtime can no longer
    beat the best timestamp found so far, no later candidate can either and the
    scan stops. A single file's mtime is not enough on its own to answer this:
    the root holds independent per-stem streams and their rotations, so the
    newest entry can live in a file that was not the most recently touched.
    """
    candidates: list[tuple[float, Path]] = []
    for path in files:
        try:
            mtime = path.stat().st_mtime
        except FileNotFoundError:
            continue
        candidates.append((mtime, path))
    candidates.sort(key=lambda item: item[0], reverse=True)

    best: datetime | None = None
    for mtime, path in candidates:
        if best is not None and datetime.fromtimestamp(mtime, tz=UTC) <= best:
            break
        ts = _tail_timestamp(path)
        if ts is not None and (best is None or ts > best):
            best = ts
    return best


def iter_recent_log_lines_merged(files: Iterable[Path], since: timedelta) -> Iterator[str]:
    """Yield lines newer than now-`since` merged across multiple files by timestamp.

    Each file is read independently; lines without a parsable leading timestamp
    inherit the previous parsed timestamp from their own file (so multi-line
    entries stay adjacent to their parent). The resulting per-file streams are
    merged via a k-way heap merge, producing a single chronologically-ordered
    output stream.

    Files whose mtime falls before the cutoff are skipped entirely — no line
    inside them can be newer than the file itself, so reading them only burns
    I/O. This matters for log directories with large rotation history or
    sibling streams (git-wrapper.log, etc.) the caller didn't explicitly
    request via `--logs`.
    """
    cutoff = _now_utc() - since

    def _file_stream(path: Path) -> Iterator[tuple[datetime, str]]:
        is_gzip = path.name.endswith(".gz")
        try:
            fb = gzip.open(path, "rb") if is_gzip else path.open("rb")
        except FileNotFoundError:
            return
        try:
            # Gzip streams cannot use the raw byte probe; stream eligible archives from member start.
            offset = 0 if is_gzip else _window_start_offset(path, cutoff)
            if offset:
                fb.seek(offset)
                fb.readline()  # discard the partial line at the seek boundary
            last_ts: datetime | None = None
            for raw in fb:
                line = raw.decode("utf-8", errors="replace")
                ts = parse_log_timestamp(line)
                if ts is None:
                    if last_ts is None or last_ts < cutoff:
                        continue
                    yield (last_ts, line)
                    continue
                last_ts = ts
                if ts < cutoff:
                    continue
                yield (ts, line)
        finally:
            fb.close()

    eligible: list[Path] = []
    for path in files:
        try:
            mtime = datetime.fromtimestamp(path.stat().st_mtime, tz=UTC)
        except OSError:
            continue
        if mtime < cutoff:
            continue
        eligible.append(path)

    streams = [_file_stream(p) for p in eligible]
    for _, line in heapq.merge(*streams, key=lambda item: item[0]):
        yield line
