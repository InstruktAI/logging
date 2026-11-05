# Agent instructions

This repository defines the logging standard used across InstruktAI services
and ships it three ways, all following one contract (`docs/contract.md`):

- the Rust tool (`crates/instrukt-ai-logs`: binaries `instrukt-ai-logs` and
  `instrukt-ai-log-setup`),
- the Rust writer crate (`crates/instrukt-ai-logging`, a `tracing` layer),
- the Python writer SDK (`instrukt_ai_logging`, PyPI `instruktai-python-logger`).

Before changing anything, read `docs/design.md` and `docs/contract.md`.

## Scope

- Python and Rust are in scope alike. A change to the contract changes both
  writers, `docs/contract.md` and the `conformance/` cases together.
- Keep the env var contract stable:
  - `{APP}_LOG_LEVEL`
  - `{APP}_THIRD_PARTY_LOG_LEVEL`
  - `{APP}_THIRD_PARTY_LOGGERS`
  - `{APP}_MUTED_LOGGERS`
- Prefer adding capabilities behind existing knobs over introducing new knobs.

## Behavioral invariants (do not break)

- Logs remain tail-friendly: prevent third-party spam from pushing signal out of the tail window.
- Output stays human-readable, single-line per event.
- Default log destination targets `$XDG_STATE_HOME/instrukt-ai/{app}/{app}.log`
  (fallback `~/.local/state`, per the XDG Base Directory spec) — one
  predictable location, no override knob.
- Rotation is owned by the library in user space (newsyslog/logrotate run as
  the producing user); never reintroduce root-owned rotation or system `/etc`
  config.
- Logging fails fast at startup; never degrade or continue when it cannot
  initialize.
- Redact secrets and truncate very long lines (tail killers).

## Implementation notes (for future work)

- Python: stdlib `logging` only, no runtime dependencies. Rust: `tracing`
  (plus `tracing-log` for the `log` crate); no panics or `unwrap`/`expect`
  outside tests.
- Third-party selection is by logger prefix (dotted namespace; in Rust also
  `::`), e.g. `httpcore` means `httpcore.*`.
- The “ours vs third-party” boundary should be deterministic and should not require a filesystem layout change.
- One unified tool (`instrukt-ai-logs`, Rust) for consistent retrieval and time
  slicing across platforms; its behaviour is pinned by `conformance/reader`
  and `conformance/setup`.
- `conformance/` is the one fixture tree. Python runs the writer cases, the
  Rust crates run all of them. Tests use temporary HOME/XDG dirs and fake
  `launchctl`/`systemctl`; never touch real ones.

## Dev workflow expectations

- Add unit tests or conformance cases for any behavior changes.
- Python: `uv run ruff format .`, `uv run ruff check .`, `uv run pyright`,
  `uv run mypy`, `uv run pytest`.
- Rust: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` (unit, integration and conformance tests).
- Versions: `python3 .github/scripts/version.py check` (one version across
  `pyproject.toml`, `uv.lock`, `Cargo.toml` and `Cargo.lock`). Never bump by
  hand: every push to `main` runs `auto-release.yml`, whose inspector decides
  `minor`, `patch` or `none`, and which builds and verifies the release commit
  before it tags and publishes anything (`docs/publishing.md`).
