# History and lineage

Two questions decide how work moves between weave branches, and rwv answers
neither from habit. How does a branch accept work, by fast-forward, by rebase or by
merge? And is the work being moved built on the line the receiving branch
descends from, or on one it has left? The first is a setting the target records.
The second is a rule that reads commits.

## The history setting

How a target accepts work is its own `history` setting, a key in its `rwv.toml`: at
the top level for the project branch and every member without its own, and in a
member's entry for that member. `rwv branch set-history` writes it. Nothing is the
default and no role implies one: a repo that needs more than a fast-forward, where
the target records nothing, refuses as `history` `unset`, naming the setter and
`--history <h>`. `--history` supplies a value for one run only where the target
records none; one that differs from a recorded setting is refused (`history`
`contradicted`).

The setting is content of the branch, so a feature cut from a target starts with the
target's. The same setting governs both directions, so a feature takes its target in
the way the target will take the feature:

| Setting | `rwv sync`: taking the target's work in | `rwv land`: landing into the target |
|---|---|---|
| `linear` | fast-forward; else rebase the feature's branch onto the merge source | fast-forward; else rebase the source's branch onto the receiving branch, then fast-forward it |
| `merge` | fast-forward; else merge the merge source in, the feature's branch as first parent | fast-forward where the receiving tip is on the feature's first-parent chain; else a merge commit, the receiving tip as first parent |
| `ff-only` | as `linear` | fast-forward; else refused (`history` `not fast-forward`), naming `rwv sync` |

`ff-only` differs from `linear` only at landing. A landing under it never rewrites the
source; the rebase happens at `sync`, where the result can be built and tested, and
the landing then only fast-forwards. This is the fast-forward merge method some
forges offer: the request is rebased onto the target, then the target
fast-forwards. A feature rebased at `sync` under `ff-only` is treated as under a
linear setting.

Reconciling a branch with its own upstream is a separate question the setting does
not govern: that is `--merge-upstream` and `--reset-to-upstream` on `rwv pull` and
`rwv update --remote`.

Projects that share a member and record different settings for it are reported by
`rwv doctor` as `history-disagreement`, as `role-disagreement` reports roles. Each
project's landings follow its own setting.

## Between published branches

Work moving between published branches moves by merge, whatever the setting. A
`--from` always merges. A range holding a merge commit is never rebased: a rebase
would flatten the second parent through which another lineage's work entered, so
under a rebasing setting `sync` and landing integrate such a range by a merge commit
instead, reported `merged`. A merge naming a branch other than the record's target
is always a merge commit, never a fast-forward, with this branch as first parent. A
feature that took its target in by a merge holds the receiving tip only through that
merge's second parent, so landing it under `linear` or `merge` makes a merge commit
whose first parent is the receiving tip, and the target keeps its own first-parent
chain.

## Rebasing, and what it keeps

A rebase is the VCS's own, from the merge base and never from an older commit. Before it
rewrites a branch, rwv keeps the branch's tip as a `rebased` backup, which lasts as
follows. The backup of a branch that was never published goes when the operation
completes, since the rebased commits then hold its work; where carrying a dependent
along drops one of its commits, the backup stays, since no rebased commit holds that
work. The backup of a published branch stays until it is republished: its push then
shows diverged, and `rwv sync` and `rwv push` print `rwv push --overwrite <repo>`,
leased to the tip the backup holds, as the way to republish it. Once that push lands,
the backup goes. A machine that did not make the rebase sees only the ordinary choices
for a diverged branch.

A rebase refuses a branch some weave branch declares and a declared-mode project
branch (`protected`): rwv never rewrites a line other work lands into. A rebased
project branch replays its commits with each pin mapped through the member rebases,
as the VCS reports which old commit became which, including for a rebase the operator
finished with the VCS's own continue; pins outside the rebased ranges are kept; then
the lock is recorded. A replayed commit left changing nothing is dropped, as the
rebase drops one. The replay reads the attribute line the tree it replays onto
commits: `rwv land` refuses `missing-lock-merge-driver` for the source's project
checkout as well as for the workspace it writes, and `rwv doctor --fix` run where the
refusal names migrates it.

### Stacks

A branch other features land into has dependents, found by their records. A rebase of
the branch carries each dependent along, parents before their own dependents,
whatever shape the tree of records takes: each dependent's commits beyond the
branch's old tip are replayed onto its new tip, in the checkout the rebase ran in,
after a `rebased` backup, and a carried dependent's project branch is replayed after
its members so its pins follow them. Every dependent's branch moves only once all of
them have replayed. A conflict in one stops the operation there, no dependent moved:
resolve it and `--continue`, or `rwv abort`. A resumed operation never carries again a
dependent it has already moved: commits the branch gains afterwards reach that
dependent at its own next `rwv sync`.

A dependent is carried when its branch here is local, contains the branch's old tip,
and is not checked out elsewhere. When any dependent cannot be carried, nothing moves,
and the refusal gives one `target` `dependents` line per dependent, each with its own
choices:

