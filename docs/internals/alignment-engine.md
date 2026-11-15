# The alignment engine

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

Where each member of a workspace sits, and the one engine that puts it there.
The operator-facing statement is
[`../explanation/joints/workweave-hierarchy.md`](../explanation/joints/workweave-hierarchy.md);
this page is the contract for someone changing `src/alignment.rs`.

## What alignment is

A workspace is **aligned** when every member is in its *expected state*, a
pure function of:

- the checked-out project branch, and its record (`Record`, read through
  `Store::view`);
- the member entries of that branch's `rwv.toml`, and its `rwv.lock`;
- the workspace's own pointer state (`WorkspaceState`);
- the refs.

Nothing else is read: no other branch's content, no kind of branch, no
operation's history. Uncommitted edits only stop a checkout from moving.

`Aligner::open` reads the workspace and fixes `Aligner::mode`: `Mode::Feature`
when the checked-out branch's record is a feature, `Mode::Declared` for a
declared branch, `Mode::Detached` when the project repo is on no branch. A
project branch with no record reads as `Mode::Declared`; the verbs that align,
lock, land or publish refuse it before they reach the engine
(`branch::require_recorded`), so the engine never aligns by a guess.

## Workspace state

`WorkspaceState` is the one file a workspace keeps for itself
(`rwv-workspace-state`, `StateFile::WorkspaceState`, in the project checkout's
private dir by `Vcs::private_dir`): the `aligned` branch (`Aligned::Branch` or
`Aligned::Detached`), the `stored` member entries and mode per branch it
aligned (`Stored`), the `records` it last saw each branch hold (`SeenRecord`),
and a `renaming` marker a `branch rename` sets until it finishes. It is written
only when its bytes change (`store_state`); a file that does not parse reads as
absent and the reading says so (`StateReading::corrupt`). Losing it loses no
record: an absent aligned branch is the unaligned case, and absent stored
entries store silently.

`ProjectStanding` is the project repo against the aligned branch: `Aligned`,
`Moved` (a raw switch: the aligned branch exists and is not checked out),
`Unaligned` (the aligned branch is gone, or none is recorded) or `Detached`.
Only the member levels refuse for `Moved`; nothing refuses at the project
level.

## The plan

`Aligner::plan` judges one `Member` against its pin and returns a `Plan`:

| Field | Meaning |
|---|---|
| `pin` | `Pin::None` (the lock lacks the member), `Pin::Unreachable` (the lock names a commit this clone lacks) or `Pin::At` |
| `target` | `Target::Branch { name, found, fast_forward_to }`, `Target::Detached { at, why }` or `Target::AsFound` |
| `unfound` | a declared branch that exists nowhere, which no alignment creates |
| `unaligned` | a feature's member branch found nowhere for a member with no pin, which no alignment creates |

`Aligner::wanted` is the expected branch `E`: for an `owned` or `fork` member
of a feature, the member's branch named after the project branch; otherwise
the member's declared branch.

`find_e` reads what the refs make of `E` (`Found`): a local branch with a
same-name upstream (`Local`); a local branch with no upstream whose name is on
one remote (`LocalGivenUpstream`, given that upstream before anything else); a
local branch whose upstream has another name (`Foreign`, refused by every verb
that aligns, lands or pushes); a branch only on a remote (`OnRemote`, taken from
the only remote that has it, else the home remote); or `Nowhere`. A remote copy
is chosen without a guess: several copies and no home remote holding one is
`Nowhere`.

`Aligner::usable` applies the first matching rule to a member with a pin:

1. a detached workspace: `Detached` at the pin (`DetachedBecause::Workspace`);
2. `E` checked out in another workspace: `Detached` at the pin, with the
   `checkout` fact naming where (`CheckedOutElsewhere`);
3. `E` declared and neither it nor its upstream reaches the pin: `Detached`
   at the pin (`NotOnDeclaredBranch`);
4. `E` behind the pin: the branch, fast-forwarded (`fast_forward_to`);
5. otherwise the branch as it is, which includes `E` ahead of or diverged
   from the pin.

A member with `Pin::None` sits on `E` as found, or stays where it is where
none is found; `Pin::Unreachable` stays where it is. In every case a pin moves
no branch backward: the engine fast-forwards or detaches, never resets.

## Applying a plan

`Aligner::apply` puts a member in its plan when it may move there and returns
`Applied::Aligned` or `Applied::Left`:

1. a `Found::Foreign` branch is left, judged as `push-branch` foreign
   upstream;
2. a `Found::LocalGivenUpstream` branch is given its upstream (`Vcs::set_upstream`);
3. a member already in its plan is finished in place;
4. `Aligner::blocked` returns the lines for what holds the member back: tracked
   edits the move would overwrite (a verb that carries edits, `switch` and
   `branch create`, asks `carrying_edits`), an operation in progress, a set-back
   marker, a non-empty stray set;
5. `standing_of` classifies what the member is on (`Off`): a branch with its
   `stray` commits (those no other branch, remote-tracking branch or backup
   reaches, less the commits already copied into `E`), a detached HEAD at a
   place a ref reaches, or a detached HEAD with unpushed commits;
6. `may_leave` decides whether it may leave: a detached HEAD with nothing
   unpushed, or a branch whose stray set is empty, or that some live weave
   branch declares, or that is recorded, or that the aligned branch's stored
   entries hold;
7. the move itself runs only with a `Departure`, the proof that every commit
   the checkout holds stays reachable from a ref after the move.
   `Departure::of_move` backs a detached HEAD up first when the destination
   does not reach it; `Departure` has no other constructor outside `cfg(test)`.

`Aligner::refuse_claimed` refuses a plan that would put a member on a branch
another operation's landing claim names (`landing_claim::refuse_moving_claimed`);
an operation's own alignment under its claim does not call it.

A successful alignment of the whole workspace records the aligned branch and
stores the branch's entries (`Aligner::next_state`, `alignment::record`). A
change to the checked-out branch's own entries since they were stored is
reported (`member-changed`) by the verb that notices it and stored by that
verb (`next_state_storing`); `Aligner::record_change_line` reports a record
changed by something other than this workspace.

## What the engine does not do

- It never creates a declared branch: `Plan::unfound` is reported, and only
  `member set-branch --new`, `--set-new-branch` or a landing creates one.
- It never moves a project branch; the project repo moves only through
  `switch`, `branch create` and `workweave create`.
- It never resets or rewrites a branch. The verbs that move a branch off a
  fast-forward (`--reset-to-upstream`, `--merge-upstream`) are separate and
  keep a backup.
- It reads no ecosystem state; integrations run after it.

## Accounting

`Accounting` is what the weave's live weave branches declare and record:
`declares` and `declarers` answer which live branches declare a branch for a
member, `is_recorded` whether a branch has a record, `history_disagreements`
which members two projects record different `history` for. A branch no live
weave branch declares and no record names is ordinary: nothing protects it and
`may_leave` does not vouch for it. `Accounting::scan` reads every project of
the weave, once per verb.

## Pins

- `Departure` is minted only in `src/alignment.rs` (and under `cfg(test)`), so
  no other module moves a member off its attachment without the stray set having
  been read.
- `branch::require_recorded` is the gate the verbs that align, lock, land or
  publish call before they change anything.
