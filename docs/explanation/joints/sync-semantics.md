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

Both verbs share a single data-driven phase machine. Plain `sync` is
the degenerate case: it runs the machine with the advance-target and
retire phases absent. `rwv sync-to` runs the full machine, with
advance-target always present and retire added only when `--retire` is
set.

This joint covers the phase machine, the record schema, the strategy
choices, abort's verified-restore contract, snapshot-read semantics,
retire-as-phase, and the named-override naming rule.

## The phase machine

One data-driven machine drives both `rwv sync` and `rwv sync-to`. The
sequence of steps in execution order:

```
guard → mark → savepoint → replay → relock → advance-target → retire → cleanup
                                              (sync-to only)   (--retire only)
```

The guard, mark, and savepoint steps run once before the driver loop.
The persisted record then names which phase the driver is in — the
single source of truth for op state. The driver writes the phase before
entering it, so a crash at any instruction re-enters the same phase on
resume (idempotent by construction). `--continue` for both verbs is:
load the owner record (following a lease pointer if invoked from a
non-owner workspace), enter the driver loop.

### guard

Runs all preconditions before any mutation. Covers: no-op-in-progress
check on every workspace the op will touch, replay-exclusion invariant
(`rwv.lock merge=ours` in committed `.gitattributes`), lock freshness,
Phase 1' ancestor check (`--strategy=ff`), and the dirty-target
preflight (`rwv sync-to`). Refusals here leave no trace.

### mark

Write the owner record at the initiating workspace (initial phase:
`replay`) and a thin lease at every other workspace the op mutates.
Source workspaces are read-only and receive no mark — safe because reads
are snapshots (see "Snapshot reads" below).

### savepoint

Write a durable pre-op snapshot reference in every repo the op may
mutate, under `refs/rwv/pre-op/<op-id>`. The op-id is nanosecond
wall-clock so concurrent or interleaved invocations cannot collide.

For `rwv sync-to`, target-workspace repos are savepointed under
`refs/rwv/pre-op/<op-id>-target` — a separate namespace from the
source-side `refs/rwv/pre-op/<op-id>`. When both sides share a git
object store (worktree topology), a single namespace would collide:
the first restore during `rwv abort` would drop the ref, leaving the
second restore unable to find it. The `-target` suffix ensures abort can
restore both sides independently.

### replay

Runs Phase 2 (manifest repos) and Phase 1' (project repo):

- **Phase 2 — manifest repos**: advance each of CWD's manifest repo
  branches to the named workspace's lock target, using the chosen
  `--strategy`. Repos already at the target SHA are no-ops. Repos behind
  the target are advanced via the strategy (ff / rebase / merge). Repos
  ahead of the target surface as `already-ahead` — the engine does not
  silently rewind CWD's working state.

- **Phase 1' — project repo, lock-excluded**: replay CWD's unique
  project commits onto the named workspace's project tip using
  `--strategy`, with `rwv.lock` excluded from each commit's effective
  diff. With `--strategy=rebase`, lock-only commits become empty patches
  and are dropped by git's `--empty=drop`. With `--strategy=merge`, the
  `merge=ours` driver resolves any lock-line collision in source's
  favour. With `--strategy=ff`, the branch pointer advances without
  replaying anything.

Re-entry rule: per-repo state is derived from the VCS itself. A repo
at its savepoint is redone; a repo mid-conflict resumes via the
VCS-native continue; a repo already at the converged target is a no-op.
No resume flags.

