---
id: "local/projects/python-logger/spec/configuration"
type: "spec"
scope: "project"
description: "Runtime configuration contract of both writers (Python SDK and Rust crate) and the tool: four per-app selection variables and an optional per-app log directory."
---

# Configuration — Spec

## What it is

The full runtime configuration surface of the two writers (Python
`configure_logging(...)`, Rust `init`/`Builder`) and the tool. There are no
config files — every knob is an environment variable. Per-app variables are
namespaced by an `{APP}` prefix derived from the app name passed to the writer.
A nonempty `{APP}_LOG_DIR` selects the complete app directory. Unset or empty
preserves the XDG/default-home location. The exact rules are `docs/contract.md`.

## Canonical fields

Per-app environment variables (where `{APP}` is the normalized env prefix,
e.g. `TELECLAUDE` for `configure_logging("teleclaude")` or
`init("teleclaude", None)`):

- `{APP}_LOG_LEVEL` — level for loggers under the app's prefix
  (e.g. `teleclaude.*`, in Rust also `teleclaude::*`). Default: `INFO`.
  Accepted: `TRACE`, `DEBUG`, `INFO`, `WARN`, `WARNING`, `ERROR`, `CRITICAL`,
  `FATAL`, `NOTSET` (no own level: app loggers fall back to the root
  threshold).
- `{APP}_THIRD_PARTY_LOG_LEVEL` — level applied to third-party loggers when
  they are allowed to emit. Default: `WARNING`.
- `{APP}_THIRD_PARTY_LOGGERS` — comma-separated logger prefixes to "spotlight"
  (e.g. `httpcore,telegram`). When set, only these non-app prefixes may emit at
  `{APP}_THIRD_PARTY_LOG_LEVEL`; all other third-party loggers are forced to
  WARNING+ via the root logger. When unset, the third-party level applies to
  all non-app loggers.
- `{APP}_MUTED_LOGGERS` — comma-separated logger prefixes forced to WARNING+.
  Applies to both app and third-party prefixes; use it to silence noisy
  sub-namespaces (e.g. `teleclaude.cli.tui`).

The separate `{APP}_LOG_DIR` variable uses the same normalized app prefix.
It selects exactly the directory containing all sources, not a parent root:
`INSTRUKT_LOG_DIR=/work/logs` gives `/work/logs/sync.log` for source `sync`.
Existing path normalization and `~` expansion apply; values are not trimmed or
restricted to absolute paths. Other apps and additional logger prefixes do not
change it. Readers must receive the same variable as writers.

These four variables retain their selection semantics. The writers and the
tool additionally _honor_ `XDG_STATE_HOME` because the log location follows the XDG
Base Directory spec — that is platform compliance, not an app knob.

In-process configuration (`configure_logging(...)` keyword args; the Rust
`Builder` has the same knobs plus `app_prefix(...)` for sibling crates that
count as the app's own):

- `source: str | None` — logical identity of the producing process within an
  app. When `None`, the file is `<app>.log`. When set (e.g. `"cron"`,
  `"docs-watch"`), the file is `<source>.log` under the same app directory.
  The `.log` extension is always appended.
- `max_message_chars: int = 4000` — per-record truncation budget; values longer
  than this are suffixed with `…(truncated)`.

Default log file path (identical on macOS and Linux):

- `$XDG_STATE_HOME/instrukt-ai/<app>/<source-or-app>.log`
  (`$XDG_STATE_HOME` falls back to `~/.local/state` when unset, per the XDG
  Base Directory spec)

## Allowed values

- Logger prefix matching is namespace prefix match on `.` (and `::` in Rust):
  `httpcore` matches `httpcore` and `httpcore.http11` but not `httpcore-x`.
  The longest matching prefix decides; a muted prefix wins a tie.
- `{APP}_THIRD_PARTY_LOGGERS` and `{APP}_MUTED_LOGGERS` parse as
  comma-separated; whitespace around items is stripped, empty items are dropped.
- Level names are case-insensitive; unrecognized names fall back to the
  default (`INFO` for app, `WARNING` for third-party).

## Known caveats

- Env vars are read **once** at initialization. Changing them in the same
  process afterwards has no effect — in Python call `configure_logging` again
  to re-apply; the Rust global `init` can run only once.
- App-prefix logger gets its level set explicitly; `{APP}_THIRD_PARTY_LOGGERS`
  entries that overlap the app prefix are skipped to avoid clobbering it.
- `{APP}_MUTED_LOGGERS` runs after spotlight assignment, so muting takes
  precedence over spotlighting for the same prefix.

- Writers create the selected directory at initialization and propagate errors;
  an unwritable override never redirects to another directory. This production
  variable changes neither `HOME` nor XDG bases and needs no test-mode switch.
- The per-user rotation scheduler is wired separately via
  `instrukt-ai-log-setup`. Its existing `root/*/*.log` rules cover the default
  shared root only; an arbitrary app directory outside that tree is not
  automatically rotated. Per-app rotation registration is not provided.
