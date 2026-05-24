# Bring workweave work home

Land work from a workweave into its parent (typically primary). The one-liner handles the common case; the manual ceremony underneath is the same operation broken into steps.

## The one-liner

```bash
cd .workweaves/web-app--payments
rwv sync --retire
```

`--retire` adds a post-sync cleanup to a normal sync:

1. Run a normal sync to the recorded parent (one hop). Bare `rwv sync` follows `.rwv-workweave`'s `parent` field.
2. Verify the workweave's manifest tips equal the parent's after sync. (The project repo will typically have an auto-relock commit on top of the parent's tip — that's expected, not a divergence.)
3. Verify no worktree is dirty.
4. If both invariants hold, delete the workweave (worktrees + ephemeral branches + directory).

If sync hits a conflict, the workweave is preserved; resolve and re-run `rwv sync --retire` (or fix manually and `rwv workweave delete`).

See [sync-semantics](../explanation/joints/sync-semantics.md) for the full phase model and the auto-relock detail.

## Preconditions

Before `rwv sync` (or `--retire`) will run, both workspaces must satisfy `rwv check --locked`: each repo's tip must match its `rwv.lock`. Concretely:

```bash
# in the workweave
git -C github/chatly/server status   # clean
git -C github/chatly/web    status   # clean
rwv lock                              # snapshot post-edit state
git -C projects/web-app commit -am "lock: payments feature"
```

The lock-precondition check is what makes sync deterministic — the lock is the target, not the working tree.

## Manual ceremony

When `--retire` doesn't fit (you want to keep the workweave for follow-up work, the parent isn't primary, you want different strategies for different repos), do it by hand:

```bash
# from the workweave: commit and lock
cd .workweaves/web-app--payments
rwv lock
git -C projects/web-app commit -am "lock: payments feature"

# from the parent (primary): bring the work in
cd ~/work
rwv sync payments
```

`rwv sync payments` aligns CWD (primary) with the workweave's committed `rwv.lock`. The phases are:

1. **Phase 2** — manifest repos advance to the workweave's lock targets.
2. **Phase 1'** — the workweave's unique project commits replay onto primary's project tip, with `rwv.lock` excluded.
3. **Phase 3** — `rwv.lock` regenerated from the post-Phase-2 manifest tips.

Then optionally clean up:

```bash
rwv workweave web-app delete payments
```

## Choosing a strategy

`rwv sync` defaults to `--strategy ff` (fast-forward only). When CWD has commits not reachable from `<source>`, `ff` refuses and the error names the alternatives:

| Strategy | When to use |
|---|---|
| `ff` (default) | One-way alignment; no commits to land from CWD |
| `rebase` | CWD has commits to land; you want linear history |
| `merge` | CWD has commits to land; you want explicit join commits |

Example — bringing primary into a feature workweave that has its own commits:

```bash
cd .workweaves/web-app--payments
rwv sync primary --strategy rebase
```

For the project repo, `rebase` and `merge` both honor the lock-as-derived contract: `rwv.lock` is excluded from the replayed/merged commits and regenerated in Phase 3, so lock-only commits become empty patches and are skipped automatically.

## n-way landing (two workweaves)

When two workweaves both have project commits, land them serially. The first lands clean; the second rebases through primary first:

```bash
# ww1 lands first (clean ff)
cd ~/work
rwv sync ww1

# ww2 is now diverged from primary; rebase ww2 over primary
cd .workweaves/web-app--ww2
rwv sync primary --strategy rebase

# now land ww2 (clean ff again)
cd ~/work
rwv sync ww2
```

`rwv.lock` is never merged — it is recomputed in Phase 3 each time, so lock-file conflicts never arise regardless of how many workweaves are in flight. See [sync-semantics — N-way merge](../explanation/joints/sync-semantics.md#n-way-merge-two-workweaves-serial-landing) for the worked example.

## Related

- [sync-semantics](../explanation/joints/sync-semantics.md) — phase ordering, strategy choices, `--retire`, parallel/NDJSON output
- [lock-as-derived](../explanation/joints/lock-as-derived.md) — why `rwv.lock` is treated specially in Phase 1' and regenerated in Phase 3
- [workweave hierarchy](../explanation/joints/workweave-hierarchy.md) — one-hop semantics, parent tracking
- [recover from sync conflict](./recover-from-sync-conflict.md) — what to do when Phase 1' or Phase 2 hits a real conflict
