# Sync semantics

Two verbs move work between weave branches, in opposite directions:

- **`rwv sync`** takes new work into the checked-out weave branch. A feature takes in
  the branch its record lands into (its target), and any weave branch can merge another's
  history in with `--from <branch>`. Only this workspace changes.
- **`rwv land`** lands the checked-out feature into its target. The work is composed in
  this workspace, then the target and each receiving branch move to exactly what was
  composed. See [landing](./landing.md).

`rwv sync` names no workspace: what it takes in is a branch, read from the branch's
record. `rwv land` names the branch that receives the work, by default the record's.

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

The **lineage rule** judges each member before anything moves, and the **history
setting** decides whether the work is rebased or merged; both are stated in
[history and lineage](./history-and-lineage.md). Where a pin of the target's history is
missing from this clone, `rwv sync` fetches first and judges after, which is why it is
the step that catches lineage problems before a landing.

**`--from <branch>`** merges another weave branch's history in, whatever the
setting, and always as a merge commit: work moving between published branches
moves by merge, never by rebase.

Every refusal is judged before anything moves and reported together. Then the
sync runs under an op record with a savepoint in every repo it may move: a
conflict stops it, `rwv sync --continue` resumes, and `rwv abort` restores
every repo to its savepoint. Once every repo has taken in, members the new
entries drop are pruned and the rest are aligned as `rwv update` aligns them.

Every refusal is judged before anything moves and reported together. Then the sync runs
as an operation, with a savepoint in every repo it may move: a conflict stops it,
`rwv sync --continue` resumes, and `rwv abort` restores every repo to its savepoint (see
[stops and abort](./stops-and-abort.md)). Once every repo has taken in, members the new
entries drop are pruned and the rest are aligned as `rwv update` aligns them
([alignment](./alignment.md)).

**`--reset-to-target <repo>`** is the consent for a squash or rebase-merge that did not
preserve ancestry: it moves the named repo's branch to its merge source instead of
integrating it, skipping the lineage rule and the push-branch judgment for that repo,
keeping the tip it leaves as a backup and listing the new work it drops. `--continue`
resumes with the plan the operation recorded. The naming rule behind flags like this one,
and why `push --overwrite` is named for the repo it replaces, is stated once in
[destructive-operations](../destructive-operations.md#2-named-overrides-are-narrow-and-informed).
Neither `sync` nor `land` takes `--force`.

## Snapshot reads

A `land` reads its target once, at the start of the operation: its project tip, with the
manifest and lock read at that revision. A change to the target after that cannot reach
anything the operation has read, and a landing's plan is checked again where it writes (a
ref that moved stops it; see [landing](./landing.md#when-the-target-is-somewhere-else)).
On `--continue` the plan is read again, so a resumed operation converges on the target as
it is then. A `sync` records the tips it takes in in its op record, and `--continue` takes
in those same tips.

## Parallel steps (`-j N`)

With `-j N` (`N > 1`), the per-repo steps of `rwv sync` and `rwv land` fan out across a
bounded worker pool. The default is `-j 1`. `--json` prints the same envelope whatever `-j` is, once
the op is done: `{"$schema": "<url>", "outcomes": [...]}`, with `outcomes`
sorted by repo path however the workers finished, beside `placements`, replay
`members`, and `complete`. The exit status is non-zero when some per-repo
outcome is `failed`, and when a prune was refused: the op completes,
reports a `prune-refused` member record, and exits 1. The envelope's
`complete` field is true when the op ran its last phase and cleared its
op record, false when a phase parked it.

## Related joints

- [landing](./landing.md): the other direction, into the target.
- [history and lineage](./history-and-lineage.md): the history setting and the lineage rule.
- [stops and abort](./stops-and-abort.md): savepoints, `--continue` and `rwv abort`.
- [lock-as-derived](./lock-as-derived.md): why the lock is recorded at each boundary.
- [workweave-hierarchy](./workweave-hierarchy.md): the tree of workweaves.
- [workweave-lifecycle](./workweave-lifecycle.md): create, work, land, delete.
- [shared-refs-drift](./shared-refs-drift.md): drift in checkouts a sync or landing moves.
- [vcs-as-seam](./vcs-as-seam.md): the lock merge driver, conflict hint text and the conflict-bail surface are `Vcs`-trait responsibilities.
