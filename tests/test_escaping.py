"""Python-only escaping: lone surrogates, which a Rust string cannot hold."""

from __future__ import annotations

import logging
import os
from collections.abc import Iterator
from pathlib import Path

import pytest
from instrukt_ai_logging import configure_logging, failure_count, get_logger
from instrukt_ai_logging.logging import _ESCAPED, _escape_quotes


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


def _lines(path: Path) -> list[str]:
    return [line.split(" ", 1)[1] for line in path.read_text(encoding="utf-8").splitlines()]


def test_a_lone_surrogate_is_written_escaped_instead_of_dropping_the_event(isolated_logging: None) -> None:
    # A file name with an undecodable byte, as os.listdir() or sys.argv hand it over.
    name = os.fsdecode(b"report-\xff.csv")
    assert name == "report-\udcff.csv"
    log_path = configure_logging("teleclaude")
    before = failure_count()

    get_logger("teleclaude.files").info("opened %s", name, path=name, high="\ud800", pair=chr(0xDBFF) + chr(0xDC00))
    get_logger(f"teleclaude.{name}").warning("logger name with a surrogate")

    assert _lines(log_path) == [
        'level=INFO logger=teleclaude.files msg="opened report-\\udcff.csv" '
        'high="\\ud800" pair="\\udbff\\udc00" path="report-\\udcff.csv"',
        'level=WARNING logger="teleclaude.report-\\udcff.csv" msg="logger name with a surrogate"',
    ]
    assert failure_count() == before


def test_every_surrogate_is_escaped() -> None:
    surrogates = [chr(c) for c in range(0xD800, 0xE000)]
    assert all(_ESCAPED.fullmatch(c) for c in surrogates)
    assert _escape_quotes("a\udcffb" + chr(0xD83D)) == "a\\udcffb\\ud83d"
    # Two lone surrogates that would form a pair are still two escapes.
    assert _escape_quotes(chr(0xDBFF) + chr(0xDC00)) == "\\udbff\\udc00"
    # A real astral character is one code point in Python, not a pair: it stays.
    assert _escape_quotes("\U0001f600") == "\U0001f600"


def test_escaping_is_per_character_whatever_the_mix() -> None:
    import random

    from instrukt_ai_logging.logging import _escape_match

    pool = list('ab\\"\n\r\x00\x1b\x1c\x7f\x85\u202e\u2066\udcff\u00a0é ')
    rng = random.Random(7)
    for _ in range(5000):
        text = "".join(rng.choice(pool) for _ in range(rng.randint(0, 40)))
        assert _escape_quotes(text) == _ESCAPED.sub(_escape_match, text), repr(text)
    # More distinct characters than the replace path takes.
    many = "".join(chr(c) for c in range(0x00, 0x20)) + '\\"'
    assert _escape_quotes(many) == _ESCAPED.sub(_escape_match, many)
