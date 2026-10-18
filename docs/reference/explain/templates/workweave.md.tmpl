# rwv workweave

## Purpose

Create, delete, or list workweaves for a project. A workweave is a parallel
working directory that materializes every repo in the project's manifest
(including the project repo itself). Every repo, whatever its `role`, becomes
a `git worktree` checkout of the canonical weave-root clone on the ephemeral
branch `{project}--{name}`. A `reference` repo's tracked files carry no write
bit, here and in the primary: every verb that writes or moves a member's files
settles them for its role, and `rwv doctor` reports a member whose files
disagree (`rwv explain member-permissions`). A member slot holding a symlink —
the shape older releases gave `reference` repos — is not one rwv operates on:
verbs that walk the workweave's members refuse it (`rwv explain
workweave-drift`), and `rwv doctor --fix` replaces it with a worktree. Each
workweave is fully
self-describing: it carries `.rwv-workweave`, recording the primary weave,
project, and parent workspace. That marker is a workweave root's only
identity file — `.rwv-active` is the primary root's project-selection
pointer, and the two are mutually exclusive.

Workweaves are the isolation primitive for concurrent work — the human works
in the primary weave while an agent works in `.workweaves/{project}--{name}/`.
They are also the landing model: `rwv sync-to --retire` brings the workweave's
commits into the parent and deletes the workweave in one step.

The parent workspace recorded in `.rwv-workweave` becomes the default sync-to
target for bare `rwv sync-to` from inside the workweave.

## Subcommands

### `rwv workweave <project> create <name>`

Create a new workweave. Forks from CWD's active workspace by default; use
`--from` to specify a different source.

```
rwv workweave <project> create <name> [--from <source>] [--replace-existing] [--capture-dirty] [--dir <path>] [--no-materialize]
```

- `--from <source>` — workspace to fork from. Accepts `primary`, an absolute
  path, or a relative path resolved against primary (e.g., a sibling workweave
  name). Defaults to CWD's active workspace. Forking from an existing
  workweave is how you DUPLICATE one — for a scratch variant, an experiment,
  or a throwaway baseline. Do not copy a workweave with `cp`: the result
  aliases the original's git state rather than duplicating it.
- `--replace-existing` — if a workweave with this name already exists and is
  clean, destroy it and recreate from scratch. Refuses if the existing workweave
  holds uncommitted changes or unmerged commits in any checkout under it, a
  member or not (work visible only in the workweave and not yet reachable from
  the parent or primary). Use `rwv workweave
  <project> delete <name> --discard-uncommitted --discard-unmerged-commits` to
  discard dirty workweaves explicitly.
- `--capture-dirty` — allow creation even when the source project directory
  has uncommitted changes, and move the uncommitted edits to `rwv.toml` and
  `rwv.lock` into the new workweave's project worktree: they are copied there,
  then discarded from the source once the workweave exists, so the workweave
  can land back on the source with `rwv sync-to`. A file is discarded only
  while it still holds exactly the bytes copied and has nothing staged its
  working file does not show; if any copy fails, nothing is discarded. Other
  uncommitted files are not captured and stay in the source. Every file left
  behind is named. Without this flag, `create` refuses and names the dirty
  files. Default behavior is recommended; use `--capture-dirty` only when you
  intentionally want to continue in-flight edits inside the workweave.
- `--dir <path>` — per-invocation placement override. Places the workweave at
  exactly `<path>` (absolute; relative paths resolve against the primary
  root). The absolute path is recorded in `.rwv-workweave-index`, so every
  `find`-direction verb (list, delete, sync targets by bare name) resolves
  it via the registry — the workweave does not have to live under the
  default container to be addressable. Useful for big-disk workweaves and
  tmpfs experiments.
- `--no-materialize` — skip the integration install hooks (`npm install`,
  `uv sync`, `cargo fetch`, …) in the new workweave. `rwv materialize` inside
  it runs them later.

**Placement.** Workweaves live at `<container>/<project>--<name>/` by default,
where `<container>` is recorded per-project in `projects/<project>/.rwv-workweave-index`.
The default container is `<parent-of-primary>/.workweaves`; set it explicitly
with `rwv workweave <project> set-container <path>`. `--dir <path>` overrides
the container for one invocation.

**Re-invocation without `--replace-existing`** (idempotent path): if the
workweave already
exists and is clean, `create` validates the `.rwv-workweave` marker (same
primary and project) and returns immediately. Non-git state written by agents
between invocations (`.runtime/`, `.claude/`, etc.) is preserved. Re-invocation
is the standard "ensure workweave exists" path.

**What `create` writes:**
1. One `git worktree` per member repo, whatever its role (plus the project
   repo), at `{workweave}/{repo-path}/`, on a fresh ephemeral branch.
2. A copy of every rwv-owned generated file the source workspace has accepted
   — its lockfiles, plus the `.rwv-owned-digests` record attesting them. These
   are gitignore-eligible, so the worktrees above arrive without them, and
   re-running the ecosystem generator here would resolve against a registry
   that has moved since the source resolved: the new workweave would hold a
   different dependency set than the workspace it forked from, with both
   reading clean. Copying is what makes a build in the workweave and a build
   at the source measure the same tree.
3. `workweave:` artifacts from `rwv.toml` — `copy:` entries are deep-copied;
   `link:` entries are absolute symlinks pointing at the source root.
4. `.rwv-workweave` marker recording `{primary, project, parent}`. `parent`
   is the workspace forked from (= `source_root`), which becomes the default
   sync-to target for bare `rwv sync-to` from inside the workweave. No
   `.rwv-active` is written beside it: selection is primary-only, and a
   workweave's project is fixed here at creation.
5. Integration activation (context verb: surfaces symlinks, then runs the
   install hooks in the new workweave unless `--no-materialize` is passed).
   A hook whose tool is not on `PATH` is skipped with a `tool-missing`
   warning. A hook that fails leaves the workweave created and registered,
   and the command exits non-zero naming `rwv materialize`, which finishes it
   once the cause is fixed. Under `--hook-mode`, and for the `WorktreeCreate`
   request `--claude-hook` handles, the agent runtime is waiting for the
   workweave: the create exits `0`, prints the path as usual, and the failure
   is a warning on stderr naming `rwv materialize`.
6. An entry in `projects/<project>/.rwv-workweave-index` recording the
   workweave's absolute path. Best-effort adds `.rwv-workweave-index` to the
   project repo's `.gitignore` so the machine-local index stays untracked.

### `rwv workweave <project> set-container <path>`

Record the workweave container for `project` — the directory `create` places
new workweaves under by default. Writes the `container` field of
`projects/<project>/.rwv-workweave-index`. Absolute paths are used as-is;
relative paths resolve against the primary root. Existing entries in the
registry are preserved. Per-workweave `--dir` overrides on `create` are
unaffected.

The recorded container is an explicit, audit-visible act, not ambient
process state: it lives in a file the operator can read, not in a shell
environment.

### `rwv workweave <project> delete <name> [--discard-uncommitted] [--discard-unmerged-commits]`

Delete a workweave. Refuses if:

- Any worktree (project repo or member repo) has **uncommitted changes**
  (staged, unstaged, or untracked files outside `.gitignore`).
- Any worktree HEAD holds **commits not reachable from the recorded parent
  or the primary weave** — i.e., unmerged work that would be permanently
  destroyed when the ephemeral branches are force-deleted.

Both checks also cover every checkout under the workweave that no member of
the project's manifest names: a member replay dropped but could not prune, or
one the workweave added that has not landed. The directory removal takes such
a checkout with it, so its uncommitted changes and its commits count the same
way. A commit in one counts as unmerged unless the parent or the primary
weave holds that repo and contains it.

The refusal message lists the dirty or diverged paths, marking a checkout
outside the manifest `(not a manifest member)`, so the operator can decide
whether to commit, land with `rwv sync-to`, or discard explicitly.

**`--discard-uncommitted`** waives the first refusal; **`--discard-unmerged-commits`**
waives the second. Each names exactly what it destroys; passing both is the
`git branch -D` contract, which consents to destroying whatever is in the
workweave. The operator must have reviewed the contents (or not care). Once a
refusal is waived, `delete` removes worktrees, prunes stale `.git/worktrees/` entries,
force-deletes all ephemeral branches, and removes the workweave directory.
The canonical clones keep everything but this workweave's own branches.

**Child adoption.** Before the workweave is destroyed, any living child
workweave that records it as `parent:` is re-pointed to the retiree's OWN
recorded parent (the grandparent; falls back to `primary`, which always
exists). One loud line is printed per child:
`adopted child workweave <name>: parent now <path>`. Lineage stays transitive
by construction — the retiree's unique commits have just landed in that
grandparent. Branch names are NOT rewritten (they are creation-time
namespaces, not lineage), which is exactly why consumers must read the parent
from the marker / `rwv status --json .parent`, never from the branch name. The
same adoption step runs on `rwv sync-to --retire`. A parent that goes away
another way (crash, hand-deletion) leaves a `dangling-parent` that
`rwv doctor --fix` re-points to primary.

### `rwv workweave <project> list`

List existing workweave names for the project. One name per line.

### `rwv workweave <project> log [--diff] [--json]`

Show this workweave's **unique commits vs its recorded parent**, per member
repo and the project repo (`projects/<project>`). Must be run from inside a
workweave.

Parent identity comes from the `.rwv-workweave` marker's `parent:` field, never
from the branch name: a workweave's branch is the flat `{project}--{name}`,
which names no parent, and adoption can re-point a child to primary. The verb
reads the marker, so it is correct for nested and adopted parents alike.

For each repo, "unique" commits are those in the workweave's history but not
the parent's — reachable from the workweave's tip but not from the parent's tip
of the same repo. This stays correct when the parent **advanced** after the
fork: commits the parent already has are excluded.

The project repo (`projects/<project>`) is included as a first-class
participant. It carries real per-workweave work — doc commits, lock bumps — and
appears as `=== (project) ===` in text output and as a top-level `project_repo`
field in JSON output. This mirrors the representation sync-to uses for
`project_repo_advance`.

- `--diff` — instead of the commit listing, show the workweave's unique diff vs
  its parent. The diff is anchored at the **common ancestor** of the workweave
  tip and the parent tip, NOT the parent tip directly: anchoring at the common
  ancestor keeps commits the parent gained after the fork from being shown as
  reversals.
- `--json` — machine-readable output (envelope: `workweave`, `parent`, `diff`,
  `repos[]`, `project_repo`; each repo entry carries `head`, `parent_tip`,
  `unique_commits[]`, and — in diff mode — `diff_base` + `diff`; `project_repo`
  has the same shape with `path` set to `"(project)"`).

This is the surface consumers use to read a workweave's parent-relative history
instead of hand-rolling branch-name derivation.

## Invocation

```
rwv workweave <project> create <name> [--from <source>] [--replace-existing] [--capture-dirty] [--dir <path>] [--no-materialize]
rwv workweave <project> delete <name> [--discard-uncommitted] [--discard-unmerged-commits]
rwv workweave <project> list
rwv workweave <project> log [--diff] [--json]
rwv workweave <project> set-container <path>
```

Run `rwv workweave --help` for the full clap surface.

## Output

`create` — no stdout output on success (workweave path to stdout only when
`--hook-mode` is set). Progress and warnings go to stderr, and so does the
output of the install hooks' tools, so the `--hook-mode` path is the only
thing on stdout. With `--no-materialize` it says so in one line: `[skipped]
core: the install hooks did not run (--no-materialize); `rwv materialize` runs
them`.

`delete` — no output on success.

`list` — one workweave name per line to stdout.

## Exit codes

- `0` — operation completed successfully.
- non-zero — operation failed; see stderr for details. For `create`, a failed
  install hook is non-zero too, though the workweave was created and is kept —
  except under `--hook-mode` or `--claude-hook`, where it is a warning and the
  create exits `0`.

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

Delete a workweave regardless of state:

```
rwv workweave foundations delete my-feature --discard-uncommitted --discard-unmerged-commits
```

List all workweaves for a project:

```
rwv workweave foundations list
```

## Common errors

- *workweave has uncommitted changes; refusing to delete without
  --discard-uncommitted* — list the named paths; commit or discard the changes,
  or pass the flag to discard them.
- *workweave has commits not merged into ...; refusing to delete without
  --discard-unmerged-commits* — the workweave HEAD holds commits not reachable
  from the parent or primary. Land the work via `rwv sync-to`, or pass the flag
  to discard them.
- *workweave directory exists but has no .rwv-workweave marker* — a previous
  failed `create` left a partial workweave; safe to recreate with
  `--replace-existing`.
- *refusing to create workweave — projects/{project} has uncommitted changes*
  — the source project directory is dirty. Commit, stash, or pass
  `--capture-dirty` to explicitly move the in-flight state.
- *workweave created at …; only its install hooks did not complete* — an
  install hook failed in the new workweave. The workweave is usable; fix the
  cause the hook reported, then run `rwv materialize` inside it.
