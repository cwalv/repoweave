# Sync semantics

Two verbs, two directions:

- **`rwv sync <source>`** — CWD absorbs the source workspace's committed
  state; CWD's unique commits land on top of source's tip. CWD changes;
  source is read-only.
- **`rwv sync-to <target>`** — CWD's committed state lands in the target
  workspace. CWD absorbs target's state first (CWD's commits on top),
  then the target fast-forwards to CWD's new tip. Both CWD and target
  change; the named target ends up with CWD's commits linearly above
  its prior state.

Together they form a direction-explicit pair. The mental model is the
same as `cp` vs. the destination-first convention in `rsync --dest`: the
argument position identifies what moves where. See "Symmetries and
asymmetries" below for the full contract.

`rwv sync` runs the core three-phase engine — Phase 2 (manifest repos),
Phase 1' (project repo, lock-excluded), Phase 3 (re-lock) — in CWD.
`rwv sync-to` wraps that engine in a 3-step orchestration: (1) run the
core engine in CWD against the target; (2) auto-relock CWD if manifest
tips moved; (3) FF-advance the target to CWD's new tip. They are
repoweave's most load-bearing verbs, the ones that make the
[pyramid of stability](./pyramid-of-stability.md) move and the
[workweave hierarchy](./workweave-hierarchy.md) navigable.

This joint covers the phase model, the strategy choices, the auto-target
behavior for `sync-to`, the `--retire` cleanup step (a `sync-to`
flag), and the parallel/NDJSON output modes.

## The three phases

Sync runs three phases in a fixed order. Naming is historical: an older
"Phase 1" hard-reset has been superseded by "Phase 1'" (replay with
lock-exclusion), so the surviving phases are numbered 2, 1', and 3 in
runtime order.

### Phase 2 — manifest repos

Advance each of CWD's manifest repo branches to the named workspace's
lock target, using the chosen `--strategy` (see below). This runs first
because the project repo's eventual lock is derived from these manifest
tips — they need to be at their final positions before Phase 3 captures
them.

Three classes of repos fall out:

- **Repos already at the target SHA.** Marked `up-to-date`; no work.
- **Repos behind the target.** Advanced via the strategy. Fast-forward
  succeeds when the lock target is a descendant of CWD's HEAD; rebase
  and merge handle divergence.
- **Repos ahead of the target.** Surfaced as `already-ahead` — the
  lock is a strict ancestor of CWD's HEAD. The engine does not silently
  rewind CWD's working state; the operator decides (rerun with
  `--strategy rebase`, accept the divergence, or relock from CWD).

Each repo's outcome is captured as a typed `RepoSyncOutcome`
(converged / already-ahead / no-op / failed); the `--json` output
serializes the same enum.

### Phase 1' — project repo, lock-excluded

Replay CWD's unique project commits — commits reachable from CWD's
project tip but not from the named workspace's — onto the named
workspace's project tip using `--strategy`, with `rwv.lock` excluded
from each commit's effective diff.

This framing is accurate for both verbs:

- For `rwv sync <source>`: CWD's unique commits replay onto the named
  source's project tip. CWD lands at the new tip (CWD absorbs source's
  state with CWD's commits sitting linearly on top).
