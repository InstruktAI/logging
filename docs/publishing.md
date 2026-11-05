# Publishing

One version covers the whole repository: `pyproject.toml`, the root
`Cargo.toml` (`[workspace.package] version` and the `instrukt-ai-logging` pin
in `[workspace.dependencies]`), `uv.lock` and `Cargo.lock`.
`python3 .github/scripts/version.py check` verifies they agree. Only the
release bumps it. A release publishes three things for one tag `vX.Y.Z`:

- **GitHub Release**: exactly 13 files. The `instrukt-ai-logs` +
  `instrukt-ai-log-setup` archives for aarch64/x86_64 macOS and x86_64/aarch64
  Linux (gnu), each with its `.sha256`, plus `sha256.sum`, the shell installer
  `instrukt-ai-logs-installer.sh`, `source.tar.gz` with its `.sha256`, and
  `dist-manifest.json`.
- **crates.io**: `instrukt-ai-logging` (writer crate), then `instrukt-ai-logs`
  (the tool).
- **PyPI**: `instruktai-python-logger` (the Python writer SDK).

## The rule: the shipped bytes are the verified bytes

Every push to `main` runs `Release (Auto)` (`auto-release.yml`). It builds and
verifies the release at its final version before anything becomes visible,
then publishes exactly the files it verified, in this order:

1. **`gate`** decides the mode: `resume` when HEAD is a release commit carrying
   its `v*` tag, `release` otherwise.
2. **`inspect`**: the release inspector (`anthropics/claude-code-action`,
   instructions in `.github/prompts/release-inspector.md`,
   `CLAUDE_CODE_OAUTH_TOKEN` secret) reads the changes since the last tag and
   decides `minor`, `patch` or `none`, with a rationale and the release notes.
   `breaking: true` requires `minor`.
   - **`none`** means nothing ships to consumers (CI and workflow files, docs,
     tests, dev-only tooling, Python dev-dependency lockfile changes). The run
     ends here with a job summary stating the rationale.
   - Anything that changes a shipped artifact is at least `patch`, including
     Rust dependency versions in `Cargo.lock`.
3. **`checks`**: `ci.yml` (actionlint, versions, Python and Rust gates).
4. **`version`** bumps every version location and commits
   `chore(release): vX.Y.Z` **locally only**. The commit travels to the later
   jobs as a patch artifact (`release-commit`) with its base and SHA; each job
   applies it with `git am` and fails unless it gets the same SHA.
5. **`preflight`** stops the release unless all of these hold:
   - tag `vX.Y.Z` is absent on the remote;
   - no GitHub Release `vX.Y.Z` exists, drafts included;
   - crates.io has neither `instrukt-ai-logging` nor `instrukt-ai-logs` at
     `X.Y.Z`, and PyPI has no `instruktai-python-logger` `X.Y.Z`. A yanked
     version counts as existing: its number can never be used again;
   - the `CARGO_REGISTRY_TOKEN` and `PYPI_TOKEN` secrets are set.
6. **`build-verify`** (`build-verify.yml`, read-only, no secrets) builds the
   release commit and proves it:
   - dist builds for the four targets, then the global build;
   - the assembled files are exactly the 13 that `dist plan` lists
     (`.github/scripts/dist-assets.sh`), every `.sha256` and `sha256.sum`
     verify, and each archive holds both binaries;
   - on all four runners, the shell installer installs from a local mirror of
     those files into a fresh HOME and fresh XDG dirs. The installed
     `instrukt-ai-logs --version` must print exactly `instrukt-ai-logs X.Y.Z`,
     and one read of fixture logs must give exact output;
   - `cargo package --workspace --locked`, and the wheel and sdist at `X.Y.Z`
     with no console scripts (`twine check`).

   It uploads `release-assets` (the 13 files) and `python-dist` (wheel and
   sdist) and writes the asset table (name, size, sha256) to the job summary.
7. **`land`** pushes the release commit to `main` and the tag, atomically and
   fast-forward only, never forced. If `main` moved since the run started, it
   fails and nothing is visible; the next push releases.
