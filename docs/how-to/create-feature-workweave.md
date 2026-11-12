# Create a feature workweave

Spin up an isolated cross-repo workspace for a feature that spans multiple repositories without disturbing the primary weave:

```bash
rwv workweave create payments
cd ../.workweaves/web-app--payments
```

The workspace the command runs in (the primary weave, or the surrounding workweave when run inside one) is the new workweave's parent: it does not switch, and the new workweave starts from its committed state. The command creates `../.workweaves/web-app--payments/` — a sibling of the weave root by default — containing:

- a git worktree for every member repo, on a `web-app--payments` feature branch for an `owned` or `fork` member and detached at its pin for the rest (`role = "reference"` members included, with read-only permissions on tracked regular files), populated via copy-on-write reflinks or read-only hardlinks where the filesystem supports them
- surfaced ecosystem workspace files (`package.json`, `go.work`, `Cargo.toml`, `pyproject.toml`) and isolated tool state (`node_modules/`, `target/`, `.venv/`), installed by the integration install hooks `create` runs in the new workweave (pass `--no-materialize` to skip them; `rwv materialize` inside the workweave runs them later)
- a `.rwv-workweave` marker recording the parent workspace and project

The new workweave starts from commits, so uncommitted files in the parent, `rwv.toml` and `rwv.lock` included, stay there as they are, and `create` names them. Commit an edit that the workweave should carry before creating it.

By default the project repo is on a feature named `web-app--payments`, onto the branch the parent has checked out; that name is never published. To name the branch, create it with `--new-branch payments`; to open a branch that already has a record, use `--branch payments`; to look at a branch another workspace has checked out, use `--detach payments`. Running the same `create` again on a clean workweave returns it; any other spelling for that name is refused (`name` taken).

## Start the feature in the workspace you are in

A new workweave is one way to isolate a feature. To start one where you are, create the branch and switch to it:

```bash
rwv branch create payments
```

This records `payments` as a feature onto the branch the project repo had checked out, gives each `owned` and `fork` member a `payments` branch at its pin, and moves the workspace onto them, carrying uncommitted edits. Return to the branch you came from, and reopen the feature, with `rwv switch`:

```bash
rwv switch main
rwv switch payments
```

`rwv branch create payments --declared` records a branch whose members keep following the branches its `rwv.toml` entries declare; `--target <T>` makes the feature land into `<T>` instead of the checked-out branch. A branch made with `git switch -c` has no record: `rwv branch set-mode <branch> feature --target <T>` records it, and then `rwv switch <branch>` opens it.

## Work across repos

Every `owned` and `fork` member is already checked out on the `web-app--payments` feature branch. Edit, test, and commit directly in each worktree:

```bash
cd github/chatly/protocol
# edit ...
git commit -am "protocol: add payment fields"

cd ../server
# edit ...
git commit -am "server: add /payments endpoint"

cd ../../..
cargo test --workspace   # or npm test --workspaces, go test ./...
```

## Land into the target

Once your changes are committed across the member repos, land them into the branch the workweave's record lands into and retire the workweave in one command:

```bash
rwv land --retire
```

`rwv land` automatically snapshots any `Ahead` member-repo tips into the workweave's `rwv.lock` at the start of the operation, composes the workweave's commits onto its branch's target, fast-forwards the target where it is checked out, and — with `--retire` — deletes the workweave once it converges cleanly.

If you want to checkpoint the workweave's `rwv.lock` without landing yet, run `rwv lock --commit` (which updates `rwv.lock` and commits it to `projects/web-app`).

## Discard an experiment

To throw away a workweave without landing its changes:

```bash
rwv workweave delete payments
```

Deletion refuses if any checkout in the workweave, a manifest member or not, has uncommitted changes, or a detached HEAD holding commits no branch reaches; pass `--discard-uncommitted` and/or `--discard-unlanded` to confirm discarding that work. A branch of the workweave whose commits have not landed on the branch it lands into is not a refusal: the workweave is removed, the branch stays, and the exit status is `3` (see [`rwv branch delete`](../reference/cli.md#rwv-branch)).

## Related

- [Bring workweave work home](./bring-workweave-work-home.md) — `rwv land` options, multi-workweave landing, and conflict recovery
- [Review a PR with a workweave](./review-pr-with-workweave.md) — using a disposable workweave to test a pull request
- [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md) — parent/child lineage and branch naming
- [Workweave lifecycle](../explanation/joints/workweave-lifecycle.md) — creation, landing, and deletion contracts
- [Lock as derived](../explanation/joints/lock-as-derived.md) — why `rwv.lock` is per-workspace
