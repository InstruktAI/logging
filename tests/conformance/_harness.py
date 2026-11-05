"""Run `conformance/reader` and `conformance/setup` cases against a command, per `conformance/README.md`.

Language-neutral apart from the command under test: the characterization test
points it at today's Python entry points. Every run gets a fresh temporary
HOME / XDG_STATE_HOME / XDG_CONFIG_HOME and a PATH holding only a directory of
fake `launchctl`/`systemctl`, so no real scheduler is reachable.
"""

from __future__ import annotations

import gzip
import json
import os
import re
import signal
import subprocess
import sys
import time
import tomllib
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[2]
CONFORMANCE_DIR = REPO_ROOT / "conformance"

_T_PLACEHOLDER = re.compile(r"\{\{T-(\d+)\}\}")
_PROCESS_TIMEOUT_S = 10.0
_FAKE_SCHEDULERS = ("launchctl", "systemctl")
_FAKE_SCHEDULER_SOURCE = """\
import json, os, sys
argv = [os.path.basename(sys.argv[0]), *sys.argv[1:]]
with open(os.environ["CONFORMANCE_FAKE_CALLS"], "a", encoding="utf-8") as calls:
    calls.write(json.dumps(argv) + "\\n")
with open(os.environ["CONFORMANCE_FAKE_TABLE"], encoding="utf-8") as table:
    entries = json.load(table)
for entry in entries:
    prefix = entry["argv_prefix"]
    if argv[: len(prefix)] == prefix:
        sys.stderr.write(entry.get("stderr", ""))
        sys.exit(entry.get("exit", 0))
sys.exit(0)
"""


def repo_version() -> str:
    with (REPO_ROOT / "pyproject.toml").open("rb") as fh:
        return str(tomllib.load(fh)["project"]["version"])


def load_cases(kind: str) -> list[tuple[str, dict[str, Any]]]:
    return [
        (p.stem, json.loads(p.read_text(encoding="utf-8"))) for p in sorted((CONFORMANCE_DIR / kind).glob("*.json"))
    ]


def format_log_timestamp(ts: datetime) -> str:
    return ts.strftime("%Y-%m-%dT%H:%M:%S") + f".{ts.microsecond // 1000:03d}Z"


@dataclass(frozen=True)
class Sandbox:
    """The temporary dirs one case runs in, plus its captured "now"."""

    home: Path
    state: Path
    config: Path
    fakebin: Path
    now: datetime

    @classmethod
    def create(cls, root: Path) -> Sandbox:
        dirs = [root / name for name in ("home", "state", "config", "fakebin")]
        for d in dirs:
            d.mkdir(parents=True)
        return cls(*dirs, now=datetime.now(UTC))

    def render(self, text: str) -> str:
        text = _T_PLACEHOLDER.sub(lambda m: format_log_timestamp(self.now - timedelta(seconds=int(m[1]))), text)
        return (
            text.replace("{{STATE}}", str(self.state))
            .replace("{{HOME}}", str(self.home))
            .replace("{{UID}}", str(os.getuid()))
            .replace("{{VERSION}}", repo_version())
        )

    def env(self) -> dict[str, str]:
        return {
            "HOME": str(self.home),
            "XDG_STATE_HOME": str(self.state),
            "XDG_CONFIG_HOME": str(self.config),
            "PATH": str(self.fakebin),
            "PYTHONUTF8": "1",
            "CONFORMANCE_FAKE_CALLS": str(self.fakebin / "calls.jsonl"),
            "CONFORMANCE_FAKE_TABLE": str(self.fakebin / "table.json"),
        }

    def install_fake_schedulers(self, table: list[dict[str, Any]]) -> None:
        (self.fakebin / "table.json").write_text(json.dumps(table), encoding="utf-8")
        for name in _FAKE_SCHEDULERS:
            fake = self.fakebin / name
            fake.write_text(f"#!{sys.executable}\n{_FAKE_SCHEDULER_SOURCE}", encoding="utf-8")
            fake.chmod(0o755)

    def recorded_calls(self) -> list[list[str]]:
        calls = self.fakebin / "calls.jsonl"
        if not calls.exists():
            return []
        return [json.loads(line) for line in calls.read_text(encoding="utf-8").splitlines()]


@dataclass(frozen=True)
class Outcome:
    exit: int
    stdout: str
    stderr: str


def _lines(text: str) -> list[str]:
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return lines


def assert_text_matches(label: str, actual: str, spec: dict[str, str], sandbox: Sandbox) -> None:
    expected = sandbox.render(spec["value"])
    match spec["match"]:
        case "exact":
            assert actual == expected, f"{label} mismatch"
        case "contains":
            assert expected in actual, f"{label} does not contain {expected!r}: {actual!r}"
        case "regex":
            assert re.search(expected, actual), f"{label} does not match /{expected}/: {actual!r}"
        case other:
            raise AssertionError(f"unknown match mode {other!r}")


