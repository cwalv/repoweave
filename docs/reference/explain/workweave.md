# rwv workweave

## Purpose

Create, delete, or list workweaves for a project. A workweave is a parallel
working directory containing `git worktree` checkouts for every repo in the
project's manifest (including the project repo itself), on ephemeral branches
named `{project}--{name}/{source-branch}`. Each workweave is fully
self-describing: it carries `.rwv-workweave` (recording the primary weave,
project, and parent workspace) and `.rwv-active` (the active project).

Workweaves are the isolation primitive for concurrent work — the human works
in the primary weave while an agent works in `.workweaves/{project}--{name}/`.
They are also the landing model: `rwv sync-to --retire` brings the workweave's
commits into the parent and deletes the workweave in one step.

See [workweave hierarchy](../explanation/joints/workweave-hierarchy.md) for
the parent-tracking and one-hop sync-to semantics.

## Subcommands

### `rwv workweave <project> create <name>`

Create a new workweave. Forks from CWD's active workspace by default; use
`--from` to specify a different source.

```
rwv workweave <project> create <name> [--from <source>] [--force] [--capture-dirty]
```

- `--from <source>` — workspace to fork from. Accepts `primary`, an absolute
  path, or a relative path resolved against primary (e.g., a sibling workweave
  name). Defaults to CWD's active workspace.
- `--force` — if a workweave with this name already exists and is clean, destroy
  it and recreate from scratch. Refuses if the existing workweave holds
  uncommitted changes or unmerged commits (work visible only in the workweave
  and not yet reachable from the parent or primary). Use `rwv workweave <project>
  delete <name> --force` to discard dirty workweaves explicitly.
- `--capture-dirty` — allow creation even when the source project directory
  has uncommitted changes; captures the dirty state into the new workweave's
  project worktree. Without this flag, `create` refuses and names the dirty
  files. Default behavior is recommended; use `--capture-dirty` only when you
  intentionally want to continue in-flight edits inside the workweave.

**Re-invocation without `--force`** (idempotent path): if the workweave already
exists and is clean, `create` validates the `.rwv-workweave` marker (same
primary and project) and returns immediately. Non-git state written by agents
between invocations (`.runtime/`, `.claude/`, etc.) is preserved. Re-invocation
is the Gas City rig's standard "ensure workweave exists" path.

**What `create` writes:**
1. One `git worktree` per manifest repo (plus the project repo) at
   `{workweave}/{repo-path}/`, on a fresh ephemeral branch.
2. `workweave:` artifacts from `rwv.yaml` — `copy:` entries are deep-copied;
   `link:` entries are absolute symlinks pointing at the source root.
3. `.rwv-workweave` marker recording `{primary, project, parent}`. `parent`
   is the workspace forked from (= `source_root`), which becomes the default
   sync-to target for bare `rwv sync-to` from inside the workweave.
4. `.rwv-active` set to `project`.
5. Integration activation (context verb: surfaces symlinks, skips install hooks).

### `rwv workweave <project> delete <name> [--force]`

Delete a workweave. Without `--force`, refuses if:

- Any worktree (project repo or manifest repo) has **uncommitted changes**
  (staged, unstaged, or untracked files outside `.gitignore`).
- Any worktree HEAD holds **commits not reachable from the recorded parent
  or the primary weave** — i.e., unmerged work that would be permanently
  destroyed when the ephemeral branches are force-deleted.

The refusal message lists the dirty or diverged paths so the operator can
decide whether to commit, land with `rwv sync-to`, or discard explicitly with
`--force`.

**`--force`** bypasses both checks. This matches the `git branch -D` contract:
it consents to destroying whatever is in the workweave. The operator must have
reviewed the contents (or not care). After the operator confirms intent,
`delete --force` removes worktrees, prunes stale `.git/worktrees/` entries,
force-deletes all ephemeral branches, and removes the workweave directory.

### `rwv workweave <project> list`

List existing workweave names for the project. One name per line.

## Invocation

```
rwv workweave <project> create <name> [--from <source>] [--force] [--capture-dirty]
rwv workweave <project> delete <name> [--force]
rwv workweave <project> list
```

Run `rwv --help workweave` for the full clap surface.

## Output

`create` — no stdout output on success (workweave path to stdout only when
`--hook-mode` is set). Progress and warnings go to stderr.

`delete` — no output on success.

`list` — one workweave name per line to stdout.

## Exit codes

- `0` — operation completed successfully.
- non-zero — operation failed; see stderr for details.

## Examples

Create a workweave for the `foundations` project:

```
rwv workweave foundations create my-feature
```

Create a workweave forked from a peer workweave (useful for stacked work):

```
rwv workweave foundations create child-feature --from .workweaves/foundations--my-feature
```

Land work and clean up in one step (from inside the workweave):

```
rwv sync-to --retire
```

Delete a workweave whose work has already landed:

```
rwv workweave foundations delete my-feature
```

Force-delete a workweave regardless of state:

```
rwv workweave foundations delete my-feature --force
```

List all workweaves for a project:

```
rwv workweave foundations list
```

## Common errors

- *workweave has uncommitted changes; refusing to delete without --force* —
  list the named paths; commit or discard the changes, or use `--force` to
  discard everything.
- *workweave has commits not merged into ...; refusing to delete without
  --force* — the workweave HEAD holds commits not reachable from the parent
  or primary. Land the work via `rwv sync-to`, or discard with `--force`.
- *workweave directory exists but has no .rwv-workweave marker* — a previous
  failed `create` left a partial workweave; safe to recreate with `--force`.
- *refusing to create workweave — projects/{project} has uncommitted changes*
  — the source project directory is dirty. Commit, stash, or pass
  `--capture-dirty` to explicitly capture the in-flight state.
