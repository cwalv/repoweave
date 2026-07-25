# rwv workweave

## Purpose

Create, delete, or list workweaves for a project. A workweave is a parallel
working directory that materializes every repo in the project's manifest
(including the project repo itself). `owned`/`fork`/`dependency` repos become
`git worktree` checkouts on ephemeral branches named
`{project}--{name}/{source-branch}`. `reference` repos — read-only study
material — are instead **symlinked** to the single canonical weave-root clone
(`<primary_root>/<repo-path>`): zero working-tree duplication, byte-identical
across workweaves, and no per-workweave branch. Each workweave is fully
self-describing: it carries `.rwv-workweave` (recording the primary weave,
project, and parent workspace) and `.rwv-active` (the active project).

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
rwv workweave <project> create <name> [--from <source>] [--replace-existing] [--capture-dirty] [--worktree-references] [--dir <path>]
```

- `--from <source>` — workspace to fork from. Accepts `primary`, an absolute
  path, or a relative path resolved against primary (e.g., a sibling workweave
  name). Defaults to CWD's active workspace. Forking from an existing
  workweave is how you DUPLICATE one — for a scratch variant, an experiment,
  or a throwaway baseline. Do not copy a workweave with `cp`: the result
  aliases the original's git state rather than duplicating it.
- `--replace-existing` — if a workweave with this name already exists and is
  clean, destroy it and recreate from scratch. Refuses if the existing workweave
  holds uncommitted changes or unmerged commits (work visible only in the
  workweave and not yet reachable from the parent or primary). Use `rwv workweave
  <project> delete <name> --discard-uncommitted --discard-unmerged-commits` to
  discard dirty workweaves explicitly.
- `--capture-dirty` — allow creation even when the source project directory
  has uncommitted changes; captures the dirty state into the new workweave's
  project worktree. Without this flag, `create` refuses and names the dirty
  files. Default behavior is recommended; use `--capture-dirty` only when you
  intentionally want to continue in-flight edits inside the workweave.
- `--worktree-references` — cut a real `git worktree` for `role: reference`
  repos instead of the default symlink to the canonical weave-root clone. This
  restores the legacy behavior (per-workweave reference refs) at the cost of
  duplicating each reference repo's full working tree into this workweave. By
  default, reference repos are symlinked — zero working-tree duplication, and
  a worktree's per-workweave branch isolation is moot for read-only material.
  This flag affects only this `create`; it records nothing, so every
  downstream command keys on the resulting on-disk shape (a worktree'd
  reference flows through every normal worktree path).
- `--dir <path>` — per-invocation placement override. Places the workweave at
  exactly `<path>` (absolute; relative paths resolve against the primary
  root). The absolute path is recorded in `.rwv-workweave-index`, so every
  `find`-direction verb (list, delete, sync targets by bare name) resolves
  it via the registry — the workweave does not have to live under the
  default container to be addressable. Useful for big-disk workweaves and
  tmpfs experiments.

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
1. One `git worktree` per `owned`/`fork`/`dependency` manifest repo (plus the
   project repo) at `{workweave}/{repo-path}/`, on a fresh ephemeral branch.
   `role: reference` repos are instead **symlinked** to the canonical
   weave-root clone (`<primary_root>/{repo-path}`) — no worktree, no ephemeral
   branch (override with `--worktree-references`).
2. `workweave:` artifacts from `rwv.yaml` — `copy:` entries are deep-copied;
   `link:` entries are absolute symlinks pointing at the source root.
3. `.rwv-workweave` marker recording `{primary, project, parent}`. `parent`
   is the workspace forked from (= `source_root`), which becomes the default
   sync-to target for bare `rwv sync-to` from inside the workweave.
4. `.rwv-active` set to `project`.
5. Integration activation (context verb: surfaces symlinks, skips install hooks).
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

This is the replacement for the deprecated `RWV_WORKWEAVE_DIR` environment
variable: an explicit, recorded, audit-visible act — not ambient process
state. When `RWV_WORKWEAVE_DIR` is set, `create` still seeds the initial
container from it and fires a loud deprecation warning; removal of the
env-var fallback ships in a follow-up release.

### `rwv workweave <project> delete <name> [--discard-uncommitted] [--discard-unmerged-commits]`

Delete a workweave. Refuses if:

- Any worktree (project repo or manifest repo) has **uncommitted changes**
  (staged, unstaged, or untracked files outside `.gitignore`).
- Any worktree HEAD holds **commits not reachable from the recorded parent
  or the primary weave** — i.e., unmerged work that would be permanently
  destroyed when the ephemeral branches are force-deleted.

The refusal message lists the dirty or diverged paths so the operator can
decide whether to commit, land with `rwv sync-to`, or discard explicitly.

**`--discard-uncommitted`** waives the first refusal; **`--discard-unmerged-commits`**
waives the second. Each names exactly what it destroys; passing both is the
`git branch -D` contract, which consents to destroying whatever is in the
workweave. The operator must have reviewed the contents (or not care). Once a
refusal is waived, `delete` removes worktrees, prunes stale `.git/worktrees/` entries,
force-deletes all ephemeral branches, and removes the workweave directory.
Reference symlinks are simply unlinked (never followed), so the shared
canonical clone they alias is left untouched — delete never mutates it,
deletes no branch in it, and is safe even when that clone is dirty.

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

Show this workweave's **unique commits vs its recorded parent**, per manifest
repo and the project repo (`projects/<project>`). Must be run from inside a
workweave.

Parent identity comes from the `.rwv-workweave` marker's `parent:` field — NOT
the branch name. Workweave branches are stacked (`{project}--wwb/{project}--wwa/main`),
so a constructed `basename(parent)/main` name silently breaks when the parent
is itself a workweave, and is wrong after adoption re-points a child to primary.
The verb reads the marker, so it is correct for stacked and adopted parents
alike.

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
rwv workweave <project> create <name> [--from <source>] [--replace-existing] [--capture-dirty] [--worktree-references] [--dir <path>]
rwv workweave <project> delete <name> [--discard-uncommitted] [--discard-unmerged-commits]
rwv workweave <project> list
rwv workweave <project> log [--diff] [--json]
rwv workweave <project> set-container <path>
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
  `--capture-dirty` to explicitly capture the in-flight state.
