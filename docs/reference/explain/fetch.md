# rwv fetch

## Purpose

Bootstrap a project into the workspace and align every repo to the lock
file. `fetch` is the lock-aligning, read-only counterpart to `rwv update`:
it never advances pinned revisions. Its job is to reproduce a known state,
not discover a new one.

Given a `source` (full URL or `owner/repo` shorthand), `fetch` clones the
project repo into `projects/<name>/`, reads its `rwv.lock`, and then clones
or checks out every manifest repo at the revision the lock records. When the
lock is absent (first-time bootstrap), `fetch` clones repos at branch HEAD
and writes a new lock from those HEAD SHAs. When a manifest entry is missing
from an existing lock, it is added additively at branch HEAD — already-pinned
entries are never moved.

`--frozen` is the CI mode: if the lock is absent or does not cover every
manifest repo, the command errors instead of writing. Reference repos that
fail to clone are skipped without failing the overall run.

## Invocation

```
rwv fetch <source> [--frozen] [--force] [--no-reference]
          [--role <role>...] [--repo <selector>...] [-j <N>]
```

- `<source>` — required. Full clone URL or `owner/repo` / `registry/owner/repo`
  shorthand. The project repo is cloned from this URL.
- `--frozen` — CI mode: error if the lock is missing or does not cover all
  manifest repos. Never writes the lock.
- `--force` — bootstrap into a non-empty directory that is not already a
  workspace (normally an error).
- `--no-reference` — skip repos with `role: reference` (useful when mirrors
  are offline).
- `--role <role>` / `--repo <selector>` — limit the repo fetch to a subset of
  the manifest. Repeat for union. Bare strings match exactly; `re:<pat>` is
  regex; `glob:<pat>` is glob. A filtered fetch does not write the lock.
- `-j <N>` — run up to `N` clone/checkout operations in parallel.

Run `rwv --help fetch` for the full clap surface. `fetch` does not currently
support `--json`; see the `rwv explain` index for which verbs are JSON-
capable today.

## Output

One line per repo on the standard text channel. Lines report whether each
repo was cloned fresh, checked out at a lock-pinned revision, or skipped.
Errors (including per-repo failures) are printed to stderr. A final summary
line reports how many repos are ready.

## Exit codes

- `0` — the project repo and every non-reference manifest repo cloned or
  checked out successfully (reference repos that failed are warnings, not
  errors).
- non-zero — the project repo failed to clone, at least one non-reference
  repo failed, `--frozen` detected a missing or stale lock, or the current
  directory is not a workspace and `--force` was not passed.

## Examples

Bootstrap a project from a GitHub shorthand:

```
rwv fetch owner/myproject
```

Same, with a full URL:

```
rwv fetch https://github.com/owner/myproject.git
```

CI mode — error if the lock is missing or stale:

```
rwv fetch owner/myproject --frozen
```

Skip reference repos (faster when mirrors are offline):

```
rwv fetch owner/myproject --no-reference
```

Parallel clone/checkout across 8 workers:

```
rwv fetch owner/myproject -j 8
```

## Common errors

- *project 'X' already exists at projects/X/* — run `rwv fetch` from a
  fresh directory, or use a scoped path hint printed in the error.
- *no repoweave workspace found and … is not empty* — the current directory
  is not a workspace; use `--force` to bootstrap here anyway.
- *lock file does not exist* (with `--frozen`) — the project has no
  `rwv.lock`; run without `--frozen` to bootstrap it.
- *lock file is stale* (with `--frozen`) — the lock does not cover all
  manifest repos; update the lock with `rwv update` first.
- *repository not found* — the source URL is wrong or the remote is private
  and credentials aren't configured.
- *network-error* — connectivity issue; retry.