def assert_stdout(actual: str, expected: list[str] | dict[str, str], sandbox: Sandbox) -> None:
    if isinstance(expected, dict):
        assert_text_matches("stdout", actual, expected, sandbox)
        return
    assert _lines(actual) == [sandbox.render(line) for line in expected]


# --- reader ------------------------------------------------------------------


def _write_log_file(sandbox: Sandbox, spec: dict[str, Any]) -> None:
    path = sandbox.state / "instrukt-ai" / spec["path"]
    path.parent.mkdir(parents=True, exist_ok=True)
    data = "".join(sandbox.render(line) + "\n" for line in spec["lines"]).encode("utf-8")
    path.write_bytes(gzip.compress(data) if spec.get("gzip", False) else data)
    mtime = sandbox.now.timestamp() + spec.get("mtime_offset_s", 0)
    os.utime(path, (mtime, mtime))


def _apply_follow_step(sandbox: Sandbox, step: dict[str, Any]) -> None:
    root = sandbox.state / "instrukt-ai"
    if "rotate" in step:
        path = root / step["rotate"]
        path.rename(path.with_name(path.name + ".0"))
        path.touch()
        return
    with (root / step["path"]).open("a", encoding="utf-8") as fh:
        fh.write("".join(sandbox.render(line) + "\n" for line in step["lines"]))


def run_reader_case(command: list[str], case: dict[str, Any], sandbox: Sandbox) -> Outcome:
    for spec in case["files"]:
        _write_log_file(sandbox, spec)
    argv = [*command, *(sandbox.render(arg) for arg in case["args"])]
    follow = case.get("follow")
    if follow is None:
        done = subprocess.run(
            argv,
            env=sandbox.env(),
            cwd=REPO_ROOT,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=_PROCESS_TIMEOUT_S,
        )
        return Outcome(done.returncode, done.stdout, done.stderr)

    proc = subprocess.Popen(
        argv,
        env=sandbox.env(),
        cwd=REPO_ROOT,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    started = time.monotonic()
    try:
        for step in sorted(follow["appends"], key=lambda s: s["after_ms"]):
            time.sleep(max(0.0, started + step["after_ms"] / 1000 - time.monotonic()))
            _apply_follow_step(sandbox, step)
        time.sleep(max(0.0, started + follow["run_ms"] / 1000 - time.monotonic()))
        proc.send_signal(signal.SIGINT)
        stdout, stderr = proc.communicate(timeout=_PROCESS_TIMEOUT_S)
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.communicate()
    return Outcome(proc.returncode, stdout, stderr)


def check_reader_case(command: list[str], case: dict[str, Any], sandbox: Sandbox) -> None:
    outcome = run_reader_case(command, case, sandbox)
    expect = case["expect"]
    assert outcome.exit == expect["exit"], f"exit {outcome.exit}, stderr: {outcome.stderr!r}"
    assert_stdout(outcome.stdout, expect["stdout"], sandbox)
    assert_text_matches("stderr", outcome.stderr, expect["stderr"], sandbox)


# --- setup -------------------------------------------------------------------


def _files_under(*roots: Path) -> set[Path]:
    return {p for root in roots for p in root.rglob("*") if p.is_file() or p.is_symlink()}


def run_setup_case(command: list[str], case: dict[str, Any], sandbox: Sandbox) -> Outcome:
    sandbox.install_fake_schedulers(case["scheduler"])
    for raw_path, content in case["preexisting"].items():
        path = Path(sandbox.render(raw_path))
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(sandbox.render(content), encoding="utf-8")
    done = subprocess.run(
        [*command, *case["args"]],
        env=sandbox.env(),
        cwd=REPO_ROOT,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=_PROCESS_TIMEOUT_S,
    )
    return Outcome(done.returncode, done.stdout, done.stderr)


def check_setup_case(command: list[str], case: dict[str, Any], sandbox: Sandbox) -> None:
    outcome = run_setup_case(command, case, sandbox)
    expect = case["expect"]
    assert outcome.exit == expect["exit"], f"exit {outcome.exit}, stderr: {outcome.stderr!r}"
    assert_stdout(outcome.stdout, expect["stdout"], sandbox)

    expected_files = {Path(sandbox.render(p)): sandbox.render(c) for p, c in expect["files"].items()}
    assert _files_under(sandbox.home, sandbox.state) == set(expected_files)
    for path, content in expected_files.items():
        assert path.read_text(encoding="utf-8") == content, f"content of {path}"

    expected_calls = [[sandbox.render(arg) for arg in call] for call in expect["calls"]]
    assert sandbox.recorded_calls() == expected_calls