| Dependent | Why it cannot be carried | Choices |
|---|---|---|
| lacks the tip | cut from an older commit of the branch | land it into the branch; rebind it onto the target; or take the branch in, there |
| checked out elsewhere | another workspace holds its branch, a landed one kept on disk included | free that checkout; land it (a landed one: `rwv land --retire` there); or rebind it |
| on another machine | known here only from its published record | land it there; or rebind it here, whose new value a push of it publishes |

A rerun after any choice shows the state that remains; nothing is printed as a
sequence. A dependent known only from another machine's publish whose every published
copy the branch's old tip already holds has landed: nothing is carried, and the
operation reports it. A `merge` setting never rewrites the branch, so its dependents
never block it.

## The lineage rule

A member's work has a **base**: the newest commit of its first-parent chain that some
lock in the target's history pins (a pin this clone lacks is skipped). A landing, a
`sync` from the target, and `--merge-upstream` on a feature's member move the member
only when the receiving branch contains that base. Otherwise the work was built on a
line the receiving branch has left, a declared-branch change or a rewritten branch,
and integrating it would carry that line along. The verb refuses before anything
moves (`pin` `off lineage`), listing the base and the branch. For work squashed, rebase-merged or built on another lineage the choice is `rwv sync --reset-to-target <m>`, which resets the member to the target's, keeping a backup; for a deliberate change of declared branch it is `rwv member set-branch <m> <b>`, in a workspace with that branch checked out. For `--merge-upstream`, `rwv update --remote --reset-to-upstream <repo>` or `rwv pull --reset-to-upstream` takes the upstream's, keeping yours in a backup, and the commit can be published under a name of your choosing instead. Where commits on the receiving branch carry the same changes as the member's, the
state adds the fact `patch-equivalent`, naming them. The rule reads commits only, so
every machine computes one answer.

### Why it holds

rwv's merges and the VCS's keep their first parent, a merge naming another branch is
never a fast-forward, and a rebase replays onto the receiving side. So no `sync` and no
other machine's merge moves a base; a reset to the target moves it, and only by name.
A feature cut from the target starts at the target's pin, so its base is that pin or
newer, which the receiving branch holds. Another lineage's work (a bump, `--from`, a
merge-back) enters as a second parent and passes. The rule fires where a lineage
changed: a declared-branch change, a raw merge carrying entries, work built on pins a
held line lacks, a fast-forward onto another lineage, a rewritten declared branch.

### Pins this clone lacks

The rule reads commits, so its answer depends on the clone in one case: a pin of the
target's history that this clone lacks. A lacked pin can only make the base newer, and
so turn a pass into a refusal. `rwv sync` judges after its fetch of every repo, so a
colleague's commit published on a branch is judged as if it had always been here;
a pin it still lacks does not decide the base. `rwv land` contacts no remote and
judges on the pins this clone holds. Neither verb refuses for a lacked pin. Each
reports one, once per run, as `history pin absent`, naming the pin and the lock commit
that holds it, where that lock commit is one the source's project branch does not
reach (a lock written since the two branches parted). An older one is not reported, so
no pin is reported for ever.

What stays outside the rule is a landing made without a sync on a clone that lacks a
pin that would have refused the work. The landing then brings back commits the target
dropped, as merging them would in single-repo git, which checks nothing for a
submodule commit that has gone. `rwv sync` first catches it wherever a branch or tag
of the member's remotes still holds the pin.

## The lock across a merge

A merge of the project branch merges members first, then the project, in this order:
the project merge is computed as a tree; the member set is read from that tree; the
members merge; checkouts the result drops are pruned once all members have merged; the
project commit is written last, with rwv's relock, which records each merged member. A
member the target's lock adds starts at the target's pin; one the feature added keeps
the feature's; one the merged `rwv.toml` drops leaves.

Where no member merge ran (a raw merge, a `dependency` or `reference` member, a member
only one side changed), pins merge three ways per member against the merge base's
lock: a side that changed the pin wins; both changing it alike keeps that pin; both
changing it under one declared branch keeps the pin containing the other, and diverged
pins print `pin` `unmerged`; the two sides declaring the member differently keep the
pin of the side whose declared branch the merge keeps, listing the commits the other
side's pin added that the kept branch lacks. The lock merge driver applies the same
rule in merges rwv does not make, reading both sides' `rwv.toml` at the commits
being merged; [vcs-as-seam](./vcs-as-seam.md) covers the mechanism.

## Related

- [Landing](./landing.md): the history setting and the lineage rule at `rwv land`.
- [Sync semantics](./sync-semantics.md): taking a target's work in.
- [Records and modes](./branch-records.md): the target and `as` the setting applies to.
- [`rwv branch set-history`](../../reference/cli.md#rwv-branch-set-history-linearmergeff-only--unset---repo-path---project-name) and the `history` and `pin` kinds in [Refusals](../../reference/refusals.md).
