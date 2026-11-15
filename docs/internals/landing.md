# Landing and taking in

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

How `rwv land` and `rwv sync` decide what moves, in which order, and how a
landing composes its result before any branch moves. The operator-facing
statements are
[`../explanation/joints/sync-semantics.md`](../explanation/joints/sync-semantics.md)
and
[`../explanation/joints/workweave-lifecycle.md`](../explanation/joints/workweave-lifecycle.md);
the phases and what each persists are
[`sync-phase-machine.md`](./sync-phase-machine.md); the records the operation
keeps are [`op-state.md`](./op-state.md).

## Who decides what

| Question | Where it is answered |
|---|---|
| Where a feature lands, and the receiving branch of each member | `land::resolve`, `land::read_target`, `land::judge` (`src/land.rs`) |
| What a feature takes in from its target | `src/feature_sync.rs` |
| How a branch that must move is integrated | the history setting, `History` (`src/manifest.rs`), read per repo from the target's `rwv.toml` by `Manifest::history_for` |
| Whether a member's work may move into the receiving branch at all | the lineage rule, `src/lineage.rs` |
| What a rebase carries along | `src/stack.rs` and `src/remap.rs` |
| Which merges the project repo needs, and the lock across them | `src/project_merge.rs`, `src/lock_merge.rs` |
| Who may land into a target now | `src/landing_claim.rs` |
| The phases, the savepoints, abort | `src/sync.rs`, `src/op_state.rs` |

