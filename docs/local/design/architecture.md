---
id: "local/projects/python-logger/design/architecture"
type: "design"
scope: "project"
description: "Architecture of the InstruktAI logging repository: a Rust tool that reads logs and wires rotation, plus two thin writers (stdlib-only Python SDK, tracing-based Rust crate) following one contract — one predictable XDG log location, rotation run as the producing user, zero root involvement."
---

# Project Architecture — Design

## Purpose

The repository enforces one logging contract (`docs/contract.md`) across
InstruktAI services, whatever their language, so that one tool and one set of
env-var knobs work uniformly. It ships three parts:

- **`instruktai-python-logger`** (PyPI), module `instrukt_ai_logging`: the
  Python writer SDK on stdlib `logging`.
- **`instrukt-ai-logging`** (crates.io, `crates/instrukt-ai-logging`): the Rust
  writer, a `tracing` layer.
- **`instrukt-ai-logs`** (crates.io and GitHub Release binaries,
  `crates/instrukt-ai-logs`): the tool, binaries `instrukt-ai-logs` (reader)
  and `instrukt-ai-log-setup` (rotation setup).

Two constraints dominate the design:

- Logs must remain queryable from a tail window (`docs/design.md`) — third-party
  debug/info noise must not push service signal out.
- The entire logging lifecycle — writing, locating, and rotating logs — runs as
  the producing user. Root appears nowhere: no `/etc` configuration, no sudo
  step, no system rotation daemon touching these files. Ownership problems are
  structurally unexpressible, not compensated for.

## Inputs/Outputs

Inputs:

- An app identifier passed to a writer (`configure_logging(name, source=...)`
  in Python, `init(app, source)` / `Builder` in Rust), normalized three ways:
  env-var prefix (`TELECLAUDE`), logger prefix (`teleclaude`), filesystem app
  name (`teleclaude`). Python: `_normalize_*` in
  `instrukt_ai_logging/logging.py`; Rust: `crates/instrukt-ai-logging/src/names.rs`.
- Environment variables: `{APP}_LOG_LEVEL`, `{APP}_THIRD_PARTY_LOG_LEVEL`,
  `{APP}_THIRD_PARTY_LOGGERS`, `{APP}_MUTED_LOGGERS`. These four per-app
  variables are the entire configuration surface — there is no log-root
  override variable, flag, or API parameter.
- Optional `source` to address per-process log files in multi-producer apps.

Outputs:

- Single-line logfmt records appended to
  `$XDG_STATE_HOME/instrukt-ai/{app}/{source-or-app}.log` — the one
  predictable, prose-citable location all InstruktAI applications document
  (fallback `~/.local/state`, on **both** macOS and Linux). Both writers
  return the resolved path. Python attaches exactly one `WatchedFileHandler`
  to the root logger; Rust installs a layer that reopens its file when the
  path no longer refers to it.
- User-space rotation assets, wired once and idempotently by
  `instrukt-ai-log-setup` (wrapped by `make install-runtime`): a rotation conf
  under `~/.config/instrukt-ai/` plus a per-user scheduler unit (launchd
  LaunchAgent on macOS, systemd user timer on Linux) that runs the platform
  rotator **as the user**.
- `instrukt-ai-logs` prints filtered, timestamp-merged lines from a chosen
  window and optionally follows the live files.

## Invariants

- **One contract, executable.** `docs/contract.md` is the specification;
  `conformance/` is its executable form, run against both writers and the
  tool binaries. Both writers emit identical lines for identical calls.
- **Path rules live once per language.** The Rust crate's
  `resolve_log_root`/`resolve_log_dir`/`resolve_log_file` serve both the Rust
  writer and the tool, rendering paths as Python's `pathlib` does; the Python
  SDK has its own `_resolve_log_root`.
- **Stdlib only at runtime (Python).** `pyproject.toml` declares
  `dependencies = []`, and the wheel holds the SDK only, with no console
  scripts.
- **One predictable location, no knobs.** `$XDG_STATE_HOME/instrukt-ai/{app}/`
  (fallback `~/.local/state`) — identical on macOS and Linux. No override
  exists. Tests isolate via `XDG_STATE_HOME`, the platform's own mechanism.
- **Rotation runs as the producing user, never root.** The scheduler unit and
  rotator invocation are per-user; rotated and recreated files therefore always
  carry the producer's ownership by construction.
- **Retention depth is one declaration, shared by both backends.** The archive
  count emitted into the newsyslog `count` field and into the logrotate
  `rotate` directive comes from the single `ARCHIVE_DEPTH` constant in
  `crates/instrukt-ai-logs/src/setup.rs`, so the two platforms cannot drift
  apart. The window it buys, and why it is bounded by count rather than by
  age, is the contract in
  `local/projects/python-logger/spec/feature/log_rotation/retention-window`.
