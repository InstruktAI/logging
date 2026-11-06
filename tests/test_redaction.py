"""Redaction boundaries and whitespace classes (docs/contract.md, Redaction).

The conformance cases pin these through the formatter; these pin the edges of
the patterns themselves, mirrored by the Rust writer's unit tests.
"""

import pytest
from instrukt_ai_logging.logging import _redact_text  # pyright: ignore[reportPrivateUsage]


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        # A combining mark is not a redaction word character.
        ("\u0301Bearer abcdefghij", "\u0301Bearer <REDACTED>"),
        ("\u0301sk-abcdefghij1234", "\u0301sk-<REDACTED>"),
        ("sk-abcdefghij1234\u0301", "sk-<REDACTED>\u0301"),
        # Nor is a non-ASCII letter or digit.
        ("\u00e9Bearer abcdefghij", "\u00e9Bearer <REDACTED>"),
        ("\u0663sk-abcdefghij1234", "\u0663sk-<REDACTED>"),
        ("sk-abcdefghij1234\u00e9", "sk-<REDACTED>\u00e9"),
        # ASCII letters, digits and underscore are.
        ("xBearer abcdefghij", "xBearer abcdefghij"),
        ("9sk-abcdefghij1234", "9sk-abcdefghij1234"),
        ("_sk-abcdefghij1234", "_sk-abcdefghij1234"),
        ("sk-abcdefghij1234_", "sk-abcdefghij1234_"),
        # ASCII punctuation is a boundary.
        ("(Bearer abcdefghij)", "(Bearer <REDACTED>"),
        ("=sk-abcdefghij1234.", "=sk-<REDACTED>."),
    ],
)
def test_word_boundary_is_ascii(text: str, expected: str) -> None:
    assert _redact_text(text) == expected


def test_rejected_start_does_not_hide_a_later_secret() -> None:
    assert _redact_text("xBearer abc Bearer def") == "xBearer abc Bearer <REDACTED>"
    assert _redact_text("xsk-abcdefghij1234 sk-abcdefghij1234") == "xsk-abcdefghij1234 sk-<REDACTED>"
    assert _redact_text("sk-abcdefghij1234_ sk-abcdefghij1234") == "sk-abcdefghij1234_ sk-<REDACTED>"


def test_adjacent_keys_are_both_redacted() -> None:
    assert _redact_text("sk-abcdefghij1234.sk-abcdefghij5678") == "sk-<REDACTED>.sk-<REDACTED>"


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        # Any Unicode White_Space separates Bearer from its token, as do U+001C-U+001F.
        ("Bearer\u00a0abcdefghij", "Bearer <REDACTED>"),
        ("Bearer\u3000abcdefghij", "Bearer <REDACTED>"),
        ("Bearer\u001cabcdefghij", "Bearer <REDACTED>"),
        ("Bearer\u001fabcdefghij", "Bearer <REDACTED>"),
        # A token ends only at Unicode White_Space; U+001C-U+001F stay inside it.
        ("Bearer abc\u001fdef ghi", "Bearer <REDACTED> ghi"),
        ("Bearer abc\u2028def", "Bearer <REDACTED>\u2028def"),
        ("api.telegram.org/bot12:AB\u001cCD/send", "api.telegram.org/bot<REDACTED>/send"),
        ("api.telegram.org/bot12:AB\u00a0CD", "api.telegram.org/bot<REDACTED>\u00a0CD"),
        # Bearer with no token is left alone.
        ("Bearer\u001f", "Bearer\u001f"),
    ],
)
def test_whitespace_classes(text: str, expected: str) -> None:
    assert _redact_text(text) == expected
