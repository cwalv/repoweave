# Sync semantics

`rwv sync <source>` aligns the current workspace ("CWD") with another
workspace's committed state. It is repoweave's most load-bearing verb —
the one that makes the [pyramid of stability](./pyramid-of-stability.md)
move and the [workweave hierarchy](./workweave-hierarchy.md) navigable.
This joint covers the phase model, the strategy choices, the auto-target
behavior, the `--retire` cleanup step, and the parallel/NDJSON output
modes.

The verb is direction-neutral on the surface: any workspace can be
source, any can be destination. But it is *not* symmetric in effect —
exactly the CWD's state changes; the source is read-only. See
"Symmetries and asymmetries" below.

## The three phases

Sync runs three phases in a fixed order. Naming is historical: an older
"Phase 1" hard-reset has been superseded by "Phase 1'" (replay with
lock-exclusion), so the surviving phases are numbered 2, 1', and 3 in
runtime order.

### Phase 2 — manifest repos

Advance each manifest repo's branch to the source's lock target, using
the chosen `--strategy` (see below). This runs first because the
project repo's eventual lock is derived from these manifest tips — they
need to be at their final positions before Phase 3 captures them.

Three classes of repos fall out:

- **Repos already at the target SHA.** Marked `up-to-date`; no work.
- **Repos behind the target.** Advanced via the strategy. Fast-forward
  succeeds when the lock target is a descendant of CWD's HEAD; rebase
  and merge handle divergence.
- **Repos ahead of the target.** Surfaced as `already-ahead` — the
  lock is a strict ancestor of CWD's HEAD. Sync does not silently rewind
  the working state; the operator decides (rerun with `--strategy
  rebase`, accept the divergence, or relock from this side).

Each repo's outcome is captured as a typed `RepoSyncOutcome`
(converged / already-ahead / no-op / failed); the `--json` output
serializes the same enum.

### Phase 1' — project repo, lock-excluded

Replay CWD's unique project commits — commits reachable from CWD's
project tip but not from source's — onto source's project tip using
`--strategy`, with `rwv.lock` excluded from each commit's effective
diff.

This is the structural fix that made [lock-as-derived](./lock-as-derived.md)
operationally tractable. Without exclusion, every cross-workweave sync
would surface a synthetic lock conflict because both sides almost
always have lock-edits on the same lines. With exclusion, lock-only
commits become empty patches and are dropped automatically (via git's
`--empty=drop` + `--no-keep-empty`); commits that touch the lock *and*
other files keep their non-lock changes and are replayed cleanly.

The exclusion mechanism is owned by the VCS layer
(`Vcs::set_replay_exclusion` — see
[vcs-as-seam](./vcs-as-seam.md)). For git, it is a `merge=ours` entry
in `.gitattributes` for `rwv.lock`; the merge driver is wired up
per-rebase invocation so no persistent `.git/config` change is needed.

### Phase 3 — re-lock + commit

Regenerate `rwv.lock` from the post-Phase-2 manifest tips. If the
result differs from what is in the project repo's working tree, sync
commits it automatically with a message like `lock: auto-relock after
sync from <source>`.

Phase 3 also reconciles disk against source's lock: repos listed in
source's lock but missing from CWD are materialized (clone in primary,
`git worktree add` in a workweave), and repos dropped from the lock are
removed — conservatively, refusing to delete worktrees with
uncommitted changes or unique local commits.

The reconciliation is intentional: the lock describes the *complete*
manifest, so a sync that advances to a new lock must also add and
remove constituents to match.

## Strategy choice

Both repo classes — project repo and manifest repos — run under the
same `--strategy` choice. The values:

| Strategy | Project repo treatment | Manifest repo treatment |
|---|---|---|
| `ff` (default) | Fast-forward only; refuse on divergence | Fast-forward only; refuse on divergence |
| `rebase` | Replay CWD's commits onto source's tip; lock excluded | Replay CWD's commits onto source's tip |
| `merge` | Merge source's tip into CWD; lock excluded | Merge source's tip into CWD |

`ff` is the default because it produces the least surprise: it cannot
mangle history, cannot create unexpected merge commits, and refuses
loudly when its preconditions don't hold. The refuse message names
`--strategy rebase` and `--strategy merge` explicitly, so the operator
sees the menu of choices at the moment they need it.

`rebase` and `merge` are not flag-noise alternatives. They are
first-class strategies that handle the common case of cross-workweave
sync where both sides have advanced. `rebase` produces linear history;
`merge` preserves both sides' commits with an explicit join. Pick by
project convention.

