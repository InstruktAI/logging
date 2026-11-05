---
id: "local/projects/python-logger/spec/build-deploy"
type: "spec"
scope: "project"
description: "Build and release pipeline: ci.yml on PRs and pushes; on push to main an inspector-decided bump (or none), the release commit built and verified at its final version before anything is visible, then the tag, the GitHub Release with exactly the verified files, crates.io and PyPI."
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
- Binaries (`dist-workspace.toml`, dist 0.33.1, `hosting = "github"`, no
  dist-generated CI): aarch64/x86_64 macOS and x86_64/aarch64 Linux gnu,
  sha256 checksums, the shell installer `instrukt-ai-logs-installer.sh`
  installing into `$XDG_BIN_HOME` or `~/.local/bin`. A release carries exactly
  13 files: 4 archives and their `.sha256`, `sha256.sum`, the installer,
  `source.tar.gz` and its `.sha256`, and `dist-manifest.json`.

Workflows (`.github/workflows/`):

- `ci.yml` — on pull requests to `main`, on pushes to `main`, and reused as
  the release checks: actionlint, version agreement, Python gates, Rust gates
  on Linux and macOS, and `build-verify.yml` on the tree as it is (the release
  call turns that off; it verifies the release commit itself). Never
  references a publishing secret.
- `build-verify.yml` — reusable, `contents: read`, no secrets: dist builds for
  the four targets, the global build, the exact planned 13-file set
  (`.github/scripts/dist-assets.sh` over `dist plan`) with every checksum and
  both binaries per archive, the shell installer on all four runners from a
  local mirror (`--version` and one fixture read), `cargo package`, and the
  wheel and sdist (`twine check`). Uploads `release-assets` and `python-dist`.
- `auto-release.yml` — on push to `main` (re-dispatched as `workflow_dispatch`
  for the inspector action; input `dry_run`, default false): `gate`
  (`resume` when HEAD is a release commit carrying its tag, else `release`),
  `inspect` (bump `minor`, `patch` or `none`; `none` ends the run), `checks`
  (`ci.yml`), `version` (bump and commit locally only; the commit travels as a
  patch with its SHA), `preflight`, `build-verify`, `land` (push main and the
  tag atomically, fast-forward only), `github-release` (draft, upload the
  verified files, check names and digests, publish), then `publish.yml` for
  crates.io and PyPI. Resume mode never rebuilds or uploads: it passes when
  the GitHub Release and both registries are complete and otherwise points
  back to "Re-run failed jobs" on the original run. A dry run stops after
  `build-verify`.
- `publish.yml` — called by `auto-release.yml` and dispatchable on a tag for
  recovery. A `release` job first requires the tag's GitHub Release to be
  public with exactly the planned asset set. Then crates.io
  (`CARGO_REGISTRY_TOKEN`, `instrukt-ai-logging` then `instrukt-ai-logs`), then
  PyPI (`PYPI_TOKEN`; in the chain exactly the verified `python-dist` files, on
  a manual dispatch a wheel and sdist rebuilt from the tag), as separate jobs
  that skip what is already published.

## Allowed values

- `bump` from the inspector is `minor`, `patch` or `none`, and
  `breaking: true` requires `minor`; any other value, or a result missing a
  required field, fails `inspect` and nothing is released.
- One version for the repository, in `pyproject.toml`, the root `Cargo.toml`
  (`[workspace.package]` and the `instrukt-ai-logging` pin), `uv.lock` and
  `Cargo.lock`; `python3 .github/scripts/version.py check` verifies they
  agree. Only the release bumps it.

## Known caveats

- `land` pushes directly to `main` as `github-actions[bot]`; branch
  protection must allow this principal or the release fails at the push
  (nothing is visible then).
- If `main` moves while a release runs, `land` refuses the push and nothing is
  released; the next push to `main` releases.
- crates.io requires a verified account email for uploads and reports it only
  to website sessions, never to an API token, so the preflight does not check
  it. The owner has confirmed the publishing account's email is verified
  (v0.7.2 published both crates with the current token).
- After `land` the tag stays; recovery is "Re-run failed jobs" on the same run
  (`docs/publishing.md`). A re-run of `land` after a push whose
  acknowledgement was lost reports "already landed".
