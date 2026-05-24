# rwv fetch

## Purpose

Clone or fetch every repo listed in the active project's manifest. `fetch`
is the bootstrap verb: it brings the workspace's on-disk state into
agreement with `rwv.yaml` so other verbs (status, sync, doctor) have
something to inspect.

Repos already on disk are fetched in place. Missing repos are cloned into
the registry path implied by their manifest path. Reference repos that fail
to clone (e.g. private mirror unreachable) are skipped without failing the
overall run.

## Invocation

```
rwv fetch [--no-reference] [-j <N>] [--project <name>]
```

- `--no-reference` skips reference-role repos (useful when only primary/fork
  clones are needed and references are offline).
- `-j <N>` runs up to `N` clone/fetch operations in parallel.

Run `rwv --help fetch` for the full clap surface. `fetch` does not currently
support `--json`; see the `rwv explain` index for which verbs are JSON-
capable today.

## Output

One line per repo on the standard text channel. Lines include the repo path
and either the fetched ref summary or a clone confirmation. Errors are
printed to stderr.

## Exit codes

- `0` — every required repo was fetched or cloned successfully (reference
  repos that failed are warnings, not errors).
- non-zero — at least one non-reference repo failed to fetch/clone, or the
  workspace could not be resolved.

## Examples

Fetch all repos in the active project:

```
rwv fetch
```

Skip reference repos (faster when working offline-ish):

```
rwv fetch --no-reference
```

Parallel fetch across 8 workers:

```
rwv fetch -j 8
```

## Common errors

- *repository not found* — the manifest URL is wrong or the remote is
  private and credentials aren't configured.
- *network-error* — connectivity issue; retry.
- *path already exists with non-git content* — the registry path has a
  directory that isn't a git repo; either remove it or relocate it.
