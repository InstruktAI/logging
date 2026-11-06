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
        # The separator stays as it was.
        ("Bearer\u00a0abcdefghij", "Bearer\u00a0<REDACTED>"),
        ("Bearer\u3000abcdefghij", "Bearer\u3000<REDACTED>"),
        ("Bearer\u001cabcdefghij", "Bearer\u001c<REDACTED>"),
        ("Bearer\u001fabcdefghij", "Bearer\u001f<REDACTED>"),
        ("Bearer  \t abcdefghij", "Bearer  \t <REDACTED>"),
        # A token ends only at Unicode White_Space; U+001C-U+001F stay inside it.
        ("Bearer abc\u001fdef ghi", "Bearer <REDACTED> ghi"),
        ("Bearer abc\u2028def", "Bearer <REDACTED>\u2028def"),
        ("api.telegram.org/bot12:AB\u001cCD/send", "api.telegram.org/bot<REDACTED>/send"),
        ("api.telegram.org/bot12:AB\u00a0CD", "api.telegram.org/bot<REDACTED>\u00a0CD"),
        # Bearer with no token is left alone, also when the separator run reaches the end.
        ("Bearer\u001f", "Bearer\u001f"),
        ("Bearer \u001c", "Bearer \u001c"),
        ("api.telegram.org/bot", "api.telegram.org/bot"),
        ("api.telegram.org/bot/x", "api.telegram.org/bot/x"),
    ],
)
def test_whitespace_classes(text: str, expected: str) -> None:
    assert _redact_text(text) == expected


def test_sk_trailing_boundary_fails_only_on_underscore() -> None:
    # The key is a maximal alphanumeric run, so the char after it is never an
    # ASCII letter or digit; of WORD only `_` is left.
    assert _redact_text("sk-abcdefghij1234_") == "sk-abcdefghij1234_"
    assert _redact_text("sk-abcdefghij1234-x") == "sk-<REDACTED>-x"
    assert _redact_text("sk-abcdefghij1234") == "sk-<REDACTED>"
    # Nine characters are too short, ten are enough.
    assert _redact_text("sk-abcdefghi") == "sk-abcdefghi"
    assert _redact_text("sk-abcdefghij") == "sk-<REDACTED>"


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        # The review's three inputs: a token that is itself `Bearer` does not
        # hide the next `Bearer`'s token.
        ("\u00e9Bearer Bearer abcdefghij", "\u00e9Bearer <REDACTED> <REDACTED>"),
        ("Bearer \u001c Bearer abcdefghij", "Bearer \u001c <REDACTED> <REDACTED>"),
        ("api.telegram.org/botx\u001cBearer abcdefghij", "api.telegram.org/bot<REDACTED> <REDACTED>"),
        # An sk- key inside, or right after, a Bearer token.
        ("Bearer sk-abcdefghij1234", "Bearer <REDACTED>"),
        ("Bearer abc.sk-abcdefghij1234", "Bearer <REDACTED>"),
        ("Bearer abc sk-abcdefghij1234", "Bearer <REDACTED> sk-<REDACTED>"),
        # A Bearer token that contains a Telegram URL.
        ("Bearer api.telegram.org/bot12:AB/send x", "Bearer <REDACTED> x"),
        # Overlapping spans of two rules become one.
        ("api.telegram.org/botA\u001cBearer\u001cTOKEN/send x", "api.telegram.org/bot<REDACTED> x"),
        # Touching spans of two rules become one.
        ("api.telegram.org/bot.Bearer\u001c/rest x", "api.telegram.org/bot<REDACTED> x"),
    ],
)
def test_spans_are_found_on_the_original_text(text: str, expected: str) -> None:
    assert _redact_text(text) == expected
