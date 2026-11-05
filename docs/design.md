# Design

This document captures the design intent and constraints of the InstruktAI
logging standard: the Python writer SDK (`instrukt_ai_logging`), the Rust writer
crate (`instrukt-ai-logging`) and the Rust tool (`instrukt-ai-logs`,
`instrukt-ai-log-setup`) that reads the logs and wires their rotation.

The exact behaviour both writers (Python and Rust) implement — line format, log
location, env vars and selection, redaction, truncation, file handling and
rotation — is specified in [`contract.md`](contract.md), with `conformance/` as
its executable form.

## Primary intent

Logs must remain meaningfully queryable even when a consumer (especially an AI) only reads a limited “tail window” (e.g. the last N characters). The most common failure mode is third‑party debug/info spam pushing important service logs out of the window.

## Core decisions

### One primary log stream per service

- Prefer a single primary file per service (rotated) instead of many per-component files.
- Query by logger namespace and key/value fields inside the log line.

### Local-first storage; predictable path

- Canonical target path is `$XDG_STATE_HOME/instrukt-ai/{app}/{app}.log`
  (fallback `~/.local/state`, per the XDG Base Directory spec), identical on
  macOS and Linux.
- The path is always user-writable by construction — no installer step, no
  sudo, no elevated permissions, and no override variable.
- Both writers fail fast: an unopenable log file/dir raises out of
  `configure_logging(...)` (Python) or is an `Err` from `init`/`Builder::init`
  (Rust) rather than degrading, since a process that cannot write its logs
  must not start blind.
- Path rules live once per language: `resolve_log_dir`/`resolve_log_root` in
  the Rust crate serve both the Rust writer and the tool.

### Keep logs human-readable (text), but structured enough for machines

- Use a consistent single-line format.
- Include lightweight key/value fields when helpful.
- Avoid JSON-first unless a consumer explicitly requires it.

### Minimal knobs; stable contract

Per-app prefix contract:

- `{APP}_LOG_LEVEL` (service logs)
- `{APP}_THIRD_PARTY_LOG_LEVEL` (verbosity for third-party logs when enabled)
- `{APP}_THIRD_PARTY_LOGGERS` (comma-separated logger prefixes to "spotlight")
- `{APP}_MUTED_LOGGERS` (comma-separated logger prefixes to force to WARNING+)

Selection semantics:

- If `{APP}_THIRD_PARTY_LOGGERS` is set, only those prefixes are allowed to emit at `{APP}_THIRD_PARTY_LOG_LEVEL`; all other third-party logs are forced to WARNING+.
- If `{APP}_THIRD_PARTY_LOGGERS` is unset/empty, `{APP}_THIRD_PARTY_LOG_LEVEL` applies to all third-party logs.
- `{APP}_MUTED_LOGGERS` forces matched prefixes to WARNING+ regardless of whether they are app or third-party loggers. Use this to silence noisy app sub-namespaces.

### One tool, two writers

- The reader and the rotation setup are one Rust tool, installed once per
  machine, independent of any service's language or virtualenv.
- Each language gets a thin writer that follows `contract.md`; the shared
  `conformance/writer` cases keep their output identical.

### Infra logs are separate

Application logs (Python `logging`, Rust `tracing`) and service-manager/process logs (stdout/stderr, crash-before-logging) are different layers. The unified tool can expose both, but the storage contract for this project focuses on application logs.

## Non-negotiables

<!-- planned-change:fix-launchd-rebootstrap-eio-spurious-rotation-warning -->

- Rotation must exist and run entirely as the producing user: a per-user
  rotation conf plus a per-user scheduler (launchd LaunchAgent on macOS,
  systemd user timer on Linux), ensured transparently and idempotently by
  `configure_logging(...)`. No `/etc` configuration, no sudo, no system
  rotation daemon — root never touches these files, so rotated/recreated
  logs cannot come back root-owned.

<!-- change:fix-launchd-rebootstrap-eio-spurious-rotation-warning -->

- Rotation must exist and run entirely as the producing user: a per-user
  rotation conf plus a per-user scheduler (launchd LaunchAgent on macOS,
  systemd user timer on Linux), wired once and idempotently by the explicit
  runtime-install step (`instrukt-ai-log-setup`, the Rust tool; `make
  install-runtime` in this repository) — never by a writer
  (`configure_logging(...)` in Python, `init` in Rust), which ensures only
  the log directory and file. No `/etc` configuration, no sudo, no system rotation daemon —
  root never touches these files, so rotated/recreated logs cannot come back
  root-owned.

<!-- /planned-change:fix-launchd-rebootstrap-eio-spurious-rotation-warning -->

- No secret leakage (redaction must be part of the standard).
- Large payloads must not dominate the tail window (truncate very long lines).
