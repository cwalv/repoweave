# The records branch

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

How each weave branch's mode and landing record is stored, merged and
published. The operator-facing statement is
[`../explanation/joints/workweave-hierarchy.md`](../explanation/joints/workweave-hierarchy.md);
this page is the contract for someone changing `src/records.rs`.

## What is stored

A **record** says how one weave branch is used: `Record::Declared`, or
`Record::Feature { target, as_mode }`, the branch it lands into and that
branch's mode when the record was written. The records live on one branch of
the project repo, `RECORDS_BRANCH` in `src/git/records.rs` (git:
`rwv-records`), which shares no history with any weave branch and carries no
`rwv.toml`, so it is itself no weave branch. `BranchName::parse` refuses the
records branch's name and every name below it (`NameError::Reserved`), which
is the one place a name is checked against it.

The tree is flat: one file per weave branch, at `entry_path(name)`, the name
with `%` written `%25` and `/` written `%2F`. No entry's path is a prefix of
another's, so a branch `a` and a branch `a/b` coexist. `entry_name` is the
inverse; a path that is not an entry's path is not read and no write touches
it.

An entry's file is TOML, parsed by `Entry::parse` under `deny_unknown_fields`
into one of three values:

| `Entry` | File |
|---|---|
| `Value(Record)` | `mode = "declared"`, or `mode = "feature"` with `target` and `as` |
| `Conflict(a, b)` | `conflict = [ {…}, {…} ]` and no other key: two values that were written differently and neither chosen |
| `Unreadable(why)` | anything else: not UTF-8, not TOML, an unknown key, a `conflict` beside other keys, a `conflict` without exactly two values |

`Entry::render` returns `None` for `Unreadable`, which is stored as found: a
writer never rewrites what it cannot read, and a field a later release writes
reads as unreadable here rather than being dropped.

Each records commit carries an `Rwv-Writer` trailer naming the release that
wrote it, which `Store::writer_of` reads back for the refusal that names it.

## Reading: a fold, not a merge commit

A reader never needs a records commit to exist. `Store::fold` takes the local
records tip and the tip of each records remote as last fetched, and computes
their entry-by-entry merge in memory:

- the **records remotes** are the project repo's url remote and every remote a
  branch of the project repo pushes to (`Store::open`, with
  `Vcs::project_url_remote` and `Vcs::branch_push_targets`); a remote added
  only to fetch a colleague's branches contributes nothing;
- the heads are reduced to the ones no other head reaches (`Vcs::merge_bases`),
  and merged pairwise against `Vcs::merge_base_against`;
- `merge_snapshots` merges per entry against the base: an entry changed on one
  side takes that side, changed alike on both takes that value, changed
  differently on both becomes `Entry::Conflict`. An entry equal by parse
  counts as alike, so reformatting is no change. Entries for different
  branches never conflict. A `Conflict` or `Unreadable` entry on either side
  wins over a `Value`, so neither is ever merged away;
- an entry the local branch holds that the fold lost is kept when its branch
  exists here, as a local branch or as a remote-tracking branch of a records
  remote (`Store::exists_here`): another machine's drop does not remove the
  record of a branch this machine has.

`Store::view` returns the fold as a `View` without writing. A verb that only
reads uses it. A verb that writes a record, aligns, locks, lands or publishes
calls `Store::settle` or `Store::write`, which commit the fold to the local
branch first, so the local branch afterwards holds what every remote last
fetched said. A raw fetch therefore counts: the fold reads whatever the
remote-tracking refs hold, and an offline verb reads what was last fetched.

`View` answers `lookup` (`Lookup`: a value, a conflict, unreadable or
unrecorded), `chain_back_to` and `rename_cycle` (the target walks that detect
a cycle), and `dependents` (the records whose target is a branch, published
ones from other machines included).

## Writing

`Store::write(decide)` is the one write path. Each attempt:

1. reads the local tip and the remotes' tips, and folds them;
2. runs `decide` on the folded `View`, which returns the `Write`s to apply; a
   write that would leave the entry as it already is is skipped
   (`Unchanged::Skip`), except `write_created`, which always commits, because
   a drop judged before its branch existed must lose its compare-and-swap and
   judge again;
