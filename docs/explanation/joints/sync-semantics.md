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

In `rwv sync <source>` the argument names the workspace CWD absorbs (like
`git merge <source>`); in `rwv sync-to <target>` it names the workspace that
receives CWD's state (like `git push <target>`).

## The phase machine

Both verbs run one machine. Plain `sync` leaves out advance-target and
retire; `sync-to` adds retire only under `--retire`:

```
guard → mark → savepoint → replay → relock → advance-target → retire → cleanup
                                              (sync-to only)   (--retire only)
```

| Phase | What it does | `sync` | `sync-to` | `sync-to --retire` |
|---|---|:---:|:---:|:---:|
| guard | Checks every precondition before anything moves | yes | yes | yes |
| mark, savepoint | Records the operation and writes a savepoint ref in every repo it may move | yes | yes | yes |
| replay | **Phase 1'** replays CWD's unique project commits onto the named workspace's project tip, with `rwv.lock` excluded from each commit; **Phase 2** advances CWD's member repos to the named workspace's lock targets | yes | yes | yes |
| relock | Regenerates `rwv.lock` from CWD's new tips (**Phase 3**) and commits it if it changed | yes | yes | yes |
| advance-target | Fast-forwards the target's member repos, then its project repo, to CWD's tips | — | yes | yes |
| retire | Checks that member tips match, that no checkout is dirty and that replay left no checkout it could not prune, then deletes the workweave | — | — | yes |
| cleanup | Drops the savepoints and the op-state | yes | yes | yes |

**Stopping and resuming.** A refusal at guard leaves no trace. Once the
savepoints exist, a conflict, failure or crash keeps the op-state in every
workspace the operation mutates. `--continue`, from either workspace, resumes
at the recorded phase; `rwv abort` restores both workspaces to where they
started. A resume at advance-target runs relock again first, so the lock it
publishes still pins CWD's tips after whatever fixed the stall. A failed
retire check keeps the op at retire, and `--continue` retries the checks.
`--retire` from a primary weave warns and is otherwise ignored.

