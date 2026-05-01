# Sync

`rwv sync <source>` aligns the current workspace ("CWD") with another
workspace's committed state. It looks symmetric on the surface — `cd primary &&
rwv sync feat` and `cd .workweaves/feat && rwv sync primary` both compile
to a `rwv sync <source>` invocation — but it is *not* symmetric in effect.
This page explains the asymmetry, why it exists, and the two repo classes
sync treats differently.

## Two repo classes, two strategies

A workspace contains two structurally different kinds of repositories.

### Project repo

`projects/<name>/` is a git repo holding `rwv.yaml`, `rwv.lock`, and project-
level docs. Its data is **snapshot-shaped**: every commit is a coherent
point-in-time view of the world (manifest + lock + state of project docs at
that moment). The lock file is a structural resource — every project commit
that updates the lock will conflict with every other project commit that
updates the lock, because both sides edit the same `rwv.lock` lines.

This rules out divergent-development merge strategies: rebase replays
project commits onto a different lock-file base and conflicts at every
commit; merge commits create lock-file conflicts that have to be hand-
resolved per merge. Both add work without adding value — the project repo
is not where divergent development belongs.

So `rwv sync` treats the project repo specially: a single hard-reset to the
source's tip. **Source-aligned**: post-sync, the destination's project tip
*equals* the source's. (Post-fix: refused if the destination would lose
commits — see "Phase 1 ancestor precondition" below.)

### Manifest repos

The repos listed in `rwv.yaml` (under `github/<owner>/<repo>/`,
`gita/<owner>/<repo>/`, etc.) are the actual code repos. Their data is
**divergent-development-shaped**: branches advance independently, merges and
rebases are normal, and conflicts when they happen are content conflicts
that operators are accustomed to resolving.

`rwv sync` treats manifest repos the way operators treat any branch
update: pick a strategy. `--strategy ff` (default) refuses to advance if
the local branch isn't an ancestor of the lock target; `--strategy rebase`
replays local commits on top of the lock target; `--strategy merge` creates
a merge commit. This is a per-invocation choice that mirrors `git pull`.

## sync's symmetries and asymmetries

`rwv sync` looks symmetric and *is* symmetric in some ways. The asymmetries
are deliberate, narrow, and easy to miss without spelling them out.

### Symmetric in surface

Any workspace can be source for any other. Any workspace can be destination
for any other. `rwv sync <other>` from primary and `rwv sync <primary>`
from a workweave are the same command exercising the same code path. There
is no per-direction logic in `run_sync` — direction-of-effect is
parameterized by which workspace is CWD.

### Symmetric in mechanism

Both directions go through the same Phase 1 (project repo hard-reset to
source's tip) and Phase 2 (per-repo `apply_strategy` against the now-
visible lock). The savepoint protocol (`refs/rwv/pre-op/<id>`) and abort
contract are also direction-neutral.

### Asymmetric in direction-of-effect

A single sync invocation updates **CWD only**. There is no `git push`
counterpart that updates the source workspace. To propagate state the
other direction, run sync from the other workspace.

This is the same shape as `git pull`: pulls update the local repo, never
the upstream. To propagate the other direction, you push (or, here, run
sync from the other side).

### Asymmetric in cost-of-operation

`rwv sync <source>` modifies the destination's project repo and manifest
repos. It only **reads** from the source. So source can safely be a stable
or shared workspace; destination is the workspace that absorbs change.

### Asymmetric per-repo-class

Already covered above, but the asymmetry is worth restating:

| Class | Strategy | Post-sync state of destination | What `--force` bypasses |
|-------|----------|--------------------------------|-------------------------|
| Project repo | hard reset (always) | tip == source's tip | The Phase 1 ancestor precondition |
| Manifest repos | `ff` / `rebase` / `merge` | tip = local lock-target reachable through the chosen strategy | (lock-freshness, when set) |

## Phase 1 ancestor precondition

Phase 1 hard-resets the destination's project repo to the source's tip.
Before doing that, sync checks the ancestor relationship between the two
project tips and refuses if the reset would discard reachable commits.

| Relation | Sync action |
|----------|-------------|
| Equal tips | No-op, allowed |
| CWD is ancestor of source (forward) | Allowed — the normal case |
| Source is ancestor of CWD (CWD ahead) | **Refused** — would discard CWD's commits |
| Diverged (neither ancestor) | **Refused** — would discard CWD's diverging commits |

`--force` bypasses the refusal. The savepoint protocol still runs — the
discarded commits are preserved at `refs/rwv/pre-op/<id>` and recoverable
via `rwv abort`.

The savepoint is a recovery mechanism, not a guard: an operator only
notices the loss if they run abort, and abort is something they have to
remember to do. The precondition is the guard — refusing first means
operators learn about the divergence before any state is lost.

## Two example scenarios

### Forward sync (clean case)

A workweave has new lock + new manifest commits; primary lags.

```text
primary/project: ─── C1
                       \
workweave/project: ──── C1 ─── C2-lock
```

```bash
cd /path/to/primary
rwv sync feat
```

- Lock-freshness: both sides are fresh. ✓
- Phase 1 ancestor: primary's project tip (C1) is ancestor of workweave's
  (C2-lock). Forward — allowed.
- Phase 1 effect: primary's project repo fast-aligns to C2-lock.
- Phase 2: each manifest repo advances per `--strategy`.

This is the design's optimal case.

### Reverse sync (post-fix refusal)

Primary has local project commits; the workweave is older.

```text
primary/project: ─── C1 ─── C2-primary
                       \
workweave/project: ──── C1
```

```bash
cd /path/to/primary
rwv sync feat
# Error: destination workspace 'primary' project repo at <sha> has 1
# commits not in source workspace 'feat'. Either sync the other direction
# first to bring those commits to source, or use `--force` if you intend
# to discard them (preserved in refs/rwv/pre-op/<id> for `rwv abort`).
```

The named recovery — "sync the other direction first" — applies in two
shapes:

- **`rwv sync` from the workweave** (workweave-as-CWD, primary-as-source).
  Forward from the workweave's perspective; brings primary's commit to the
  workweave. Then `rwv sync feat` from primary becomes a no-op.
- **Push primary's project branch to a remote and merge upstream**, the
  workweave fetches and either pulls or `rwv sync`s. This is the workflow
  for cross-machine collaboration where the workspaces don't share a
  worktree.

`--force` is the right answer when the operator *intends* to discard
primary's project commits (e.g. "this was a bad idea, let me throw it
away"). The escape hatch's escape hatch is `rwv abort`, which restores
from the savepoint.

## Where this lives in code

- `src/sync.rs::check_phase1_ancestor` — the precondition.
- `src/sync.rs::check_lock_freshness` — both lock-freshness errors.
- `src/sync.rs::run_sync` — the orchestrator (mid-op guard, lock freshness,
  ancestor check, savepoint, Phase 1, Phase 2).
- `tests/e2e_sync_abort_test.rs` — end-to-end coverage for every relation
  above plus the error-message structure.
