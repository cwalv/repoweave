# Create a feature workweave

Spin up an isolated cross-repo branch for a feature that spans multiple repos. The weave stays undisturbed; the workweave gets its own worktrees, ecosystem files, and tool state.

## Create the workweave

```bash
rwv workweave web-app create payments
cd .workweaves/web-app--payments
```

This forks from CWD's active workspace (primary when invoked from primary, the surrounding workweave when invoked from inside one) and writes `.workweaves/web-app--payments/` containing:

- a git worktree for each repo on an ephemeral branch
- generated ecosystem files (`package.json`, `go.work`, `Cargo.toml`, ...)
- a `.rwv-workweave` marker recording the parent workspace and project

`rwv workweave create` snapshots the parent's *committed* `rwv.yaml`. Commit any pending manifest edits before creating the workweave or they will be silently dropped.

## Work across repos

Inside the workweave, edit, test, commit as usual — the workspace wiring makes cross-repo imports resolve to the worktrees:

```bash
cd github/chatly/protocol
git checkout -b feat/payment-fields
# edit ...
git commit -am "protocol: add payment fields"

cd ../server
git checkout -b feat/payment-endpoint
# edit ...
git commit -am "server: add /payments endpoint"

cd ../..
cargo test --workspace   # or npm test --workspaces, go test ./...
```

The branches you push from the workweave are the same refs you'd push from primary — the workweave is just a worktree on an ephemeral branch.

## Lock the cross-repo state

Once the work is committed across the manifest repos, snapshot it:

```bash
rwv lock
git -C projects/web-app commit -am "lock: payments feature"
```

`rwv lock` is per-workspace — it updates the workweave's `rwv.lock`, not primary's. Each workspace owns its own lock (see [lock-as-derived](../explanation/joints/lock-as-derived.md)).

## Sync back to the parent

Bring the work home with one verb:

```bash
rwv sync --retire
```

Bare `rwv sync` follows the parent edge recorded in `.rwv-workweave`. `--retire` adds a post-sync cleanup step: if the workweave converges with its parent and no worktree is dirty, the workweave is deleted. See [bring workweave work home](./bring-workweave-work-home.md) for the manual ceremony and conflict recovery.

## Related

- [workweave hierarchy](../explanation/joints/workweave-hierarchy.md) — tree model, flow direction, what the tool tracks vs. discipline
- [lock-as-derived](../explanation/joints/lock-as-derived.md) — why `rwv.lock` is per-workspace
- [bring workweave work home](./bring-workweave-work-home.md) — sync semantics, `--retire`, manual ceremony
- [review a PR with a workweave](./review-pr-with-workweave.md) — same primitive, different use case