**Reference repos sync like any other repo.** When the phases finish, each
workspace the op touched settles every repo for the role its manifest gives
it, so a `reference` repo's tracked files are read-only again and a role
change that arrived with the manifest takes effect on landing. A workweave
holding a repo as a symlink is refused before any phase runs
([`workweave-drift`](../../reference/doctor-findings.md#workweave-drift)).

**Replay's member set follows the replayed manifest.** After Phase 1', CWD's
`rwv.toml` alone decides which checkouts exist: a member it names that CWD
lacks is materialized, and a member it dropped is pruned, behind the dirty,
unique-commit and store-claim refusals. Pruning waits until every repo has
replayed, so an op that parks has removed nothing — `rwv abort` restores refs
and cannot bring back a deleted checkout. A refused prune leaves its checkout
on disk: the op still runs to completion and clears its op-state, then exits
1 naming the checkout to remove by hand. Under `--retire` the retire check
refuses first (`retire-unpruned-checkout`), so the workweave is not deleted
with that checkout in it.

**Whose lock is the target.** Phase 2 advances to the named workspace's
lock, with one exception: a plain `sync` from a workweave whose lock is
behind its committed HEAD takes that workweave's committed tips instead and
prints a note naming the lag. `sync-to` refuses a target whose lock is behind
its HEAD, naming the repo, its commit count and the `rwv lock --commit` that
fixes it. A `sync` destination whose own lock lags is announced, not refused:
relock regenerates that lock at the end of the op. A repo already ahead of
its target is reported `already-ahead` and left where it is.

**Members land before the project repo.** Advance-target moves the target's
project repo only after every member repo landed, so the target never holds
a lock naming revisions it lacks. Members that landed stay landed, leaving a
lock behind HEAD that `rwv lock --commit` refreshes; `rwv abort` rolls them
back.

The op-state is an owner record at the initiating workspace and a lease
pointer at every other workspace the op mutates, which keeps a second
operation out; a source that `rwv sync` only reads is not marked. See
[formats](../../reference/formats.md#rwv-op--owner-op-state-record).

## Abort: verified-restore contract

`rwv abort` restores every workspace the operation involved — CWD, and the
target of a `sync-to` — to its pre-op state from the savepoint refs.

**Savepoints.** Before replay moves anything, each repo the op may move gets
a savepoint ref, `refs/rwv/pre-op/<op-id>`, and a `sync-to` target's repos get
`refs/rwv/pre-op/<op-id>-target`, because worktrees of one repo share a ref
store.

**Rail 1 — a pre-abort ref.** Before restoring any repo, abort writes
`refs/rwv/pre-abort/<op-id>` (`-target` on the target side) at the repo's
current tip, and never deletes it. Whatever abort resets away stays
reachable there.

**Rail 2 — HEAD-verified restore.** Abort resets a repo to its savepoint only
when the repo's tip is one the operation accounts for: the savepoint itself
(nothing to restore), a tip the op recorded while advancing or converging
that repo, or a rebase, merge or cherry-pick the op left in progress. Any
other tip, such as a commit made after the op stopped, is a foreign tip:
abort refuses that repo, keeps the op-state, and lists the blocking commits.
`--abandon-foreign-tip <repo>` consents to restoring over it, and the
abandoned commits stay reachable at the pre-abort ref.

**First write wins, with one exception.** A pre-abort ref an earlier abort
attempt wrote is kept rather than overwritten, because by then it may be the
only name left for that tip. The exception is a repo named by
`--abandon-foreign-tip` whose earlier capture is an ancestor of its current
tip: the ref advances to the current tip, which still reaches every commit
the earlier capture named. A capture that has diverged from the current tip,
after a foreign rebase or reset, is kept, and abort refuses to abandon that
repo until the branch is reconciled or the ref is deleted.

## Snapshot reads

The source is pinned once, at the start of the operation (T₀): its project
`HEAD`, with the manifest and lock read at that revision, and — for the
workweave-source exception above — its member tips. A change to the source after T₀
cannot reach anything the op has read. On `--continue` a new T₀ is pinned,
so a resumed op converges on the source as it is then.

## Strategy choice

Both repo classes — the project repo (Phase 1') and member repos (Phase 2) —
run under the same `--strategy`:

| Strategy | Project repo treatment | Member repo treatment |
|---|---|---|
| `ff` (default for `sync`) | Fast-forward only; refuse on divergence | Fast-forward only; refuse on divergence |
| `rebase` (default for `sync-to`) | Replay CWD's unique commits onto named-workspace's tip; lock excluded | Advance CWD's repos to named-workspace's lock targets |

`--strategy=rebase` requires the line `rwv.lock merge=rwv-ours` in the
project repo's **committed** `.gitattributes`: a rebase replays each commit
as a three-way merge, and that driver keeps a lock-only commit from
conflicting on `rwv.lock`. `rwv doctor --fix` adds the line. rwv also defines
the driver on each rebase it runs and in the project repo's local git config,
so a bare `git rebase --continue` there is safe; [vcs-as-seam](./vcs-as-seam.md)
covers member repos that declare it. `--strategy=ff` needs no precondition.

### Why no `merge` strategy

`--strategy` accepts only `ff` and `rebase`. A merge strategy earns its place
only when rebase would rewrite commits that are already published, and in
repoweave it does not:

- **Workweave branches are unpublished by policy.** `rwv push` refuses from a
  workweave. That is policy, not physics: no hook or doctor check stops a
  plain `git push`, but while nobody bypasses rwv that way, rebasing a
  workweave branch rewrites nothing anyone else can have seen.
- **Landing rebases only the unpublished side.** `sync-to` rebases CWD (the
  workweave) against the target, then fast-forwards the target.
- **Divergence that arrives through a remote** is `update`'s and git's to
  handle, not a sync strategy's.

For two workspaces that share no origin and both carry published history,
reconcile each repo by hand with `git merge`, then run `rwv lock` to pin the
merged tips.

## Named overrides

Neither verb takes `--force`. Each precondition that can be waived has its
own override:

- `--allow-stale-lock` — skip the lock-freshness precondition on both source
  and destination, for a lock that is intentionally ahead of HEAD. On a
  `sync-to` whose target lock is *behind* its HEAD it only defers the refusal
  to advance-target's fast-forward, so refresh that lock instead.
- `--discard-local-commits` — hard-reset CWD's project repo to the source
  tip, discarding committed divergence on the destination side. Refused if
  the project repo has uncommitted changes, which the reset would destroy
  unrecoverably. The discarded commits outlive the op: the project repo's
  savepoint is kept as a tombstone, and once `rwv abort` no longer applies,
  `git reset --hard refs/rwv/pre-op/<op-id>` in the project repo recovers
  them.

`--continue` resumes with the overrides the op started with. The naming rule
behind these flags, and why `push --force` is exempt from it, is stated once
in [destructive-operations](../destructive-operations.md#2-named-overrides-are-narrow-and-informed).

## The Phase 1' ancestor precondition

Before Phase 1', the engine checks the ancestor relationship between CWD's
project tip and the named workspace's project tip:

| Relation | `ff` | `rebase` |
|---|---|---|
| Equal tips | No-op, allowed | No-op, allowed |
| CWD is ancestor of named workspace | Allowed — fast-forward | Allowed |
| Named workspace is ancestor of CWD (CWD ahead) | Refused | Handled by strategy |
| Diverged (neither ancestor) | Refused | Handled by strategy |

The `ff` refusal names `--strategy rebase`, which *lands* CWD's project
commits, and `--discard-local-commits`, which *discards* them into the
savepoint tombstone.

## Parallel sync (`-j N`)

With `-j N` (`N > 1`), Phase 2 fans out across a bounded worker pool. The
default is `-j 1`. `--json` prints the same envelope whatever `-j` is, once
the op is done: `{"$schema": "<url>", "outcomes": [...]}`, with `outcomes`
sorted by repo path however the workers finished, beside `placements`, replay
`members`, and `complete`. The exit status is non-zero when some per-repo
outcome is `failed`, and when a prune was refused: the op completes,
reports a `prune-refused` member record, and exits 1. The envelope's
`complete` field is true when the op ran its last phase and cleared its
op-state, false when a phase parked it.

## Auto-target

Bare `rwv sync-to` inside a workweave targets the recorded parent from
`.rwv-workweave`; in a primary weave it is an error. `rwv sync` always
requires an explicit `<source>`: a landing has one meaning, advancing the
parent to CWD's tip, while a downward pull would have to pick a shape (which
of the parent's repos, at whose lock, under which strategy) that nothing has
settled.

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
