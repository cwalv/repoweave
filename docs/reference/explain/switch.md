# rwv switch

## Purpose

Put the project repo on a weave branch and align the members to it, in the
workspace you are in; or detach the workspace at a revision. It is the verb for
moving between branches the weave already has: `rwv branch create` makes a new
one, `rwv update` aligns members for the branch already checked out.

A branch's **record** says how it is used (see `rwv explain branch`), and
`switch` reads it:

- A recorded branch reopens bare: `rwv switch feat-x`. A feature's `owned` and
  `fork` members move onto branches named after it; a `declared` branch's
  members move to the branches its `rwv.toml` entries declare.
- An unrecorded branch is never opened by guess. `--target <T>` records it as a
  feature that lands into `<T>` (which needs a record of its own), `--declared`
  as `declared`; the record is written as `rwv branch set-mode` would write it
  and is published with the next push of the branch. Without a spelling the
  branch is refused as `target` `unrecorded`.
- A spelling that differs from the record is refused as `target` `conflict`,
  listing both values and naming `rwv branch set-mode`; one that agrees is
  accepted.
- `<W>` may exist locally or only on a remote as last fetched; a remote-only
  branch is created from the remote's copy, taking in from it.

`rwv switch` with no branch is refused: aligning members for the checked-out
branch is `rwv update`.

## What it checks first

Every repo it would move is judged before anything moves, and every refusal is
reported together. The project repo moves as the VCS moves it, carrying edits
the move does not overwrite; a member that holds edits moves the same way.

- a branch checked out in another workspace is refused (`checkout`), naming the
  workspace and `rwv switch` there, or `rwv workweave delete`; `--detach <rev>`
  is the way to look at it
- an operation in progress in a repo it would move is refused (`edits`
  `mid-operation`)
- edits the move would overwrite are refused (`edits` `dirty`)
- an uncommitted `rwv.toml` or `rwv.lock` is refused (`edits` `rwv.toml or
  rwv.lock uncommitted`), naming `rwv lock --commit`; in a detached project repo
  it names `rwv branch create`, which carries both files
- a member that cannot move is refused as `alignment` reports it: commits it
  would leave on no other branch (`on branch with stray commits`), a detached
  HEAD holding unpushed commits
- a branch the project repo lacks is refused (`name` `no-such-branch`), naming
  `rwv fetch`; where a member has it, that member is named with the way to look
  at it without a weave branch

From a detached project repo, the commits its HEAD holds that no branch reaches
are kept as a backup (`rwv backup list`) before it moves. The project repo is
never left detached by a branch switch: only `--detach` does that.

## `--detach <rev>`

Detaches the project repo at `<rev>`, and every member at the pin the project's
lock holds at that revision: the workspace is a detached workspace. No record is
written, and `rwv update` keeps it detached. A branch another workspace has
checked out can be looked at this way. Switching onto a branch again leaves it.

## Onto a branch an earlier release wrote

A branch whose `rwv.toml` names member-entry keys an earlier release wrote is
switched onto and the run stops before any member moves, refused as
`unparseable-project` `here`; `rwv doctor --fix`, run there, rewrites the file.

## Invocation

```
rwv switch <W> [--target <T> | --declared] [--project <name>]
rwv switch --detach <rev> [--project <name>]
```

Run `rwv switch --help` for the full clap surface.

## Output

`switched to <W>` after the project repo moved, `recorded <W>: <record>` where
an opening spelling wrote one, then the alignment `update` reports, under the
`rwv switch` prefix: `moved from <branch>`, `checkout` facts, and each member
left where it was with its choices. `--detach` prints `detached at <rev>`.

## Exit codes

- `0` — the workspace is on the branch (or detached) and the members are aligned.
- non-zero — a refusal, listing every repo judged, or a branch whose `rwv.toml`
  needs `rwv doctor --fix` before its members can move.

## Examples

Go back to `main` and forward again, in the workspace you are in:

```
rwv switch main
rwv switch feat-x
```

Open a branch made by hand as a feature that lands into `main`:

```
rwv switch hotfix --target main
```

Look at a commit without a branch:

```
rwv switch --detach v1.2.0
```

## Common errors

- *target unrecorded* — the branch has no record. Add `--target <T>` or
  `--declared`, or run `rwv fetch` (its record may be published but not yet
  fetched).
- *target conflict* — the spelling differs from the record; `rwv branch set-mode`
  changes the record.
- *checkout* — the branch is checked out in another workspace (named).
- *edits* — a repo it would move holds an operation in progress, edits the move
  would overwrite, or an uncommitted `rwv.toml` or `rwv.lock`.
- *name no-such-branch* — the project repo has no such branch, locally or on a
  remote as last fetched.
- *unparseable-project here* — the branch's `rwv.toml` uses keys an earlier
  release wrote; the project repo is on it, the members are not moved.