- **Writers never wire rotation.** Neither `configure_logging` nor `init`
  writes rotation assets or runs `launchctl`/`systemctl`.
- **Logging fails fast at startup.** An unopenable log directory or file
  raises out of `configure_logging` or is an `Err` from `init`; the host
  process must not start blind. There is deliberately no degraded mode.
- **One line per event.** Embedded newlines are escaped; Python attaches
  exception text as `exc=...` with its newlines escaped too.
- **Third-party selection is deterministic.** The longest matching prefix
  among the app prefix, spotlight prefixes and muted prefixes decides the
  threshold; without a match the root threshold applies (the third-party
  level, or WARNING when a spotlight is set).
- **Tail-window protection.** Long values are truncated (default 4000
  characters) and Telegram bot tokens, Bearer tokens and `sk-` keys are
  redacted, in both writers.
- **Rotation keeps working around a live writer and reader.** Both writers
  reopen on inode change; the tool's follow mode reopens from the start after
  rotation or truncation.

## Primary flows

Writer configuration (once at process start, either language):

```
caller passes app="teleclaude" (and optionally source)
  → normalize into env_prefix / logger_prefix / app_name
  → read env vars; compute app level, third-party level, spotlight, muted
  → resolve $XDG_STATE_HOME/instrukt-ai/{app}/, create the dir, open the file
    for append (any error raises / returns Err — fail fast)
  → Python: replace root handlers with one WatchedFileHandler, set per-prefix
    levels, coerce loggers to InstruktAILogger (so .info(msg, **kv) works)
  → Rust: build the selection and the layer; init() installs it globally and
    bridges the log crate
  → return the resolved path
```

Rotation-ensure flow (`instrukt-ai-log-setup`, run once per machine — directly
or via `make install-runtime`):

```
derive desired conf + scheduler-unit content from the resolved log root
  → compare with what is on disk; rewrite only on drift (idempotent re-runs)
  → macOS: ~/.config/instrukt-ai/newsyslog.conf (one glob line
    "<root>/*/*.log 640 30 50000 * ZG": 30 gzipped archives at 50 MB) +
    ~/Library/LaunchAgents/ai.instrukt.log-rotate.plist running
    /usr/sbin/newsyslog -r -f <conf> every 30 min; bootstrap via launchctl
  → Linux: ~/.config/instrukt-ai/logrotate.conf (size 50M, rotate 30, compress,
    delaycompress, missingok, notifempty) + systemd user service+timer running
    logrotate --state $XDG_STATE_HOME/instrukt-ai/logrotate.state <conf>;
    enable via systemctl --user
  → re-runs on an already-wired machine report healthy (including macOS
    launchd's EIO already-loaded bootstrap signal, confirmed by launchctl
    print); problems are printed and the exit code is 1
```

Read flow (`instrukt-ai-logs <app>`):

```
parse --since (e.g. "10m"); compile --grep / --exclude (Python re syntax)
  → list <log dir>/*.log* (or the exact --logs stems), gzip archives included
  → skip files whose mtime is before the cutoff; find each plain file's
    window start with a backward probe from EOF
  → k-way merge of the per-file streams by leading timestamp; continuation
    lines inherit their file's previous timestamp
  → keep --grep matches, drop --exclude matches, write to stdout; on an empty
    result note the directory read and its newest entry on stderr
  → with --follow, poll each live <stem>.log every 250 ms from its end
```

## Failure modes

- **Unwritable log dir or file.** Both writers stop at initialization.
  Intentional fail-fast: logging is an essential subsystem and a process that
  cannot write durable logs must not run. Because path, writer, and rotator
  all live in user space, this only occurs on genuine local misconfiguration.
- **Rotation-ensure failure.** `launchctl`/`systemctl` unavailable or refusing
  (e.g. no user service manager in a container), or an unwritable asset
  directory: `instrukt-ai-log-setup` reports the problem and exits 1; logging
  itself is unaffected, and log growth is unbounded until the operator
  resolves it — surfaced at install-runtime, never silent, never fatal to the
  producing process.
- **New producers/apps.** The rotation conf covers the log root with a glob
  evaluated at rotation time (newsyslog `G` flag / logrotate glob), so new
  `{app}`/`{source}` files are rotated without any re-install step.
- **Lines without a timestamp in a merged read.** They are continuation lines
  and inherit the previous parsed timestamp from the same file, so multi-line
  entries stay adjacent to their parent.
- **Rotation during follow.** The follower detects an inode change and
  reopens; a truncation rewinds to the start.
- **Unreadable file or corrupt archive while reading.** One line on stderr,
  exit 1.
- **Large/sensitive payloads.** Mitigated by truncation and redaction.
  Adding new redactions requires strong justification — they execute on every
  value of every line, in both writers.