`--force` retains hard-reset semantics on the project repo — the way to
*intentionally discard* CWD's project commits. It bypasses the
ancestor precondition. The savepoint protocol still runs, so discarded
commits are recoverable via `rwv abort`.

### The Phase 1' ancestor precondition

Before Phase 1', sync checks the ancestor relationship between the two
project tips and applies strategy-aware logic:

| Relation | `ff` | `rebase` / `merge` |
|---|---|---|
| Equal tips | No-op, allowed | No-op, allowed |
| CWD is ancestor of source | Allowed — fast-forward | Allowed |
| Source is ancestor of CWD (CWD ahead) | Refused | Handled by strategy |
| Diverged (neither ancestor) | Refused | Handled by strategy |

With `ff`, sync refuses when CWD has project commits not reachable
from source. The error message points at `--strategy rebase` or
`--strategy merge` as the paths that *land* CWD's commits, and at
`--force` as the path that *discards* them.

With `rebase` or `merge`, the strategy itself is the answer to
divergence; the precondition is bypassed.

## `--retire` — close out a workweave in one step

`rwv sync --retire` adds a post-sync cleanup step: when the sync
succeeds, the workweave is deleted. The full sequence:

1. Run a normal sync to the recorded parent (see "Auto-target via
   parent tracking" below). Strategy and conflict behavior are
   unchanged.
2. Verify the workweave's manifest-repo tips equal the parent's after
   sync. (The project repo will typically have an extra auto-relock
   commit beyond the parent; that's expected and not a divergence.)
3. Verify no worktree in the workweave has uncommitted changes.
4. If both invariants hold, delete the workweave (worktrees + ephemeral
   branches + directory).

If sync hits a conflict, the workweave is preserved; the operator
resolves and re-runs `rwv sync --retire` (or uses `rwv workweave
delete` manually after resolving).

Why `--retire` and not `--land`: "land" overloads PR-merge vocabulary
and misleads when parent isn't primary. "Retire" is honest about what
the flag does — close out this workweave — regardless of where in the
[tree](./workweave-hierarchy.md) it sits.

`--retire` is only meaningful inside a workweave. Run from a primary
weave, it emits a warning and is otherwise a no-op so the operator
notices the misuse.

Anchored by `src/sync.rs::retire_workweave_after_sync` (the manifest-tip
equality check intentionally tolerates the project-repo's auto-relock
delta).

## Auto-target via parent tracking

Running `rwv sync` with no `<source>` argument syncs to the recorded
parent of the current workweave. The parent is whatever workspace was
CWD when `rwv workweave create` ran; it lives in the workweave's
`.rwv-workweave` marker. See
[workweave-hierarchy](./workweave-hierarchy.md) for the marker shape and
the one-hop semantics.

In a primary weave, bare `rwv sync` has no parent to follow and is an
error. The operator must name a source.

## Parallel sync (`-j N`) and NDJSON

The manifest-repo loop in Phase 2 fans out across a bounded worker pool
when `-j N` is set with `N > 1`. Workers run independently per repo
(syncing a manifest repo does not depend on any other repo's
in-progress sync); their outputs are surfaced live with a `[<repo>]`
prefix so interleaved lines remain attributable.

`--json` interacts with parallel mode in a specific way:

- **Serial / envelope mode** (`-j 1`, the default for `--json`):
  outcomes are collected as they come in, then a single JSON document is
  printed to stdout at the end: `{"$schema": "<url>", "outcomes":
  [...]}`. The envelope key is `"outcomes"` (not `"results"` —
  documented for consumers).
- **Parallel / NDJSON mode** (`-j N > 1` with `--json`): each per-repo
  outcome is streamed as one JSON line to stdout the moment its worker
  finishes. The envelope wrapper is dropped, and each line carries its
  own `"$schema"` field so a consumer can identify any single line
  without out-of-band context.

The branch-on-shape pattern lets consumers handle both modes uniformly:
read the first character of stdout; `{` plus newline-trailing close-brace
is one envelope JSON document, `{` followed immediately by another
record on the next line is NDJSON.

Exit semantics under `--json` are the same in both modes: non-zero iff
at least one per-repo outcome is `failed`. The schema is committed at
`docs/reference/schemas/sync.json` and reachable via `rwv explain sync
--json-schema`.

Anchored by `tests/doc_claims_sync_test.rs` (envelope shape; `outcomes`
key) and `tests/sync_json_test.rs` (NDJSON streaming).

## Symmetries and asymmetries

### Symmetric in surface

Any workspace can be source for any other. `cd primary && rwv sync
feat` and `cd .workweaves/web-app--feat && rwv sync primary` invoke the
same code path. There is no per-direction branching in `run_sync`;
direction-of-effect is parameterized by which workspace is CWD.

### Symmetric in mechanism

Both directions run the same Phase 2 → Phase 1' → Phase 3. The
savepoint protocol (`refs/rwv/pre-op/<id>`) and abort contract are
direction-neutral.

### Asymmetric in direction-of-effect

A single sync invocation updates **CWD only**. There is no `git push`
counterpart that updates the source workspace. To propagate state the
other direction, run sync from the other workspace.

This is the same shape as `git pull`: pulls update the local repo,
never the upstream. To propagate the other direction, you push (or, here,
run sync from the other side).

### Asymmetric in cost-of-operation

`rwv sync <source>` modifies CWD's project repo and manifest repos. It
only **reads** from the source. So source can safely be a stable or
shared workspace; destination is the workspace that absorbs change.
This is what makes parent-tracking auto-target safe: a workweave
syncing to its parent never disturbs the parent.

## Abort and recovery

Before any mutating phase, sync writes savepoints under
`refs/rwv/pre-op/<id>` capturing each repo's pre-op HEAD. The
identifier is wall-clock nanosecond-resolution so concurrent or
interleaved sync attempts cannot collide.

`rwv abort` rolls every repo back to its savepoint. The discarded
commits remain reachable from the savepoint ref until git's normal
unreferenced-object collection runs, so recovery is possible by hand
even after an abort.

Conflict resolution hint text (the "edit conflicted files; `git add
<files>`; `git rebase --continue`" block) is owned by the VCS layer —
see `Vcs::conflict_resolution_hint` in
[vcs-as-seam](./vcs-as-seam.md). Sync embeds that text verbatim into
its bail messages so the operator sees concrete next steps.

## Two worked scenarios

### Forward sync (clean case)

A workweave finishes work, locks, and primary syncs in:

```text
primary/project: ─── C1
                       \
workweave/project: ──── C1 ─── C2-lock
```

`cd /path/to/primary && rwv sync feat`:

- Phase 2: manifest repos fast-forward to workweave's lock targets.
- Phase 1': primary's project tip (C1) is an ancestor of workweave's
  (C2-lock). Fast-forward. C2-lock's only change is `rwv.lock`, which
  is excluded — so Phase 1' fast-forwards the non-lock project content
  (none here) and Phase 3 picks up the lock.
- Phase 3: lock regenerated and committed.

### N-way merge (two workweaves, serial landing)

Two workweaves both have project commits. `ww1` lands first; `ww2`
syncs in and lands second.

```text
primary/project: ─── C1
                       \
ww1/project: ──────── C1 ─── CA   (doc + lock)
ww2/project: ──────── C1 ─── CB   (doc + lock)
```

Step 1 — sync ww1 into primary (ff):

```
cd /path/to/primary && rwv sync ww1
# primary now at CA; manifest repos at ww1's lock targets
```

Step 2 — bring primary into ww2 (rebase):

ww2's project tip (CB) is now diverged from primary (CA). `ff` would
refuse — use rebase:

```
cd /path/to/ww2 && rwv sync primary --strategy rebase
# Phase 2: ww2's manifest repos advance to primary's lock targets
# Phase 1': CB replayed onto CA with rwv.lock excluded
#           CB's lock-only lines produce an empty patch and are skipped;
#           CB's doc changes replay cleanly onto CA
# Phase 3: lock regenerated from post-Phase-2 manifest tips
```

ww2 is now rebased on top of primary; its project history is linear.

Step 3 — sync ww2 into primary (ff):

```
cd /path/to/primary && rwv sync ww2
# fast-forward; ww2 is ahead of primary in a straight line
```

Both workweaves' project commits land without manual intervention.
`rwv.lock` is never merged — it is recomputed in Phase 3 each time.

## Related joints

- [lock-as-derived](./lock-as-derived.md) — the property the entire
  phase model is structured around.
- [workweave-hierarchy](./workweave-hierarchy.md) — the tree the
  parent-tracking auto-target walks.
- [shared-refs-drift](./shared-refs-drift.md) — the post-Phase-2
  reconciliation interacts with shared-refs drift in workweaves;
  worth knowing the joint mechanic.
- [vcs-as-seam](./vcs-as-seam.md) — replay exclusion, conflict hint
  text, and the conflict-bail surface are all Vcs-trait responsibilities.
