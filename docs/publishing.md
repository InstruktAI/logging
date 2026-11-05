# Publishing

One version covers the whole repository: `pyproject.toml`, the root
`Cargo.toml` (`[workspace.package] version` and the `instrukt-ai-logging` pin
in `[workspace.dependencies]`), `uv.lock` and `Cargo.lock`.
`python3 .github/scripts/version.py check` verifies they agree. A release of
`X.Y.Z` publishes three things:

- **GitHub Release `vX.Y.Z`** with exactly these 13 assets:
  `instrukt-ai-logs-<target>.tar.xz` and its `.sha256` for each of
  aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu and
  aarch64-unknown-linux-gnu (each archive holds `instrukt-ai-logs` and
  `instrukt-ai-log-setup`), `sha256.sum`, the shell installer
  `instrukt-ai-logs-installer.sh`, `source.tar.gz` and its `.sha256`, and
  `dist-manifest.json`.
- **crates.io**: `instrukt-ai-logging` (writer crate), then `instrukt-ai-logs`
  (the tool).
- **PyPI**: `instruktai-python-logger` (the Python writer SDK).

A release happens only when someone dispatches the `Release chain` workflow
on `main` with `mode=release`. A push, a merge (by a bot or a person), a tag,
a pull request or a schedule never releases anything, and no workflow ever
commits to `main`.

## Preparing a version

A version is prepared in an ordinary pull request:

```sh
python3 .github/scripts/version.py set X.Y.Z   # pyproject.toml, Cargo.toml, Cargo.lock, uv.lock
```

`set` re-locks with `cargo update --workspace --offline` (run `cargo fetch`
first if it reports a missing package) and `uv lock`, then runs `check`. In
the same pull request, add a `## X.Y.Z` section to `CHANGELOG.md`: it becomes
the release notes, so it must exist and say something. CI rejects changelog
text the Actions runner masks as a credential (the word "Bearer" followed by a
word); reword it.

Every pull request and every push to `main` runs `ci.yml`, which builds and
verifies every release artifact (see below). A green pull request has already
proved that the binaries build for all four targets, the asset set is exact,
and the installer installs working binaries on all four runners.

## Dry run, then release

After the version pull request is merged:

```sh
gh workflow run release-chain.yml --ref main                  # mode=dry-run (default)
gh workflow run release-chain.yml --ref main -f mode=release
```

A dry run (on any branch) runs `preflight` and `checks` and publishes nothing.
It fails wherever a real release would fail, with one exception: an existing
tag or GitHub Release for the version is reported and passes, so a dry run can
re-check a version already released. The run summary says which mode ran.
`mode=release` from any ref other than `main` fails at once.

Only one release chain runs at a time (concurrency group `release-chain`);
a second dispatch waits for the first.

## What each job verifies

`release-chain.yml` runs one gated chain. A job that fails stops every job
after it.

1. **`preflight`** (read-only token) computes release mode once, from
   `workflow_dispatch && mode == release && ref == refs/heads/main`; every
   side-effecting job is gated on that output. It checks:
   - `version.py check`, and that `CHANGELOG.md` has a non-empty `## X.Y.Z`
     section and no masked text;
   - no tag `vX.Y.Z` and no published GitHub Release `vX.Y.Z` (the read-only
     token cannot list drafts; `github-release` re-checks with drafts
     visible);
   - crates.io has neither `instrukt-ai-logging X.Y.Z` nor
     `instrukt-ai-logs X.Y.Z`, and PyPI has no `instruktai-python-logger
     X.Y.Z`. A yanked version counts as present: it can never be published
     again;
   - `CARGO_REGISTRY_TOKEN` authenticates on crates.io, and `PYPI_TOKEN` is
     set.
2. **`checks`** runs `ci.yml`: actionlint and shellcheck, the version and
   changelog guards, the Python gates (ruff, pyright, mypy, pytest), the Rust
   gates on Linux and macOS (fmt, clippy, tests), and `build-verify.yml`:
   - `dist build --artifacts=local` for each of the four targets on its
     runner, then `dist build --artifacts=global` (installer, `sha256.sum`,
     source tarball) and `dist manifest` (`dist-manifest.json`);
   - the assembled directory must equal exactly the set
     `.github/scripts/dist-assets.sh` derives from `dist plan` (13 files),
     every `.sha256` and `sha256.sum` must check, and every archive must hold
     both binaries (`.github/scripts/verify-assets.sh`);
   - on each of the four runners: serve the assets over a local HTTP mirror,
     run the installer with `INSTRUKT_AI_LOGS_DOWNLOAD_URL` pointing at it, a
     fresh `HOME` and `XDG_*` dirs and `INSTRUKT_AI_LOGS_NO_MODIFY_PATH=1`;
     the installed `instrukt-ai-logs --version` must print exactly
     `instrukt-ai-logs X.Y.Z`; then one read of fixture logs with `--since
     5m` and `--grep`, asserting exact stdout and exit 0
     (`.github/scripts/fixture-read.sh`). `instrukt-ai-log-setup` runs with
     `--version` only, never against the runner's scheduler;
   - `cargo package --workspace --locked`; `uv build`, then the wheel and
     sdist must both carry `X.Y.Z` and the wheel no `entry_points.txt`
     (`.github/scripts/check-python-dist.py`), and `twine check --strict`
     must pass.

   Its artifacts `release-assets` and `python-dist` are the exact files the
   later jobs publish; nothing is rebuilt. Until here, nothing outside the
   run has changed.
