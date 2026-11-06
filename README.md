# InstruktAI logging

One logging contract for InstruktAI services, in three parts:

- **`instrukt-ai-logs`** and **`instrukt-ai-log-setup`**: the Rust tool that
  reads and merges an app's logs and wires per-user log rotation.
- **`instrukt-ai-logging`**: a Rust writer crate (a `tracing` layer).
- **`instruktai-python-logger`**: the Python writer SDK (module
  `instrukt_ai_logging`, stdlib `logging`).

Both writers follow the same contract, [`docs/contract.md`](docs/contract.md):
the same four env vars, the same single-line logfmt format, the same file
location. A Rust service and a Python service of one app write identical lines
into the same directory, and the tool reads both. The executable form of the
contract is the shared fixture tree under [`conformance/`](conformance/).

The contract keeps logs useful from a tail window, for people and for AIs that
only read the last lines: third-party noise is held back, every event is one
human-readable line, secrets are redacted and very long values truncated.
Background in [`docs/design.md`](docs/design.md); release process in
[`docs/publishing.md`](docs/publishing.md).

## Install the tool

```bash
curl -LsSf https://github.com/InstruktAI/logging/releases/latest/download/instrukt-ai-logs-installer.sh | sh
```

This installs both binaries, `instrukt-ai-logs` and `instrukt-ai-log-setup`,
into `$XDG_BIN_HOME` or `~/.local/bin` (macOS on Apple silicon and Intel, Linux
x86_64 and aarch64). Rust users can build it instead:

```bash
cargo install instrukt-ai-logs
```

Up to 0.6.x the two commands came with the Python package. If you installed
them that way (`uv tool install instruktai-python-logger`, `pipx`, or `pip`),
remove that install first so the old scripts do not shadow the new binaries,
for example `uv tool uninstall instruktai-python-logger`. See
[`CHANGELOG.md`](CHANGELOG.md).

## Read logs

```bash
# Last 10 minutes (the default)
instrukt-ai-logs teleclaude

# Last 2 hours
instrukt-ai-logs teleclaude --since 2h

# Keep matching lines, then drop matching lines (Python-style regex)
instrukt-ai-logs teleclaude --since 10m --grep 'logger=teleclaude' --exclude 'level=DEBUG'

# Print the window, then follow new lines (tail -f style); Ctrl-C to stop
instrukt-ai-logs teleclaude --since 10m --follow

# Only some per-process files
instrukt-ai-logs teleclaude --since 1h --logs=cron,docs-watch
```

`--since` takes `<n>s`, `<n>m`, `<n>h` or `<n>d`. Without `--logs`, every
`*.log*` file in the app's directory is merged by timestamp, rotated and
gzip-compressed archives included. `--logs` (alias `--include`) selects exact
stems: `--logs=cron` reads `cron.log`, `cron.log.0`, `cron.log.1.gz` and never
`cron-backup.log`. Follow mode follows each stem's live `<stem>.log`, waits
for one that does not exist yet, and keeps following across rotation and
truncation. When nothing matches, a note on stderr names the directory read
and its newest entry.

## Write logs from Python

```bash
pip install instruktai-python-logger
```

```py
import logging
from instrukt_ai_logging import configure_logging

configure_logging("teleclaude")
logger = logging.getLogger("teleclaude.core")
logger.info("job_started", job_id="abc123", user_id=123)
```

```text
2026-11-05T09:30:12.345Z level=INFO logger=teleclaude.core msg="job_started" job_id=abc123 user_id=123
```

`configure_logging` returns the log file path. `get_logger(name)` returns a
logger typed for named `**kv` fields; `TRACE` is a level below `DEBUG`.
`resolve_log_file(app, source=...)` and `resolve_log_files(app, stems=...)`
locate files without configuring anything.

## Write logs from Rust

```bash
cargo add instrukt-ai-logging tracing
```

```rust
fn main() -> Result<(), instrukt_ai_logging::Error> {
    // $XDG_STATE_HOME/instrukt-ai/teleclaude/teleclaude.log
    let path = instrukt_ai_logging::init("teleclaude", None)?;
    tracing::info!(job_id = "abc123", attempt = 3, "job_started");
    println!("logging to {}", path.display());
    Ok(())
}
```

