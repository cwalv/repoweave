# Op-state records

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

The on-disk records `rwv sync`, `rwv land`, and `rwv pull` and `rwv update
--remote` with `--merge-upstream` or `--reset-to-upstream` use to make a
multi-workspace operation resumable across a crash. Written and cleared by the verbs; read on
`--continue` and by `rwv abort`.

Nothing outside `rwv` should parse these. A plugin that needs to know whether an
operation is in flight calls back into `rwv` — `rwv status --json`, and probe
the field rather than gating on a version. [`../reference/plugin-protocol.md`](../reference/plugin-protocol.md)
names both files under the write prohibition; the field tables below exist for
someone changing `rwv`'s own resume logic, not for a consumer.

## `.rwv-op` — owner op-state record

Written at the **initiating workspace** (the owner) when a `rwv sync` or
`rwv land` operation starts. Holds all op parameters plus the current phase.
It is the sole copy of mutable op state. Cleared on success, precondition
refusal, and after `rwv abort`; preserved on phase failure so `--continue` and
`rwv abort` can resume.

Schema v7. The number moves with the record's shape: a test in `op_state`
pins a digest of the declarations the wire record reaches in that file beside
`OP_RECORD_SCHEMA`, so a field added, removed, renamed, retyped or given a
serde attribute there without a new number fails it. Every reader refuses a record whose `schema` is another number or
absent, and one carrying a field this release does not know (every struct of
the record denies unknown fields), as `unreadable-op-record`, naming the
release that wrote it (`release`, or "a release before" the running one for a
record that has none): it is finished or aborted with that release, or ended
here by `rwv abort --discard-unreadable-record`, and nothing of it is read. A
record whose op id, verb, project and workspaces do not parse is refused the
same way. JSON. The top-level keys are mandatory, so a record missing one fails
to parse rather than defaulting, and abort reads it header-only; inside
`take_in`, fields marked `serde(default)` in `TakeInRecord` read as empty when
absent.

```json
{
  "schema": 7,
  "release": "0.25.0",
  "id": "1779769917405921588",
  "verb": "sync",
  "history": null,
  "project": "web-app",
  "source": "/abs/path/src",
  "target": "/abs/path/tgt",
  "retire": false,
  "phase": "replay",
  "converged_tips": {},
  "started_at": "2026-06-10T21:14:03Z",
  "take_in": null
}
```