- For `rwv sync-to <target>`: the same replay runs first (CWD's unique
  commits onto the named target's tip; CWD lands at the new tip). Then,
  in step 3, the target FF-advances to CWD's new tip — so the target
  ends up with CWD's commits linearly above the target's prior state.

The engine is the same in both cases; what differs is where the final
result is written (CWD for `sync`, target for `sync-to`).

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

#### Why rebased lock-only commits drop silently — and why FF keeps them

A lock-only commit is, from the replay's point of view, a husk: its
diff was computed against its original parent. After rebase onto a
different parent — one that already has a different lock — the diff
either conflicts textually (without `merge=ours`) or, with `merge=ours`,
produces an empty patch. In both cases the semantic the commit captured
(pinning a specific manifest tip) is unreachable from the new parent:
Phase 3 will regenerate the correct lock from the post-Phase-2 manifest
tips regardless.

The empty-patch outcome is what `--empty=drop` and `--no-keep-empty`
act on: git drops the commit rather than recording an empty change.
The result is that the history stays linear and meaningful — lock churn
that happened at an earlier parent isn't replayed as a do-nothing
commit.

Fast-forward is different. FF does not replay anything — it advances
the branch pointer, so a lock-only commit retains its original parents
and its attribution is preserved verbatim. There is no "patch against a
new parent" step; the commit object doesn't change. FF therefore keeps
lock-only commits in the history. This is intentional: FF is for the
clean landing path (workweave → primary, already linear) where history
fidelity matters more than compaction.

#### The `.gitattributes` precondition for rebase and merge

Both `--strategy=rebase` and `--strategy=merge` depend on `rwv.lock
merge=ours` being present in the project repo's **committed**
`.gitattributes` before the operation starts. The mechanism has two
halves that must both be in place: rwv passes `-c
merge.ours.driver=true` on each git invocation, which *defines* a
driver named "ours"; the `.gitattributes` line *assigns* that driver
to `rwv.lock`. Without the assignment, git's default 3-way merge runs
on `rwv.lock` and conflicts whenever both sides have lock edits — as
in the N-way serial landing scenario above.

`--strategy=ff` does not need the precondition: FF advances the
branch pointer without performing a merge, so no driver is consulted.

The sync engine (used by both `rwv sync` and `rwv sync-to`) checks the
precondition before any git operation and bails with an actionable
message if the line is absent:

```
sync --strategy=rebase and --strategy=merge require `rwv.lock merge=ours`
in the project repo's committed .gitattributes …

To fix: run `rwv doctor --fix` from this workspace …
```

The check fires against the **committed** file (via `git show
HEAD:.gitattributes`) not just the working tree, because the invariant
must survive rebases: an uncommitted `.gitattributes` would not be
carried through a `git reset --hard` or a fresh clone.

`rwv doctor --fix` is the prescribed repair path: it writes the line,
leaving a clean commit for the operator to make. The sync engine
intentionally does not auto-write the file mid-operation — that would
violate the invariant of "only change what the source says to change".

### Phase 3 — re-lock + commit

Regenerate `rwv.lock` from the post-Phase-2 manifest tips in CWD. If
the result differs from what is in CWD's project repo working tree, the
engine commits it automatically with a message like `lock: auto-relock
after sync from <named-workspace>`.

Phase 3 also reconciles CWD's disk against the named workspace's lock:
repos listed in the named workspace's lock but missing from CWD are
materialized (clone in primary, `git worktree add` in a workweave), and
repos dropped from the lock are removed — conservatively, refusing to
delete worktrees with uncommitted changes or unique local commits.

The reconciliation is intentional: the lock describes the *complete*
manifest, so a sync that advances to a new lock must also add and
remove constituents to match.

## Strategy choice

Both repo classes — project repo and manifest repos — run under the
same `--strategy` choice. The values:

| Strategy | Project repo treatment | Manifest repo treatment |
|---|---|---|
| `ff` (default) | Fast-forward only; refuse on divergence | Fast-forward only; refuse on divergence |
| `rebase` | Replay CWD's unique commits onto named-workspace's tip; lock excluded | Advance CWD's repos to named-workspace's lock targets |
| `merge` | Merge named-workspace's tip into CWD's history; lock excluded | Advance CWD's repos to named-workspace's lock targets |

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

Before Phase 1', the engine checks the ancestor relationship between
CWD's project tip and the named workspace's project tip and applies
strategy-aware logic:

| Relation | `ff` | `rebase` / `merge` |
|---|---|---|
| Equal tips | No-op, allowed | No-op, allowed |
| CWD is ancestor of named workspace | Allowed — fast-forward | Allowed |
| Named workspace is ancestor of CWD (CWD ahead) | Refused | Handled by strategy |
| Diverged (neither ancestor) | Refused | Handled by strategy |

With `ff`, the engine refuses when CWD has project commits not reachable
from the named workspace. The error message points at `--strategy
rebase` or `--strategy merge` as the paths that *land* those commits,
and at `--force` as the path that *discards* them.

With `rebase` or `merge`, the strategy itself is the answer to
divergence; the precondition is bypassed.

## `--retire` — close out a workweave in one step

`rwv sync-to --retire` is the one-shot landing verb: it runs the full
sync-to orchestration against the recorded parent and then deletes the
workweave on success. The orchestration is three steps, not one:

1. **Replay CWD's commits onto the parent's tip.** CWD's unique project
   commits are replayed onto the parent's project tip (with `rwv.lock`
   excluded), and CWD advances to the new tip. Manifest repos in CWD
   are aligned to the parent's lock targets via `--strategy`. This is
   the same engine as `rwv sync <parent>` — CWD absorbs the parent's
   state with CWD's commits sitting linearly on top.