`--strategy=ff` with `rwv sync-to`: replay is a no-op (CWD must be
strictly ahead of target per the guard's ff precondition); the
advance-target phase does all the work.

### relock

Regenerate `rwv.lock` from the post-replay manifest tips in CWD. If
the result differs from what is currently committed, the engine commits
it automatically with a message like `lock: auto-relock after sync from
<named-workspace>`.

On completion, record the per-repo **converged tips** in the owner
record: the HEAD of each manifest repo and the project repo, keyed by
repo path (e.g. `github/foo/bar`) and `"(project)"`. These are consumed
by advance-target and by abort's HEAD-verified restore.

Re-entry rule: regenerating a lock that is already current is a no-op.

`--strategy=ff` with `rwv sync-to`: relock is a no-op (replay was a
no-op).

### advance-target (sync-to only)

FF-advance every manifest repo and the project repo in the target
workspace to CWD's converged tips (written during relock). This step is
always fast-forward regardless of `--strategy` — all rewriting happened
in CWD during replay.

Re-entry rule: ff to an already-reached tip is a no-op.

### retire (--retire only)

Run the merged-check (manifest repo tips equal in CWD and target after
the preceding advance-target) and the dirty-check (no worktree has
uncommitted changes), then delete the workweave. A failure from either
check preserves the op record at phase `retire` — `--continue` retries
after the operator reconciles; `rwv abort` rolls back the whole op,
target included. The removal itself is idempotent (a missing workweave
directory is a no-op).

Retire compares **manifest repo tips** rather than project repo tips.
The project repo's post-sync state typically diverges from the target
by exactly the auto-relock commit (Phase 3 always writes the
workweave's `workweave:` field into the lock, which the primary's lock
lacks). That commit is purely derived, so manifest tip equality is the
honest "work has converged" signal.

Retire is only meaningful inside a workweave. Run from a primary weave,
it emits a warning and is otherwise a no-op.

### cleanup

Drop all savepoints and clear the owner record and lease. Cleanup is
not a persisted phase: a crash before cleanup completes leaves the
on-disk phase at the last work phase (idempotent), and `--continue`
re-runs that phase before reaching cleanup again.

Exception: when `--discard-local-commits` discarded project commits
(recorded as the `discard-local-commits` override), the project
savepoint is preserved as a tombstone — the only remaining reference to
the discarded commits. Manifest-repo savepoints are dropped regardless.

## Record schema v2

### Owner record (`.rwv-op`)

Written at the initiating workspace (the owner). Holds all op
parameters plus the current phase. It is the sole copy of mutable op
state.

```yaml
id: "1779769917405921588"       # op id, shared with savepoint refs
verb: sync                       # "sync" | "sync-to"
strategy: rebase                 # "ff" | "rebase" | "merge"
source: /abs/path/src
target: /abs/path/tgt
retire: false
phase: replay                    # replay | relock | advance-target | retire
converged_tips: {}               # written at relock completion; empty before
overrides: []                    # named overrides supplied at invocation
started_at: 2026-06-10T21:14:03Z
```

`source` is the workspace content flows FROM; `target` is where it
flows TO. For plain `sync`, `target` is CWD (the op writes into the
owner workspace). For `sync-to`, `target` is the named target workspace.
All path fields are absolute. `started_at` is RFC3339 UTC.
`converged_tips` is populated at relock completion; empty before.

### Thin lease (`.rwv-op-lease`)

Written at every other workspace the op mutates (never at the owner).
Immutable once written; a mutex plus a redirect, nothing else.

```yaml
id: "1779769917405921588"
owner: /abs/path/to/owner/workspace
```

`--continue` and `abort` invoked from a leased workspace follow the
`owner` pointer and operate identically to owner-side invocation.

### Read-only workspaces

Not marked. Safe because source reads are snapshots (see below).

### Cleanup-ownership table

| Exit path | Record + leases |
|---|---|
| Success (all phases incl. retire) | Cleared everywhere |
| Precondition refusal (before any mutation) | Cleared everywhere (no trace) |
| Phase failure or crash | Kept everywhere (`--continue` and `abort` remain) |
| `abort` | Cleared after restore |

## Abort: verified-restore contract

`rwv abort` restores every involved workspace (both CWD and the target
for `sync-to` ops) to its pre-op state using the savepoint refs. Two
hardening rails apply to every repo:

### Rail 1 — pre-abort reference

Before restoring any repo, abort writes a durable reference at the
repo's current tip under `refs/rwv/pre-abort/<op-id>`. This reference
is never deleted by abort's cleanup. Abort is itself information-
preserving and undoable: the pre-abort ref is the cheapest path back
from any abort.

The reference is written for every repo before any restore — even for
repos that will be classified as `untouched` (no restore needed).

### Rail 2 — HEAD-verified restore

The destructive `reset --hard` to the savepoint is gated on the repo's
current tip being **attributable to the op**. The classification has
four outcomes:

| Current tip | Outcome |
|---|---|
| Equal to the savepoint | `Untouched` — op never moved this repo; HEAD not touched |
| Equal to the recorded converged tip | `RestoredFromConverged` — op converged this repo; reset to savepoint |
| Repo is in a VCS-native mid-op state (rebase / merge / cherry-pick) | `RestoredFromMidOp` — mid-op cancelled; reset to savepoint |
| Anything else | `ForeignTip` — restore **refused**; violation reported; op-state retained |

The `ForeignTip` case means commits landed in the repo after the op
crashed that abort cannot attribute to the op. Abort reports the
observed tip, the expected savepoint and converged tip, the pre-abort
ref label, and recovery options. Op-state is retained so the operator
can re-run `rwv abort` after manually reconciling the divergence.

**Post-replay-pre-relock crash case (documented deviation from initial
design):** A repo can converge in replay before relock records
`converged_tips`. If the op crashes in this window, the tip is neither
the savepoint, nor a recorded converged tip (empty before relock), nor
a mid-op state. This case is classified as `ForeignTip` — abort refuses
rather than trying to re-derive the source's lock to re-classify the
tip. The rationale: re-pinning the source has a TOCTOU race (source may
have moved between the crash and abort), and foreign-tip refusal is the
conservative safe position. The refusal message explicitly names this
case and offers the recovery option of manually accepting the converged
tip and running `rwv lock` to re-pin.

### First-write-wins for pre-abort refs

If a pre-abort ref already exists for this op (from a prior abort
attempt), it is returned unchanged rather than overwritten — the
earlier capture is the more valuable one, and by the time abort is
re-run it may be the only remaining reference to that tip. This makes
pre-abort-ref writes idempotent across abort retries. (Mechanically:
the ref is resolved first and the write is skipped when it exists —
see `Vcs::create_pre_abort_ref`.)

### Side-specific ref namespaces

CWD-side repos use `refs/rwv/pre-abort/<op-id>`. Target-side repos
in a `sync-to` op use `refs/rwv/pre-abort/<op-id>-target`. The same
reasoning as for savepoints applies: worktree pairs share a git object
store, and a single namespace would collide.

## Snapshot reads

The source is pinned once at the start of the operation (T₀): one
atomic read of the source project repo's current `HEAD`. The manifest
and lock are then read at that revision via `Vcs::read_file_at_revision`
(git: `git show <rev>:<path>`), not from the working tree. Per-repo sync
targets are the lock's revision IDs — content-addressed, immutable. A
concurrent mutation of the source after T₀ changes refs but cannot
touch anything the op has read.

The result is "synced to source-as-of-T₀" — a coherent state that
actually existed. Combined with the start-time no-op-in-progress check
on the source workspace, source reads are effectively serializable with
no locks.

**T₀ is per-session, not per-op.** On `--continue`, the source snapshot
is re-established at the start of the resumed session — a new T₀ is
pinned, not the one from the original session. Per-repo no-op detection
(repos already at the converged target are skipped) handles repos that
converged in a prior session. The resumed session is "synced to
source-as-of-new-T₀"; the op's overall convergence target may differ
from the original session's if the source advanced between sessions.
This is the correct behavior: operators resolving conflicts between
sessions generally want the latest state, not a stale pin.

## Strategy choice

Both repo classes — project repo (Phase 1') and manifest repos (Phase
2) — run under the same `--strategy` choice.

| Strategy | Project repo treatment | Manifest repo treatment |
|---|---|---|
| `ff` (default for `sync`) | Fast-forward only; refuse on divergence | Fast-forward only; refuse on divergence |
| `rebase` (default for `sync-to`) | Replay CWD's unique commits onto named-workspace's tip; lock excluded | Advance CWD's repos to named-workspace's lock targets |
| `merge` | Merge named-workspace's tip into CWD's history; lock excluded | Advance CWD's repos to named-workspace's lock targets |

`--strategy=rebase` and `--strategy=merge` require `rwv.lock merge=ours`
in the project repo's **committed** `.gitattributes`. Both halves must
be in place: `rwv` passes `-c merge.ours.driver=true` on each git
invocation (defines the driver), and the `.gitattributes` line assigns
that driver to `rwv.lock`. The check fires against the committed file
(via `git show HEAD:.gitattributes`), not the working tree, because the
invariant must survive rebases. Run `rwv doctor --fix` to add the line.

`--strategy=ff` does not need the precondition: FF advances the branch
pointer without performing a merge.

## Named overrides and the naming rule

`--force` was removed from both `sync` and `sync-to`. Each precondition
that `--force` previously bypassed now has its own named override:

- `--allow-stale-lock` — skip the lock-freshness precondition on both
  source and destination. Use when the lock is intentionally ahead of
  HEAD. Recorded as `allow-stale-lock` in `overrides`.
- `--discard-local-commits` — hard-reset the CWD project repo to the
  source tip, discarding any destination-only committed divergence.
  Refused if the project repo has uncommitted changes (those would be
  destroyed unrecoverably by the reset, unlike committed divergence
  which is preserved in the savepoint). Recorded as
  `discard-local-commits` in `overrides`.

**The house rule:** a flag's name states what it destroys — consent to
a consequence, never a category. `--discard-local-commits` names the
exact loss; the operator reading it knows what they are signing.

Named overrides are recorded in the owner record's `overrides` field so
`--continue` resumes with the same consents without requiring the
operator to re-supply flags. The record is an audit trail.

## The Phase 1' ancestor precondition

Before Phase 1', the engine checks the ancestor relationship between
CWD's project tip and the named workspace's project tip:

| Relation | `ff` | `rebase` / `merge` |
|---|---|---|
| Equal tips | No-op, allowed | No-op, allowed |
| CWD is ancestor of named workspace | Allowed — fast-forward | Allowed |
| Named workspace is ancestor of CWD (CWD ahead) | Refused | Handled by strategy |
| Diverged (neither ancestor) | Refused | Handled by strategy |

With `ff`, the engine refuses when CWD has project commits not reachable
from the named workspace. The error message points at `--strategy
rebase` or `--strategy merge` as the paths that *land* those commits,
and at `--discard-local-commits` as the path that *discards* them
(preserving them in the savepoint tombstone).

With `rebase` or `merge`, the strategy itself is the answer to
divergence; the precondition is bypassed.

## VCS mapping (git)

This document is written VCS-neutrally. The git realization of each
mechanism is:

| Neutral mechanism | Git realization |
|---|---|
| Durable pre-op snapshot reference | `refs/rwv/pre-op/<op-id>` |
| Target-side pre-op snapshot reference | `refs/rwv/pre-op/<op-id>-target` |
| Pre-abort reference | `refs/rwv/pre-abort/<op-id>` |
| Target-side pre-abort reference | `refs/rwv/pre-abort/<op-id>-target` |
| VCS-native mid-op state / continue / abort | rebase/merge/cherry-pick state dirs; `--continue` / `--abort` |
| Atomic source pin (T₀) | one `HEAD` resolution of the source project repo |
| Read manifest/lock at a revision | `git show <rev>:<path>` (`Vcs::read_file_at_revision`) |
| ff-advance to converged tip | `git merge --ff-only <rev>` (never `reset --hard`) |
| Verified restore | `git reset --hard <savepoint>` gated on tip ∈ {savepoint, converged tip, mid-op} |
| Object transfer of pinned revisions | shared object store (worktrees: no-op) |
| Lock replay-exclusion (rebase/merge) | `rwv.lock merge=ours` in `.gitattributes`; `-c merge.ours.driver=true` per invocation |

All of these are intent-named `Vcs` trait methods; the phase machine
contains no VCS-specific spellings.

## Parallel sync (`-j N`) and NDJSON

Both `rwv sync` and `rwv sync-to` support parallel execution. The
manifest-repo loop in Phase 2 fans out across a bounded worker pool
when `-j N` is set with `N > 1`.

`--json` interacts with parallel mode in a specific way:

- **Serial / envelope mode** (`-j 1`, the default for `--json`):
  outcomes are collected as they come in, then a single JSON document is
  printed to stdout at the end: `{"$schema": "<url>", "outcomes":
  [...]}`. The envelope key is `"outcomes"`.
- **Parallel / NDJSON mode** (`-j N > 1` with `--json`): each per-repo
  outcome is streamed as one JSON line to stdout the moment its worker
  finishes. The envelope wrapper is dropped, and each line carries its
  own `"$schema"` field.

Exit semantics under `--json` are the same in both modes: non-zero iff
at least one per-repo outcome is `failed`.

## `--retire` — close out a workweave in one step

`rwv sync-to --retire` is the one-shot landing verb. It runs the full
sync-to machine (replay → relock → advance-target → retire), where
retire runs the merged-check and dirty-check before deleting the
workweave. Op-state spans all four phases, so a merged-check failure is
resumable (`rwv sync-to --continue`) and the whole op is abortable
(`rwv abort`).

Why `--retire` lives on `sync-to` and not on `sync`: the dominant "land
my work" workflow goes workweave → parent, which is the `sync-to`
direction.

Why `--retire` and not `--land`: "land" overloads PR-merge vocabulary
and misleads when parent isn't primary. "Retire" is honest about what
the flag does — close out this workweave — regardless of where in the
workweave tree it sits.

`--retire` is only meaningful inside a workweave. Run from a primary
weave, it emits a warning and is otherwise a no-op.

## Symmetries and asymmetries

### Explicit direction pair: `sync` vs. `sync-to`

| Verb | Named workspace role | CWD's commits replay onto… | Final result written to |
|---|---|---|---|
| `rwv sync <source>` | State base (read-only) | Named source's tip | CWD |
| `rwv sync-to <target>` | State base + final recipient | Named target's tip (replay) | CWD, then target FF-advances |

The mental model is the `cp`/`rsync` convention: the argument is the
source in `cp src dest` (source first), and the argument is the
destination in `rsync --dest <target>` (dest-explicit). Here, `sync
<source>` names what CWD will absorb; `sync-to <target>` names where
CWD's state will land.

### Shared machine; sync-to adds orchestration

Both verbs use the same phase machine. Plain `sync` omits
advance-target and retire. The savepoint protocol and abort contract are
shared. `rwv sync-to`'s advance-target phase is what lands the result in
the named workspace.

### Asymmetric in which workspace(s) change

`rwv sync <source>` changes exactly CWD. The named source is read-only.

`rwv sync-to <target>` changes CWD first (replay: CWD absorbs target's
state with CWD's commits on top; relock: auto-relock CWD), then changes
the target (advance-target: target FF-advances to CWD's new tip). Both
workspaces are mutated, in that sequence.

### Auto-target

`rwv sync-to` (no argument, inside a workweave) auto-targets the
recorded parent from `.rwv-workweave`. In a primary weave, bare
`rwv sync-to` is an error.

`rwv sync` always requires an explicit `<source>`. There is no
"absorb from parent" default.

## Abort and recovery

Before any mutating phase, the engine writes savepoints under
`refs/rwv/pre-op/<op-id>` (and `refs/rwv/pre-op/<op-id>-target` for
target-side repos in `sync-to`). `rwv abort` applies the
verified-restore contract to every involved repo. See "Abort:
verified-restore contract" above.

After `--discard-local-commits`, the project savepoint is preserved as
a tombstone even after the successful sync completes. `rwv abort` will
refuse (no op-state), but the tombstone ref at
`refs/rwv/pre-op/<op-id>` remains:

```
git reset --hard refs/rwv/pre-op/<op-id>
```

## Related joints

- [lock-as-derived](./lock-as-derived.md) — the property the entire
  phase model is structured around.
- [workweave-hierarchy](./workweave-hierarchy.md) — the tree the
  parent-tracking auto-target walks.
- [workweave-lifecycle](./workweave-lifecycle.md) — the operator-facing
  lifecycle (create → work → sync-to --retire → delete) that the phase
  machine underpins.
- [shared-refs-drift](./shared-refs-drift.md) — the post-Phase-2
  reconciliation interacts with shared-refs drift in workweaves.
- [vcs-as-seam](./vcs-as-seam.md) — replay exclusion, conflict hint
  text, and the conflict-bail surface are all Vcs-trait responsibilities.
