# rwv update

## Purpose

Advance the project's `rwv.lock` to the latest HEAD of each repo (or to a
specified ref). `update` is the "bump the lock" verb — it's how new SHAs
make it into the lockfile after upstream commits land.

Unlike `sync` (which moves repos toward the lock), `update` moves the lock
toward repos. Run `update` to record current state; run `sync` to converge
other workweaves or fresh clones to that state.

## Invocation

```
rwv update [--project <name>]
```

Run `rwv --help update` for the full clap surface. `update` does not
currently support `--json`.

## Output

One line per updated lock entry on the standard text channel, summarizing
the old → new SHA transition. Unchanged entries are not printed.

## Exit codes

- `0` — lock updated successfully (or no changes were needed).
- non-zero — workspace could not be resolved, manifest parse failure, or
  one or more repos couldn't be inspected.

## Examples

Update the lock for the active project:

```
rwv update
```

Update a specific project without changing `.rwv-active`:

```
rwv update --project web-app
```

## Common errors

- *workspace could not be resolved* — `cwd` is not inside a weave or
  workweave.
- *repo not on disk* — a manifest entry references a repo that hasn't been
  fetched; run `rwv fetch` first.
- *HEAD detached* — a repo is in a detached-HEAD state; either check out a
  branch or accept the SHA as the new lock target.
