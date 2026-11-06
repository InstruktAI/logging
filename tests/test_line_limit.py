"""The 16384-byte line limit: Python-only paths (exc, custom level names) and the bound itself."""

from __future__ import annotations

import logging

import pytest
from instrukt_ai_logging.logging import _MAX_LINE_BYTES, LogfmtFormatter

_CUT = '"…(truncated)"'


def _record(name: str = "app", msg: str = "m", level: int = logging.INFO, **kv: object) -> logging.LogRecord:
    record = logging.LogRecord(name, level, __file__, 1, msg, None, None)
    record.kv = kv
    return record


def _format(record: logging.LogRecord, max_chars: int = 0) -> str:
    return LogfmtFormatter(max_message_chars=max_chars).format(record)


@pytest.mark.parametrize(
    "record",
    [
        _record(v="x" * 40_000),
        _record(msg="é" * 20_000, v="v"),
        _record(msg="\x1b" * 5_000),
        _record(name="a" * 20_000),
        _record(msg="\udcff" * 5_000),
    ],
)
def test_every_line_is_at_most_the_limit(record: logging.LogRecord) -> None:
    assert len(_format(record).encode("utf-8")) <= _MAX_LINE_BYTES


def test_a_line_that_fits_is_whole_and_has_no_marker() -> None:
    line = _format(_record(a="x", b=2))
    assert line.endswith(' level=INFO logger=app msg="m" a=x b=2')


def test_the_exc_field_counts_as_the_last_field() -> None:
    try:
        raise ValueError("boom")
    except ValueError:
        import sys

        record = _record(a="x" * 16_300)
        record.exc_info = sys.exc_info()
    line = _format(record)
    assert line.endswith(" a=" + "x" * 16_300 + " fields_dropped=1")
    assert " exc=" not in line


def test_a_level_too_long_for_even_empty_logger_and_message_is_cut_too() -> None:
    level = 7
    logging.addLevelName(level, "L" * 20_000)
    try:
        line = _format(_record(name="g" * 20_000, level=level))
    finally:
        logging.addLevelName(level, "Level 7")
    assert ' level="LLL' in line
    assert line.endswith(f'…(truncated)" logger={_CUT} msg={_CUT}')
    assert len(line.encode("utf-8")) == _MAX_LINE_BYTES
