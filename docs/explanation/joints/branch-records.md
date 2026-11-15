# Records and modes

A weave branch is a branch of the project repo that carries `rwv.toml` and
`rwv.lock`. rwv needs one answer to a question the branch name alone cannot
give: how is this branch used? A branch that a release line keeps its members on
is used differently from a branch cut to build one feature and land it, and
guessing which from the name, from where the branch was cut, or from what its
`rwv.toml` says would make the same branch mean different things on different
machines. So the answer is written down, once, as the branch's **record**, and
no verb reads anything else to decide it.

## Two modes

| Mode | Members sit | Work goes |
|---|---|---|
| `declared` | on the branches the branch's own `rwv.toml` entries declare | nowhere: the branch is a line other work lands into |
| `feature` | `owned` and `fork` members on branches named after the feature, which rwv creates; `dependency` and `reference` members as declared | into its **target**, the branch the record names |

A `feature` record is `{target, as}`: `target` is the branch the feature lands
into, and `as` is that target's own mode when the record was written. A
feature onto `main`, with `main` recorded `declared`, lands into the branches
`main`'s entries declare; a feature onto another feature lands into that
feature's member branches, which is how stacked features work (see
[landing](./landing.md)). The recorded `as` is checked at landing: if the
target's mode has changed since, the landing refuses (`target` `mode
changed`) instead of landing somewhere the author did not mean.

## Records are spelled, never inferred

A record is written only by a spelling that names it:

| Spelling | Writes |
|---|---|
| `rwv branch create <W>` | a feature onto the checked-out branch |
| `rwv branch create <W> --target <T>` | a feature onto `T` |
| `rwv branch create <W> --declared` | a declared branch |
| `rwv branch set-mode <W> feature --target <T>` or `declared` | the record of an existing branch, new or changed |
| `rwv workweave create` | the same spellings as `branch create`, for the branch the workweave ends on; a bare create records its minted name as a feature onto the branch the parent has checked out |
| `rwv init`, `rwv clone` | `declared`, for the project's trunk, when it has no record |

`rwv branch rename` carries a record to the new name, and nothing else moves
one. A merge of weave branches never changes a record: `rwv.toml` and
`rwv.lock` travel with merges, and a record is not in either.

A branch with no record is **unrecorded**, and the verbs that align members,
lock, land or publish refuse it (`target` `unrecorded`). Nothing falls back to
`declared`. The refusal names `rwv fetch`, because the record may be published
and not yet fetched, and `rwv branch set-mode`. A raw `git switch -c`, a
branch made on a forge, and a branch pushed by hand are unrecorded until a
spelling names them.

An opening spelling that disagrees with a branch's record is refused, never
redirected. `rwv switch <W>` and `rwv workweave create --branch <W>` open a
recorded branch bare; `--target` and `--declared` go with creating a branch,
and an opened one takes neither. `rwv branch set-mode` is the way to change a
record, and it says what the change leaves behind: the member branches a
branch stops owning when it leaves `feature`, and each feature that lands into
it and recorded its mode differently, which refuses to land until rebound.

## Where records live

Records are published with the branches they describe. They live on a branch of
the project repo of their own (`rwv-records`), with no history in common with a
weave branch, holding one small file per weave branch. rwv writes it without
a checkout, so `rwv branch set-mode` works in any workspace and no working tree
ever holds a half-edited record. It is the same device git-annex uses for its
metadata branch. The file shapes are in [formats](../../reference/formats.md#branch-records).

Because the record travels with the branch, any machine that opens a published
branch reads one answer. `rwv push` publishes the record of the branch it
pushes, and no other: the record of a branch never pushed, a minted workweave
name included, stays on the machine that wrote it. Another machine that sees
such a branch finds it unrecorded and is asked to spell it once. A record whose
branch has been deleted everywhere, with no workspace of this machine aligned
for it, is dropped by the next write.

A remote that does not accept the records branch does not stop a push. The
project branch is published without its record there and the rejection is
reported as a fact (`publish` `records rejected`). A machine that opens the
branch from that remote is asked to spell it once.

Every verb that reads a record first merges the remotes' records branches into
the local one, entry by entry. Entries for different branches never conflict. Two
machines that recorded one branch name differently meet in one entry, which then
holds both values (`target` `conflict`) and refuses the verbs that read that
branch until `rwv branch set-mode` writes one; the conflict is never published, so
it stays on the machine whose merge met it, as the second pusher alone meets a
rejected push. An entry that does not parse is `target` `unreadable` for that
branch alone, settled the same way. A record that would make a chain of targets
return to its own branch is refused when written and, when two machines each wrote
half of one, settled when met (`target` `cycle`).

## A changed record is reported once

A record can change under a workspace: a colleague runs `rwv branch set-mode` on
a published branch and pushes it, and the next fetch brings the change. Each
workspace stores the record it last saw for each branch. When the checked-out
branch's record differs from the stored one and this workspace did not make the
change, `rwv status`, `rwv update`, `rwv pull`, `rwv switch`, `rwv push` and
`rwv land` report it with the old and new value and how it arrived
(`target` `record changed`). `rwv sync` and `rwv land` refuse once on an unseen
change. The two choices are `rwv update`, which accepts the new record and stores
it, and `rwv branch set-mode`, which restores the old one. A workspace with
nothing stored stores what it reads, without a report.

## What a record is not

A record says how a branch is used. It does not say where members sit (that is
[alignment](./alignment.md), a function of the record, the branch's entries and
its lock), how the branch accepts work (that is its `history` setting, in
`rwv.toml`; see [history and lineage](./history-and-lineage.md)), or whether it
has landed (that is ancestry; see [landing](./landing.md)). Keeping those apart is
what lets a hand edit of `rwv.toml` or a raw VCS move leave the record alone:
nothing in a record can drift out of step with a file it is not in.

## Related

- [Alignment](./alignment.md): where members sit for a recorded branch.
- [Landing](./landing.md): what the target and `as` mean at `rwv land`.
- [Formats: branch records](../../reference/formats.md#branch-records): the file shapes.
- [`rwv branch`](../../reference/cli.md#rwv-branch) and [`rwv explain branch`](../../reference/explain/branch.md).