2. **Auto-relock CWD if manifest tips moved.** If step 1's manifest-repo
   advances changed any lock targets, `rwv.lock` is regenerated and
   committed into CWD automatically (message: `lock: auto-relock after
   sync-to`). This keeps CWD's lock consistent before step 3.
3. **Parent fast-forwards to CWD's new tip.** The parent's project repo
   fast-forwards to CWD's tip. The parent now has CWD's commits linearly
   above the parent's prior state. Manifest tips are already aligned from
   step 1; the parent's lock is regenerated to match.

Then, if all three steps succeed: verify no worktree in the workweave
has uncommitted changes, then delete the workweave (worktrees + ephemeral
branches + directory).

If any step hits a conflict, the workweave is preserved and multi-step
op-state is written so the operation can be resumed. The operator
resolves conflicts and re-runs `rwv sync-to --retire --continue` (or
uses `rwv workweave delete` manually after resolving).

Why `--retire` lives on `sync-to` and not on `sync`: the dominant
"land my work" workflow goes workweave → parent, which is the
`sync-to` direction. `sync` (CWD absorbs source) is the wrong
direction for retirement — syncing from the parent into the workweave
before deleting the workweave is the inverse of what the operator wants.

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

Running `rwv sync-to` with no `<target>` argument pushes to the recorded
parent of the current workweave. The parent is whatever workspace was
CWD when `rwv workweave create` ran; it lives in the workweave's
`.rwv-workweave` marker. See
[workweave-hierarchy](./workweave-hierarchy.md) for the marker shape and
the one-hop semantics.

In a primary weave, bare `rwv sync-to` has no parent to follow and is an
error. The operator must name an explicit target.

`rwv sync` (CWD absorbs a source) never has an auto-target — it always
requires an explicit `<source>` argument, in both primary and workweave
contexts. The auto-target is `sync-to`'s feature because `sync-to` is
the dominant landing direction: most invocations are from a workweave
pushing its work toward its parent.

## Parallel sync (`-j N`) and NDJSON

Both `rwv sync` and `rwv sync-to` support parallel execution. The
manifest-repo loop in Phase 2 fans out across a bounded worker pool
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
`docs/reference/schemas/sync.json` and embedded inside the `rwv explain
sync` bundle.

Anchored by `tests/doc_claims_sync_test.rs` (envelope shape; `outcomes`
key) and `tests/sync_json_test.rs` (NDJSON streaming).

## Symmetries and asymmetries

### Explicit direction pair: `sync` vs. `sync-to`

`rwv sync <source>` and `rwv sync-to <target>` are a direction-explicit
pair, not two spellings of the same thing. In both verbs, CWD's unique
commits are replayed onto the named workspace's tip; what differs is
where the result is written:

| Verb | Named workspace role | CWD's commits replay onto… | Final result written to |
|---|---|---|---|
| `rwv sync <source>` | State base (read-only) | Named source's tip | CWD |
| `rwv sync-to <target>` | State base + final recipient | Named target's tip (step 1) | CWD, then target FF-advances (step 3) |

The mental model is the `cp`/`rsync` convention: the argument is the
source in `cp src dest` (source first), and the argument is the
destination in `rsync --dest <target>` (dest-explicit). Here, `sync
<source>` names what CWD will absorb; `sync-to <target>` names where
CWD's state will land.

The argument-position mnemonic works because in both cases the *named*
workspace is the base CWD aligns against — the distinction is
whether the final landing is in CWD (sync) or in the named workspace
(sync-to).

### Shared core engine; sync-to adds orchestration

Both verbs use the same Phase 2 → Phase 1' → Phase 3 engine for the
core replay step. The savepoint protocol (`refs/rwv/pre-op/<id>`) and
abort contract are shared.

`rwv sync-to` wraps that engine in a 3-step orchestration: (1) run the
core sync engine with CWD as destination and named target as source of
state; (2) auto-relock CWD if manifest tips moved; (3) FF-advance the
target to CWD's new tip. The core engine is unchanged; `sync-to` adds
the steps that land the result in the named workspace.

### Asymmetric in which workspace(s) change