| Field | Description |
|---|---|
| `schema` | The record schema, `7`. A record carrying another, or none, is refused as `unreadable-op-record` |
| `release` | The rwv release that wrote the record |
| `id` | Unique operation identifier (nanosecond wall-clock string). Shared with savepoint refs and lease files |
| `verb` | Which top-level verb started this op: `sync`, `land`, `pull`, `update-remote` or `cherry-pick` |
| `history` | The `--history` the op was run with for one run (`linear`, `merge`, `ff-only`), or `null`. It governs a repo only where the target records no `history` for it, and `--continue` re-applies it |
| `project` | The project the op operates on. Both `source` and `target` are resolved under it; neither workspace's `.rwv-active` is consulted, since an operator may retarget a pointer while the op is parked. `rwv abort` reads it to know which manifest to enumerate on each side |
| `source` | Absolute path of the source workspace |
| `target` | Absolute path of the target workspace. For `sync`: same as the owner workspace. For `land`: the workspace that has the target checked out, or `null` where it is checked out nowhere and the landing moves it as refs (no workspace is leased then; the project repo's entry in `take_in.receiving` is `{"as-ref": {"create": false}}`) |
| `retire` | Whether `--retire` was passed |
| `phase` | Current phase in execution order: `replay` → `relock` → `advance-target` (land only) → `retire` (`--retire` only) |
| `converged_tips` | Per-repo converged tips written at relock completion. Empty before. Consumed by advance-target and the landing's target judgment at abort. Key: repo path relative to workspace root, or `(project)` for the project repo (the flag spelling `projects/<name>` is the operator's, and is not stored). Value: SHA string |
| `started_at` | RFC3339 UTC timestamp when the op started |
| `take_in` | For `land`: the landing's plan — `sync_from` naming the target; `receiving`, the one map of the landing's receiving branches, holding under `project` the target branch and its tip and under each planned member's path its receiving branch, that branch's tip as the landing read it, and how the landing moves it (`{"branch": ..., "tip": ..., "moves": ...}`, `moves` one of `"in-checkout"`: composed onto the tip, then fast-forwarded in the target workspace's checkout, which must be on that branch; `{"as-ref": {"create": ...}}`: composed, then moved as a ref no checkout holds, created at the tip first when `create`; `{"in-place": {"workspace": ...}}`: composed, then fast-forwarded in place in the clean checkout that workspace, neither the source nor the target's, has of the member, the landing holding a lease on that workspace until it releases the target and a savepoint of that checkout under `<id>-in-place`; `"enters"`: a member the target lacks, its branch created at the tip, its pin in the feature, nothing composed; `{"follows": {"create": ...}}`: takes the tip, merging nothing, the branch not moved, and created at the tip first when `create`). Advance-target and `--continue` check and move each of these by its name, to the repo's converged tip; `allow_member_change`, the repos `--allow-member-change` named, at the start or by a `--continue`; `created_refs`, each branch the landing created, which `rwv abort` deletes; `source_tip`, the source's project tip; `relocked`, the tip of each member the source's lock was behind, read once before anything moved and recorded by the landing's project commit (a `--continue` reads it, never re-reading the members); and `moved_in_target`, each repo of the target workspace the landing moves (by its fast-forward, by moving its receiving branch as a ref, or by the alignment of the target workspace that follows), written before the repo moves and taken back where the landing stopped before moving it, or where aligning the target workspace left it on the commit and the branch it stood on before aligning; `wrote_in_target`, the tip each of those checkouts is moved to (the composed tip for a fast-forward, the target's new pin for one aligning moves), against which abort judges whether something else moved the target since; `made_in_target`, each member checkout of the target's workspace the landing makes (materializing a member the workspace lacked, or by aligning the workspace), written before it is made, which `rwv abort` removes from its store where it holds nothing a commit does not (an edit, an untracked or ignored file, or an operation in progress there blocks the whole abort, and a checkout that is its own store is left); `moved_in_place`, each checkout the landing fast-forwards in place in a third workspace (`{"workspace": ..., "repo": ..., "wrote": ...}`, `wrote` the tip it moves it to), written before it moves and taken back where the landing stopped before moving it, kept apart from `moved_in_target` so one member moved in both is two entries; `moved_refs`, each branch the landing moves as a ref, as `{"repo": ..., "branch": ..., "to": ...}`, written before it moves, which abort alone puts back. `reports`, each move the landing reported as it made it (`created`, `followed`, `repinned`, `merged`: a range holding a merge integrated by a merge commit, `rebased`: a branch a rebase rewrote and the backup keeping its old tip, and `taken-in`: the target's upstream commits taken in with each pin and member entry they changed), which a resumed landing reports again and never twice. `merged`, the repos the landing composes by a merge commit (first parent the receiving tip) rather than a rebase; and `backups`, each `rebased` backup the op made of a branch it rewrote (`{"repo": ..., "name": ..., "published": ...}`). One of a branch it found unpublished is dropped once the landing completes, and one of a published branch stays until the republish replaces its push branch; abort drops every `rebased` backup a branch reaches once it has restored, and reads this journal only to name, on a line of its own, each one it leaves holding a commit no branch reaches. `rwv abort` restores a target repo or branch the journal names and leaves every other one there where it stands, a repo stopped in someone else's rebase or merge included. A creation is journaled in `created_refs` before the branch is created, so abort undoes whichever step a stop interrupts. A `land` record without the target's entry in `receiving` is refused on `--continue`, naming `rwv abort`. For `sync`: the branch it takes in (`sync_from`: `{"kind": "target" or "from", "branch": ...}`), the repos `--reset-to-target` named (`reset_to_target`), and in `upstream_tips` the tip each member takes in (its merge source) and, under `project`, the tip of the branch taken in; `merged`, the repos it integrates by a merge commit (first parent the feature's) rather than a rebase, as the target's `history` setting or a range holding a merge decided at plan time; every other repo that still needs more than a fast-forward when the sync acts, a `--continue` included, is judged again from the repository as it then stands (the setting recorded at the tip taken in, else the run's `--history` from `history`; a range now holding a merge is merged; a rebase refuses a declared branch and a dependent it cannot carry along, before anything is rewritten); and `backups`, as for `land`, dropped when the sync completes. For every verb, `merges`: each merge the op started (`{"repo": ..., "source": ...}`, the commit it merges in), journaled before the merge starts. With `rebases`, `carries` and the pick's commits it is what `rwv abort` reads to tell the op's own operation in progress from one the operator started: only an operation that takes in what one of these does is canceled as the op's. And `abort_begun`, set by `rwv abort` before it moves anything; `--continue` refuses a record that holds it, so a refused or interrupted abort leaves an op only `rwv abort` takes up. For `pull` and `update-remote`: what `--continue` re-applies (`merge_upstream`, `reset_to_upstream`, the `--role` and `--repo` selection, `dirty`, `commit` and `message`, `no_materialize`), the upstream tip each repo was merged or reset against (`upstream_tips`, so a resumed op acts on the tips the first run planned), and each branch moved as a ref with no checkout (`ref_moves`: repo, branch, from, to). For `cherry-pick`: `pick`, holding the project commit ported (`rev`), its `mainline`, the repos `--except` named (`except`), each `owned` or `fork` member's copies (`members`: `repo`, `commits` oldest first, the member's `mainline`, `branch` and `start`, the branch the copies go onto and its tip when the port was planned, `copying`, set before a copy starts and cleared once its outcome is recorded, and as the copies run `copied` as `[from, to]` pairs, `dropped`, and `left_out`, the commits the branch already held), each carried `dependency` or `reference` pin (`carried`), each excepted member's recorded tip (`excepted`), `head`, the project repo's tip when the port was planned, and `project_started`, set before the project commit's copy begins. Each step is recorded before it moves a repo, and `--continue` reads the repo to learn what the step did: a copy in progress is finished; a member one commit past where the port left it, that commit recording the copy started as its source, holds that copy, and a project repo one commit past `head` recording `<rev>` holds the project commit; a project repo still at `head` with no copy in progress has the copy made again. Any other tip, such as a commit made by hand at the stop, is taken in: a member's copies go on from it, and the project commit is made on it, `head` moved to it; `--continue` refuses only a member on another branch than `branch`. A repo whose savepoint is missing gets one before it moves, where it still sits at its planned tip, so `rwv abort` restores a member the port moved |

Lives at the workspace root (same directory as `.rwv-active`). Source:
`src/op_state.rs`.

### `pull` and `update --remote`

These two take the owner record with `source` and `target` both the workspace
they ran in, and no leases. Their phases are `replay` (the member merges, the
resets, the project merge up to its commit) and `relock`. Each checkout the op
may move gets a savepoint under the op's own id, and one that cannot be filed
stops the op before anything moves. A branch the op moves as a ref, because no checkout has it
attached, has no HEAD to savepoint, so the move goes in `take_in.ref_moves`
instead, and `rwv abort` puts it back by compare-and-swap, keeping first a tip that
moved since onto commits the savepoint does not reach, and refusing one a
checkout holds and one that is gone, each with the choice that clears it:
freeing the checkout, or putting the branch back where the op found it. Every step is idempotent: a `--continue`
finishes a merge left staged in a checkout, skips one already made, and
recomputes the rest.

### What the record's refs hold

An op files its savepoints under a key: its id, its id with `-target` on
a `land` target's side, and its id with `-in-place` for each checkout a
landing fast-forwards in place in a third workspace (one per member store,
since a branch is checked out in one place). Under a key there is one savepoint per ref the op may
move: each checkout's, which names the branch the checkout was on
(`refs/rwv/pre-op/<key>/checkout/<branch>`, or `.../detached`), and each
branch a landing moves as a ref (`refs/rwv/pre-op/<key>/ref/<branch>`), filed
at the tip the landing read before it composes. `rwv abort` puts a checkout
the op switched back on its saved branch before any reset, resets no branch
but the one a savepoint names, and only once every checkout is back puts
each branch moved as a ref back by compare-and-swap against the tip the op
wrote. For a landing it first reads every receiving branch it moved, by the
branch's tip wherever it moved it, and every checkout of `T`'s workspace that
aligning moved off the receiving branches by its HEAD, against the tip it
wrote (`moved_refs`, `wrote_in_target`, `created_refs`) and the tip it found
(`receiving`, or the savepoint): one elsewhere, or a ref-moved branch
checked out outside both workspaces, refuses the whole abort before anything
moves, and `--leave-target` drops the target's savepoints instead of
consuming them.

#### Abort is planned whole, then executed

`rwv abort` plans every write it would make, on both sides, before it makes
any: each checkout's restore (`Vcs::plan_checkout_restore`: a stopped
operation, the checkout's branch and HEAD against its savepoint, the
uncommitted edits a move would discard, who holds the branch it switches
back to), each branch moved as a ref (`Vcs::plan_ref_restore`, held as the
plan leaves the checkouts), each branch moved in place in a workspace since
gone or in a checkout its operator since moved off it
(`left_off_its_branch`; the branch goes back as a ref and the checkout
stays), each created branch it deletes (judged against the branches as the
restore leaves them: one no other branch would then reach blocks), and each
ref an update moved. What the plan cannot read (a checkout, a branch
savepoint, the other workspace's project) is a blocked step naming what
could not be read. A cancel the VCS would refuse (a path the stopped
operation staged and the working tree changed since) blocks too. The plan
only reads, with one exception: predicting a switch back refreshes the
index's stat data first (`update-index --refresh`), as `git status` does,
so that files written by reflink do not read as edits; a held or unwritable
index lock is reported as a step that could not be read. The plan
is the restore: the restore executes those steps and decides nothing the plan
did not. Any blocked step, or a landing's target judged moved as a unit,
refuses the whole abort with one report naming each obstacle and the one
command that ends the operation; nothing is written, `abort_begun` stays
unset, and `--continue` stays open. The plan is made again under the retaken
claim and lease, judged again, and only then executed. The target's
project-branch write runs first: a later write that loses a race leaves the
target's lock behind its members, every commit on a branch, never a lock
pinning commits the restored members left. `--leave-target` is accepted
exactly where the plan does not restore the target, writes nothing until
the source's plan is unblocked, and gives up the target (its savepoints, the
lease and the claim) only once every source step executed as planned.

