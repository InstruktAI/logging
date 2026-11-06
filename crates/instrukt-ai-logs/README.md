# instrukt-ai-logs

Read and merge InstruktAI service logs, and wire per-user log rotation. Two
binaries:

- **`instrukt-ai-logs`**: prints an app's log lines from a time window, merged
  by timestamp across its per-process files and their rotated (including
  gzip-compressed) archives, with regex filters and a `tail -f` style follow
  mode.
- **`instrukt-ai-log-setup`**: writes the per-user rotation config and
  scheduler (launchd + newsyslog on macOS, systemd user timer + logrotate on
  Linux).

They read what the InstruktAI writers produce: the Rust crate
[`instrukt-ai-logging`](https://crates.io/crates/instrukt-ai-logging) and the
Python SDK [`instruktai-python-logger`](https://pypi.org/project/instruktai-python-logger/).

## Install

```bash
curl -LsSf https://github.com/InstruktAI/logging/releases/latest/download/instrukt-ai-logs-installer.sh | sh
```

installs prebuilt binaries into `$XDG_BIN_HOME` or `~/.local/bin`. Or build
from source:

```bash
cargo install instrukt-ai-logs
```

## Read logs

```bash
instrukt-ai-logs teleclaude                      # last 10 minutes
instrukt-ai-logs teleclaude --since 2h           # s, m, h or d
instrukt-ai-logs teleclaude --grep 'level=ERROR' --exclude 'logger=httpx'
instrukt-ai-logs teleclaude --since 10m --follow # then follow; Ctrl-C stops
instrukt-ai-logs teleclaude --logs=cron,docs-watch
```

Logs live in `$XDG_STATE_HOME/instrukt-ai/<app>/` (`~/.local/state` when
`XDG_STATE_HOME` is unset). Without `--logs` (alias `--include`) every
`*.log*` file there is merged; with it, only the exact stems named.
`--grep` keeps and `--exclude` then drops matching lines; patterns use Python
`re` syntax, lookaround and backreferences included. An empty result prints a
note on stderr naming the directory read and its newest entry.

## Wire rotation

```bash
instrukt-ai-log-setup
```

Every 30 minutes, run as the user, each `*.log` under the log root that has
passed 50 MB is rotated (a trigger checked then, not a cap), keeping 30
gzipped archives (on Linux the newest stays plain until the next rotation; on
macOS a writer can lose one in-flight line per rotation; see the contract). Re-running is idempotent and reports
`status: healthy`; problems are printed and the exit code is 1.

## Contract

The arguments, output, exit codes, rotation files and scheduler labels match
the 0.6.x Python commands they replace, apart from the changes listed in the
[changelog](https://github.com/InstruktAI/logging/blob/main/CHANGELOG.md). The
full logging contract:
[`docs/contract.md`](https://github.com/InstruktAI/logging/blob/main/docs/contract.md).

## License

MIT
