---
id: "local/projects/python-logger/spec/build-deploy"
type: "spec"
scope: "project"
description: "Build and release pipeline: PR checks in ci.yml; on push to main an inspector-decided bump, one version for Python and Rust, then dist binaries on the GitHub Release, crates.io and PyPI for the tag."
---

# Build and Deploy — Spec

## What it is

The pipeline that turns a commit on `main` into a tagged release published in
three places: binaries and installer on the GitHub Release, the two crates on
crates.io, and the Python SDK on PyPI. `docs/publishing.md` is the operator
guide, including recovery recipes.

## Canonical fields

Local commands (`Makefile`):

- `make install` — Python dev environment (`uv`), `cargo fetch`, git hooks.
- `make install-runtime` — `cargo run --release -q -p instrukt-ai-logs --bin instrukt-ai-log-setup`.
- `make lint` — ruff, pyright, mypy, jscpd, `cargo clippy --workspace --all-targets -- -D warnings`.
- `make test` — `uv run pytest`, then `cargo test --workspace`.
- `make format` — ruff, prettier for Markdown, `cargo fmt --all`.

Builds:

- Python (`pyproject.toml`): setuptools (`requires = ["setuptools>=69", "wheel"]`),
  packages `instrukt_ai_logging*` with `py.typed`; the wheel holds the SDK only
  and declares no console scripts.
- Rust (`Cargo.toml` workspace): `crates/instrukt-ai-logging` and
  `crates/instrukt-ai-logs`; the `dist` profile (thin LTO) builds release
  binaries.
- Binaries (`dist-workspace.toml`, dist 0.33.1): aarch64/x86_64 macOS and
  x86_64/aarch64 Linux gnu, sha256 checksums, the shell installer
  `instrukt-ai-logs-installer.sh` installing into `$XDG_BIN_HOME` or
  `~/.local/bin`.

Workflows (`.github/workflows/`):

- `ci.yml` — on pull requests, and reused as the release checks: actionlint,
  version agreement, Python gates, Rust gates on Linux and macOS, `cargo
  package`, dist builds for every target, installer smoke test.
- `auto-release.yml` — on push to `main` (re-dispatched as
  `workflow_dispatch` for the inspector action): `gate` (mode `release`,
  `resume` or nothing), `inspect` (release inspector with a read-only token,
  `CLAUDE_CODE_OAUTH_TOKEN`), `checks` (`ci.yml`), `version` (bump every
  version location, commit `chore(release): vX.Y.Z`, tag, atomic push),
  `publish` (create the GitHub Release unless it exists, dispatch
  `release.yml` and `publish.yml`; never bumps).
- `release.yml` — dist-generated, dispatched with the tag; uploads binaries
  and installer to the existing GitHub Release. Never edited by hand.
- `publish.yml` — dispatched on the tag: crates.io (`CARGO_REGISTRY_TOKEN`,
  `instrukt-ai-logging` then `instrukt-ai-logs`) and PyPI (`PYPI_TOKEN`) as
  separate jobs that skip what is already published.

## Allowed values

- `bump` from the inspector is `minor` or `patch`; any other value, or a
  result missing a required field, fails `inspect` and nothing is released.
- One version for the repository, in `pyproject.toml`, the root `Cargo.toml`
  (`[workspace.package]` and the `instrukt-ai-logging` pin), `uv.lock` and
  `Cargo.lock`; `python3 .github/scripts/version.py check` verifies they
  agree. Only the release bumps it.

## Known caveats

- The `version` job pushes directly to `main` as `github-actions[bot]`;
  branch protection must allow this principal or the release fails at the
  push.
- After the tag is pushed nothing bumps again; recovery is re-running
  `publish`, or dispatching the publish workflows by hand
  (`docs/publishing.md`).
- `release.yml`'s asset upload is not idempotent after a partial upload; the
  uploaded assets must be deleted before re-running it.
