---
id: "local/projects/python-logger/design/test-strategy"
type: "design"
scope: "project"
description: "Test invariants for the Python SDK and the Rust crates: one shared conformance fixture tree, hermeticity, isolation, time budgets. Constraints on future tests — the suites themselves are their own documentation."
---

# Test Strategy — Design

## Purpose

Constrain how this repository is tested. The tests are the contract's
executable spec — they fail when the public behavior changes. This document
holds only the invariants that bind every current and future test; it does not
describe individual tests — the suites under `tests/`, `crates/*/src` (unit
tests), `crates/*/tests` and the cases under `conformance/` are their own
documentation.

## Inputs/Outputs

Inputs: the pytest suite under `tests/`, the cargo tests of both crates, and
the JSON cases under `conformance/`. Outputs: `uv run pytest` and
`cargo test --workspace` pass/fail status (both run by `make test` and
`telec code test`) that gates commits and CI.

## Invariants

- **One fixture tree, run by every implementation it describes.**
  `conformance/` (schema in `conformance/README.md`) is the only fixture tree.
  `writer/` cases run against the Python writer (`tests/conformance/`) and the
  Rust writer (`crates/instrukt-ai-logging/tests/conformance.rs`);
  `reader/` and `setup/` cases run against the built binaries
  (`crates/instrukt-ai-logs/tests/conformance.rs`). Expected values come from
  the contract, or from 0.6.x where they pin preserved behaviour; a deliberate
  departure is a `tool-*` case. A case one runner cannot run is a defect in
  the case, never a skip.
- **Feature-spec scenarios are bound by name.** A use case in a
  `docs/local/spec/feature/` snippet is bound by a conformance case or test
  whose description or doc comment names the spec and scenario id.
- **No shared mutable state across tests.** Logging is a process-wide
  singleton; Python tests that configure it snapshot and restore
  `logging.root.handlers` and `logging.root.level`, and the Python writer
  runner resets every logger level between cases. Rust writer cases go
  through a per-case layer, never the global subscriber, except the one
  dedicated `init` test.
- **Filesystem isolation via the platform's own mechanism.** Every test and
  case gets temporary `HOME`, `XDG_STATE_HOME` and `XDG_CONFIG_HOME`
  (Python: an autouse `tests/conftest.py` fixture; Rust: the runners' sandbox
  and `tempfile`). No test writes to the real `~/.local/state`, `~/.config`
  or `~/Library/LaunchAgents`.
- **No real scheduler, ever.** Setup cases run with `PATH` holding only fake
  `launchctl`/`systemctl` that record their argv and answer from the case's
  table. Third-party rotator/scheduler behavior (newsyslog, logrotate,
  launchctl, systemctl) is out of test scope — only this repository's own
  logic is tested.
- **A runner reads the fixtures of the checkout it runs in.** The Rust
  runners locate `conformance/` through the run-time `CARGO_MANIFEST_DIR`,
  because a shared cargo target dir can reuse a test binary built in another
  checkout.
- **Non-negotiable time budgets.** `pytest-timeout` enforces a 5-second
  per-test signal timeout and a 90-second session timeout
  (`[tool.pytest.ini_options]`, `--strict-config --strict-markers`); reader
  processes in the Rust runner are killed after 10 s. A hanging or
  over-budget test is a defect to fix — split, rewrite, or remove — never a
  reason to wait or raise the budget.
- **Quiet output.** `pyproject.toml` sets `addopts = "-q"` and
  `testpaths = ["tests"]`.

## Primary flows

None prescribed. Tests follow the invariants above and the conventions
already present in the suites; per-test setup recipes live in the tests
themselves, not here.

## Failure modes

- **A test that cannot satisfy the invariants does not get written.**
  Behavior that requires a real scheduler binary, real network, or the real
  user home tree is out of automated-test scope; it is verified by an
  explicit operator/deployment action instead.