3. applies the drop rule in the same commit (below);
4. writes the commit through `Vcs::write_commit`, with no working tree, and
   moves the local branch with `Vcs::advance_records`, a compare-and-swap on
   the tip read in step 1.

A lost compare-and-swap (`RecordsAdvance::Lost`) repeats from step 1, so
`decide` runs again against what the winner wrote. `WRITE_ATTEMPTS` bounds the
loop; exhausting it is `WriteError::Busy`. Entries for different branches never
conflict, so two verbs on one machine lose nothing to each other. `decide`
returns `WriteError` for the refusals a record write owns: a target chain that
returns to its own branch (`Cycle`), a target with no record
(`TargetUnrecorded`), a target whose record is not one value
(`TargetUnreadable`), and a branch with no record to move (`Unrecorded`).

`Store::rename` moves one entry and rewrites every `target` naming the old
name, in one commit. No other verb, and no merge of weave branches, changes a
record; the only writers are `branch create`, `workweave create`,
`branch set-mode`, an opening spelling on an unrecorded branch, `branch
rename`, `init` and `clone` (the trunk, `declared`), and the drop rule.

## The drop rule

`write_dropping` removes, in the commit it writes, each entry the local branch
already held whose branch the caller's `Droppable` names. `drop_records` in
`src/branch.rs` is the one `Droppable`: a branch is gone when it exists as no
local branch, no remote-tracking branch, and as no workspace's aligned branch
on this machine (`ExistingBranches`). Three guards keep a record from being
lost in a gap:

- nothing drops after a pruning fetch of the project repo that went
  unconfirmed;
- an entry a merge brought in is not among the candidates of the merge that
  brought it, because candidates are the entries the local branch held before
  the fold;
- a writer that creates a branch creates it before it writes its record, and
  `write_created` always commits, so a drop judged before the branch existed
  loses its compare-and-swap.

## Publishing

`Store::publish(branch, remote, refetch)` builds one commit on the remote's
records tip as last fetched: it changes `branch`'s own entry and removes the
entries the settled local branch no longer holds, and nothing else. No other
local entry is sent: the record of a branch rwv has not pushed never leaves
the machine, and an entry changed here for a colleague's branch is sent by a
push of that branch. A conflicted or unreadable entry is never sent, so the
remote keeps its value, and a conflict stays on the machine whose merge met it.

`Vcs::publish_records` pushes the commit as a fast-forward leased to the tip it
was built on and answers `RecordsPublish`:

| Answer | Effect |
|---|---|
| `Published` | `Published::Sent`, with the entries it dropped; an entry already on the remote is `Published::AlreadyThere` and nothing is pushed |
| `RemoteMoved` | `refetch` runs, the records are settled again and the commit is rebuilt, up to `WRITE_ATTEMPTS` |
| `Rejected(why)` | `Published::Rejected`; `push` reports it as the fact `publish` `records rejected`, listing what the remote answered, and still publishes the project branch without its record; another machine opening the branch finds it unrecorded and is asked once how to treat it (`target` `unrecorded`) |

A remote with no records branch gets one on the first such push; no other
verb creates it remotely.

## The seam

Core never names the branch or a ref. It reads and writes through
`Vcs::records_branch` (the name, for `BranchName::parse`), `records_tip`
(`RecordsSite::Local` or `Remote`), `files_at`, `write_commit`,
`advance_records` and `publish_records`. The git spelling is in
`src/git/records.rs`: the local ref is `refs/heads/rwv-records`, a remote's is
its remote-tracking ref, and the compare-and-swap is `update-ref` with the old
value. A backend with no branches would keep the same entries in whatever
structure orders its own history, and must supply the compare-and-swap, the
no-working-tree commit, and the fast-forward-only publish.

## Pins

- `every_name_round_trips_through_its_path_and_no_path_is_below_another`
  (`src/records.rs`) holds the codec.
- The test-only pauses `records-publish-settled` and `records-publish`
  (`src/test_pause.rs`, compiled only under the `test-pause` feature) let a
  race between a publish and a writer be driven from outside the process.
- Nothing outside `src/records.rs` and `src/git/records.rs` spells the records
  branch's name; core reaches it through `Vcs::records_branch`.
