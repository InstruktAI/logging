# Changelog

## 0.7.0

The repository becomes InstruktAI/logging: a Rust tool, a Rust writer crate
and the Python writer SDK, all following one contract,
[`docs/contract.md`](docs/contract.md), pinned by the shared fixtures under
[`conformance/`](conformance/).

### Breaking: the commands move from the Python package to a Rust tool

`instruktai-python-logger` no longer ships `instrukt-ai-logs` or
`instrukt-ai-log-setup`. It is now the Python writer SDK only, and its import
API is unchanged (`configure_logging`, `get_logger`, `InstruktAILogger`,
`InstruktAILoggerProtocol`, `TRACE`, `resolve_log_file`, `resolve_log_files`,
`source=`). The reader-only helpers in `instrukt_ai_logging.logging`
(`parse_since`, `parse_log_timestamp`, `iter_recent_log_lines`,
`iter_recent_log_lines_merged`, `newest_log_timestamp`) are gone with the CLI.

Both commands are now the Rust binaries of the `instrukt-ai-logs` crate, with
the same names, arguments, output, exit codes, rotation files and scheduler
labels. To migrate:

1. Remove the old install so its scripts do not shadow the new binaries:
   `uv tool uninstall instruktai-python-logger` for a `uv tool` install,
   `pipx uninstall instruktai-python-logger` for pipx. A plain `pip install`
   into a project environment can stay (the SDK is still used there); upgrading
   it to 0.7.0 removes the scripts.
2. Install the tool:
   `curl -LsSf https://github.com/InstruktAI/logging/releases/latest/download/instrukt-ai-logs-installer.sh | sh`
   (into `$XDG_BIN_HOME` or `~/.local/bin`), or `cargo install instrukt-ai-logs`.
3. Re-run `instrukt-ai-log-setup` once. It rewrites nothing on an already
   wired machine and reports `status: healthy`.

Anything that called `uv run instrukt-ai-log-setup` from a project that only
depends on the SDK has to call the installed binary instead.

### New: the `instrukt-ai-logging` Rust crate

A `tracing` layer plus one-call `init(app, source)` that writes the same lines
to the same files as the Python writer, bridges the `log` crate, and reopens
its file after rotation. A `Builder` sets `source`, `max_message_chars`, extra
app prefixes, or returns a `Layer` for your own subscriber;
`resolve_log_root`, `resolve_log_dir` and `resolve_log_file` locate files.
`cargo add instrukt-ai-logging`.

### Writer behaviour changes (Python)

- **Secret redaction works.** The patterns were written with doubled
  backslashes and never matched, so Bearer tokens, `sk-` keys and Telegram bot
  tokens reached the log in clear. They are now redacted in the message and in
  every string value.
- **Spotlight keeps third-party warnings and errors.** With
  `{APP}_THIRD_PARTY_LOGGERS` set, every record from a third-party logger
  outside the spotlight was dropped, errors included. Those loggers now pass at
  WARNING and above, as documented.

### Tool behaviour changes (`instrukt-ai-logs`)

- `--logs` / `--include` select exact stems, with no glob: `--logs '*'`
  selects nothing, and a stem selects only `<stem>.log` and
  `<stem>.log.<suffix>` (so `cron.logfoo` is no longer read for `cron`).
- `--follow` waits for a missing `<stem>.log` of each requested stem and reads
  it from its start once it appears, instead of exiting at once when only
  rotated history existed. A stem named twice is followed once.
- An invalid `--grep` or `--exclude` regex is a one-line error, exit 1, instead
  of a traceback.
- An app name that normalizes to nothing prints the error message, exit 1,
  instead of a traceback.
- A closed stdout (`instrukt-ai-logs app | head`) ends quietly with exit 0
  instead of a BrokenPipe traceback.
- A file whose last line has no final newline gets one, so merged output
  never glues two lines together. For `--grep`/`--exclude` that line is also
  matched as if it ended in `\n`.
- A read error (an unreadable file, a corrupt gzip archive) is one line on
  stderr, exit 1, instead of a traceback.
- A line starting with a bare `YYYY-MM-DDZ` token no longer crashes the merge;
  it counts as a line without a timestamp.
- Regex patterns keep Python `re` syntax (lookaround and backreferences
  included) and its inline flags. With `(?ai)`, case folding stays Unicode
  rather than ASCII-only.

### Tool behaviour changes (`instrukt-ai-log-setup`)

- It gains `--help` and `--version`. Any other argument is a usage error,
  exit 2, where 0.6.x ignored arguments and ran setup.

### Unchanged on purpose

- The rotation files stay under `~/.config/instrukt-ai/` and
  `~/.config/systemd/user/` (and `~/Library/LaunchAgents/` on macOS),
  regardless of `XDG_CONFIG_HOME`, with byte-identical content.
- The launchd label `ai.instrukt.log-rotate` and the systemd units
  `instrukt-ai-logrotate.service`/`.timer` are the same, so an existing setup
  is picked up as is.
- The log location, line format and the four env vars are unchanged.
