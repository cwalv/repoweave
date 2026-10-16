# Review a PR with a workweave

Test a pull request in an isolated workweave without disturbing your primary weave's branches or build state:

```bash
rwv workweave web-app create pr-42
cd ../.workweaves/web-app--pr-42/github/chatly/server
git fetch origin pull/42/head
git checkout --detach FETCH_HEAD
```

Checking out `FETCH_HEAD` detached keeps the worktree's ephemeral branch ref intact (avoiding `rwv doctor` branch-drift warnings while you inspect the PR). For a multi-repo PR, fetch and detach `FETCH_HEAD` in each affected member worktree; untouched repositories remain at the parent workspace's locked revisions.

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
cd ../../..   # back to ../.workweaves/web-app--pr-42
cargo test --workspace
# or: npm test --workspaces, go test ./...
```

Because `target/`, `node_modules/`, and `.venv/` are isolated per workweave, dependency changes in the PR do not touch your primary weave.

## Clean up

Return to the weave root and delete the review workweave, passing `--discard-unmerged-commits` because the fetched PR commits are not merged into the parent workspace (add `--discard-uncommitted` if the build or review also left uncommitted edits):

```bash
cd ../..
rwv workweave web-app delete pr-42 --discard-unmerged-commits
```

If you made review fixes on the worktree's ephemeral branch that you want to keep, land them into the parent with `rwv sync-to --retire` or push the branch to the PR remote before deleting.

## Related

- [Create a feature workweave](./create-feature-workweave.md) — using a workweave to develop and land a feature
- [Cache builds across workweaves](./cache-builds-across-workweaves.md) — sharing compiled artifacts across review workweaves
- [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md) — how workweaves relate to the primary weave
