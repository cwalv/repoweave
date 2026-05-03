# Sync

`rwv sync <source>` aligns the current workspace ("CWD") with another
workspace's committed state. It looks symmetric on the surface — `cd primary &&
rwv sync feat` and `cd .workweaves/feat && rwv sync primary` both compile
to a `rwv sync <source>` invocation — but it is *not* symmetric in effect.
This page explains how sync works, the lock-as-derived contract, and the
phase ordering.

## The lock-as-derived contract

`rwv.lock` records manifest repo tips. It is *derived state*: its content
is fully determined by running `rwv lock` against the current manifest
tips. This means `rwv.lock` is never an input to a merge — it is always
an output of sync.

This distinction matters when two workspaces both have project commits
(commits inside `projects/<name>/`). Naively merging their project repos
produces lock-file conflicts on every line both sides touched. But those
"conflicts" are synthetic — the authoritative value is whatever `rwv lock`
computes from the post-merge manifest tips. Treating `rwv.lock` as an
input to merge would force operators to manually resolve a conflict that
has a deterministic answer.

`rwv sync` handles this by excluding `rwv.lock` from Phase 1' (the
project-repo step) and regenerating it in Phase 3.

## Unified strategy model

Both repo classes — the project repo (`projects/<name>/`) and manifest
repos (`github/<owner>/<repo>/` etc.) — run under the same `--strategy`
choice. The distinction between them is not *whether* to merge but *how*
`rwv.lock` is handled during the merge:

| Class | Strategy | `rwv.lock` treatment |
|-------|----------|----------------------|
| Project repo | `ff` / `rebase` / `merge` | Excluded from Phase 1' inputs; regenerated in Phase 3 |
| Manifest repos | `ff` / `rebase` / `merge` | Normal content; no exclusion |

`--force` retains hard-reset semantics on the project repo — the way to
*discard* CWD's project commits intentionally. It is no longer the only
path that handles project-repo divergence; `--strategy rebase` or
`--strategy merge` are the paths that *land* CWD's project commits.

## Phase ordering

Sync runs three phases in this order:

**Phase 2 (manifest repos)** runs first, advancing each manifest repo's
branch to the source's lock target using `--strategy`. This happens
before the project repo is touched because Phase 3 (re-lock) needs the
final manifest tips.

**Phase 1' (project repo, lock-excluded)** replays CWD's unique project
commits — commits reachable from CWD's project tip but not from source's
— onto source's project tip using `--strategy`, with `rwv.lock` excluded
from each commit's effective diff. Commits whose only change was
`rwv.lock` become empty patches and are skipped automatically.

**Phase 3 (re-lock)** regenerates `rwv.lock` from the post-Phase-2
manifest tips. If the lock differs from what is in the project repo's
working tree, sync commits it automatically with a message like
`lock: auto-relock after sync from <source>`.

The old Phase 1 (hard-reset project repo to source's tip) is superseded
by Phase 1' + Phase 3. The effect is the same when CWD's project repo is
a strict ancestor of source's (ff case): Phase 1' fast-forwards and Phase
3 regenerates the lock. When there is divergence, Phase 1' replays or
merges instead of refusing.

## sync's symmetries and asymmetries

### Symmetric in surface

Any workspace can be source for any other. Any workspace can be
destination for any other. `rwv sync <other>` from primary and
`rwv sync <primary>` from a workweave are the same command exercising the
same code path. There is no per-direction logic in `run_sync` —
direction-of-effect is parameterized by which workspace is CWD.

### Symmetric in mechanism

Both directions go through the same Phase 2 (manifest repos), Phase 1'
(project repo), and Phase 3 (re-lock). The savepoint protocol
(`refs/rwv/pre-op/<id>`) and abort contract are direction-neutral.

### Asymmetric in direction-of-effect

A single sync invocation updates **CWD only**. There is no `git push`
counterpart that updates the source workspace. To propagate state the
other direction, run sync from the other workspace.

This is the same shape as `git pull`: pulls update the local repo, never
the upstream. To propagate the other direction, you push (or, here, run
sync from the other side).

### Asymmetric in cost-of-operation

`rwv sync <source>` modifies the destination's project repo and manifest
repos. It only **reads** from the source. So source can safely be a
stable or shared workspace; destination is the workspace that absorbs
change.

## Phase 1 ancestor precondition

Before Phase 1', sync checks the ancestor relationship between the two
project tips and applies strategy-aware logic:

| Relation | `ff` | `rebase` / `merge` |
|----------|------|---------------------|
| Equal tips | No-op, allowed | No-op, allowed |
| CWD is ancestor of source (forward) | Allowed — fast-forward | Allowed |
| Source is ancestor of CWD (CWD ahead) | **Refused** | Handled by strategy |
| Diverged (neither ancestor) | **Refused** | Handled by strategy |

With `ff` (the default), sync refuses when CWD's project tip is not an
ancestor of source's. The error message suggests `--strategy rebase` or
`--strategy merge` as alternatives to `--force`.

With `rebase` or `merge`, the strategy itself handles divergence. The
ancestor precondition is bypassed.

`--force` bypasses the precondition and hard-resets the project repo to
source's tip — use this when you want to intentionally discard CWD's
project commits. The savepoint protocol still runs; discarded commits are
preserved at `refs/rwv/pre-op/<id>` and recoverable via `rwv abort`.

## Two example scenarios

### Forward sync (clean case)

A workweave finishes work, locks, and primary syncs in:

```text
primary/project: ─── C1
                       \
