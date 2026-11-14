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

The rule reads commits, so its answer depends on which commits the clone holds
in one case: a pin of the target's history that this clone lacks. A lacked pin
can only make the base newer, and so turn a pass into a refusal, and only where
the commit it names held some of the very work being landed. `rwv sync` judges
after its fetch of every repo, so a colleague's commit published on a branch is
judged as if it had always been here. A pin it still lacks then does not decide
the base.

`rwv land` contacts no remote: it judges on the pins this clone holds. Neither
verb refuses for a lacked pin. Each reports one, once per run, as `history pin
absent`, naming the pin and its lock commit, where a lock commit the source's
project branch does not reach holds it: the target's history since the two
branches parted. An older lacked pin is not reported, so no pin is reported
for ever. The landing's note says this clone does not hold the pin and that
`rwv sync` fetches and judges it.

What stays outside the rule is a landing made without a sync, on a clone that
lacks a pin that would have refused the work as `pin` `off lineage`. Where the
pin is from since the branches parted, the landing passes with the note. Where
it is older, the landing passes with nothing said: work built on a published
branch the target pinned and moved off before the branches parted, a
fast-forward onto a colleague's branch for instance. `rwv sync` first catches
both wherever a branch or tag of the member's remotes still holds the pin,
since its fetch brings the pin and the rule then refuses; nothing catches them
where no remote does. Either way the landing brings back commits the target
dropped, as merging them would in single-repo git, which checks nothing for a
submodule commit that has gone.

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
| cleanup | Drops the savepoints and the op record | yes | yes |

**Stopping and resuming.** A refusal at guard leaves no trace. Once the
savepoints exist, a conflict, failure or crash keeps the op record in every
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
on disk: the op still runs to completion and clears its op record, then exits
1 naming `rwv member remove <path> --delete`, the verb that removes it. Under `--retire` the dirty check
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

The op record is an owner record at the initiating workspace and a lease
pointer at every other workspace the op mutates, which keeps a second
operation out. See
[formats](../../reference/formats.md#rwv-op--owner-op-record).

## Abort: verified-restore contract

`rwv abort` restores every workspace the operation involved — CWD, and the
target of a `land` — to its pre-op state from its savepoints. A
`sync` involves only CWD.

**Savepoints.** Before replay moves anything, each ref the op may move gets
a savepoint, a `land` target's filed apart from its source's, since the
checkouts of one repo in both workspaces share its refs: each checkout's, naming the branch it was on, and each branch a
landing moves without a checkout, at the tip the landing read. Abort puts a
checkout back on its branch before it resets anything, and puts each such
branch back once no checkout is left on it.

**Keeping what it discards.** Abort resets each repo to its savepoint and,
before it does, keeps what the reset or a cancel discards, without asking who
made it: the tip of each branch it resets, where that tip holds a commit
neither the savepoint nor another branch reaches, in the branch's one slot,
and the working copy of each checkout it cancels a stopped rebase, merge or
copy in, or discards edits in, in the checkout's one slot, from whichever
workspace abort runs in. The next abort that keeps something in a slot
overwrites it, and what it held stays in the VCS's history of the slot until
that expires. This is the bar a single-repository reset sets, where the VCS
keeps the last tip a reset moved off, one deep: an operation's own commits and
a commit you made at a stop are kept alike, so no record of whose a tip is can
be wrong about it. Abort prints one line per repo it keeps something for, with
the command that shows it; `rwv backup list` shows the slots and
`rwv backup drop` drops one.

**Planned whole.** Abort plans every write on both sides before it makes any. One
blocked step (a branch gone or checked out elsewhere, a branch of a landing's
target that something else moved, something unreadable) refuses the whole abort:
nothing of either side is restored and `--continue` stays open. After it begins,
`--continue` is closed, and a write that finds a repo moved since the plan stops
there; a rerun of `rwv abort` completes the rest. `--leave-target` ends a landing
whose target abort cannot restore, restoring the source first and giving up the
target only after that.

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
resolve it and `--continue`, or `rwv abort`. A resumed operation never carries
again a dependent it has already moved: commits the branch gains after the
carry reach that dependent at its own next `rwv sync`. A dependent that cannot be
carried refuses before anything moves, one line each: cut from an older tip
(`target` dependent lacks the tip), checked out in another workspace
(`target` dependent checked out elsewhere), or known here only from another
machine's publish (`target` dependent on another machine), each with its own
choices. A dependent known only from another machine's publish whose every
published copy the branch's old tip already holds has landed: nothing is
carried, and the operation reports it, naming the remotes that still hold its
branch. A `merge` setting never rewrites the branch, so its dependents never
block it.

**Backups.** Before a rebase rewrites a branch, rwv keeps its tip as a
`rebased` backup. A branch is published when its push target holds a copy of
its name, whether or not it takes in from it. The backup of a branch that was
not published goes when the operation completes: the rebased commits hold its
work, and the line that names the backup says it is kept while the operation
runs. Where carrying a dependent along drops one of its commits (a conflict
resolved to the incoming side can leave a commit with no change), the backup
stays: no rebased commit holds that commit's work. The backup of a published branch stays until it is republished: that
branch's push then shows it diverged, and `rwv sync` and `rwv push` print
`rwv push --overwrite <repo>`, leased to the tip the backup holds, as the way
to republish it; once that push lands, the backup goes, and so does one that a
push leased to a newer tip left behind. Deleting the branch, as retiring a
landed workweave does, drops its `rebased` backups too, unless a live lock
pins a commit only one reaches. A machine that did not make the rebase sees
only the ordinary choices for a diverged branch.

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
op record, false when a phase parked it.

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
