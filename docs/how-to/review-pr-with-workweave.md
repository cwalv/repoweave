# Review a PR with a workweave

Run a PR locally without disturbing your in-progress work. The workweave gets its own worktrees and tool state; your primary weave stays on whatever branches you were on.

## Create a review workweave

```bash
rwv workweave web-app create review-pr-42
cd ../.workweaves/web-app--review-pr-42/github/chatly/server
```

Workweaves live at `<parent>/.workweaves/<project>--<name>/` — by default a sibling of the weave root, so from the weave root the path is `../.workweaves/<project>--<name>/`.

## Check out the PR

```bash
git fetch origin pull/42/head:pr-42
git checkout pr-42
```

For PRs that touch multiple repos, do the same in each affected repo's worktree. The other repos stay on the parent's tips, so the cross-repo build sees the PR's changes against everything else as it is on the parent.

## Preview what the PR adds

To see the commits and diffs the PR introduces relative to the recorded parent, from the workweave root:

```bash
rwv workweave <project> log            # commit listing per repo, versus recorded parent
rwv workweave <project> log --diff     # unique diff versus the recorded parent
```

Anchored at the common ancestor, so commits the parent gained since the workweave was created are not shown as reversals — the output is exactly what the PR contributes on top of the parent. Add `--json` for machine-readable output.

## Test

The workweave has its own ecosystem files and tool state. Run the build/test commands from the workweave root:

```bash
cd ../../..   # back to the workweave root
cargo test --workspace
# or: npm test --workspaces, go test ./...
```

`node_modules/`, `.venv/`, and `target/` are per-workweave, so a PR's dependency changes can't corrupt your primary weave's state.

## Clean up

```bash
cd ~/work
rwv workweave web-app delete review-pr-42
```

If you committed locally and want to bail out without losing the commits, push the branches first or copy the worktrees aside. To bypass the dirty-tree safety:

```bash
rwv workweave web-app delete review-pr-42 --discard-uncommitted
```

`--discard-uncommitted` waives the dirty-tree refusal; default deletion refuses if any worktree is dirty.

## Related

- [workweave hierarchy](../explanation/joints/workweave-hierarchy.md) — how workweaves relate to the primary weave
- [create a feature workweave](./create-feature-workweave.md) — same primitive for shipping work, not reviewing
