# The re-pin record

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

`pin` `waived` lets a merge that left a member's pins unmerged through a push
only where a lock in the range re-pinned that member backward **by name**. By
name is a fact about the verb that wrote the lock, and history does not hold
it unless rwv writes it down. Nothing is inferred from the shape of history: a
raw merge whose lock conflict was settled with the other side's lock looks,
pin for pin, like a backward re-pin, and named nothing.

## The format

One `Rwv-Repin` annotation per member whose pin the commit moves:

```
Rwv-Repin: "<member>" <old> <new> <verb>
```

- `<member>` is the member path as a JSON string literal, so a path holding
  `/`, spaces or quotes reads back whole, and the value is one line whatever
  the path holds.
- `<old>` is the full commit id one of the commit's parents' locks pinned (the
  first parent's for a verb that names members; the checkout's side for a
  rebase record), or `none` where it pinned nothing (a first pin is a forward
  re-pin from absent).
- `<new>` is the full commit id the commit's own lock pins.
- `<verb>` is the rest of the line: the verb as the operator ran it
  (`member pin`, `member set-branch`, `branch create`, `workweave create`,
  `lock --commit`, `update --remote`), or `rebase` for a record of a rebase
  rwv made.

A commit carries several when its verb names several members. The record is
written for a forward re-pin as for a backward one: D13's Terms define by
name as "a re-pin of a member named by" a verb, either direction, and only
`pin` `waived` restricts itself to the backward ones. A member whose pin does
not change gets none.

## The seam

`Vcs::commit_only` and `Vcs::commit_merge` take `&[Annotation]`, key/value
pairs the commit records beside its message, and `Vcs::annotation_values`
reads the values one commit records under a key. Git spells an annotation as
a trailer appended in a paragraph of its own, and reads them back with
`%(trailers:key=<key>,valueonly,unfold)`.

## One writer

`src/repin.rs` is the only caller of either commit operation outside the seam
(`tests/source/repin_writers_test.rs` pins it). `repin::commit` and
`repin::commit_merge` take the lock being committed and a `Naming`:

- `Naming::Unnamed` for every relock, rwv's own merges, first pins, and the
  setters that move no pin;
- `Naming::ByName { verb, members }` for a verb that names members.

The record is derived, never handed in: for each named member, the pin the
first parent's lock holds against the pin the committed lock holds. A caller
cannot write a record that disagrees with its own lock change.

Callers reach it through `lock::commit_lock_file_annotated`,
`lock::commit_project_lock` (a `PendingLockCommit` takes its naming with
`by_name`), and `manifest_commit::commit` (`MemberCommit.naming`). A verb
whose by-name re-pin is a step inside its own merge passes its naming to
`repin::commit_merge`.

A member that `rwv land` or `rwv sync` rebased is the one record not derived
from a `Naming`: `remap::rebase_jumps` reads the op's journaled rebases, and
where the replay record maps the pin the checkout's side held to the pin the
commit records, or to a commit on that pin's first-parent chain, the
commit records `<that pin> <the new pin> rebase`. The landing's project commit
(merge or relock), `rwv sync`'s project merge (`ProjectMerge::rebases`) and
its relock pass these as extra annotations. Without a replay record nothing is
written: `remap::member_replays` refuses before the commit.

A project commit a replay copies (`remap::remap_project`, behind a linear
landing, `rwv sync` under a linear setting and a stack carry) keeps its
message, and its records are rewritten through the same member images its
lock's pins go through (`repin::copy_with_lock`, `Vcs::copy_replayed` with
replacement annotations), so a copied record still names the pins its copy and
the copy's parent hold. A record the images leave moving nothing is dropped.

Every file that calls `lock::report_backward_repin` also builds a by-name
commit (`tests/source/repin_writers_test.rs`): a verb that reports a pin moved
backward by name names the member in its commit.

## The reader

`unmerged::analyze` walks the pushed range from each merge forward. A commit
waives the merge for a member when it records a re-pin of that member whose
`<old>` and `<new>` are the pins one of its parents' locks and its own lock
hold, `<old>` does not reach `<new>`, and `<new>` reaches no pin the merge
left unmerged. The merge's own commit is read like any other. A record whose
pins disagree with the lock change waives nothing and is listed under
`pin` `unmerged`; a value that does not parse is ignored.

## The lineage reader

`lineage::PinHistory::of` reads one lock per commit `rev-list --full-history
-- rwv.lock` lists; nothing else is read until the lineage rule meets a pin
this clone lacks. Then `PinHistory::superseded` reads the commit graph once
(`Vcs::ancestry`). A parent's lock is the lock of the nearest listed commit
on its first-parent chain: an unlisted commit is TREESAME to every parent.
Each lock commit whose parent pinned the member at `X` and which pins `Y != X`
is an exit from `X`. An exit is

- recorded where the commit records `<X> <Y>` for the member,
- waived where the commit is a merge and a later commit holds a trusted
  backward re-pin of the member whose `<new>` does not contain `X` (the
  `pin` `waived` test, applied to the pin the merge left), and
- an advance where the clone lacks `Y`, or holds both and `X` reaches `Y`.
  Ancestry is asked only of commits the clone resolved (`lineage::Held`).

`X` is superseded unless a chain of exits that are neither recorded nor waived
reaches the tip's pin, no pin, or a held pin that is no advance: a least fixed
point over the exits reached from the pins asked about. Records are read only
for exits reached that way, one `annotation_values` per commit. The fact
names the oldest record the chain reaches (`Superseded::by`). It prints
unless the source's project branch already holds that record (`seen_from`).

The answer is per clone. A clone that holds a superseded pin judges by it, so
the same history can refuse work as `pin` `off lineage` on one clone and land
it from another that lacks the pin. This is an accepted residual: it is the
parity with single-repo git, where work built on a commit another clone reset
away comes back when it is merged. A pin left by an unrecorded jump to a pin
this clone holds refuses `pin` `unreachable` on every clone that lacks it.
Only obtaining the commit clears that refusal (`rwv fetch` from a remote that
holds it).

`rwv push` asks `superseded` only for a pin of the pushed range that no branch
or backup holds, to leave it out of `pin held by no branch`.
