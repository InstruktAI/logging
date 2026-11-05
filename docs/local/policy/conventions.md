---
id: "local/projects/python-logger/policy/conventions"
type: "policy"
scope: "project"
description: "Code conventions: snake_case Python with type hints, ruff at line-length 120, stdlib-only runtime for the SDK; Rust 2024 with rustfmt, clippy -D warnings, no panics outside tests; one contract pinned by conformance cases."
---

# Project Conventions — Policy

## Rules

- **Python target:** `python_requires = ">=3.11"` and `target-version = "py311"`
  in `[tool.ruff]`. Use only syntax and stdlib features available on 3.11+.
- **Stdlib only at runtime.** `[project] dependencies = []`. Add a runtime
  dependency only with explicit justification — the project's value
  proposition includes "drop-in, no transitive deps".
- **Naming:**
  - Modules and functions: `snake_case`
    (`configure_logging`, `resolve_log_files`).
  - Classes: `PascalCase` (`InstruktAILogger`, `LogfmtFormatter`,
    `UtcMillisFormatter`, `LoggingContract`).
  - Internal helpers: leading underscore (`_normalize_env_prefix`,
    `_ThirdPartySelectorFilter`, `_REDACTION_PATTERNS`).
  - Distribution name uses hyphens (`instruktai-python-logger`); module name
    uses underscores (`instrukt_ai_logging`).
- **Imports:**
  - `from __future__ import annotations` at the top of every source module.
  - stdlib first, then first-party (`from instrukt_ai_logging.logging import ...`).
- **Type hints required.** All public functions and classes are annotated;
  `py.typed` is shipped under `instrukt_ai_logging/` and declared in
  `[tool.setuptools.package-data]`.
- **Rust:** edition 2024, `rust-version = "1.85"`, `unsafe_code` forbidden
  workspace-wide, `dbg!`/`todo!`/`unimplemented!` denied. No `unwrap`/`expect`
  or panics outside tests; fallible paths return `Result` (the writer's
  `Error`, the tool's `Exit`). Format with `cargo fmt --all`; lint with
  `cargo clippy --workspace --all-targets -- -D warnings`.
- **One contract, two writers.** A behaviour change to the line format, path,
  env vars or selection lands in both writers, in `docs/contract.md` and as
  `conformance/writer` cases in the same change. Tool behaviour lands as
  `conformance/reader` or `conformance/setup` cases; a deliberate departure
  from 0.6.x is a `tool-*` case and a CHANGELOG entry.
- **Formatting and lint (Python):**
  - Ruff with `line-length = 120`, `target-version = "py311"`.
  - Lint: `uv run ruff check .` (or `telec code lint`).
  - Format: `uv run ruff format .` (or `telec code format`).
- **Tests:**
  - `pytest` with `addopts = "-q"` and `testpaths = ["tests"]`; `cargo test --workspace`.
  - Invariants in `local/projects/python-logger/design/test-strategy`.
  - Run via `make test`, `uv run pytest` / `cargo test --workspace`, or `telec code test`.
- **Logging output format:** single-line logfmt-ish, ordered as
  `<UTC ms timestamp> level=... logger=... msg="..." [<sorted **kv pairs>] [exc=...]`.
  Do not introduce JSON-first formatting unless a consumer explicitly requires it
  (per `docs/design.md`).

- **Env-var contract is stable:** `{APP}_LOG_LEVEL`,
  `{APP}_THIRD_PARTY_LOG_LEVEL`, `{APP}_THIRD_PARTY_LOGGERS`,
  `{APP}_MUTED_LOGGERS` — these four per-app variables are the entire
  contract. The log location is a fixed rule
  (`$XDG_STATE_HOME/instrukt-ai/{app}/`, fallback `~/.local/state`), not an
  env knob; no override variable exists. Add capability behind existing knobs
  before introducing a new env var (per `AGENTS.md`).

- **Install split (`install` / `install-runtime`):** `make install` performs
  local development setup only (dependencies, tooling, git hooks); it never
  touches host or user machinery. Wiring the per-user rotation scheduler into
  launchd/systemd is an explicit, opt-in step: `make install-runtime`, running
  the Rust `instrukt-ai-log-setup` from this checkout. Consuming services
  invoke the installed `instrukt-ai-log-setup` binary from their own
  deploy/setup — the binary, not the Make target, is the consumer-facing
  entry. (Mirrors the
  itsUP `install`/`install-runtime` convention at per-user granularity.)

- **Commits and CI:** No `pre-commit`-framework config exists for this
  project; commit-time and test-turn-boundary gating are TeleClaude platform
  mechanisms rather than project-owned tooling (see Enforcement).

## Rationale

- A logging library in the import path of every service must not cause
  dependency churn — hence the stdlib-only runtime invariant.
- A small, stable env-var surface is easier to teach, document, and grep for
  across services than a sprawling option set.
- `from __future__ import annotations` keeps signatures cheap to read on 3.11
  and ready for any deferred-eval changes upstream.
- Single-line logfmt output is the precondition for a useful tail window —
  multi-line records would defeat the contract's primary intent.

## Scope

Applies to all source files under `instrukt_ai_logging/` and `crates/`, all
tests under `tests/`, `crates/*/tests/` and `conformance/`, and any
documentation or tooling that affects the published API.

## Enforcement

- Lint (`telec code lint`: ruff, pyright, mypy, clippy) — gated by
  `telec code commit`, and by CI (`.github/workflows/ci.yml`).
- Tests (`telec code test`: pytest and cargo test) — gated by TeleClaude's
  checkpoint system for the scope declared in `teleclaude.yml`, and by CI
  (`.github/workflows/ci.yml`).
- `[tool.setuptools.packages.find]` is scoped to `include = ["instrukt_ai_logging*"]`
  so accidental top-level modules cannot be packaged.
- The hardcoded-HOME-path guardrail (part of `telec code lint`) blocks a
  `*.md` commit containing an _unescaped_ absolute home path ending in
  `.teleclaude/...`; an `@`-escaped reference (the form this doc's own
  TeleClaude path mentions use) is exempt (see TeleClaude's
  `tools/lint/home_paths.py`).

## Exceptions

- Adding a runtime dependency requires explicit reasoning in the commit body
  and an update to `pyproject.toml` plus `uv.lock`.
- Public API changes (anything in `instrukt_ai_logging/__init__.py::__all__`,
  the Rust crates' public API, the binaries' arguments and output, the env-var
  names, or the log file layout) are breaking and require a major
  or minor version bump, prepared with `version.py set` in a pull request
  (`docs/publishing.md`).