`init` installs the global subscriber and bridges the `log` crate. The
`Builder` sets a `source`, `max_message_chars`, extra app prefixes
(`app_prefix("my_app_core")`), or returns a `Layer` for a subscriber you
compose yourself. Details in
[`crates/instrukt-ai-logging/README.md`](crates/instrukt-ai-logging/README.md).

## Per-process log files

Several processes of one app (a daemon, a watcher, a cron job) can each write
their own file in the app's directory by passing a source:

```py
configure_logging("teleclaude")  # teleclaude.log
configure_logging("teleclaude", source="docs-watch")  # docs-watch.log
```

```rust
instrukt_ai_logging::init("teleclaude", Some("cron"))?; // cron.log
```

Pass the bare stem; `.log` is always appended. `instrukt-ai-logs` merges them.

## Environment variables

Per app, with the app name upper-cased as prefix (here `TELECLAUDE_`):

| Variable | Meaning | Default |
| --- | --- | --- |
| `TELECLAUDE_LOG_LEVEL` | threshold for the app's own loggers | `INFO` |
| `TELECLAUDE_THIRD_PARTY_LOG_LEVEL` | threshold for third-party loggers | `WARNING` |
| `TELECLAUDE_THIRD_PARTY_LOGGERS` | comma-separated third-party prefixes to spotlight at the third-party level, e.g. `httpcore,telegram`; every other third-party logger then passes at `WARNING`+ | none |
| `TELECLAUDE_MUTED_LOGGERS` | comma-separated prefixes held at `WARNING`+, e.g. `teleclaude.cli.tui` | none |

These four are the whole configuration surface. The longest matching prefix
decides; the exact rules are in [`docs/contract.md`](docs/contract.md).

## Log location

```text
$XDG_STATE_HOME/instrukt-ai/{app}/{source or app}.log
```

with `~/.local/state` when `XDG_STATE_HOME` is unset, on macOS and Linux
alike. There is no override. The location is user-writable, and both writers
fail at startup when the file cannot be opened: a process that cannot write
its logs does not start blind.

## Rotation

Rotation runs entirely as the user. Run once per machine:

```bash
instrukt-ai-log-setup
```

It writes a rotation config under `~/.config/instrukt-ai/` and a per-user
scheduler: a launchd LaunchAgent running `newsyslog` on macOS, a systemd user
timer running `logrotate` on Linux. Each log rotates at 50 MB and 30 archives
are kept: gzipped on Linux except the newest (`delaycompress`), uncompressed on
macOS, where newsyslog cannot delay compressing the archive a writer may still
append to (see `docs/contract.md`, Rotation). Re-running on a wired machine reports healthy; problems are
printed and the exit code is 1. The writers never touch rotation; they reopen
their file when it has been rotated away. No `/etc` configuration and no sudo.

## Development

`make install` prepares the Python and Rust toolchains; `make install-runtime`
runs the setup from this checkout. Gates:

```bash
uv run ruff check . && uv run ruff format --check . && uv run pyright && uv run mypy && uv run pytest
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

### Benchmarks

Both writers have the same benchmark cases, one log call per iteration:
`crates/instrukt-ai-logging/benches/` (divan, through CodSpeed's
`codspeed-divan-compat`) and `benchmarks/` (pytest, through `pytest-codspeed`).
Neither gate above runs them.

```bash
cargo bench -p instrukt-ai-logging --bench writer          # wall clock
cargo bench -p instrukt-ai-logging --bench writer_allocs   # heap allocations per call
uv run pytest benchmarks --codspeed                        # wall clock
uv run pytest benchmarks                                   # each case once, as a smoke check
```

Every case writes to a real log file in a temporary directory, with the
per-line rotation check, as in production (the writers refuse a symlink at the
log path, so the cases can no longer log to `/dev/null`). CodSpeed's CPU
simulation does not count system calls, so in CI they measure selection,
formatting and redaction. The `file` case is `app_line` under its historical
name. The 1 MiB adversarial redaction cases carry the `slow_bench` marker in
Python.

## License

MIT
