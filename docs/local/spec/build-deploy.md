---
id: "local/projects/python-logger/spec/build-deploy"
type: "spec"
scope: "project"
description: "Build and release pipeline: ci.yml on every pull request and push to main builds and verifies every release artifact; a manual Release chain dispatch on main publishes one prepared version to the GitHub Release, crates.io and PyPI."
---

# Build and Deploy — Spec

## What it is

The pipeline that verifies every commit's release artifacts and, on a manual
dispatch, publishes one prepared version in three places: binaries and
installer on the GitHub Release, the two crates on crates.io, and the Python
SDK on PyPI. `docs/publishing.md` is the operator guide, including rollback
semantics and recovery.

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

- `ci.yml` — on pull requests to `main`, pushes to `main`, and called by the
  release chain: actionlint and shellcheck, version agreement, the changelog
  masked-text guard, Python gates, Rust gates on Linux and macOS, and
  `build-verify.yml`. Publishes nothing and references no publishing secret.
- `build-verify.yml` — `workflow_call`, read-only: dist builds for the four
  targets, global artifacts and manifest, the exact 13-file asset set and its
  checksums, an installer install test with a fixture read on all four
  runners, `cargo package`, and the checked wheel and sdist. Artifacts
  `release-assets` and `python-dist`; output `version`.
- `release-chain.yml` (`Release chain`) — `workflow_dispatch` only, input
  `mode` (`dry-run` default, `release` on `main` only): `preflight`,
  `checks` (`ci.yml`), then in release mode `github-release` (draft with the
  verified assets, read back by name and digest), `crates`, `pypi`,
  `finalize` (un-draft, which creates the tag at the dispatched commit),
  `post-verify`, `post-verify-registries`; `rollback` undoes this run's
  release, tag and crate versions when a step from `github-release` to
  `finalize` did not succeed; `report` summarizes both modes.

## Allowed values

- One version for the repository, in `pyproject.toml`, the root `Cargo.toml`
  (`[workspace.package]` and the `instrukt-ai-logging` pin), `uv.lock` and
  `Cargo.lock`; `python3 .github/scripts/version.py check` verifies they
  agree, and `version.py set X.Y.Z` sets them all in a pull request. No
  workflow changes it.
- A release publishes exactly the version the files carry; its notes are the
  non-empty `## X.Y.Z` section of `CHANGELOG.md`.
- `mode` is `dry-run` or `release`; release mode requires
  `workflow_dispatch` on `refs/heads/main`.

## Known caveats

- A yank burns a version number on crates.io and PyPI; after a rollback that
  yanked anything, the next release is a new patch version.
- A PyPI upload cannot be undone with an API token; a part-way PyPI failure
  leaves the uploaded file for a person to yank.
- The crates.io account's verified-email state is not readable with an API
  token; it is confirmed by hand before a release, and a missing one fails
  `crates` before any registry changes.
- Rollback assumes immutable releases stay disabled for the repository.
