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
- `<old>` is the full commit id the commit's first parent's lock pinned, or
  `none` where it pinned nothing (a first pin is a forward re-pin from absent).
- `<new>` is the full commit id the commit's own lock pins.
- `<verb>` is the rest of the line: the verb as the operator ran it
  (`member pin`, `member set-branch`, `branch create`, `workweave create`,
  `lock --commit`, `update --remote`).

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

Callers reach it through `lock::commit_lock_file_with_message`,
`lock::commit_project_lock` (a `PendingLockCommit` takes its naming with
`by_name`), and `manifest_commit::commit` (`MemberCommit.naming`). A verb
whose by-name re-pin is a step inside its own merge passes its naming to
`repin::commit_merge`.

Every file that calls `lock::report_backward_repin` also builds a by-name
commit (`tests/source/repin_writers_test.rs`): a verb that reports a pin moved
backward by name names the member in its commit.

## The reader

`unmerged::analyze` walks the pushed range from each merge forward. A commit
waives the merge for a member when it records a re-pin of that member whose
`<old>` and `<new>` are the pins its first parent's lock and its own lock
hold, `<old>` does not reach `<new>`, and `<new>` reaches no pin the merge
left unmerged. The merge's own commit is read like any other. A record whose
pins disagree with the lock change waives nothing and is listed under
`pin` `unmerged`; a value that does not parse is ignored.