workweave/project: ──── C1 ─── C2-lock
```

```bash
cd /path/to/primary
rwv sync feat
```

- Phase 2: manifest repos fast-forward to workweave's lock targets.
- Phase 1': CWD's project tip (C1) is an ancestor of workweave's (C2-lock).
  Phase 1' fast-forwards. C2-lock's only change is `rwv.lock`, which is
  excluded — so Phase 1' fast-forwards the non-lock project content
  (none here) and Phase 3 picks up the lock.
- Phase 3: lock regenerated and committed if changed.

### N-way merge (two workweaves, serial landing)

Two workweaves both have project commits. `ww1` lands first; `ww2` syncs
in and lands second.

```text
primary/project: ─── C1
                       \
ww1/project: ──────── C1 ─── CA    (ww1 doc changes + lock)
ww2/project: ──────── C1 ─── CB    (ww2 doc changes + lock)
```

**Step 1: sync ww1 into primary (ff)**

```bash
cd /path/to/primary
rwv sync ww1
# primary now at CA; manifest repos at ww1's lock targets
```

**Step 2: bring primary into ww2 (rebase)**

ww2's project tip (CB) is now diverged from primary (CA). `ff` would
refuse — use rebase:

```bash
cd /path/to/ww2
rwv sync primary --strategy rebase
# Phase 2: ww2's manifest repos advance to primary's lock targets
# Phase 1': CB replayed onto CA with rwv.lock excluded
#   CB's lock-only lines produce an empty patch and are skipped;
#   CB's doc changes replay cleanly onto CA
# Phase 3: lock regenerated from post-Phase-2 manifest tips
```

ww2 is now rebased on top of primary. Its project history is linear.

**Step 3: sync ww2 into primary (ff)**

```bash
cd /path/to/primary
rwv sync ww2
# fast-forward; ww2 is already ahead of primary in a straight line
```

Both workweaves' project commits land without manual intervention.
`rwv.lock` is never merged — it is computed by Phase 3 each time.

## Where this lives in code

- `src/sync.rs::check_phase1_ancestor` — the strategy-aware precondition.
- `src/sync.rs::run_sync` — the orchestrator (Phase 2 → Phase 1' → Phase 3).
- `src/lock.rs` — Phase 3 re-lock codepath.
- `tests/e2e_two_workweaves_test.rs` — acceptance tests for the n-way
  merge contract (lock-only changes, doc changes, genuine conflict).
- `tests/e2e_sync_abort_test.rs` — Phase 1 ancestor precondition and
  error message coverage.
