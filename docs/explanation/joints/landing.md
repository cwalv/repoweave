# Landing

`rwv land` lands the checked-out feature into its target: the branch its
[record](./branch-records.md) names, or `--target <T>` for one run. It is the
one verb that moves work from a feature onto the branch other work builds on,
and it is built around one rule: **landed means ancestry**. A branch has landed
on a target when it is an ancestor of the target's branch, and nothing else
proves it. rwv does not recognize how a forge merged a pull request; a squash or
a rebase-merge is confirmed by name (`rwv sync --reset-to-target`), never inferred.

## Compose in the source, then move

A landing never builds its result on the target. Every merge or rebase is composed
first, in the source workspace's checkouts, as the target's
[history setting](./history-and-lineage.md) says for each repo; the project commit with the
relock is built last. Only then does anything the other side holds move, and
every move is a fast-forward to exactly what was composed. This keeps a conflict
in a place the operator can resolve it (the source), and keeps the target from
ever holding a half-landed state.

| Step | What happens |
|---|---|
| Judge | every refusal is judged first and reported together: the source's own edits, alignment and lock, the target's record and mode, history, lineage, member changes, dependents; with `--retire`, what retiring refuses too |
| Compose | each `owned` or `fork` member holding work its **receiving branch** lacks is rebased, merged or fast-forwarded onto that branch's tip; the project branch likewise onto the target's tip; the lock is recorded and committed |
| Move | each receiving branch and the target fast-forward: as refs where nothing has them checked out, else in the checkout that has them, which must be clean |
| Align | the workspace where the target is checked out aligns for it; the source's members follow the target's new pins; from the primary, the primary switches to the target |
| Retire | with `--retire`, the workweave is deleted by the [deletion rule](./deletion-rule.md) |

A **receiving branch** is where a member lands: its branch named after the target
when the target is itself a feature, else the branch the merged entries declare.
One that exists nowhere is created at the target's pin (for a member the target
does not pin, at the feature's), reported `created`. The target's tip, and each
receiving branch's, is the newer of it and its upstream as last fetched, so a
target behind its upstream takes the upstream in, fast-forward only, and the landing
reports what it took in.

## The source's lock and its checkouts

The feature's lock is what lands, with one addition: a checkout holding work
that was never locked. Where an `owned` or `fork` member's checkout is ahead of
the pin the feature's lock commits, and holds commits the target's pin lacks,
the landing records that checkout in place of the committed pin, printing
`<member>: lock behind HEAD by <n> commits — the landing records <tip> in place
of the pin <pin>`. Every judgment the landing makes reads that tip: the member
changes it refuses or reports, the lineage rule, and the lock it commits, so
what the refusal shows, what `--allow-member-change` consents to and what lands
are one pin.

A checkout the target's pin already reaches brings nothing to land, and its pin
lands as the feature committed it. So a pin put back by name, with
`rwv member pin <m> <rev> --commit`, lands at `<rev>` while the checkout stays at
its branch's tip; where `<rev>` is behind the target's pin, that is a backward
pin the landing names. A `dependency` or `reference` member's pin lands as the
feature committed it whatever its checkout holds: it moves only by name or by
`rwv update --remote`. The landing prints nothing for such a checkout;
`rwv status` and `rwv doctor` report it as `pin` `ahead`, and `rwv lock --commit`
records it.

A member the feature did not move is not touched. `dependency` and `reference` members
are never moved: they take the feature's pin where the feature changed it, which the
branch they sit on in the target must reach, else the target's. Roles bound change in
the same way everywhere: a landing that changes a member's role names it.

## The target's mode is checked

A feature's record stores the target's mode at the time it was written (`as`). Landing
first requires the target's mode to equal it. If the target was changed to another mode
since, the landing refuses (`target` `mode changed`) and offers to rebind the feature or
restore the mode, never landing somewhere the record did not describe. A target that
exists only on a remote is taken with a same-name upstream first. A target gone
everywhere refuses as `target` `gone`, listing the branches the feature could be rebound
to.

## Member changes are named

A landing changes the target's member entries, and its top-level `history`, only where
the operator names them with `--allow-member-change <repo>`. A change is any field of a
member's entry that the feature changed to a value the target does not hold (`type`,
`url`, `role`, `branch`, `history`, `recurse-submodules`), a member's removal, or a
backward pin. A field only the target changed stays the target's. Adding a member is
no change, and the other top-level tables land as content. The refusal (`target`
`member changes`) lists each changed field with the target's value and the feature's,
then the choices: rebind onto a branch whose entries match the feature's, keep the
target's value here with that field's setter, and last the consent, stating that it
changes the target's entry. This keeps a branch that was meant to carry a code change
from silently re-declaring where a line's members live.

## Stacks and dependents

A feature can target another feature. Landing into such a target moves each member onto
the target's member branch, and the target's own landing later carries everything
along. A feature other branches' records land into has dependents; a landing that would
rewrite its branches carries them, or refuses naming each one that cannot be carried
([history and lineage](./history-and-lineage.md#stacks)). `rwv land --retire`
refuses up front while the feature has dependents.

## When the target is somewhere else

Where the target is checked out in the primary or another workweave, the move happens
in that checkout, which must be clean. The landing leases that workspace while it runs,
and `rwv abort`, run in either workspace, rolls the landing back whole. Where the target
is checked out nowhere, the target and each receiving branch move as refs, each by
compare-and-swap against the tip the landing read. If another verb moved one since, or
a checkout there switched to another branch, the landing stops (`target` `moved
mid-operation`) with its op record kept. `rwv abort` restores every ref the landing
moved, then `rwv land` judges the new tips. `--continue` refuses it: the plan no
longer describes the landing.

For a project the target's workspace does not present (an inactive project), a member
the presented project shares is that project's checkout. The landing judges neither its
pin nor its alignment, fast-forwards it only where it is on the receiving branch, else
moves the receiving branch as a ref or in place, and names `rwv activate <p> --update`
for a shared member it may not move.

## After the landing

The target is reported ahead of its push branch where its push branch lacks the
landing (`push-branch` `ahead`), naming `rwv push --branch <T>`, which publishes it
from any workspace: landing publishes nothing. A forge that squashes or rebases a
pull request produces commits that do not descend from the feature's, so the
feature then shows `pin` `diverged` or `off remote`; `rwv sync --reset-to-target
<repo>` for each member re-pins it to the target's, keeping backups, then
`rwv push`, and `rwv workweave delete <name> --discard-unlanded` or `rwv branch
delete <W> --discard-unlanded` once the operator confirms the work merged. The fact
`patch-equivalent` names commits on the target that carry the same changes, as
evidence for that confirmation and never a substitute for it.

## Stopping

A conflict stops the landing with its op record kept, in the source workspace, with the
conflict the VCS reports in the checkout it names. The exits are `rwv land --continue`
and `rwv abort`; see [Stops and abort](./stops-and-abort.md). A target or receiving branch
that moves mid-operation stops it the same way, but only `rwv abort` exits it.

## Related

- [Sync semantics](./sync-semantics.md): taking the target's work into the feature before landing.
- [History and lineage](./history-and-lineage.md): how each repo is composed, and the lineage rule.
- [Workweave lifecycle](./workweave-lifecycle.md): create, work, land, retire.
- [`rwv land`](../../reference/cli.md#rwv-land---target-t-) and [`rwv explain land`](../../reference/explain/land.md).
