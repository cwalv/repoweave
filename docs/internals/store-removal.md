# Removing a store

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

When a verb removes a checkout, it may remove an object store with it: the
primary's clone of a member is the store every workweave's checkout is linked
into, and a checkout can hold stores nested inside it (git: submodule clones).
Everything such a store holds that no remote has is gone once it is removed.
This page is the contract for the judgment that stands in front of every
removal, in `src/sync.rs` (`plan_checkout_removal`, `judge_removed_stores`)
and its callers.

## What the judgment asks

The one question is `Vcs::held_by_no_remote`: what a store holds whose commits
no remote-tracking branch of a remote it has reaches, as a list of
`HeldAlone` (`HeldKind`: a branch, a tag, a backup, edits set aside, a
savepoint, a commit a checkout stands on with no branch, or other). Empty means
every commit the store holds is on a remote as last fetched. The answer is only
as good as "as last fetched", so the judgment fetches first.

## Which stores a removal removes

`StoresRemoved` says it:

| Value | When | Stores removed |
|---|---|---|
| `OwnAndNested` | the checkout is the repo's own clone in the primary (`Checkout::Primary`) | the clone's store and every store nested in the checkout (`Vcs::nested_stores`) |
| `Nested` | the checkout is a linked workspace of a store that outlives it | only the stores nested in the checkout |

`Vcs::nested_stores` lists every store nested in a checkout at any depth,
checked out or not: each gitlink of the index whose path holds a repository,
whether or not the submodule file describes it, then each repository under the
checkout's `modules` directory and each found store's, and where the checkout is
its own store, each linked worktree's. A checkout that no longer stands has no
index to read: a workweave whose directory is gone is judged by the stores its
store keeps for its record (`Vcs::stores_kept_for`, git: every repository under
the `modules` directory of the worktree registration), which go with the
record. `judge_stores_kept_for_gone` runs that judgment, and the retire reads
the branch the record has checked out through `Vcs::registered_branch` in place
of the workspace state that went with the directory.

A workweave checkout that is itself a standalone store, with no primary-side
clone to compare against, is refused (`clone-topology`, `RefusalKind::CloneTopology`) rather than removed:
nothing has established that its objects exist anywhere else.

## The order of the judgment

`judge_removed_stores`, for the stores a removal removes:

0. **Leave out a store that holds only what it fetched.** A nested store for
   which `Vcs::holds_only_what_it_fetched` is true (refs only branches and
   remote-tracking branches, and a record of where its checkout stood that
   reaches back to its first entry and names only clones, checkouts and resets
   to commits its remote-tracking branches reach) is neither fetched nor judged:
   no remote's answer changes what removing it loses. The answer is false where
   the record is missing, expired in part, or names anything else. A commit,
   amend, merge, rebase, pull, cherry-pick or patch counts as made in it. The
   clone's own store is never excused this way.
1. **Fetch every store, then judge.** `refresh_remotes` runs a pruning fetch
   into each store: the clone's own through `fetch::pruning_fetch`, which keeps
   the superseded tips it sees, and a nested store through
   `fetch::refreshing_fetch`, which writes only remote-tracking branches and
   forgets them, since nothing rwv can drop may be left in a store about to be
   removed. A remote that did not answer is named (`remote`, cause).
2. **A remote that did not answer refuses.** Without `--discard-unpublished` a
   store whose remotes could not all be confirmed refuses, even where what it
   holds cannot be read, as
   `remotes-unconfirmed` (`RefusalKind::RemotesUnconfirmed`), because what it
   holds that no remote has cannot be told. What a remote has is judged against
   the remotes as they answer now, never against a stale fetch.
3. **Read what is held.** `Vcs::held_by_no_remote` is read for each store; a
   store that cannot be listed or read refuses as
   `dropped-repo-has-unique-commits` (`RefusalKind::DroppedRepoHasUniqueCommits`)
   rather than being assumed empty.
4. **Refuse what is held.** Anything held refuses as
   `dropped-repo-has-unique-commits`, naming each item (and the nested store it
   is in) and the ways to keep it: publish it, commit what was set aside, drop
   the backups it names (`rwv backup drop`), or remove with
   `--discard-unpublished`.
5. **`--discard-unpublished` removes both**, and a list of stores that could not
   be listed refuses without it (`dropped-repo-has-unique-commits`) and is
   removed with it. It lets a removal go past an
   unconfirmed remote and past held items, and names what goes on stderr. It is
   `DiscardUnpublishedConsent`, minted only from that flag in `src/cli/consent.rs`
   (`from_flag`, `pub(in crate::cli)`), so no removal path carries the consent
   without the operator having typed the flag; nothing else discards.

No remote is asked where the removal removes no store.

`--discard-unpublished` is accepted only where a landing's retire stopped over
what the stores it removes hold: with `--continue` of a `land`, and only when
the op record's phase is `retire`. Anywhere else it refuses as
`inapplicable-flag` (`RefusalKind::InapplicableFlag`); the retire's own stop
lists what it would remove.

## Who runs it

`RemovalCause` names the situation in a refusal and picks the rerun line:

| Cause | Verb |
|---|---|
| `DroppedFromLock` | a replay or relock whose merged manifest drops a member (`prune_dropped_repo`); the prune follows every member merge, so an aborted op prunes nothing |
| `RemoveDelete` | `rwv member remove --delete` |
| `WorkweaveDelete` | `rwv workweave delete` (`judge_nested_stores`, each checkout of the workweave) |
| `Retire` | `rwv land --retire`, and `delete_workweave_for_retire`, which carries the consent a `--continue` of the retire passes |

Each runs the judgment before the manifest is written or any directory is
removed, so a refused removal leaves the state as it was. A prune whose
judgment refuses leaves the checkout in place: the next operation starts from
a manifest that no longer names it, and `rwv member remove --delete` is the
way to remove it.

## What it does not judge

- Edits in the working tree: `dirty_file_names` runs beside it and refuses
  with its own state (`dirty_beside_drift`), with the store variant of the
  choices where the removal removes a store.
- Whether a branch's commits are landed. That is the deletion rule
  (`src/deletion.rs`), which judges branches, not stores.
- A checkout the VCS can release without removing a store: its registration is
  released (`workweave::release_checkout`), and the store keeps everything.

## Pins

- `tests/weave/workweave_submodule_test.rs` and
  `tests/scenario/scenario_harness_test/scenarios.rs` hold the refusals of an
  unconfirmed remote and of held items, each as a literal
  `RefusalKind::RemotesUnconfirmed` or `DroppedRepoHasUniqueCommits`.
- `DiscardUnpublishedConsent` is minted only from the flag
  (`src/cli/consent.rs`); `src/sync.rs` has a unit test that
  `judge_store_checkouts` refuses when the claims cannot be read.
