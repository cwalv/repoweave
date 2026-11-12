# Sync semantics

Two verbs, two directions:

- **`rwv sync`** — the checked-out weave branch takes in new work: a
  feature takes in the branch its record lands into (its target), and any
  weave branch can merge another's history in with `--from <branch>`. Only
  this workspace changes.
- **`rwv land`** — CWD's committed state lands in the target
  workspace. CWD absorbs target's state first (CWD's commits on top),
  then the target fast-forwards to CWD's new tip. Both CWD and target
  change; the named target ends up with CWD's commits linearly above
  its prior state.

`rwv sync` names no workspace: what it takes in is a branch, read from the
branch's record. `rwv land` names the workspace that receives
CWD's state (like `git push <target>`).

## Taking in: `rwv sync`

A feature is work meant to land in its target, so the question `rwv sync`
answers is how the target's new work enters the feature without moving
anything the landing will later need. Each `owned` or `fork` member takes in
its **merge source**: the tip of the branch the member will land on, read as
the newer of that branch and its upstream, or the target's pin where that
branch exists nowhere. Taking in the branch rather than the pin is what keeps
a member current with commits the target's lock has not recorded yet; the
sync lists those commits, since the landing will pin them.

A member with no new work follows its merge source, never backward. One with
new work is integrated as the target's `history` setting says: `linear` and
`ff-only` replay the work onto the merge source after a backup, and `merge`
merges the merge source in, the feature's branch as first parent. A range
holding a merge commit is never rebased, since the rebase would flatten the
second parent through which another lineage's work entered: a rebasing
setting integrates it by a merge commit instead, reported `merged`. Then the
project branch takes in the target's tip the same way, and
the lock is recorded once: each member's pin merged three ways against the
merge base's lock, with every member the sync moved at its new tip.

**The lineage rule.** A member's work has a base: the newest commit of its
first-parent chain that some lock in the target's history pins. Taking the
target in is safe only when the branch the member lands on contains that base;
otherwise the work was built on a line the target has left (a declared-branch
change, a rewritten branch), and integrating it would carry that line along.
The sync refuses such a member as `pin` `off lineage` before anything moves.
The rule reads commits only, so every machine computes the same answer.

**`--from <branch>`** merges another weave branch's history in, whatever the
setting, and always as a merge commit: work moving between published branches
moves by merge, never by rebase.

Every refusal is judged before anything moves and reported together. Then the
sync runs under an op record with a savepoint in every repo it may move: a
conflict stops it, `rwv sync --continue` resumes, and `rwv abort` restores
every repo to its savepoint. Once every repo has taken in, members the new
entries drop are pruned and the rest are aligned as `rwv update` aligns them.

## The `land` phase machine

`land` runs one machine; retire runs only under `--retire`:

```
guard → mark → savepoint → replay → relock → advance-target → retire → cleanup
                                                               (--retire only)
```

| Phase | What it does | `land` | `land --retire` |
|---|---|:---:|:---:|
| guard | Checks every precondition before anything moves | yes | yes |
| mark, savepoint | Records the operation and writes a savepoint ref in every repo it may move | yes | yes |
| replay | **Phase 1'** replays CWD's unique project commits onto the target's project tip, with `rwv.lock` excluded from each commit; **Phase 2** advances CWD's member repos to the target's lock targets | yes | yes |
| relock | Regenerates `rwv.lock` from CWD's new tips (**Phase 3**) and commits it if it changed | yes | yes |
| advance-target | Fast-forwards the target's member repos, then its project repo, to CWD's tips | yes | yes |
| retire | Checks that member tips match, that no checkout is dirty and that replay left no checkout it could not prune, then deletes the workweave | — | yes |
| cleanup | Drops the savepoints and the op-state | yes | yes |

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
1 naming the checkout to remove by hand. Under `--retire` the dirty check
refuses first if that checkout holds uncommitted changes, so the workweave is
not deleted over them; what it committed stays on its branch, which the
deletion rule keeps unless it landed.

**Whose lock is the target.** Phase 2 advances to the target's lock.
`land` refuses a target whose lock is behind its HEAD, naming the repo, its
commit count and the `rwv lock --commit` that fixes it. A repo already ahead
of its target is reported `already-ahead` and left where it is.

