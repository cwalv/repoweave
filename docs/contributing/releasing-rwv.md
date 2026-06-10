# Releasing rwv

How the `rwv` binary itself gets published. Distinct from
[Release a package](../how-to/release-a-package.md), which covers user-project package releases.

## Pipeline

Two workflows, one tag:

1. **`.github/workflows/release.yml`** — fires on `vX.Y.Z` tag push. Builds
   platform binaries with cargo-dist, creates the GitHub Release, updates the
   Homebrew tap.
2. **`.github/workflows/publish-shims.yml`** — fires via `workflow_run` when
   Release completes successfully. Rewrites `python/repoweave/pyproject.toml`
   to the tag version, builds platform wheels + sdist, publishes to PyPI via
   OIDC.

## Tag pattern

`v[0-9]+.[0-9]+.[0-9]+*` (e.g. `v0.4.2`, `v1.0.0-rc1`). Anything else is
ignored. The same pattern gates `publish-shims.yml`.

## Pre-tag test gate

Tagging a broken commit will not produce a release. `release.yml`'s `test` job
runs `scripts/ci-local.sh` (the single source of truth shared with `ci.yml`
and local dev) before any artifact build. If `cargo check`, `cargo test
--release`, `cargo clippy --all-targets -- -D warnings`, or `cargo fmt --check`
fails, `plan` does not run.

## Prerequisites

### Homebrew tap

`release.yml` pushes the generated formula to **`cwalv/homebrew-tap`** using
the **`HOMEBREW_TAP_TOKEN`** repository secret. Both must exist. The tap
config lives in `dist-workspace.toml` (`tap = "cwalv/homebrew-tap"`).

### PyPI trusted publisher

PyPI uploads use OIDC, not a stored token. One-time setup on PyPI:

- Visit `https://pypi.org/manage/project/repoweave/settings/publishing/`
- Add a trusted publisher:
  - Publisher: GitHub
  - Repo: `cwalv/repoweave`
  - Workflow: `publish-shims.yml`

### `environment: pypi`

`publish-shims.yml`'s `publish-pypi` job runs with `environment: pypi`. The
environment is decorative unless it's configured with required reviewers on
GitHub. To verify or change:

- Repo settings → Environments → `pypi` → Required reviewers.

If no protection is wanted, remove the `environment: pypi` line from
`publish-shims.yml`. The comment promising "optional: gate behind a
required-reviewer environment" should not outlive the configuration it
describes.

## Cutting a release

```bash
# 1. Bump the Cargo version
$EDITOR Cargo.toml                    # version = "X.Y.Z"
cargo check                            # refresh the (untracked) Cargo.lock

# 2. Commit and tag (Cargo.lock is gitignored in this repo — Cargo.toml only)
git add Cargo.toml
git commit -m "chore(version): bump to X.Y.Z"
git tag vX.Y.Z

# 3. Push both
git push origin main vX.Y.Z

# 4. Watch
gh run watch                           # release.yml, then publish-shims.yml
```

`pyproject.toml`'s version stays at `0.0.0` — it's a sentinel that
`publish-shims.yml` rewrites from the tag at build time. Do not bump it by
hand.
