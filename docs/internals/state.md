# Every piece of state rwv keeps

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

One list of what rwv leaves on disk and in the VCS's own structures, who writes
each piece, how long it lives, and what a killed process leaves behind. Field
schemas are in [op-state](./op-state.md); the claims and the temp sweep are in
[concurrency](./concurrency.md).

Scope words: *workspace* is a weave root or a workweave root; *project* is
`projects/<p>/`; *store* is the git directory a clone's worktrees share;
*private dir* is the git directory of one checkout (`.git/worktrees/<n>/` in a
workweave).

## Files that are a state record

Each is a variant of `StateFile` (`src/state_file.rs`) and is written through
`StatePath`, which publishes durably (`src/durable_file.rs`).

| File | Where | Lifetime | A kill leaves |
|---|---|---|---|
| `.rwv-active` | primary weave root | the workspace | nothing: published whole |
| `.rwv-workweave` | each workweave root | the workweave | nothing; restored from the index when missing or disagreeing |
| `rwv-workweave-index` | store of the project clone | the project clone | nothing: published whole under its claim |
| `rwv-workspace-state` | private dir of the project checkout | the workspace | nothing; unreadable reads as absent and is rewritten by the next `update` or `switch` |
| `rwv.lock` | project, tracked | history | nothing |
| `.rwv-owned-digests` | project, machine-local | the workspace | nothing: published whole under its claim |
| `.rwv-op` | the workspace that owns an operation | one operation | the record, by design; `rwv abort` or `--continue` ends it |
| `.rwv-op-lease` | each other workspace an operation mutates | one operation | the lease; `rwv abort` removes it, and `rwv doctor --fix` removes a dead one |
| `.rwv-landing.lock` | `rwv-landings/<digest>/` in the store of the project clone | one operation (a landing carries it) | the claim; the next landing takes over a dead one |
| `.rwv-fetch.lock`, `rwv-workweave-index.lock`, `.rwv-owned-digests.lock` | store, store, project | one critical section | a file whose holder died, which the next claimant takes over |
| `<name>.tmp.<pid>.<key>.<n>` | beside each file above | one write | a temp, swept by the next publish of its name |

`rwv-landings/` is removed with its last claim. A short claim is a lock on its
file (`src/liveness.rs`); where the filesystem has no advisory locks it is the
file's existence, and a holder on another host is cleared by
`rwv doctor --fix --release-unreachable-claims`.

## Refs

Every ref rwv writes is below `refs/rwv/` and in a namespace of `RwvRef`
(`src/git/rwv_ref.rs`), which names it, its owner and how long it lives.
`rwv doctor` reports a ref below `refs/rwv/` in no namespace as
`unrecognized-savepoint-ref`.

| Namespace | Owner | Lifetime | What it holds |
|---|---|---|---|
| `refs/rwv/pre-op/<key>/…` | `src/git.rs` | one operation | savepoints: the checkout (`checkout/<branch>` or `detached`) and each moved branch (`ref/<branch>`) at the start of an operation; `<key>` is the op id, with `-target` or `-in-place` for another workspace's. Abort consumes them; `rwv doctor --fix` drops or converts an orphaned one |
| `refs/rwv/backup/<branch>/<id>` | `src/git/backup.rs` | until dropped | backups, marked `rebased` or `superseded` by the id's prefix; abort's slots `<branch>/aborted` and `<branch>/aborted-working-copy` by the whole id, overwritten by the next abort. `rwv backup list`, `drop` and `prune` are the only readers and writers of the ones an operator sees; a `superseded` one is the old tip of a remote-tracking branch a fetch found rewritten, and the unpushed-commit counts read it as published |

A branch `rwv-records` (local and remote-tracking), one TOML per branch, holds
the branch records (`src/records.rs`, `src/git/records.rs`); it is advanced by
compare-and-swap and published like any branch.

## Commit trailers

| Trailer | Written by | Read by |
|---|---|---|
| `Rwv-Writer:` on records commits | `src/records.rs` | `writer_of`, the release that last wrote a branch's entry |
| `Rwv-Repin:` on project commits | `src/repin.rs` | the unmerged-commit listing, log, cherry-pick |
| `Rwv-Picked-From:` on a cherry-picked project commit | `src/cherry_pick.rs` | `cherry-pick --continue` |

## Files in a private dir or store

| Name | Written by | Lifetime | A kill leaves |
|---|---|---|---|
| `rwv-replayed-<old>-<onto>`, with `.todo` and `.rewritten` | `src/git/replay.rs`, from the rebase's own `exec` | one operation | the files; abort and cleanup remove every `rwv-replayed-*` |
| `rwv-scratch-<pid>` and `rwv-scratch-<pid>-<name>` | `Scratch`, `src/git/scratch.rs`: the drift refresh's two indexes, the commit, records, backup and replay indexes, the merge checkout | one call | the files; the next call to start sweeps what no live process holds |

## What rwv keeps in the VCS's own structures

| Where | What | Written by |
|---|---|---|
| the HEAD reflog | a drift refresh's set-back marker (subject `rwv: set back to the tip its files hold, from <branch>`), read back to find the tip a checkout was set back from | `src/git/carry_edits.rs` |
| the stash | edits set aside by `--set-aside-edited-drift` (subject `rwv: edits set aside from <branch>`), read back to restore them | `src/git/carry_edits.rs` |
| the HEAD reflog | the drift baseline: the entry a refresh compares the checkout against; a checkout with no entry refuses | `src/git.rs` |
| git config `rwv.url-remote` (`--local`, project repo) | which remote the project's URL names | `src/git.rs` |
| git config `merge.rwv-lock.*`, `merge.rwv-ours.*`, and a committed `.gitattributes` line for `rwv.lock` | the merge drivers git runs when merging `rwv.lock` | `src/git.rs`, `rwv setup` |
| `.git/info/exclude` (`.gitignore` when the directory is no repo root) | lines that exclude machine-local state files, appended and never removed | `ensure_ignored_in_dir`, `src/workweave_index.rs` |
| `.git/worktrees/<n>/` | a workweave's checkout registration | the VCS, through `rwv workweave create` |
| `rebase-merge/`, `MERGE_HEAD`, `CHERRY_PICK_HEAD`, unmerged index entries | operations in progress, left as the VCS leaves them; abort cancels only those the op record names | the VCS |
