# What the seam requires of a backend

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

`Vcs` (`src/vcs.rs`) is the whole VCS surface. Core states every rule in terms
of branches, remotes, ancestry and commits and computes every fact itself; a
backend supplies the operations below and owns every spelling. `GitVcs`
(`src/git.rs`, `src/git/`) is the one implementor. This page states what a
second one must provide, so the trait's shape is a requirement and not an
accident of git. The operator-facing contract is
[`../explanation/joints/vcs-as-seam.md`](../explanation/joints/vcs-as-seam.md);
[`git-floor.md`](./git-floor.md) says which git release the git backend runs on.

## What core never does

- Spell a remote: a remote crosses the seam as a `RemoteHandle`, which only a
  backend constructs and which does not give its name back. Core reaches one by
  role.
- Spell a ref, a flag, a path argument or a command: `GitCommand`
  (`src/git/argv.rs`) is the only way git is spawned.
- Read a VCS's wording: outcomes are told apart by exit status, by formats the
  VCS documents for machines, or by the repository's state.
- Print a VCS action itself: an operator's VCS-specific line comes from
  `Vcs::hint(VcsAction)`, printed as "VCS action (git: ...)".

## What a backend provides

Each group is a capability. A method is a requirement when core cannot compute
its answer from the others.

| Capability | Methods (not exhaustive) | Requirement |
|---|---|---|
| Identity and readiness | `name`, `minimum_release`, `tool_readiness` | the oldest release of the tool the backend runs on, and whether the installed one meets it |
| Observation | `head_attachment`, `observe_head`, `resolve_revision`, `resolve_local_branch_tip`, `named_branches`, `branch_tips`, `is_ancestor`, `merge_bases`, `first_parent_chain`, `ancestry`, `commits_in_range`, `log` | read refs and history; no observation moves anything |
| Roles of remotes | `url_remote`, `add_url_remote`, `project_url_remote`, `record_project_url_remote`, `home_remote`, `push_target`, `push_target_of`, `own_push_remote`, `push_branch`, `upstream`, `upstream_of`, `remote_copies`, `recorded_urls` | resolve each role from the url and the VCS's own configuration; the project repo's url remote is recorded by the verb that created it |
| Relation | `relation` (even, behind, ahead, diverged), `reached_by_remote`, `unpushed_commits`, `held_by_no_remote`, `patch_equivalents` | ancestry facts, and the patch-equivalence fact, which is advice and never settles anything |
| Fetching | `fetch_pruning` (takes a `&FetchClaim`), `resolve_branch_on_remote` | a pruning fetch that reports what it could not complete and keeps superseded tips |
| Ref writes with proof | `advance_attached_ref`, `advance_detached_head`, `advance_branch_ref` (compare-and-swap), `delete_branch(DestroyProof)`, `create_branch`, `rename_branch`, `set_upstream`, `push_ref` with a `Replace` | the kinds in [`branch-model.md`](./branch-model.md); a ref moved without a checkout moves by compare-and-swap against the tip read |
| Commits with no working tree | `write_commit`, `files_at`, `file_in_tree`, `advance_records`, `records_tip`, `publish_records` | build a commit from file contents and parents, and move a ref to it conditionally; the records branch needs nothing else ([`records-branch.md`](./records-branch.md)) |
| History-changing operations | `rebase`, `replayed`, `take_rewritten`, `merge_as_refs`, `commit_merge`, `copy_commit`, `continue_copy`, `copied_from`, `copied_into`, `tree_merges` | each reports what it did (which old commit became which, which commits were copied from where, a copy that came out empty) and stops on a conflict as the VCS does; `tree_merges` is optional, the caller takes its fallback on `None` |
| Operations in progress | `mid_operation`, `branch_under_rebase`, `merging_commits`, `conflict_sides`, `cancel_in_flight_op`, `ending_stopped` | observe the operation a checkout is stopped in and cancel it; core never reads the VCS's own markers |
| Savepoints and backups | `create_ref_savepoint`, `ref_savepoints`, `plan_ref_restore`, `execute_ref_restore`, `plan_checkout_restore`, `execute_checkout_restore`, `create_backup`, `list_backups`, `drop_backup`, `keep_in_slot`, `backup_edits` | refs rwv owns, one per ref an operation may move; a slot is a backup with a fixed name whose earlier values stay readable |
| Checkouts and stores | `store_of`, `private_dir`, `main_checkout`, `checkouts_of_branch`, `gone_checkouts_of_branch`, `nested_stores` (gitlinks included), `stores_kept_for`, `registered_branch`, `holds_only_what_it_fetched` (nested stores only), `list_worktrees`, `create_worktree_on`, `materialize_worktree_detached`, `remove_worktree`, `worktree_registration`, `list_stale_worktree_registrations` | the relation between a workspace and the store whose refs it shares; a per-workspace private directory where workspace state lives; for a checkout that no longer stands, the stores the store keeps for its record and the branch the record has checked out |
| Working copy | `dirty_file_names`, `tracked_dirty_file_names`, `carry_edits_onto`, `check_out_branch`, `set_edits_aside`, `stage_paths`, `commit_only`, `has_commit_identity` | edits are observed and carried or set aside by the backend |
| Lock driver | `lock_driver_setup`, `declare_lock_driver_in`, `define_lock_driver`, `lock_driver_declaration_file` | the VCS runs `rwv lock --merge-driver` when it merges `rwv.lock`; one that cannot run fails the merge into the VCS's ordinary conflict, never a silent side |

## What a backend may answer without asking

Each process start is a cost the operator pays per question, several times
over on macOS and Windows, and core asks the same question from many layers.
A backend may give an answer again without asking only where the answer cannot
change while the repository stands: where the repository and its private and
shared directories are, and what a full commit id names (its files, a file at
it, that it resolves). Never a ref, `HEAD`, the index, configuration or the
worktree list: those move under core between two of its own questions. The git
backend keeps such answers per process in `src/git/recalled.rs`, reuses one
only while the directory's `.git` entry is the entry it was read through (same
kind, same creation time, same bytes for a file), keeps no failure and no
"nothing there", and forgets them all when it starts a git that can create,
move or remove a repository or worktree. A filesystem that keeps no creation
time gets no reuse. `tests/scenario/git_spawns_test.rs` bounds how many git
each of a fixed set of commands starts.

## What the seam holds by type

The types in `src/vcs.rs` that carry a rule rather than data:

- observation witnesses (`AttachedRef`, `UnbornRef`, `DetachedHead`) no one but
  the default `head_attachment` constructs, and `verify_attachment`, which
  fails with `VcsError::StaleRefWitness` when the world moved;
- `DestroyProof` and `BackupWitness`, the no-loss warrant;
- `FoundBranch`, the only thing a worktree is born on;
- `CloneUrl` (`src/registry.rs`), the only URL a method hands the VCS;
- `RemoteHandle`, `ResolvedRevisionId` (serializes, does not deserialize),
  `SavepointKey`.

## What is not mechanized

One message site in core still writes `origin` as a literal (the doctor's
`origin-url-mismatch` rendering, whose wire kind carries the word too), and a
path turned into a `&str` by hand and given to a text slot is a review catch,
not a compile error. The residue is stated in
[`../explanation/joints/vcs-as-seam.md`](../explanation/joints/vcs-as-seam.md).
