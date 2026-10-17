# Review a PR with a workweave

Test a pull request in an isolated workweave without disturbing your primary weave's branches or build state:

```bash
rwv workweave web-app create pr-42
cd ../.workweaves/web-app--pr-42
git -C github/chatly/server fetch origin pull/42/head
git -C github/chatly/server checkout --detach FETCH_HEAD
rwv materialize
```

The PR is checked out on a detached `HEAD`, so the member's `web-app--pr-42` ephemeral branch stays at the parent workspace's locked revision. For a multi-repo PR, fetch and check out `FETCH_HEAD` detached in each affected member worktree; untouched repositories stay at the parent's locked revisions. `rwv workweave create` ran the install hooks against the parent's state; `rwv materialize` runs them again against the PR's.

A review workweave is never landed: do not run `rwv sync-to` from it. `sync-to` would record the detached PR commits in the workweave's `rwv.lock`, with only a warning, and fast-forward the parent's branch onto them, landing the unreviewed PR.

## What `rwv doctor` reports while you review

`rwv status` shows the PR member as `ahead` of `rwv.lock`, with no branch. `rwv doctor` exits 1 with a `stale-lock` error for that member and a `branch-discipline` / `detached` warning for its checkout. Both are expected in a review workweave. Leave their remedies alone here: `rwv lock` would pin the PR commits, and `rwv doctor --fix --adopt-detached-checkouts` would put the ephemeral branch on the PR tip.

## Preview what the PR introduces

From anywhere inside the workweave, inspect the commits and diff relative to the recorded parent's common ancestor:

```bash
rwv workweave web-app log            # commit listing per repo vs. recorded parent
rwv workweave web-app log --diff     # unique diff vs. recorded parent
```

Add `--json` for machine-readable output.

## Build and test

Run your ecosystem's build and test commands from the workweave root:

```bash
cargo test --workspace
# or: npm test --workspaces, go test ./...
```

Because `target/`, `node_modules/`, and `.venv/` are isolated per workweave, dependency changes in the PR do not touch your primary weave.

## Push review fixes to the PR

Commit fixes on the detached `HEAD` and push them to the PR's branch with git (`rwv push` refuses inside a workweave):

```bash
git -C github/chatly/server commit -am "review: fix"
git -C github/chatly/server push origin HEAD:<pr-branch>
```

## Clean up

Return to the primary weave root (`cd ../../<weave>` from `../.workweaves/web-app--pr-42`, where `<weave>` is the weave root's directory name) and delete the review workweave:

```bash
cd ../../<weave>
rwv workweave web-app delete pr-42 --discard-unmerged-commits
```

`--discard-unmerged-commits` discards only the PR commits checked out in the workweave, which the PR's remote still holds. Add `--discard-uncommitted` when the review left uncommitted changes, for example a project lockfile that `rwv materialize` rewrote for the PR's dependencies; check `rwv status` and `git status` first for anything you meant to keep. `rwv workweave web-app delete` also works from inside the workweave.

## Related

- [Create a feature workweave](./create-feature-workweave.md) — using a workweave to develop and land a feature
- [Cache builds across workweaves](./cache-builds-across-workweaves.md) — sharing compiled artifacts across review workweaves
- [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md) — how workweaves relate to the primary weave
