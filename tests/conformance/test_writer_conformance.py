"""Run the shared writer conformance cases (`conformance/writer/*.json`) against the Python writer.

The same files are run by the Rust writer's test suite; see `conformance/README.md`
for the schema. Each case gets a fresh temporary HOME/XDG tree, a clean logging
state and only the contract env vars it sets.
"""

from __future__ import annotations

import json
import logging
import os
import re
from collections.abc import Iterator
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

import pytest
from instrukt_ai_logging import TRACE, configure_logging, get_logger
from instrukt_ai_logging.logging import _normalize_env_prefix

_CASES_DIR = Path(__file__).resolve().parents[2] / "conformance" / "writer"
_CASES = sorted(_CASES_DIR.glob("*.json"))

_CASE_KEYS = {"description", "app", "source", "max_message_chars", "env", "calls", "expect"}
_CALL_KEYS = {"logger", "level", "msg", "kv"}
_KV_KEYS = {"key", "type", "value"}
_EXPECT_KEYS = {"path", "lines"}
_LEVELS = {
    "TRACE": TRACE,
    "DEBUG": logging.DEBUG,
    "INFO": logging.INFO,
    "WARNING": logging.WARNING,
    "ERROR": logging.ERROR,
}
_KV_TYPES: dict[str, tuple[type, ...]] = {"str": (str,), "int": (int,), "float": (float, int), "bool": (bool,)}
_CONTRACT_SUFFIXES = ("_LOG_LEVEL", "_THIRD_PARTY_LOG_LEVEL", "_THIRD_PARTY_LOGGERS", "_MUTED_LOGGERS")
_EXTRA_ENV_KEYS = {"XDG_STATE_HOME"}
_TS = re.compile(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z) ")


def _load_case(path: Path) -> dict[str, Any]:
    case: dict[str, Any] = json.loads(path.read_text(encoding="utf-8"))
    # Strict schema: an unknown key is a fixture defect that one runner would silently ignore.
    assert set(case) == _CASE_KEYS, f"case keys {sorted(case)}"
    assert set(case["expect"]) == _EXPECT_KEYS, f"expect keys {sorted(case['expect'])}"
    prefix = _normalize_env_prefix(case["app"])
    allowed_env = {prefix + suffix for suffix in _CONTRACT_SUFFIXES} | _EXTRA_ENV_KEYS
    assert set(case["env"]) <= allowed_env, f"env keys outside the contract: {sorted(set(case['env']) - allowed_env)}"
    for call in case["calls"]:
        assert set(call) == _CALL_KEYS, f"call keys {sorted(call)}"
        assert call["level"] in _LEVELS, f"level {call['level']!r}"
        assert "::" not in call["logger"], "fixtures use '.' as the logger separator"
        for kv in call["kv"]:
            assert set(kv) == _KV_KEYS, f"kv keys {sorted(kv)}"
            assert kv["type"] in _KV_TYPES, f"kv type {kv['type']!r}"
            allowed = _KV_TYPES[kv["type"]]
            value = kv["value"]
            ok = isinstance(value, allowed) and (kv["type"] == "bool" or not isinstance(value, bool))
            assert ok, f"kv {kv['key']!r}: {value!r} is not a {kv['type']}"
    return case


@pytest.fixture()
def clean_logging() -> Iterator[None]:
    """Give each case the logging state of a fresh process and restore the suite's afterwards."""
    manager = logging.root.manager
    saved_handlers = list(logging.root.handlers)
    saved_root_level = logging.root.level
    saved_levels = {name: obj.level for name, obj in manager.loggerDict.items() if isinstance(obj, logging.Logger)}

    def _reset() -> None:
        for handler in logging.root.handlers:
            handler.close()
        logging.root.handlers = []
        # configure_logging only sets levels on the prefixes it is told about; a level a
        # previous case left on another logger would leak into this one.
        for obj in manager.loggerDict.values():
            if isinstance(obj, logging.Logger):
                obj.setLevel(logging.NOTSET)

    _reset()
    try:
        yield
    finally:
        _reset()
        logging.root.handlers = saved_handlers
        logging.root.setLevel(saved_root_level)
        for name, level in saved_levels.items():
            logging.getLogger(name).setLevel(level)


def _render(text: str, *, state: Path, home: Path) -> str:
    return text.replace("{{STATE}}", str(state)).replace("{{HOME}}", str(home))


def _kv_value(kv: dict[str, Any]) -> object:
    if kv["type"] == "float":
        return float(kv["value"])
    return kv["value"]


def _floor_ms(moment: datetime) -> datetime:
    return moment.replace(microsecond=moment.microsecond // 1000 * 1000)


@pytest.mark.functional
@pytest.mark.parametrize("case_path", _CASES, ids=[p.stem for p in _CASES])
def test_writer_case(case_path: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, clean_logging: None) -> None:
    case = _load_case(case_path)
    home = tmp_path / "case-home"
    state = tmp_path / "case-state"
    home.mkdir()
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("XDG_STATE_HOME", str(state))
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "case-config"))
    prefix = _normalize_env_prefix(case["app"])
    for suffix in _CONTRACT_SUFFIXES:
        monkeypatch.delenv(prefix + suffix, raising=False)
    for key, value in case["env"].items():
        monkeypatch.setenv(key, value)

    kwargs: dict[str, Any] = {}
    if case["source"] is not None:
        kwargs["source"] = case["source"]
    if case["max_message_chars"] is not None:
        kwargs["max_message_chars"] = case["max_message_chars"]

    started = _floor_ms(datetime.now(tz=UTC))
    log_path = configure_logging(case["app"], **kwargs)
    for call in case["calls"]:
        kv = {item["key"]: _kv_value(item) for item in call["kv"]}
        # `extra={"kv": ...}` rather than `**kv`: fixture keys such as `msg` or `level`
        # collide with the logger methods' own parameters.
        get_logger(call["logger"]).log(_LEVELS[call["level"]], call["msg"], extra={"kv": kv})
    for handler in logging.root.handlers:
        handler.flush()
    finished = datetime.now(tz=UTC)

    expected_path = Path(_render(case["expect"]["path"], state=state, home=home))
    assert log_path == expected_path
    assert expected_path.is_file()

    raw = expected_path.read_text(encoding="utf-8")
    assert raw == "" or raw.endswith("\n"), "every line ends with a newline"
    stripped: list[str] = []
    for line in raw.splitlines():
        match = _TS.match(line)
        assert match, f"line does not start with a timestamp: {line!r}"
        ts = datetime.strptime(match.group(1), "%Y-%m-%dT%H:%M:%S.%fZ").replace(tzinfo=UTC)
        assert started <= ts <= finished + timedelta(milliseconds=1), f"timestamp {ts} outside the case window"
        stripped.append(line[match.end() :])

    expected = [_render(line, state=state, home=home) for line in case["expect"]["lines"]]
    assert stripped == expected


def test_cases_exist() -> None:
    assert _CASES, f"no writer conformance cases under {_CASES_DIR}"
    assert os.path.isdir(_CASES_DIR)
