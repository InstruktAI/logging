"""One file shared by several writers: every line goes out in one `os.write`."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import instrukt_ai_logging.logging as logging_module
import pytest

_APP = "proc-app"
_WRITERS = 4
_LINES_PER_WRITER = 200
_FILLER = 15 * 1024

_CHILD = """
import logging, sys
from instrukt_ai_logging import configure_logging
writer, lines, filler = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
configure_logging("proc-app", max_message_chars=0)
log = logging.getLogger("proc_app")
for i in range(lines):
    log.info("w%s i%d %s", writer, i, "x" * filler)
"""


def _payload(writer: int, i: int) -> str:
    return f"w{writer} i{i} {'x' * _FILLER}"


@pytest.mark.process
@pytest.mark.timeout(60)
def test_concurrent_processes_append_whole_lines(tmp_path: Path) -> None:
    # The children inherit the autouse fixture's temporary HOME and XDG dirs.
    env = dict(os.environ)
    children = [
        subprocess.Popen(
            [sys.executable, "-c", _CHILD, str(w), str(_LINES_PER_WRITER), str(_FILLER)],
            env=env,
        )
        for w in range(_WRITERS)
    ]
    for child in children:
        assert child.wait(timeout=50) == 0
    log_file = Path(os.environ["XDG_STATE_HOME"]) / "instrukt-ai" / _APP / f"{_APP}.log"

    seen = [0] * _WRITERS
    for line in log_file.read_text(encoding="utf-8").split("\n")[:-1]:
        _, _, rest = line.partition(' msg="')
        assert rest.endswith('"'), f"malformed line of {len(line)} bytes"
        msg = rest[:-1]
        writer_part, i_part, _ = msg.split(" ", 2)
        writer, i = int(writer_part[1:]), int(i_part[1:])
        assert msg == _payload(writer, i), "line torn or merged"
        assert i == seen[writer], "lines of one writer out of order"
        seen[writer] += 1
    assert seen == [_LINES_PER_WRITER] * _WRITERS


def test_a_short_write_is_an_error_and_never_followed_by_a_second_write(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    log_path = tmp_path / "app.log"
    handler = logging_module.LogFileHandler(log_path)
    calls: list[int] = []
    real_write = os.write

    def short_write(fd: int, data: bytes) -> int:
        calls.append(len(data))
        return real_write(fd, data[:10])

    try:
        with monkeypatch.context() as m:
            m.setattr(logging_module.os, "write", short_write)
            with pytest.raises(OSError, match="short write: 10 of 17 bytes"):
                handler.write_line(b"0123456789abcdef\n")
    finally:
        handler.close()
    assert calls == [17]
    assert log_path.read_bytes() == b"0123456789"
