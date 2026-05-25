# rwv update

## Purpose

Advance the project's `rwv.lock` to the latest HEAD of each repo's branch
and re-snapshot the lock (network bump). `update` is semantically analogous
to `cargo update` / `npm update` — it is the verb that mutates the lock by
pulling fresh tips from the network.

The key distinction from `fetch`: `rwv fetch` aligns local clones to the
*existing* lock without advancing it; `rwv update` fetches from the remote,
checks out the new branch tip, and writes a new lock. Run `update` when you
want to consume upstream commits; run `fetch` (or `sync`) when you want to
converge to an already-recorded state.

## Invocation

```
rwv update [--dirty] [--commit] [--project <name>]
           [--role <role>]... [--repo <selector>]... [-j <n>]
```

Run `rwv --help update` for the full clap surface. `update` does not
currently support `--json`.

## Output

A fetch-progress line per repo (prefixed with `[<repo>]` under `-j > 1`),
followed by a summary line of the form `rwv update: advanced N repo(s)`.
The subsequent lock re-snapshot emits `Wrote <path>` on stderr. Unchanged
entries are not individually reported.

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
- *clone missing on disk* — a manifest entry references a repo that hasn't
  been fetched; run `rwv fetch` first.
- *could not resolve branch on role-conventional remote* — the branch named
  in the manifest's `version:` field doesn't exist on the upstream remote;
  verify the branch name and remote configuration.
- *git fetch failed* — network error or remote authentication problem;
  inspect the git output for details.
- *lock not written* — reported when one or more repos fail to advance; the
  lock is left unchanged so a partial update never lands.