**Members land before the project repo.** Advance-target moves the target's
project repo only after every member repo landed, so the target never holds
a lock naming revisions it lacks. Members that landed stay landed, leaving a
lock behind HEAD that `rwv lock --commit` refreshes; `rwv abort` rolls them
back.

The op-state is an owner record at the initiating workspace and a lease
pointer at every other workspace the op mutates, which keeps a second
operation out. See
[formats](../../reference/formats.md#rwv-op--owner-op-state-record).

## Abort: verified-restore contract

`rwv abort` restores every workspace the operation involved — CWD, and the
target of a `land` — to its pre-op state from the savepoint refs. A
`sync` involves only CWD.

**Savepoints.** Before replay moves anything, each ref the op may move gets
a savepoint under `refs/rwv/pre-op/<op-id>`, and a `land` target's under
`refs/rwv/pre-op/<op-id>-target`, because worktrees of one repo share a ref
store: each checkout's, naming the branch it was on, and each branch a
landing moves without a checkout, at the tip the landing read. Abort puts a
checkout back on its branch before it resets anything, and puts each such
branch back once no checkout is left on it.

**Verified restore.** Abort resets a repo to its savepoint only
when the repo's tip is one the operation accounts for: the savepoint itself
(nothing to restore), a tip the op recorded while advancing or converging
that repo, or a rebase, merge or cherry-pick the op left in progress. Every
such tip is a commit the op made, so a clean abort writes no ref: what it
discards is the op's own output. Any other tip, such as a commit made after
the op stopped, is a foreign tip: abort refuses that repo, leaves its tip in
place, keeps the op-state, and lists the blocking commits.
`--abandon-foreign-tip <repo>` consents to restoring over it, and the tip is
first kept as a backup marked `abandoned`, named for the branch it moves
off. The consent applies to the tip abort finds when it runs, so a foreign
agent that kept committing between two runs has the tip as it then stands
kept. `rwv backup list` shows the backup and `rwv backup drop` drops it.

## Snapshot reads

A `land` reads its target once, at the start of the operation (T₀): its
project `HEAD`, with the manifest and lock read at that revision. A change to
the target after T₀ cannot reach anything the op has read. On `--continue` a
new T₀ is pinned, so a resumed op converges on the target as it is then. A
`sync` records the tips it takes in in its op record, and `--continue` takes in
those same tips.

## The history setting

How a target accepts work is its own `history` setting, a key in its
`rwv.toml`: at the top level for the project branch and every member without
its own, and in a member's entry for that member. `rwv branch set-history`
writes it. Nothing is the default: a repo that needs more than a fast-forward,
where the target records nothing, refuses as `history` `unset`, naming the
setter and `--history <h>`, which supplies a value for one run only where the
target records none. The same setting governs both directions, so a feature
takes its target in the way the target will take the feature:

| Setting | `rwv sync` | `rwv land` |
|---|---|---|
| `linear` | fast-forward; else rebase the feature's branch onto the merge source | fast-forward; else rebase the feature's branch onto the receiving branch, then fast-forward it |
| `merge` | fast-forward; else merge the merge source in, the feature's branch first | fast-forward where the receiving tip is on the feature's first-parent chain; else a merge commit, the receiving tip first |
| `ff-only` | as `linear` | fast-forward; else refused as `history` `not fast-forward`, naming `rwv sync` |

`ff-only` differs from `linear` only at landing: the rebase happens at `sync`,
where the result can be built and tested, and the landing then only
fast-forwards. Under every setting the target's own move is a fast-forward:
whatever is rebased or merged is composed in the source first.

Work moving between published branches moves by merge: `--from` always
merges, and a range holding a merge commit is integrated by a merge commit
under a rebasing setting rather than flattened. A feature that took its
target in by a merge holds the receiving tip only through that merge's second
parent; landing it under `linear` or `merge` makes a merge commit whose first
parent is the receiving tip, so the target keeps its own first-parent chain.

A rebase replays each commit as a three-way merge; rwv holds the lock merge
driver for the replay, so a lock-only commit replays without a conflict, and
once the member rebases are done each replayed project commit is given the
lock that pins what they made of the commits it pinned: the VCS reports which
old commit became which, including for a rebase the operator finished with the
VCS's own continue. A replayed commit left changing nothing is dropped, as
the rebase drops one. The relock records anything beyond. The replay reads the
attribute line the tree it replays onto commits: `rwv land` refuses
`missing-lock-merge-driver` for the source's project checkout as well as for
the workspace it writes, and `rwv doctor --fix`, run where the refusal names,
migrates it. Where that tree assigns `rwv.lock` no driver, rwv's own replay
merges the lock as text and stops on a conflict there, never taking a side. A bare
`git rebase --continue` runs the clone's lock merge driver instead, which
merges pins three-way; [vcs-as-seam](./vcs-as-seam.md) covers the mechanism.

**Stacks.** A branch other features land into (its dependents, by their
records) is rebased with them: each dependent's commits beyond the branch's
old tip are replayed onto its new tip, parents before their own dependents,
each in the checkout the rebase ran in, and every dependent's branch moves
only once all of them have replayed, a `rebased` backup kept of each. A
dependent's project branch is replayed after its members, so its pins follow
them. A conflict in one stops the operation there, no dependent moved yet:
resolve it and `--continue`, or `rwv abort`. A dependent that cannot be
carried refuses before anything moves, one line each: cut from an older tip
(`target` dependent lacks the tip), checked out in another workspace
(`target` dependent checked out elsewhere), or known here only from another
machine's publish (`target` dependent on another machine), each with its own
choices. A `merge` setting never rewrites the branch, so its dependents never
block it.

**Backups.** Before a rebase rewrites a branch, rwv keeps its tip as a
`rebased` backup. One of a branch that was not published goes when the
operation completes: the rebased commits hold its work. One of a published
branch stays until it is republished: that branch's push then shows it
diverged, and `rwv push` prints `rwv push --overwrite <repo>`, leased to the
tip the backup holds, as the way to republish it; once that push lands, the
backup goes. A machine that did not make the rebase sees only the ordinary
choices for a diverged branch.

## Named overrides

Neither verb takes `--force`. Each precondition that can be waived has its
own override, named for what it gives up:

- `rwv sync --reset-to-target <repo>` — move the named repo's branch to what
  it lands on instead of integrating it, keeping the tip it leaves as a backup
  and listing the work it drops.
`--continue` resumes with the plan the op recorded. The naming rule
behind these flags, and why `push --overwrite` is named for the repo it replaces, is stated once
in [destructive-operations](../destructive-operations.md#2-named-overrides-are-narrow-and-informed).

## Parallel landing (`-j N`)

With `-j N` (`N > 1`), `land`'s Phase 2 fans out across a bounded worker pool. The
default is `-j 1`. `--json` prints the same envelope whatever `-j` is, once
the op is done: `{"$schema": "<url>", "outcomes": [...]}`, with `outcomes`
sorted by repo path however the workers finished, beside `placements`, replay
`members`, and `complete`. The exit status is non-zero when some per-repo
outcome is `failed`, and when a prune was refused: the op completes,
reports a `prune-refused` member record, and exits 1. The envelope's
`complete` field is true when the op ran its last phase and cleared its
op-state, false when a phase parked it.

## Auto-target

`rwv land` lands into the checked-out feature's record's target, wherever that
target is checked out; `--target <T>` lands into another branch for one run. `rwv sync` reads what it
takes in from the checked-out branch's record, and `--target <T>` takes in
another branch for one run.

## Related joints

- [lock-as-derived](./lock-as-derived.md) — the property the entire
  phase model is structured around.
- [workweave-hierarchy](./workweave-hierarchy.md) — the tree the
  parent-tracking auto-target walks.
- [workweave-lifecycle](./workweave-lifecycle.md) — the operator-facing
  lifecycle (create → work → land --retire → delete) that the phase
  machine underpins.
- [shared-refs-drift](./shared-refs-drift.md) — the post-Phase-2
  reconciliation interacts with shared-refs drift in workweaves.
- [vcs-as-seam](./vcs-as-seam.md) — the lock merge driver, conflict hint
  text, and the conflict-bail surface are all Vcs-trait responsibilities.
