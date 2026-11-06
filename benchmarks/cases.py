"""The Python writer benchmark cases, mirroring the Rust ones in
`crates/instrukt-ai-logging/benches/cases/mod.rs`.

Each case is one log call through `configure_logging` and
`logging.getLogger(...)`. Only that public surface is used, so the same file
runs unchanged against any release with it (0.8.0 on), which is how the
baseline compares releases.

Sink: every case writes to a real log file in the temporary state tree, with
the per-record rotation check, as in production. The writer refuses a symlink
at the log path, so a `/dev/null` link (the sink before 0.10) is no longer
possible. The `file` case is the `app_line` call under its historical name,
kept so its history continues.
"""

from __future__ import annotations

import logging
from collections.abc import Callable, Mapping
from dataclasses import dataclass, field
from typing import Protocol, cast

APP = "bench-app"
"""App name: env prefix `BENCH_APP`, app logger prefix `bench_app`."""

APP_LOGGER = "bench_app.http"
THIRD_PARTY_LOGGER = "hyper.client.pool"


class _KvLogger(Protocol):
    """The kv-accepting logger `configure_logging` installs."""

    def debug(self, msg: object, *args: object, **kwargs: object) -> None: ...

    def info(self, msg: object, *args: object, **kwargs: object) -> None: ...


def _logger(name: str) -> _KvLogger:
    return cast("_KvLogger", logging.getLogger(name))


def _repeat_to(unit: str, length: int) -> str:
    return (unit * (length // len(unit) + 1))[:length]


def clean_200() -> str:
    """About 200 characters of ordinary text with no secret in it."""
    return _repeat_to("GET /api/v1/users/42/sessions?page=2&limit=50 status=200 took=12ms skip=task-runner ", 200)


def bearer_200() -> str:
    """About 200 characters with one Bearer token in the middle."""
    token = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhbGljZSJ9.c2lnbmF0dXJlLXZhbHVl"
    head = _repeat_to("POST /api/v1/sessions retry=1 ", 60)
    tail = _repeat_to(" upstream=auth-gateway region=eu-west-1 ", 200 - 60 - 22 - len(token))
    return f"{head}Authorization: Bearer {token}{tail}"


def long_4100() -> str:
    """4100 characters: over the default 4000-character limit, so it is cut."""
    return _repeat_to("lorem ipsum dolor sit amet, consectetur adipiscing elit ", 4100)


_MB = 1 << 20


def rejected_starts_1mb() -> str:
    """About 1 MiB of Bearer starts that the boundary rule rejects (`xBearer`)."""
    unit = "xBearer\u001c"
    return unit * (_MB // len(unit)) + "a"


def chain_1mb() -> str:
    """About 1 MiB of Bearers, each one the token of the one before."""
    unit = "Bearer\u001c"
    return " " + unit * (_MB // len(unit)) + "a"


@dataclass(frozen=True)
class Case:
    """One benchmark: its environment and the call it times."""

    name: str
    make: Callable[[], Callable[[], None]]
    """Called after `configure_logging`; returns the timed call."""
    env: Mapping[str, str] = field(default_factory=dict[str, str])
    slow: bool = False
    """A 1 MiB input: kept out of the per-PR CodSpeed run."""


def _disabled() -> Callable[[], None]:
    log = _logger(APP_LOGGER)
    return lambda: log.debug("request handled", user="alice", attempt=3, cached=True)


def _line(name: str) -> Callable[[], Callable[[], None]]:
    def make() -> Callable[[], None]:
        log = _logger(name)
        return lambda: log.info("request handled", user="alice", attempt=3, cached=True)

    return make


def _payload(value: Callable[[], str]) -> Callable[[], Callable[[], None]]:
    def make() -> Callable[[], None]:
        log = _logger(APP_LOGGER)
        payload = value()
        return lambda: log.info("request handled", payload=payload)

    return make


CASES: tuple[Case, ...] = (
    Case("disabled", _disabled),
    Case("app_line", _line(APP_LOGGER)),
    Case(
        "third_party",
        _line(THIRD_PARTY_LOGGER),
        env={"BENCH_APP_THIRD_PARTY_LOGGERS": "hyper", "BENCH_APP_THIRD_PARTY_LOG_LEVEL": "INFO"},
    ),
    Case("file", _line(APP_LOGGER)),
    Case("redaction_clean_200", _payload(clean_200)),
    Case("redaction_bearer_200", _payload(bearer_200)),
    Case("redaction_truncated_4100", _payload(long_4100)),
    Case("redaction_adversarial_rejected_starts_1mb", _payload(rejected_starts_1mb), slow=True),
    Case("redaction_adversarial_chain_1mb", _payload(chain_1mb), slow=True),
)

ENV_KEYS = (
    "BENCH_APP_LOG_LEVEL",
    "BENCH_APP_THIRD_PARTY_LOG_LEVEL",
    "BENCH_APP_THIRD_PARTY_LOGGERS",
    "BENCH_APP_MUTED_LOGGERS",
)
"""Every contract variable for `APP`; a case's `env` sets a subset."""


def install(case: Case) -> Callable[[], None]:
    """Configure the writer for `case` and return its timed call.

    The caller has already pointed `HOME`, `XDG_STATE_HOME` and the case's
    variables (`ENV_KEYS`) at isolated values in `os.environ`.
    """
    # Imported here so a caller can choose which release's package is found.
    from instrukt_ai_logging import configure_logging

    configure_logging(APP)
    return case.make()


def uninstall() -> None:
    """Close the handler and clear the levels `install` set."""
    for handler in logging.root.handlers:
        handler.close()
    logging.root.handlers = []
    logging.root.setLevel(logging.WARNING)
    for name in ("bench_app", "hyper"):
        logging.getLogger(name).setLevel(logging.NOTSET)