8. **`github-release`** creates `vX.Y.Z` as a **draft** with the inspector's
   notes and uploads the 13 verified files. It reads the draft back: the asset
   names must equal the set, and each asset's `digest` must equal the local
   sha256. Only then does it publish the release as latest. If any step fails
   it deletes the draft.
9. **`crates`**, then **`pypi`** (`publish.yml`): first its `release` job
   requires `vX.Y.Z` to be a public GitHub Release with exactly the planned
   asset set; then `instrukt-ai-logging`, then `instrukt-ai-logs`, from the
   tag; then exactly the verified `python-dist` files. Each skips a version
   already published.

CI (`ci.yml`) runs on every pull request and every push to `main`, and runs
the same `build-verify.yml` on the tree as it is. A merge has therefore already
proven that the artifacts build, install and run. CI never references a
publishing secret.

### Why the registries come last

A crates.io or PyPI version cannot be deleted, only yanked, and a yanked number
is burnt for good. So nothing reaches a registry before the tag and the GitHub
Release are in place, and nothing is tagged before the artifacts are verified.

### Account preconditions

- The crates.io account behind `CARGO_REGISTRY_TOKEN` has a verified email
  address, which crates.io requires for every upload. The owner has confirmed
  this, and the record agrees: the v0.7.0 publish run (41967787010) was refused
  with "A verified email address is required", and the v0.7.2 run later the
  same day, with the same token, published both crates. The preflight does not
  re-check it because crates.io reports the status only to a signed-in website
  session (`/api/v1/me` refuses API tokens).
- Branch protection on `main` must let `github-actions[bot]` push, or `land`
  fails (nothing is visible then).

## Dry run

```sh
gh workflow run auto-release.yml --ref main -f dry_run=true
```

Steps 1–6 run in full, including the preflight and the whole build-verify.
`land`, `github-release`, `crates` and `pypi` are skipped. The run summary
reports "dry run", the inspector's bump and rationale, the version, and the
verified asset table. A push to `main` always runs without `dry_run`.

## Failure semantics and recovery

**Up to and including `build-verify`**, nothing exists outside the run: no
commit on `main`, no tag, no release, no upload. Fix the cause on `main`; the
next push releases. "Re-run failed jobs" also works while `main` has not moved.

**After `land`**, the tag is correct and stays. Never bump again, and never
delete or move a pushed tag. Recover with **"Re-run failed jobs" on the same
run**. Every job after `land` reuses that run's verified artifacts and never
rebuilds the binaries:

- `land` succeeds as "already landed" when main and the tag on origin already
  point at the release commit (a push whose acknowledgement was lost). Any
  other mismatch fails without pushing.
- `github-release` publishes a release that is already public with exactly the
  verified assets as a no-op. It deletes a draft left by an interrupted attempt
  and redoes it.
- `crates` and `pypi` skip what the registries already have.

Specific cases:

- **A registry refused the upload** (for example a revoked or expired
  token): fix the secret, then "Re-run failed jobs".
- **The run's artifacts are gone** (retention expired), or the run cannot be
  re-run: dispatch `publish.yml` on the tag. It first requires the GitHub
  Release `vX.Y.Z` to be public with exactly the planned asset set and stops
  otherwise. crates.io publishes from the tag as always; **PyPI then gets a
  wheel and sdist rebuilt from the tag, not the files build-verify checked**.
  PyPI runs after crates.io: with `registry=all` only when crates.io
  succeeded, and with `registry=pypi` only when both crates are already on
  crates.io at that version:

  ```sh
  gh workflow run publish.yml --ref vX.Y.Z               # -f registry=crates|pypi
  ```

- **A published release carries the wrong assets**: `github-release` stops and
  does not touch it. This needs a person.

### Resume mode

A new dispatch of `Release (Auto)` while HEAD is still the tagged release
commit (`gh workflow run auto-release.yml --ref main`) runs only `resume`. It
never rebuilds, re-verifies or uploads anything, because the verified files
exist only in the run that pushed the tag. It passes when the GitHub Release
is public with exactly the planned asset set and crates.io and PyPI both have
the version. Anything missing fails with the instruction to use "Re-run failed
jobs" on that original run, which holds the verified `python-dist` and
`release-assets` artifacts.
