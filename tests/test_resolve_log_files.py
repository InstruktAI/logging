"""`resolve_log_files`: the SDK's lookup of an app's log files (the reader itself is the Rust tool)."""

from __future__ import annotations

from collections.abc import Iterator
from pathlib import Path
from tempfile import TemporaryDirectory

import pytest
from instrukt_ai_logging import resolve_log_files


def _write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


@pytest.fixture()
def app_log_dir(monkeypatch: pytest.MonkeyPatch) -> Iterator[Path]:
    tmp = TemporaryDirectory()
    root = Path(tmp.name)
    monkeypatch.setenv("XDG_STATE_HOME", str(root))

    app_dir = root / "instrukt-ai" / "demo-app"
    app_dir.mkdir(parents=True)
    yield app_dir
    tmp.cleanup()


def test_resolve_log_files_default_returns_all_log_files(app_log_dir: Path) -> None:
    _write(app_log_dir / "demo-app.log", "")
    _write(app_log_dir / "cron.log", "")
    _write(app_log_dir / "docs-watch.log", "")
    _write(app_log_dir / "demo-app.log.1", "")
    _write(app_log_dir / "demo-app.log.2.gz", "")

    result = resolve_log_files("demo-app")
    names = sorted(p.name for p in result)
    assert names == ["cron.log", "demo-app.log", "demo-app.log.1", "demo-app.log.2.gz", "docs-watch.log"]


def test_resolve_log_files_with_stems_filters_to_matching_files(app_log_dir: Path) -> None:
    _write(app_log_dir / "demo-app.log", "")
    _write(app_log_dir / "cron.log", "")
    _write(app_log_dir / "cron.log.0", "")
    _write(app_log_dir / "cron-backup.log", "")  # excluded: not matched by `cron` stem
    _write(app_log_dir / "docs-watch.log", "")

    result = resolve_log_files("demo-app", stems=["cron", "docs-watch"])
    names = sorted(p.name for p in result)
    assert names == ["cron.log", "cron.log.0", "docs-watch.log"]


def test_resolve_log_files_returns_empty_for_unknown_stem(app_log_dir: Path) -> None:
    _write(app_log_dir / "demo-app.log", "")

    assert resolve_log_files("demo-app", stems=["nope"]) == []


def test_resolve_log_files_returns_empty_when_dir_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    with TemporaryDirectory() as tmp:
        monkeypatch.setenv("XDG_STATE_HOME", tmp)
        # Directory for the app does not exist.
        assert resolve_log_files("nonexistent-app") == []
