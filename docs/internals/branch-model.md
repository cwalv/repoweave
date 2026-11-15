# The branch model

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

What a branch is for in rwv, the rule that governs every ref write, and the
types that make the rule hold by construction. This is the maintainer
contract for someone changing ref-handling code in `src/`. The operator-facing
statements are
[`../explanation/joints/workweave-hierarchy.md`](../explanation/joints/workweave-hierarchy.md)
and
[`../explanation/joints/workweave-lifecycle.md`](../explanation/joints/workweave-lifecycle.md).
The records, alignment, landing and removal contracts have pages of their own:
[`records-branch.md`](./records-branch.md),
[`alignment-engine.md`](./alignment-engine.md),
[`landing.md`](./landing.md), [`store-removal.md`](./store-removal.md).

## What a branch is

A branch is never identity, but it routes work.

- A **project branch** carries `rwv.toml` and `rwv.lock` and has a **mode**
  (`records::Mode`): `feature`, which lands into a **target** and whose `owned`
  and `fork` members sit on branches of its own name, or `declared`, whose
  members sit on the branches its member entries name. The mode and the target
  are the branch's **record**, kept on the records branch and published with the
  branch it describes. The record answers where the branch lands (git's
  `branch.<b>.merge`); the target's `history` setting answers how it is taken in
  (git's `branch.<b>.rebase`).
- A **member branch** belongs to a label (a declared branch of a member some live
  weave branch declares) or to a feature (a branch named after the feature). A
  branch no live weave branch declares and no record names is an ordinary branch
  rwv neither protects nor owns.
- A branch the operator made by hand is legal and unaccounted: while a place sits
  on one holding commits nothing else reaches (its stray set), alignment refuses
  to move that place off it; afterwards it is an ordinary branch.
- A minted name (`<project>--<workweave>`) is never published, nor is its
  record. Which branch a workweave was created from is provenance only: no verb
  reads a workweave's `parent` to decide where work lands.
- There are no purpose types and no record of who made a ref. What rwv may
  destroy is decided from the refs and the records, never from a record that rwv
  created a ref.
- A branch is checked out in at most one place; the primary is a place like any
  workweave. A member rests detached only where rwv detached it, and only for
  the reasons alignment names.
- The identity of a place is the VCS's linked-worktree relation: the workweave
  marker and the index entry are two records of it, and a verb repairs whichever
  disagrees (`src/workspace/reconcile.rs`); the destructive verbs refuse on any
  disagreement instead.

## The rule: four kinds of ref write

Every ref write rwv performs is exactly one of four kinds, and the kind decides
what proof it needs. The `Vcs` methods are grouped by kind in `src/vcs.rs`.

| Kind | What it is | The proof the method takes |
|---|---|---|
| **MOVE** | a branch or a detached HEAD moves along its own history or to a named place | an observation witness, `AttachedRef` or `DetachedHead`, from `Vcs::head_attachment` (`advance_attached_ref`, `advance_detached_head`); a move off a fast-forward takes a `BackupWitness` (`reset_attached_ref_keeping`) |
| **ATTACH** | HEAD is pointed at a branch, or a worktree is born on one | a `FoundBranch` for a birth (`create_worktree_on`), a `Departure` for a checkout leaving its attachment (`src/alignment.rs`), a consent token otherwise |
| **DESTROY** | a branch is deleted | a `DestroyProof` (`delete_branch`) |
| **DESTROY-STORE** | an object store goes with a checkout | no method: it is not a ref operation, and the judgment that stands in front of it is [`store-removal.md`](./store-removal.md) |

### The no-loss warrant

rwv destroys a ref only under a warrant proving nothing becomes unpreserved:
the deletion rule's landed clause after a pruning fetch, a backup holding what
the ref held, or a named consent. `DestroyProof` carries it. It is an opaque
struct over a private enum, so no code writes the landed variant without
running the ancestry check it certifies, and it names the store, the branch and
the tip it was proved about; `Vcs::delete_branch` takes the proof and no name,
and a branch that moved since is left.

- `DestroyProof::landed` and `landed_on` run the check: `Some` only when the
  branch's tip is an ancestor of a ref that outlives it, a branch of the
  destroyed branch's own name never counting.
- `DestroyProof::backed_up` makes a backup of the tip first and holds the
  `BackupWitness`. It takes a `DiscardUnlandedConsent`, minted only from
  `--discard-unlanded` (`src/cli/consent.rs`), so an unlanded branch cannot be
  destroyed without the operator's word and a backup.
- `BackupWitness` is minted only by `Vcs::create_backup`, so a value of the type
  proves the ref exists on disk.

`FoundBranch` is the branch the alignment rules found in a store, with the
revision a birth would start it at; a worktree is born only on one. The
witnesses and `FoundBranch` have no `as_str`, so a proof cannot be laundered
back into a string. `tests/source/branch_model_compile_fail_test.rs` pins each
of these by compiling a snippet that must fail.

### Consent tokens

Consent tokens live in `src/cli/consent.rs`, minted only from their named flag
(`from_flag`, `pub(in crate::cli)`) or, on a `--continue`, from the op record
`run` resolved. A blanket override does not exist: each token names the one
thing it destroys (`DiscardUnlandedConsent`, `DiscardUnpublishedConsent`,
`DiscardUncommittedConsent`, `OverwriteConsent` naming repos, and the drift and
claim tokens). That privacy is why `src/main.rs` stays empty.

## The deletion rule

`src/deletion.rs` decides every deletion made by `branch delete`,
`workweave delete` and `land --retire`; none decides one itself. What belongs
to a weave branch `W` is the project branch `W` and, in each member `W`'s
`rwv.toml` lists, the branch named `W` that no live weave branch declares.
`Rule::open` reads what belongs and refuses what the rule cannot judge (the
branch is missing, its target has no record that reads, something lands into
it, a workspace holds it, a landing claim names it); `Rule::apply` judges each.
A branch that belongs is **deleted** when:

1. every commit on it is reached by the branch it lands into or that branch's
   upstream (`landing_refs`, `DestroyProof::landed_on`);
2. no live lock in any project pins a commit only it reaches
   (`pins::stranded_by`);
3. for the project branch, no published copy is an **open head**, a push branch
   holding commits the target does not reach, which a pull request may be open
   against, and none of its member branches survive.

Otherwise it **survives** with a `Reason`: `Unlanded`, `Pinned`, `OpenHead`,
`Members` or `Unconfirmed` (a remote it has a copy on could not be fetched, or
did not take the delete). A push branch is deleted only with `--delete-remote`
(landed ones) or `--delete-remote-unlanded <repo>` (named, leased, with a
backup). A declared branch is never destroyed by rwv: `protected` lists the
live branches declaring it. The record of a deleted branch goes by the drop rule
([`records-branch.md`](./records-branch.md)).

## Backups

A backup is a ref rwv makes before any step that would leave a commit or an edit
unpreserved. It is `Vcs::create_backup` (idempotent per tip, source and mark),
listed by `Vcs::list_backups` and dropped by `rwv backup drop` (git:
`refs/rwv/backup/<branch>/<id>`, `src/git/backup.rs`). `BackupMark` says why it
exists and how long it lives:

| Mark | Made by | Lifetime |
|---|---|---|
| none | a destroy under consent, an edits backup | until `rwv backup drop`, or `prune` when redundant |
| `Rebased` | a rebase, before it rewrites a branch | dropped when the operation completes (unpublished branch) or when the republish replaces the push branch (published) |
| `Superseded` | a pruning fetch that found a remote-tracking branch rewritten | dropped with the branch, or once its push branch is present and the branch reaches nothing only the backup reaches |
| `Aborted` | `rwv abort`: the branch's one slot for the tip a reset moved it off | overwritten by the next abort that resets that branch; the earlier value stays in the VCS's own history of the slot |
| `AbortedWorkingCopy` | `rwv abort`: the checkout's one slot for the working copy it discarded | overwritten likewise |

A **savepoint** is not a backup: it is filed per ref the operation may move
(`SavepointKey`, `Vcs::create_ref_savepoint`, `create_savepoint_ref`), lasts
one operation and serves `abort`; a savepoint that outlives its record is
reported as `orphaned-savepoint` and converted to a backup by `rwv doctor --fix`
([`op-state.md`](./op-state.md)). The abort slots hold what abort discards;
abort attributes no tip to the operation (`Vcs::keep_in_slot`).

## Branch discipline in the doctor

`rwv doctor`'s branch-discipline findings are judgments of the refs by this
page's rules and are documented under their own kinds in
`docs/reference/doctor-findings.md`; the kind is the key, not a section number
here.

## Pins

- `tests/source/branch_model_compile_fail_test.rs`: attached, tracking and found
  refs cannot be compared or spelled as strings, a witness or `DestroyProof`
  cannot be written by hand, a landed proof is reached only through the ancestry
  check, a backup proof needs the operator's consent token and takes no `bool`,
  a delete without a proof or by name does not compile.
- `tests/source/destructive_ops_audit_test.rs` audits the primitives that move,
  attach or destroy a ref.
- `Vcs::delete_branch` has two production callers: `Rule::apply`, and
  `undo_created` (`src/land.rs`), which `rwv abort` runs to delete a branch the
  aborted landing created, under a landed proof judged against the branches as
  the restore leaves them.
