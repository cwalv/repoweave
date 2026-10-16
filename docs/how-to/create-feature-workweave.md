# Create a feature workweave

Spin up an isolated cross-repo workspace for a feature that spans multiple repositories without disturbing the primary weave:

```bash
rwv workweave web-app create payments
cd ../.workweaves/web-app--payments
```

This forks from CWD's active workspace (the primary weave, or the surrounding workweave when invoked inside one) and creates `../.workweaves/web-app--payments/` — a sibling of the weave root by default — containing:

- a git worktree on an ephemeral `web-app--payments` branch for every member repo (`role = "reference"` members included, with read-only permissions on tracked regular files), populated via copy-on-write reflinks or read-only hardlinks where the filesystem supports them
- surfaced ecosystem workspace files (`package.json`, `go.work`, `Cargo.toml`, `pyproject.toml`) and isolated tool state (`node_modules/`, `target/`, `.venv/`)
- a `.rwv-workweave` marker recording the parent workspace and project

`rwv workweave create` checks for uncommitted changes in `projects/<project>/` before proceeding. If the project repo is dirty, it refuses and lists three options:

```text
Error: rwv workweave create: refusing to create workweave — projects/web-app has uncommitted changes:
  rwv.toml

To proceed, do one of:
  1. commit the changes: git -C projects/web-app commit
  2. stash the changes: git -C projects/web-app stash
  3. capture them into the workweave: rwv workweave web-app create payments --capture-dirty
```

Pass `--capture-dirty` when the uncommitted `rwv.toml` edit is meant for the new workweave:

```bash
rwv workweave web-app create payments --capture-dirty
```

Commit or stash captured edits in the workweave before running `rwv sync-to`.

## Work across repos

Every member repo is already checked out on its `web-app--payments` ephemeral branch. Edit, test, and commit directly in each worktree:

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

## Land back to the parent

Once your changes are committed across the member repos, land them into the recorded parent workspace and retire the workweave in one command:

```bash
rwv sync-to --retire
```

`rwv sync-to` automatically snapshots any `Ahead` member-repo tips into the workweave's `rwv.lock` at the start of the operation, replays the workweave's commits onto the parent workspace, fast-forwards the parent, and — with `--retire` — deletes the workweave once it converges cleanly.

If you want to checkpoint the workweave's `rwv.lock` without landing yet, run `rwv lock --commit` (which updates `rwv.lock` and commits it to `projects/web-app`).

## Discard an experiment

To throw away a workweave without landing its changes:

```bash
rwv workweave web-app delete payments
```

Deletion refuses if any worktree has uncommitted changes or commits not reachable from the recorded parent or primary weave; pass `--discard-uncommitted` and/or `--discard-unmerged-commits` to confirm discarding that work.

## Related

- [Bring workweave work home](./bring-workweave-work-home.md) — `rwv sync-to` options, multi-workweave landing, and conflict recovery
- [Review a PR with a workweave](./review-pr-with-workweave.md) — using a disposable workweave to test a pull request
- [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md) — parent/child lineage and ephemeral branch naming
- [Workweave lifecycle](../explanation/joints/workweave-lifecycle.md) — creation, landing, and deletion contracts
- [Lock as derived](../explanation/joints/lock-as-derived.md) — why `rwv.lock` is per-workspace
