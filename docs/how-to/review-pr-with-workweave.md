# Review a PR with a workweave

Test a pull request in an isolated workweave without disturbing your primary weave's branches or build state:

```bash
rwv workweave create pr-42
cd ../.workweaves/web-app--pr-42
git -C github/chatly/server fetch origin pull/42/head
git -C github/chatly/server checkout --detach FETCH_HEAD
rwv materialize
```

The PR is checked out on a detached `HEAD`, so the member's `web-app--pr-42` feature branch stays at the parent workspace's locked revision. For a multi-repo PR, fetch and check out `FETCH_HEAD` detached in each affected member worktree; untouched repositories stay at the parent's locked revisions. `rwv workweave create` ran the install hooks against the parent's committed state; `rwv materialize` runs them again against the PR's.

A review workweave is never landed: do not run `rwv land` from it. With the PR checked out detached, `land` refuses (`alignment` `detached with unpushed commits`, or `detached` once a remote-tracking branch holds the PR's commits) before it changes anything; attaching a branch to the PR commits would let it record them in the workweave's `rwv.lock` and fast-forward the parent's branch onto them, landing the unreviewed PR.

## What `rwv doctor` reports while you review

`rwv status` shows the PR member as `ahead` of `rwv.lock`, with no branch, and an `alignment` line for it. `rwv doctor` exits 1 with a [`pin`](../reference/doctor-findings.md#pin) / `unpinned` error for that member and an [`alignment`](../reference/doctor-findings.md#alignment) / `detached-with-unpushed-commits` warning for its checkout, listing the PR's commits, which no branch reaches. Both are expected in a review workweave. Where the PR comes from a branch of the same repository and a fetch has brought that branch in, the commits are published: the states are `pin` `ahead` and `alignment` `detached`, both warnings, and `rwv doctor` exits 0. Leave their remedies alone here: `rwv lock` would pin the PR commits, and `rwv update` leaves a detached checkout holding commits no branch reaches where it is. The checkout is meant to stay detached on the PR, which keeps `web-app--pr-42` at the parent's locked revision. Keeping the commits on a branch, as the finding's choices say, is the way back to the branch, for when you want it.

## Preview what the PR introduces

From anywhere inside the workweave, list the commits and the diff beyond the pins of the weave branch the PR targets (here `main`):

```bash
rwv log --against main            # commit listing per repo beyond main's pins
rwv log --against main --diff     # diff per repo from the merge-base of that pin and HEAD
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

Commit fixes on the detached `HEAD` and push them to the PR's branch with git (`rwv push` refuses a detached workspace):

```bash
git -C github/chatly/server commit -am "review: fix"
git -C github/chatly/server push origin HEAD:<pr-branch>
```

## Keep a fix of your own

A review workweave made without a branch is on the minted name `web-app--pr-42`, which `rwv push` never publishes. A member the review left alone is still on that branch, so a fix you want to keep that is unrelated to the PR goes there, never on the detached `HEAD` the PR sits on. Commit it, name the workweave's branch for the fix, pin the member's new commit by name, then publish the member that carries it:

```bash
git -C github/chatly/lib commit -am "fix the lib typo"
rwv branch rename lib-fix
rwv member pin github/chatly/lib HEAD --commit
rwv push --repo github/chatly/lib
```

`rwv member pin` records only that member's commit in `rwv.lock`; `rwv lock` and `rwv update` would also act on the PR's detached checkout, which this workweave leaves alone. `rwv push` refuses a member whose commits are beyond its pin (`pin` `unpinned`), so the pin comes first.

`rwv branch rename` renames the project branch and each `owned` and `fork` member's branch of the same name, moves the branch's record, and stops each from taking in from a remote branch of the old name. A member branch some live weave branch declares is never renamed: it stays, and the member's branch of the new name starts at its tip. A `<new>` that already exists is refused (`name` `exists`); `--adopt` takes up a remote's.

## Clean up

Return to the primary weave root (`cd ../../<weave>` from `../.workweaves/web-app--pr-42`, where `<weave>` is the weave root's directory name) and delete the review workweave:

```bash
cd ../../<weave>
rwv workweave delete pr-42 --discard-unlanded
```

`--discard-unlanded` deletes the branch holding the PR commits checked out in the workweave, which the PR's remote still holds, after keeping its tip as a backup. Add `--discard-uncommitted` when the review left uncommitted changes, for example a project lockfile that `rwv materialize` rewrote for the PR's dependencies; check `rwv status` and `git status` first for anything you meant to keep. `rwv workweave delete` also works from inside the workweave.

## Related

- [Create a feature workweave](./create-feature-workweave.md) — using a workweave to develop and land a feature
- [Cache builds across workweaves](./cache-builds-across-workweaves.md) — sharing compiled artifacts across review workweaves
- [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md) — how workweaves relate to the primary weave
