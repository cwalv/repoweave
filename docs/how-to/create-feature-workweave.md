# Create a feature workweave

Spin up an isolated cross-repo branch for a feature that spans multiple repos. The weave stays undisturbed; the workweave gets its own worktrees, ecosystem files, and tool state.

## Create the workweave

```bash
rwv workweave web-app create payments
cd ../.workweaves/web-app--payments
```

This forks from CWD's active workspace (primary when invoked from primary, the surrounding workweave when invoked from inside one) and writes `../.workweaves/web-app--payments/` (by default a sibling of the weave root, not a child) containing:

- a git worktree for each `owned`/`fork`/`dependency` repo on an ephemeral branch
- a symlink to the canonical weave-root clone for each `role: reference` repo (read-only study material — no working-tree duplication; pass `--worktree-references` to cut worktrees for them instead)
- generated ecosystem files (`package.json`, `go.work`, `Cargo.toml`, ...)
- a `.rwv-workweave` marker recording the parent workspace and project

`rwv workweave create` checks for uncommitted changes in `projects/<project>/` before proceeding. If the project directory is dirty it refuses with a clear error naming the dirty files and suggests three remediation paths:

```
Error: rwv workweave create: refusing to create workweave — projects/web-app has uncommitted changes:
  rwv.yaml

To proceed, do one of:
  1. commit the changes: git -C projects/web-app commit
  2. stash the changes: git -C projects/web-app stash
  3. capture them into the workweave: rwv workweave web-app create payments --capture-dirty
```

If the dirty state is intentional — for example, you are editing `rwv.yaml` specifically to configure the new workweave — pass `--capture-dirty` to opt in:

```bash
rwv workweave web-app create payments --capture-dirty
```

The workweave will then reflect the uncommitted edits. Note that captured dirty state becomes an obstacle at retire time if the changes are also present in the primary; commit or stash them in the workweave before running `rwv sync-to`.

## Work across repos

Inside the workweave, edit, test, commit as usual — the workspace wiring makes cross-repo imports resolve to the worktrees. Every manifest repo is already on its own ephemeral branch from `create`; commit directly onto it:

```bash
cd github/chatly/protocol
# edit ...
git commit -am "protocol: add payment fields"

cd ../server
# edit ...
git commit -am "server: add /payments endpoint"

cd ../..
cargo test --workspace   # or npm test --workspaces, go test ./...
```

See [workweave hierarchy](../explanation/joints/workweave-hierarchy.md#ephemeral-branch-names-and-the-git-worktree-constraint) for why each worktree gets its own branch name.

## Lock the cross-repo state

Once the work is committed across the manifest repos, snapshot it:

```bash
rwv lock
git -C projects/web-app commit -am "lock: payments feature"
```

`rwv lock` is per-workspace — it updates the workweave's `rwv.lock`, not primary's. Each workspace owns its own lock (see [lock-as-derived](../explanation/joints/lock-as-derived.md)).

## Land back to the parent

Bring the work home with one verb:

```bash
rwv sync-to --retire
```

Bare `rwv sync-to` auto-targets the parent edge recorded in `.rwv-workweave`. `--retire` adds a post-landing cleanup step: if the workweave converges with its parent and no worktree is dirty, the workweave is deleted. See [bring workweave work home](./bring-workweave-work-home.md) for the manual ceremony and conflict recovery.

## Discard an experiment

If the work is a dead end and you want to throw it away without landing anything, delete the workweave directly:

```bash
rwv workweave web-app delete payments
```

Default deletion refuses if any worktree is dirty or holds commits not reachable from the recorded parent or primary; pass `--discard-uncommitted` and/or `--discard-unmerged-commits` to consent to losing that work. See [workweave lifecycle](../explanation/joints/workweave-lifecycle.md#deletion) for the full delete contract.

## Related

- [workweave hierarchy](../explanation/joints/workweave-hierarchy.md) — tree model, flow direction, what the tool tracks vs. discipline
- [lock-as-derived](../explanation/joints/lock-as-derived.md) — why `rwv.lock` is per-workspace
- [bring workweave work home](./bring-workweave-work-home.md) — sync semantics, `--retire`, manual ceremony
- [review a PR with a workweave](./review-pr-with-workweave.md) — same primitive, different use case