Uncommitted edits a move would discard are kept as a backup first in either
workspace of the operation. A checkout whose switch back over edits git
refuses has them kept and cleared first. A submodule's own state is never
in the way: a hard move of its superproject leaves the submodule's checkout
as it stands.

The residual against raw git: each planned write checks, immediately before
it writes, the state the plan judged (HEAD, attachment, the operation in
progress, a branch's tip, its holders) and a branch moves by compare-and-swap,
so a raw git change in between makes that one write refuse, never overwrite;
execution stops at that step, the writes already made stay, and a rerun of
`rwv abort` completes the rest. Git refusing a cancel for any cause the plan
does not read has the same aftermath.

A savepoint at the key's own name, with nothing below it, was filed
by an earlier release; only `rwv doctor --fix` reads one. Savepoints live
exactly as long as the record: cleanup drops them with it, over every member
either side listed before the op and after it, and `rwv abort` consumes
them. They are the only refs an op owns. Abort resets off a commit without
a backup only where the op wrote it or the savepoint reaches it. What abort
keeps is a backup (`refs/rwv/backup/<branch>/<id>`), which outlives the
record and belongs to `rwv backup`: a tip it moves a branch off that holds
commits the op did not make, the working copy of a checkout stopped in a
rebase, merge or copy, the uncommitted edits a reset would discard, the
commits a detached HEAD holds that no branch reaches, each backup the op
journaled that the restore does not reach, and a
savepoint's tip no branch reaches when abort drops the savepoint of a repo
it leaves. A savepoint that outlives its record is not
a backup either: `rwv doctor` reports it as `orphaned-savepoint`, and `--fix`
is what turns one that holds the last reference to its commits into a backup.

## `.rwv-op-lease` — thin lease pointer

Written at every **other workspace the op mutates** (never at the owner
workspace). Immutable once written. Provides mutex semantics (prevents
concurrent ops on the same workspace) and a pointer back to the owner record
for `--continue` and `rwv abort`. A landing clears it as its record moves to
`retire` or as it ends, and `rwv abort` of a landing at `retire` writes it
again before it restores. A lease that points at another owner is never
cleared by this op.

```json
{
  "id": "1779769917405921588",
  "owner": "/abs/path/to/owner/workspace",
  "created_at": "2026-06-10T21:14:03Z"
}
```

| Field | Description |
|---|---|
| `id` | Unique operation identifier. Same as the owner record's `id` |
| `owner` | Absolute path to the owner workspace. Follow this pointer to load the full op state |
| `created_at` | RFC3339 UTC timestamp at which the lease was written. Surfaced by `rwv doctor` as observability-only context, never a decision input |

Lives at the workspace root of the non-owner mutated workspace. Source:
`src/op_state.rs`.

## Where else to look

- [`../reference/formats.md`](../reference/formats.md) — the published entry for
  both files: what they are, which verbs write and clear them.
- [`../how-to/resume-or-abort-mid-op-sync.md`](../how-to/resume-or-abort-mid-op-sync.md)
  — the operator-facing recovery path (`--continue`, `rwv abort`).
- [`../explanation/joints/sync-semantics.md`](../explanation/joints/sync-semantics.md)
  — the phase machine these records persist.