Every refusal a landing owes is judged before anything moves
(`land::judge`, then `feature_sync`'s own judgment); the phase machine
re-reads and composes.

## The history setting

`History` is `Linear`, `Merge` or `FfOnly`. A target's `rwv.toml` records it
at the top level and, per member, in the member's entry, which overrides the
top level for that member. There is no default and no role implies one:
`Manifest::history_for` returns `None` where neither is recorded. A fast-forward
reads no setting; where more than a fast-forward is needed and the target
records none, the verb refuses (`history` unset), and `--history` supplies one
for one run only where the target records none for that repo
(`history` contradicted otherwise). The run's value is carried in the op
record's `history`, and a `--continue` re-applies it.

| Setting | `sync` (take the target in) | Landing (move each member into its receiving branch, then the project branch into the target) |
|---|---|---|
| `Linear` | fast-forward, else rebase the feature's branch onto the merge source | fast-forward, else rebase the source's branch onto the receiving branch, then fast-forward |
| `Merge` | fast-forward, else merge the source in, first parent the feature's | fast-forward when the receiving branch is linear on the work's base, else a merge commit, first parent the receiving branch |
| `FfOnly` | as `Linear` | fast-forward, else refused (`history` not fast-forward) |

Two rules override the table. A range holding a merge commit is never rebased:
where the setting would rebase it, the verb merges instead and reports it. A
merge that names another branch than the record's target (`sync --from`) is
always a merge commit (`Order::HeadFirstAlwaysMerge`). `--merge-upstream`,
which reconciles a branch with its own upstream, is not governed by the
setting (`src/merge_upstream.rs`).

Rebase refuses a declared branch, a declared-mode project branch, and a branch
with dependents it cannot carry (`protected`, `target` dependents). It runs
after a `rebased` backup of every branch it rewrites.

## The lineage rule

`lineage::judge_by_history` returns a `Verdict`. The **base** of a member's
work `W_m` is the newest commit of its first-parent chain that some lock in the
target's history pins for the member (`PinHistory`, built through every parent
of the target's lock history), or that is an ancestor of such a pin. The rule:
a landing, a `sync` from the target and `--merge-upstream` on a feature's
member move a member only when the receiving branch contains the base
(`Verdict::Passes`); otherwise the verb refuses (`pin` off lineage,
`Verdict::Off`). Pins this clone lacks do not decide the base; they are kept in
`Verdict::Passes::absent` and named, because the verdict is only as good as the
history this clone holds, and the refusal says whether the verb fetched first
(`Fetch`).

The rule reads commits only, so every machine computes one answer. It holds
because first parents are preserved by rwv's merges and git's, a merge naming
another branch is never a fast-forward, and a rebase replays onto the receiving
side. A merge of another lineage enters as a second parent and passes.

## What a landing plans

`land::resolve` reads the checked-out feature's record and finds `T`, `T`'s
tip (the newer of `T` and its upstream; diverged refuses), and the workspace
that has `T` checked out, if any. `land::read_target` re-reads under the claim
(below). `land::judge` returns a `Plan` with a `Receiving` for the project and
for each member:

- an `owned` or `fork` member with unlanded commits is **moved**: it lands
  into its receiving branch `U`, which is `T`'s branch of the same name when
  the record's `as` is `feature`, else the branch the merged entries declare;
- a member whose declared branch the landing changes and that has no new work
  **follows**: it takes `U`'s tip as its pin, merging nothing;
- a `dependency` or `reference` member is never moved: it takes the feature's
  pin where the feature changed it since the merge base, which `U` must reach;
- a member the target lacks **enters** at the feature's pin;
- the mode the record stored (`as`) must equal `T`'s mode now; it is never
  redirected.

Any change the landing would make to a member entry `T` lists, in any field,
its removal, or a backward pin, refuses unless the operator names the member
(`--allow-member-change`); a change to `T`'s top-level `history` needs
`projects/<p>`. `entry_changes` and `judge_entry_changes` compute them from the
merged manifest.

The source's pins are `W`'s committed lock overlaid with `Plan::relocked`,
which `Sides::relocks` selects before any judgment from the checkouts ahead of
their pins (`CheckoutAhead`): an `owned` or `fork` member whose checkout `T`'s
pin does not reach. `Sides::our_pin` reads the overlay, so `entry_changes`, the
carried-pin and lineage judgments and `plan_realigned` read the same pins; the
op record journals the overlay as `take_in.relocked`, which `relock_landing`
overlays on the source's lock before the three-way merge. Each entry is
reported `relocked` (`emit_relock`); the report is not journaled, and
`--continue` rebuilds it from `take_in.relocked` and the lock at
`take_in.source_tip` (`relocks_recorded`). A checkout `T`'s pin reaches, and every `dependency` or
`reference` checkout, keeps `W`'s committed pin. The `target` `member changes`
consent prints the landing as typed (`landing_as_started`) with each change's
`--allow-member-change`.

`Moves` is how each receiving branch moves, recorded in the op record:
`InCheckout`, `AsRef`, `InPlace`, `Enters`, `Follows`.

## Composed before anything moves

The project merge is computed as a tree first (`Vcs::tree_merges`, with the
checkout fallback where the backend has no tree merge), and the member set is
read from that result: members merge, checkouts the merged `rwv.toml` drops are
pruned after every member has merged, and the project commit with the relock is
written last (`src/project_merge.rs`). A landing composes in the source
workspace: rebases and conflicting merges in its checkouts, the rest as commits
no ref names in the shared object store. Every composed tip is written to the
op record (`converged_tips`) before advance-target; advance-target
fast-forwards `U` and `T` to those tips, as refs by compare-and-swap where no
checkout holds them (`Vcs::advance_branch_ref`), else in the checkout that has
them, which must be clean.

A compare-and-swap that loses, a ref found gone, or a receiving branch newly
checked out with edits stops the operation with its record kept and the refs
already moved left moved: `target` moved mid-operation. `--continue` refuses it
(the plan no longer describes the landing), and `rwv abort` restores every ref
the operation moved to its savepoint. The target is read again on the next
landing.

## The landing claim

`landing_claim` serializes landings into one target. The claim is held from
before the operation reads `T`'s tips until it releases `T` at its retire, ends
or is aborted; a landing stopped on a conflict or a lost race still holds it, so
no other landing builds on what it moved. The claim names its op and the
workspace holding that op's record, which is written before the claim and
removed after it is released. A claim whose recorded workspace is here and
holds no such op is dead and the next landing takes it over; a claim whose
op is recorded is never taken from it; a claim whose workspace cannot be read
is never judged dead (a landing waits on it and refuses; `rwv doctor --fix`
releases it, and `--release-unreadable-claims` releases one whose bytes this
release does not read). Everything that takes no claim is guarded by the
compare-and-swap of each move. `Aligner::refuse_claimed` refuses an alignment
onto a branch a live claim names.

## Stacks

A rebase of a feature's branches carries each dependent's branch in that repo
along, in dependency order (`src/stack.rs`): each dependent's commits beyond
the target's pre-rebase tip replay onto its new tip, after a `rebased` backup,
and a carried project branch re-maps its pins through the member rebases
(`src/remap.rs`: `Image::Replayed`, or `Image::Dropped` for a pin whose commit
the rebase dropped). Dependents are those records whose target is the branch,
published ones from other machines included; they form a tree. A dependent is
carried when its branch here is local, contains the pre-rebase tip and is not
checked out elsewhere. When any cannot be carried, nothing moves and the verb
refuses with one `target` `dependents` line per uncarried dependent, each a
state with its own choices. A `Merge` setting never rewrites the feature, so
dependents never block it.

## Pins

- `tests/scenario/scenario_harness_test/scenarios.rs` holds the landing's
  refusals as scenario steps, each with a literal `Exit::refuses(Repo, State)`.
- `tests/kill_matrix` kills a landing at every pause point it passes
  ([`testing.md`](./testing.md), "The kill matrix").