`rwv sync <source>` changes exactly CWD. The named source is read-only.

`rwv sync-to <target>` changes CWD first (step 1: CWD absorbs target's
state with CWD's commits on top; step 2: auto-relock CWD), then changes
the target (step 3: target FF-advances to CWD's new tip). Both
workspaces are mutated, in that sequence.

This is what makes the auto-target-to-parent the right default for
`sync-to`: a workweave landing to its parent should update the parent,
which is the intended direction for closing out work.

### Asymmetric in auto-target

`rwv sync-to` (no argument) auto-targets the recorded parent.
`rwv sync` always requires an explicit source — there is no "absorb
from parent" default because the primary use case for the bare invocation
is landing work upward, not pulling downward.

## Abort and recovery

Before any mutating phase, the sync engine (for both `rwv sync` and
`rwv sync-to`) writes savepoints under `refs/rwv/pre-op/<id>` capturing
each repo's pre-op HEAD. The identifier is wall-clock
nanosecond-resolution so concurrent or interleaved invocations cannot
collide.

`rwv abort` rolls every repo back to its savepoint. The discarded
commits remain reachable from the savepoint ref until git's normal
unreferenced-object collection runs, so recovery is possible by hand
even after an abort.

Conflict resolution hint text (the "edit conflicted files; `git add
<files>`; `git rebase --continue`" block) is owned by the VCS layer —
see `Vcs::conflict_resolution_hint` in
[vcs-as-seam](./vcs-as-seam.md). The sync engine embeds that text
verbatim into its bail messages so the operator sees concrete next steps.

## Two worked scenarios

### Forward sync (clean case)

A workweave finishes work and the workweave operator runs `sync-to`
to land:

```text
primary/project: ─── C1
                       \
workweave/project: ──── C1 ─── C2-lock
```

From inside the workweave: `rwv sync-to primary` (or bare `rwv sync-to`,
which auto-targets the recorded parent). Under Option B's 3-step
orchestration:

Step 1 (core sync engine runs in CWD against primary):

- Phase 2: align CWD's manifest repos to primary's lock targets. CWD's
  repos are already ahead of primary's (C2-lock pinned them further
  along) — each shows `already-ahead`. No advancement needed.
- Phase 1': CWD's unique commits (C2-lock) replayed onto primary's
  project tip (C1). C2-lock is a linear descendant of C1 — fast-forward.
  C2-lock's only change is `rwv.lock`, which is excluded from replay,
  so the non-lock project content (none here) fast-forwards as an
  empty-patch no-op. CWD remains at C2-lock.
- Phase 3: lock regenerated in CWD from CWD's post-Phase-2 manifest
  tips (unchanged). Lock is identical to C2-lock's; no new commit.

Step 2: auto-relock CWD — no-op, lock didn't change.

Step 3: primary fast-forwards to CWD's tip (C2-lock). Primary now holds
C2-lock, which records the workweave's final manifest state.
`rwv.lock` in primary now reflects those manifest tips.

### N-way merge (two workweaves, serial landing)

Two workweaves both have project commits. `ww1` lands first; `ww2`
rebases and lands second.

```text
primary/project: ─── C1
                       \
ww1/project: ──────── C1 ─── CA   (doc + lock)
ww2/project: ──────── C1 ─── CB   (doc + lock)
```

Step 1 — land ww1 into primary (ff):

```
cd /path/to/ww1
rwv sync-to primary         # or bare: rwv sync-to
# primary now at CA; manifest repos at ww1's lock targets
```

Step 2 — rebase ww2 onto primary's new tip:

ww2's project tip (CB) is now diverged from primary (CA). A bare
`sync-to` from ww2 would refuse with `ff` — the workweave operator
must first absorb primary's state into ww2:

```
cd /path/to/ww2
rwv sync primary --strategy rebase
# Phase 2: ww2's manifest repos advance to primary's lock targets
# Phase 1': CB replayed onto CA with rwv.lock excluded
#           CB's lock-only lines produce an empty patch and are skipped;
#           CB's doc changes replay cleanly onto CA
# Phase 3: lock regenerated from post-Phase-2 manifest tips
```

ww2 is now rebased on top of primary; its project history is linear.

Step 3 — land ww2 into primary (ff):

```
cd /path/to/ww2
rwv sync-to primary
# fast-forward; ww2 is strictly ahead of primary in a straight line
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