3. **`github-release`** (release mode only, `contents: write`) checks again,
   now with drafts visible, that no release or tag `vX.Y.Z` exists, then
   creates the **draft** `vX.Y.Z` targeting the dispatched commit, with the
   changelog section as notes and every verified asset. It reads the draft
   back: the asset names must equal the verified set and each asset's
   `digest` must equal the local sha256, and the tag must still not exist (a
   draft creates no tag).
4. **`crates`** publishes `instrukt-ai-logging`, then `instrukt-ai-logs`,
   with `cargo publish --locked`.
5. **`pypi`** uploads the verified wheel and sdist from `python-dist`.
6. **`finalize`** publishes the draft and marks it latest; that creates the
   tag `vX.Y.Z` at the dispatched commit. It then confirms the tag points at
   exactly that commit.
7. **`post-verify`** runs the README's install line
   (`curl -LsSf https://github.com/InstruktAI/logging/releases/latest/download/instrukt-ai-logs-installer.sh | sh`)
   in a clean `HOME` on Linux and macOS, checks the installed `--version` and
   the fixture read, and **`post-verify-registries`** checks that crates.io
   serves both crates and PyPI serves the package at `X.Y.Z` (bounded retries
   for propagation). These fail loudly; they never roll back.
8. **`report`** writes the mode, the version, every job's result and the
   verified asset table (name, size, sha256) to the run summary, in both
   modes.

## Rollback and residual risk

`rollback` runs in release mode when `github-release`, `crates`, `pypi` or
`finalize` did not succeed (failed or cancelled), once `github-release`'s
guard has proved no `vX.Y.Z` release or tag existed. It logs each action to
the run summary:

- it deletes every GitHub Release `vX.Y.Z` (the draft, or the published
  release if `finalize` got that far) and the tag `vX.Y.Z` if it exists;
- it yanks each crate that crates.io serves at `X.Y.Z` (preflight proved
  neither existed, so any found is this run's).

It relies on immutable releases staying disabled for the repository: with
them on, a published release's tag and assets cannot be deleted.

Residual risks, stated plainly:

- **A yank burns the version number.** A yanked crate version (and a yanked
  or deleted PyPI file) can never be published again. After a rollback that
  yanked anything, prepare the next patch version; never re-release the same
  one.
- **PyPI cannot be undone with an API token.** It goes last among the
  registries for that reason, and its failure modes are prevented up front:
  the version is absent (preflight), the token is set (preflight) and the
  files pass `twine check` (build-verify). If the PyPI upload itself fails
  part-way (the wheel uploaded, the sdist not), rollback still deletes the
  draft and yanks the crates, but the uploaded PyPI file stays: yank it by
  hand on pypi.org.
- **The crates.io account's email cannot be checked with a token.** crates.io
  only shows the verified-email state to a browser session (`GET /api/v1/me`
  does not accept API tokens), and refuses to publish without it. Before a
  release, confirm the publishing account's email is verified at
  <https://crates.io/settings/profile>. If it is not, `crates` fails on the
  first crate before anything is published to a registry, and rollback
  deletes the draft.
- **`instrukt-ai-logs` is published after `instrukt-ai-logging`.** If the
  second publish fails, rollback yanks the first.
- **After `finalize` succeeds, nothing rolls back automatically.** The release
  is public; a failing `post-verify` needs a person: yank the crates
  (`cargo yank --version X.Y.Z <crate>`), yank the PyPI release, and delete
  or edit the GitHub Release as the failure requires.

## Recovery

- **`preflight` or `checks` failed**: nothing changed. Fix the cause in a pull
  request and dispatch again.
- **Something failed after the draft and `rollback` succeeded**: the
  summary lists what it undid. If no registry was touched (no crate yanked,
  nothing on PyPI), fix the cause and dispatch the same version again.
  Otherwise the version number is burnt: prepare the next patch version.
- **`rollback` itself failed**: its log names the action that failed. Finish
  it by hand:

  ```sh
  gh release delete vX.Y.Z -R InstruktAI/logging --cleanup-tag -y
  cargo yank --version X.Y.Z instrukt-ai-logs
  cargo yank --version X.Y.Z instrukt-ai-logging
  ```

- **`post-verify` failed after a published release**: see the last residual
  risk above.
