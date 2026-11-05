---
id: "local/projects/python-logger/spec/entry-points"
type: "spec"
scope: "project"
description: "Public entry points of the InstruktAI logging repository: the Rust binaries instrukt-ai-logs and instrukt-ai-log-setup, the Rust writer crate API, and the Python writer SDK API re-exported from instrukt_ai_logging."
---

# Entry Points — Spec

## What it is

The public surface the repository ships. Anything outside this list is internal
and may change without a version bump. All three parts follow
`docs/contract.md`; `conformance/` pins their behaviour.

## Canonical fields

### Rust tool (crate `instrukt-ai-logs`)

- `instrukt-ai-logs <app> [--since 10m] [-f|--follow] [--grep PATTERN] [--exclude PATTERN] [--logs/--include stem1,stem2] [--version]`
  — print the app's log lines from the `--since` window, merged by timestamp
  across every `*.log*` file in the app's directory (rotated and gzipped
  archives included), keep (`--grep`) then drop (`--exclude`) by Python-style
  regex, select exact stems (`--logs`), then optionally follow the live
  `<stem>.log` files. Pinned by `conformance/reader`.
- `instrukt-ai-log-setup [--help|--version]` — the runtime-install entry:
  idempotently write the per-user rotation conf and scheduler unit, load the
  scheduler, print the status report (log root, conf path, scheduler file,
  problems or `status: healthy`). Exit 1 when a problem is found; any other
  argument is a usage error (exit 2). User-space only; run once per machine,
  directly or via `make install-runtime`. Pinned by `conformance/setup`.

Installed by the release's shell installer into `$XDG_BIN_HOME` or
`~/.local/bin`, or by `cargo install instrukt-ai-logs`.

### Rust writer crate (`instrukt-ai-logging`)

- `init(app, source) -> Result<PathBuf, Error>` — install the global `tracing`
  subscriber writing the contract's lines, bridge the `log` crate, return the
  log file path. A second call or an existing global subscriber is an `Err`.
- `Builder::new(app)` with `.source(..)`, `.max_message_chars(..)`,
  `.app_prefix(..)`, then `.init()` or `.layer()` (a `Layer` plus the path).
- `resolve_log_root()`, `resolve_log_dir(app)`, `resolve_log_file(app, source)`
  — locations, nothing created. The tool resolves its paths through these.
- `Error`, `DEFAULT_MAX_MESSAGE_CHARS`.

### Python writer SDK (`instrukt_ai_logging/__init__.py::__all__`)

- `__version__` — string resolved via `importlib.metadata` at import time.
- `configure_logging(name, *, source=None, max_message_chars=4000) -> Path` —
  configure root logging per the contract; returns the resolved log file path.
- `get_logger(name) -> InstruktAILogger` — return a logger that accepts
  arbitrary `**kv` fields.
- `InstruktAILogger` — `logging.Logger` subclass that captures `**kv` kwargs
  and routes them through `extra={"kv": ...}` to the formatter.
- `InstruktAILoggerProtocol` — `runtime_checkable` Protocol describing the
  same surface for type-only contexts.
- `resolve_log_file(app_name, *, source=None) -> Path` — canonical path
  builder.
- `resolve_log_files(app_name, *, stems=None) -> list[Path]` — enumerate the
  app's log files, optionally filtered by stem, including rotated `.gz`
  archives.
- `TRACE` — integer level value `5`, registered with `logging.addLevelName`
  and assigned as `logging.TRACE` for downstream introspection.

The Python package declares no console scripts.

## Allowed values

- App names are normalized three ways (env prefix, logger prefix, filesystem
  app name); any string with at least one ASCII alphanumeric character
  produces a valid contract.
- `--since` accepts an integer suffixed with `s|m|h|d` (case-insensitive).
- `--logs` (alias `--include`) is a comma-separated list of stems; matching is
  exact-stem (`cron` matches `cron.log`, `cron.log.0`, never `cron-backup.log`),
  with no glob.

## Known caveats

- Importing the Python package does not configure logging. Callers must invoke
  `configure_logging(...)` explicitly during process start; otherwise no
  handler is attached and records fall back to Python's default behavior.
- `configure_logging` replaces (does not extend) `logging.root.handlers`. Code
  that pre-attaches handlers will lose them.
- No writer sets up rotation. A machine that has not run
  `instrukt-ai-log-setup` writes logs without a rotation scheduler, and log
  growth is unbounded until that one-time setup step runs.
- The Rust writer emits event fields only; span fields and span context are
  not written.
- `__version__` falls back to a placeholder only when the package is loaded
  outside an installed distribution; other metadata errors propagate.
