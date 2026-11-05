---
id: "local/projects/python-logger/spec/dependencies"
type: "spec"
scope: "project"
description: "Runtime and development dependencies: the Python SDK is stdlib-only by design; the Rust writer crate builds on tracing; the Rust tool adds clap, fancy-regex, flate2 and friends."
---

# Dependencies — Spec

## What it is

The dependency surface declared in `pyproject.toml` (Python SDK) and the
`Cargo.toml` manifests (Rust crates). The Python SDK is intentionally
stdlib-only at runtime so that consuming services do not inherit transitive
dependency conflicts from their logger; the Rust writer keeps to `tracing`,
`regex` and `time` for the same reason.

## Canonical fields

Build backend (`[build-system]`):

- `requires = ["setuptools>=69", "wheel"]`
- `build-backend = "setuptools.build_meta"`

Runtime dependencies (`[project] dependencies`):

- _empty_ — no runtime dependencies. The SDK uses only Python stdlib
  modules (`logging`, `logging.handlers.WatchedFileHandler`, `pathlib`,
  `dataclasses`, `datetime`, `re`, `os`, `typing`, `importlib.metadata`).

Development dependencies (`[dependency-groups] dev`):

- `pytest>=8`, `pytest-timeout>=2.2.0`, `syrupy>=4`
- `ruff>=0.8`
- `pyright>=1.1`
- `mypy>=1.10`

Rust writer crate (`crates/instrukt-ai-logging/Cargo.toml`): `tracing`,
`tracing-subscriber`, `tracing-log` (the `log` bridge), `regex` (redaction), `time`.

Rust tool (`crates/instrukt-ai-logs/Cargo.toml`): `instrukt-ai-logging`
(path rules), `clap` (argparse-shaped CLI), `fancy-regex` (Python-style
lookaround and backreferences), `flate2` (gzip archives), `ctrlc`, `rustix`
(uid for launchd), `time`. Dev: `serde_json`, `tempfile`.

Toolchain expectations:

- Lockfile managed by `uv` (`uv.lock` checked in; `chore(deps): refresh uv.lock`
  appears in recent commit history).
- Sync dev deps with `uv sync --group dev` (the `dev` group installs by
  default; the Makefile `install` target uses `uv run --group dev`).
- `Cargo.lock` is checked in; the Rust toolchain is pinned in CI, with
  `rust-version = "1.88"` as the crates' minimum.

CI-only tools (not declared in `pyproject.toml`):

- GitHub Actions, all pinned to commit SHAs in `.github/workflows/`
  (checkout, setup-python, setup-uv, rust-toolchain, upload/download-artifact,
  `anthropics/claude-code-action`, `pypa/gh-action-pypi-publish`), plus dist
  0.33.1 (`.github/actions/install-dist`) and actionlint.
- Renovate is configured (`renovate.json` at repo root) for automated updates.

## Allowed values

- Python interpreter: `>=3.11` (`requires-python` in `[project]`,
  `target-version = "py311"` in `[tool.ruff]`).
- Adding a Python runtime dependency, or a dependency to the Rust writer
  crate, would change this snippet and the
  `local/projects/python-logger/policy/conventions` snippet — both must be
  updated together.

## Known caveats

- The package keeps `py.typed` under `instrukt_ai_logging/` and declares it in
  `[tool.setuptools.package-data]` so downstream type checkers see public
  signatures. Removing it would silently break consumer type checks.
- `__version__` reports the installed distribution's version without scanning
  the other installed distributions. When this package is not installed as a
  distribution, it falls back to the placeholder owned by
  `instrukt_ai_logging/__init__.py`; any other metadata error propagates.
