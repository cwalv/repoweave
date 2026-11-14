//! Destructive-operation tripwire.
//!
//! Policy lives in `docs/explanation/destructive-operations.md` —
//! satisfy-the-precondition-or-stop, informed override flags, discards stay
//! recoverable. Read that before editing the allowlist below; this
//! header carries the enforcement-mechanics summary only.
//!
//! Enforcement: this test inventories every destructive call site in
//! `src/` by scanning for the patterns in `TRACKED` (and refusing the
//! patterns in `FORBIDDEN` outright). Adding, moving, or removing a
//! tracked site fails the build here until the `ALLOWLIST` below is
//! updated with the new count and a justification that names which
//! precondition guards the site, what the override consent looks like,
//! and how discards stay recoverable. That is intentional friction:
//! the cheapest moment to catch an unguarded `reset --hard` is the
//! commit that introduces it.
//!
//! Counts are per file and exclude comment lines, so prose mentioning a
//! pattern does not trip the wire. Audit each new site against the
//! policy linked above before bumping its count.
//!
//! A literal count cannot see a new CALLER of an already-counted site,
//! which is the same hazard one step out — so an entry whose justification
//! enumerates callers instead of naming a type gate also carries the
//! measured caller count, repo-wide, in `callers`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// One audited (file, pattern) entry: how many call sites are allowed and
/// why each is safe.
struct Allowed {
    /// Path relative to `src/`.
    file: &'static str,
    pattern: &'static str,
    count: usize,
    /// Wrappers whose callers the justification below ENUMERATES, with the
    /// number of call sites that enumeration accounts for. Empty where the
    /// justification rests on a type gate instead — a proof parameter or a
    /// sealed constructor binds a new caller exactly as it binds an old one,
    /// so such an entry survives one; a prose enumeration does not.
    callers: &'static [(&'static str, usize)],
    justification: &'static str,
}

/// Patterns that identify destructive call sites. Substring match against
/// non-comment source lines.
const TRACKED: &[&str] = &[
    "\"--hard\"",               // git reset --hard
    "remove_dir_all",           // recursive directory deletion
    "remove_file",              // file deletion
    "symlink::remove(",         // typed symlink unlink (remove_file/remove_dir by link type)
    "\"-D\"",                   // git branch force-delete
    "\"worktree\", \"remove\"", // git worktree remove
    "--force-with-lease=",      // git push --force-with-lease, leased to a tip
    "\"checkout\"",             // git checkout (worktree overwrite when forced)
    "\"update-ref\"",           // ref surgery
    "\"stash\"",                // stash flows hide work; drop and clear lose it
    "\"read-tree\", \"-m\"",    // a merge into the real index, rewriting files with -u
];

/// Patterns that must not appear at all. Each introduces a destruction
/// vector this codebase has no audited use for. If you need one, add it to
/// TRACKED with an allowlist entry instead.
///
/// `-M` is here because the branch model has one rename
/// ([`Vcs::rename_local_ref`]) and the uppercase form would let it rename
/// *over* an existing branch — destroying
/// that branch's ref with no proof, and doing it silently,
/// since git stops refusing. The lowercase `-m` refuses, which is what makes
/// a leftover in the way a reported obstacle instead of a casualty.
const FORBIDDEN: &[&str] = &[
    "\"clean\"",            // git clean: deletes untracked files
    "\"filter-branch\"",    // history rewrite
    "\"checkout\", \"-f\"", // force-checkout bypasses git's dirty refusal
    "\"reflog\"",           // expire/delete cuts the last recovery path
    "\"-M\"",               // git branch -M: renames OVER an existing branch
];

const ALLOWLIST: &[Allowed] = &[
    Allowed {
        file: "add_remove.rs",
        pattern: "remove_dir_all",
        count: 1,
        callers: &[],
        justification: "AddLeftovers::refused, taking back a repository \
            `rwv member add --new` created before it was refused: the bare upstream \
            or the new member. Only a path recorded as Made::Repo, and \
            recorded only after the add saw nothing there, so it holds \
            nothing but what this add put in it: an empty bare repository, \
            or a clone of one, or a fresh `git init`. Never reached once \
            rwv.toml lists the member (a failed install hook leaves the \
            member standing). Directories made to hold them are removed \
            with remove_dir, which refuses a non-empty one.",
    },
    Allowed {
        file: "sync.rs",
        pattern: "remove_dir_all",
        count: 1,
        callers: &[],
        justification: "CheckoutRemoval::perform, for the two variants \
            that delete a directory. A CheckoutRemoval is a sealed type: its \
            variants are private to sync.rs and plan_checkout_removal is \
            its only constructor, so every caller of perform (sync's \
            prune_dropped_repo, and rwv member remove --delete, which in the \
            primary also runs the shared-clone check before it) holds a \
            removal that passed these guards. plan_checkout_removal: both \
            arms behind the uncommitted-changes judgment at the top of the \
            function, which refuses with every checkout derived from a \
            primary store judged beside it (fails safe: a status git \
            cannot read refuses). \
            Each arm additionally establishes WHAT it is about to delete \
            before deleting it, because the two removals are different \
            operations and the preconditions that gate them do not \
            transfer. \
            (1) PRIMARY arm — a DESTROY-STORE: \
            `dest` there IS the canonical store, so the delete would take \
            the object database and the worktree administration of every \
            live workweave checkout of that repo with it. Behind the \
            local-only-branch scan AND, in FRONT of the delete, R4 via \
            judge_store_checkouts — no worktree still registered against \
            the store. The \
            local-only refusal is NOT relaxed in exchange: it is \
            incidentally what has been keeping this call off a live \
            workweave's store, so a workweave's branch stays inside its \
            predicate (§5, prune_dropped_repo row). That scan is now \
            fail-CLOSED throughout: an unreadable branch list refuses, an \
            absent remote counterpart refuses, and a count git could not \
            take refuses with its own message instead of being read as \
            \"nothing unpushed\" (the previous unwrap_or(0), recorded here \
            as a measured fail-open and since fixed; mutation-verified by \
            prune_dropped_repo_refuses_when_the_ahead_count_cannot_be_taken). \
            (2) WORKWEAVE arm — a checkout removal, and it is one because \
            the arm proves it rather than inferring it. The divergence \
            refusal covers only the `canonical.exists()` branch, which ends \
            at workweave::release_checkout; the remove_dir_all is in the `else`, where \
            there is no canonical to compare against and that check is not \
            available. So before deleting, the else branch resolves the \
            checkout's actual store (workweave::resolved_worktree_parent, \
            the same helper delete_workweave's is_lone_canonical uses) and \
            compares it against the checkout: a linked workspace has its \
            refdb and objects in a store this delete never touches, and is \
            removable as a working tree. A checkout that IS its own store — \
            inverted topology, joints/clone-topology.md I1, which the \
            absence of a primary-side clone does NOT rule out — is refused \
            with a message naming the path and pointing at `rwv doctor`. \
            An unresolvable store falls back to the checkout path and so \
            refuses too. Mutation-verified by \
            prune_dropped_repo_refuses_a_workweave_checkout_that_is_itself_the_store, \
            with prune_dropped_repo_removes_a_workweave_checkout_linked_into_a_store_elsewhere \
            as the control that the arm still removes what it should.",
    },
    Allowed {
        file: "workweave.rs",
        pattern: "remove_dir_all",
        count: 3,
        callers: &[],
        justification: "(1) CreateRollbackGuard::drop: removes the \
            partially-built workweave of a failed create. \
            (2) CreateRollbackGuard::rollback_and_collect_failures: same \
            intent as (1) but for explicit bail! paths so cleanup failures \
            can be appended to the returned error; defuses Drop to prevent \
            double-rollback. (3) delete_workweave: behind the \
            dirty refusal unless --discard-uncommitted, and behind the \
            detached-commits refusal unless the caller holds a \
            DiscardUnlandedConsent — the token minted only from \
            --discard-unlanded at CLI dispatch. \
            Both list what is lost first. The branch deletions inside \
            the same verb each additionally hold their own DestroyProof (R3); \
            this entry covers the directory removal only. \
            (4) runs through remove_tree_outlasting_child_handles, whose \
            other caller is replace_standalone_member, the `doctor --fix` \
            arm for standalone-in-workweave: it removes the full clone it \
            renamed aside only after the worktree replacing it was born, \
            and only behind refuse_unless_replaceable, which refuses while \
            the clone holds a ref whose tip no canonical ref reaches, any \
            file no commit records (ignored files included), or a worktree \
            of its own, and lists each. No override. What goes with it \
            that no precondition reads: its reflog and its repository-local \
            config and hooks, as with a workweave delete.",
    },
    Allowed {
        file: "workweave.rs",
        pattern: "symlink::remove(",
        count: 1,
        callers: &[],
        justification: "materialize_member: unlinks a SYMLINK at a member \
            slot — the shape older releases gave reference members — before \
            creating the worktree there. symlink::remove unlinks the link \
            itself by its Windows type, never following it, so the canonical \
            clone or dangling path it pointed at is never touched. No waiver \
            needed: a link holds no work.",
    },
    Allowed {
        file: "symlink.rs",
        pattern: "remove_file",
        count: 1,
        callers: &[],
        justification: "the unlink primitive inside symlink::remove, the \
            typed-unlink seam every symlink removal routes through; its \
            callers are the destructive sites and are audited under the \
            symlink::remove( pattern. The remove_dir fallback beside it \
            fires only when the path is verified a symlink, so a real \
            directory is never removed here.",
    },
    Allowed {
        file: "settle.rs",
        pattern: "remove_file",
        count: 1,
        callers: &[],
        justification: "settle's unshare: removes the private copy it wrote \
            beside a shared file in the same call, and only when replacing \
            the shared file with it failed. The path is minted there and \
            holds nothing but that copy; the shared file and every other \
            name for it are untouched.",
    },
    Allowed {
        file: "share.rs",
        pattern: "remove_file",
        count: 3,
        callers: &[],
        justification: "names placement itself made in the same call, and \
            nothing else: the probe that asks whether a source can be cloned or \
            linked from at all; a staged clone or link whose rename over a member \
            file failed; and a new link whose file turned out to have write \
            bits, which removes only that new name, never the source's. The \
            member file a staged name would have replaced is untouched.",
    },
    Allowed {
        file: "git.rs",
        pattern: "remove_dir_all",
        count: 2,
        callers: &[],
        justification: "register_worktree_in_place: removes the staging \
            directory it created empty in the same call, only when the \
            `git worktree add --no-checkout` into it failed. It holds nothing \
            but what that add wrote; the checkout being registered is never \
            under it. keep_working_copy_before_cancel: removes an untracked \
            directory the restore plan named as in the way of canceling a \
            stopped operation, in the same function and after the snapshot \
            of the whole working copy, untracked and in-the-way ignored \
            paths included, is kept as a backup.",
    },
    Allowed {
        file: "git.rs",
        pattern: "remove_file",
        count: 4,
        callers: &[],
        justification: "register_worktree_in_place: removes the checkout's \
            `.git` link file, which names a git directory that is not there \
            — the only state it is reached in — so the link git just wrote \
            for the new registration can take its place. No tracked or \
            untracked file of the checkout is touched. \
            discard_captured_change: deletes a working file the \
            parent commit does not record — the second half of a move whose \
            first half copied it elsewhere. Gated inside the method, not on \
            its callers: the file is read back and must hold exactly the \
            bytes the caller passes as captured, and status must report it \
            untracked, or added with nothing staged its working file does \
            not show. Any other state returns an outcome and touches nothing, \
            so what is deleted is always a byte-for-byte copy of what the \
            caller holds. refresh_paths_if_left: removes the scratch index it \
            built the merge's target tree in. keep_working_copy_before_cancel: \
            removes an untracked file the restore plan named as in the way \
            of canceling a stopped operation, under the same terms as its \
            remove_dir_all entry: only after the working copy that holds it \
            is kept as a backup in the same function.",
    },
    Allowed {
        file: "git/scratch.rs",
        pattern: "remove_file",
        count: 2,
        callers: &[],
        justification: "remove_scratch_files: removes a call's owner file and \
            the scratch files named for it, under the checkout's private git \
            directory, when the call ends or, for one a killed process left, \
            when no process holds the owner file's lock. Only names rwv gives \
            its own scratch are matched; the checkout's index and working \
            files are never opened for writing.",
    },
    Allowed {
        file: "git/scratch.rs",
        pattern: "remove_dir_all",
        count: 1,
        callers: &[],
        justification: "remove_scratch_files: removes the detached checkout a \
            merge made inside a scratch name in the private git directory. \
            That checkout holds no work of anyone's: it is a copy of a \
            commit, made and merged by the call that owns the name, and \
            a killed call's is swept only once its owner lock is free.",
    },
    Allowed {
        file: "git.rs",
        pattern: "\"worktree\", \"remove\"",
        count: 2,
        callers: &[
            ("remove_worktree", 4),
            ("prune_orphan_worktrees_for", 2),
            ("prune_stale_worktree_registration", 3),
        ],
        justification: "remove_worktree. NOTE the `--force`: git will remove \
            a DIRTY worktree here, so what protects each caller has to be \
            named per caller — the previous blanket \"every caller checks \
            for uncommitted changes and unique commits first\" was measured \
            false for the rollback callers, which check neither. \
            (1) workweave::release_checkout, the one release of a \
            workweave's member checkout, reached two ways. From \
            delete_workweave's member loop: behind the dirty refusal unless \
            --discard-uncommitted and the detached-commits refusal unless \
            DiscardUnlandedConsent (see the workweave.rs/remove_dir_all \
            entry), after resolving each worktree's actual canonical-store \
            parent (Vcs::store_of) and refusing when that \
            checkout is itself a canonical store with dependents outside the \
            workweave — the tier-0 topology precondition, not bypassable by \
            any waiver. From CheckoutRemoval::perform's RegisteredWorktree \
            arm, for prune_dropped_repo and for rwv member remove --delete in a \
            workweave: plan_checkout_removal is that arm's only constructor, \
            and builds it only behind the uncommitted-changes refusal and \
            the divergence refusal of its `canonical.exists()` branch. \
            (2) delete_workweave's project repo: behind the same two \
            refusals as (1)'s delete route. \
            (3) create rollback — CreateRollbackGuard::drop and \
            rollback_and_collect_failures: NO dirty check, and none is \
            wanted. These iterate `self.registered_worktrees`, which holds \
            exactly the worktrees THIS create registered moments earlier; \
            nothing an operator could have dirtied is in the set. \
            (4) create's pre-create orphan prune: the same call, but reached \
            only on the path where workweave_dir does NOT exist, so the \
            `worktree_path.exists()` guard inside \
            prune_orphan_worktrees_for is false for every pair and only \
            `worktree prune` (admin entries for absent dirs) actually runs. \
            (5) rwv abort's removal of a checkout of the target's workspace \
            the landing made (sync::run_step, AbortStep::RemoveMade): only a \
            checkout the record journals as made by this landing; planned \
            only where it is a linked worktree, not its own store, and holds \
            no operation in progress, no ignored file and no uncommitted \
            change, and that same check is read again immediately before the \
            removal; the commits a detached HEAD holds that no branch reaches \
            are kept as a backup first, and the branch it was on keeps the \
            rest. \
            The second site is prune_stale_worktree_registration, WITHOUT \
            `--force`, and only after it has read the registration as \
            unlocked and found nothing at its path, so git drops the \
            administrative entry alone; a locked registration or a standing \
            directory makes git refuse even if those reads were wrong. Its \
            callers are doctor --fix's registration repair, \
            fix_state_hygiene for a stale-worktree-registration finding; \
            workweave::materialize_member, which drops the \
            registration of the very slot it is about to give a worktree, \
            only when nothing stands there; and workweave::create_workweave, \
            which does the same for the project checkout's destination.",
    },
    Allowed {
        file: "git.rs",
        pattern: "--force-with-lease=",
        count: 2,
        callers: &[],
        justification: "Both sites replace a remote branch only while it still \
            holds the tip the caller read, so a remote that moved since refuses \
            instead of losing the move. (1) push_ref: a leased replacement only \
            under Replace::Leased, whose Lease is minted only by \
            OverwriteConsent::lease, which exists only for a repo the operator \
            named with rwv push --overwrite; the pushed branch is the PublishRef \
            the caller names, and push.rs keeps the remote tip as a backup first. \
            (2) delete_remote: a remote delete takes a DestroyProof, whose \
            landed or backed-up ground it ran the checks for.",
    },
    Allowed {
        file: "git.rs",
        pattern: "\"checkout\"",
        count: 4,
        callers: &[],
        justification: "The bare `checkout()` that used to head this list is \
            DELETED: fetch and update had already moved to \
            the branch-model primitives below, which classify what HEAD is \
            before writing anything, and with its last \
            caller gone the method went with the rest of the old surface. \
            What remains is four sites, none of which can reposition a \
            checkout without first saying which kind of ref write it is \
            performing. \
            (1) set_detached_head: the branch-model ATTACH/MOVE primitive \
            for a HEAD that names no branch. No -f, so git's own refusal to \
            overwrite modified paths still applies. Reachable only through \
            detach_head, which requires a Departure proof alignment mints \
            and re-verifies the attachment witness first, or through advance_detached_head, which is a MOVE of an \
            already-detached HEAD and refuses when the repo is mid-op \
            (including mid-bisect). \
            (2) attach_head_to: reattaches to an EXISTING local branch. \
            Omitting -b does NOT make git refuse an absent branch — it \
            invents one from a remote-tracking ref (checkout.guess), or \
            detaches when the name is a tag's, or treats the name as a \
            pathspec and reverts uncommitted edits to that path, all \
            exiting 0. So the refusal is rwv's: the branch tip is resolved \
            through refs/heads/<name> first and an absent branch is an \
            error before git runs, with --no-guess and the -- terminator as \
            defense in depth for the window after the check. No -f, so \
            git's own refusal to overwrite modified paths still applies. \
            Reachable only through reattach_head, which requires a \
            Departure proof alignment mints and refuses when the observed \
            HEAD state differs from the one the caller planned against. \
            (3) clone_attached_at: the second half of a birth, and it can \
            only ever run against a repo the first half of the same call \
            just created — there is no signature that points it at an \
            existing one. `-B` therefore repositions a ref git minted \
            moments earlier with no working tree hanging off it and no \
            observer, which is what makes it a birth rather than a MOVE \
            (`rwv update` for a member not on disk is the other birth site). With an explicit \
            start point there is no checkout.guess and no tag lookup, and \
            the -- terminator keeps a path-shaped name out of the pathspec \
            list; the clone runs --no-checkout, so no working tree is ever \
            materialized at the remote tip for this to overwrite. \
            (4) create_branch_and_attach: creates the branch a verb is \
            about to put a checkout on and attaches HEAD to it in one git \
            step (`checkout -b <name> <start>`), carrying edits as git's own \
            branch creation does. It refuses a name already taken before \
            git runs, and git itself refuses, with nothing created and \
            nothing moved, a move that would overwrite modified paths; no -f. \
            It takes the Departure proof, which for the project repo's own \
            move keeps a detached HEAD's unreachable commits as a backup \
            first.",
    },
    Allowed {
        file: "git.rs",
        pattern: "\"--hard\"",
        count: 4,
        callers: &[
            ("reset_to_savepoint", 1),
            ("hard_reset", 1),
            ("discard_edits_kept", 1),
            ("keep_working_copy_before_cancel", 1),
        ],
        justification: "(1) hard_reset(): the operation's intent is to \
            discard divergent commits. It has NO caller outside the trait: \
            its one caller is Vcs::reset_attached_ref_keeping, which takes \
            an AttachedRef witness (which a detached or unborn HEAD cannot \
            produce), re-verifies it against the repo, and takes a \
            BackupWitness of the tip being left, refusing a witness that \
            backs up another repo or tip, so the rewind is type-gated and \
            the commits it leaves stay reachable from the backup. \
            (2) reset_to_savepoint(): its one caller executes a \
            planned Reset of a checkout to its savepoint; the executor first \
            checks the checkout still holds the tip the plan judged. In the \
            same function, before the reset, the working copy the move \
            discards is kept in the checkout's slot and the tip, where it \
            holds a commit no other branch reaches, in the branch's slot, \
            so the commits it moves the branch off remain reachable. A checkout stopped \
            mid-operation is never reset directly: it is canceled and its \
            tip classified again. \
            (3) discard_edits_kept(): the reset to HEAD that lets a \
            checkout switch back over edits that collide with its saved \
            branch, only in the workspace abort runs in, and only after the \
            edits are kept as a backup in the same function; HEAD does not \
            move, so no commit is touched. \
            (4) keep_working_copy_before_cancel(): the reset to HEAD that \
            clears what would make the VCS refuse to cancel a stopped \
            operation (an unstaged edit to a path the operation staged, an \
            untracked path the cancel writes). It runs in the same function \
            after the whole working copy is kept as a backup, and only where \
            the plan named something in the cancel's way, which it plans \
            only in the workspace abort runs in; HEAD does not move. \
            restore_plan_tests::an_untracked_file_in_a_cancels_way_is_kept_before_it_is_cleared \
            reads the backup back. The unverified restore_savepoint() that \
            used to sit beside these — a bare `reset --hard` on the public \
            trait, gated by nothing — was deleted, so \
            verified_restore_savepoint is the only way to reach a \
            savepoint-driven rewind.",
    },
    Allowed {
        file: "git.rs",
        pattern: "\"update-ref\"",
        count: 10,
        callers: &[],
        justification: "(1)-(2) savepoint creates, a checkout's and a \
            branch's moved as a ref; (A)-(C) savepoint drops: every one \
            under a key, a checkout's, one branch's. All five write or delete \
            only under refs/rwv/pre-op/<key>/ (relocated from sync.rs). (3) create_unpopulated_submodule(): \
            detaches HEAD, with --no-deref, in a submodule repository the \
            same call cloned an instant before and that nothing else has \
            seen; its branches are left as the clone made them. None of \
            (1)-(3) or (A)-(C) \
            touch user refs. (4) delete_branch: `update-ref -d <ref> \
            <tip>`, where the old value is a compare-and-swap, so a branch \
            that moved since the proof was made is left. Reachable only \
            through Vcs::delete_branch, whose single parameter is a \
            DestroyProof (R3): an opaque struct over a private enum whose \
            constructors RUN the check they certify, landed (the tip is an \
            ancestor of a branch that outlives it) or backed_up (the tip \
            was kept as a backup first, under DiscardUnlandedConsent, \
            minted only from --discard-unlanded). The proof names the \
            store, so it cannot authorize a delete in a different refdb. \
            It refuses a branch checked out in any worktree before it \
            deletes. (5) the same command spelled in the argument list of \
            the error that refusal returns; it is not a call. (6) the same command \
            spelled in the argument list of the error advance_branch_ref \
            returns when the move is not a fast-forward; it is not a call. \
            (7) record_checkout_at_head: a HEAD reflog entry at the HEAD the \
            refresh read, by a compare-and-swap whose old and new values are \
            both that commit, so it moves no ref; a branch moved since makes \
            it fail rather than move back or record the newer tip.",
    },
    Allowed {
        file: "git.rs",
        pattern: "\"read-tree\", \"-m\"",
        count: 1,
        callers: &[("refresh_paths_if_left", 3)],
        justification: "refresh_paths_if_left, the one write of a drift \
            refresh: a two-way merge from the moved-from tip to that tip's \
            tree with the stale paths taken from HEAD, so in an index that \
            exists every other path is one both sides agree on and git keeps \
            its index entry and file; an index with no entries reads no \
            moved-from tip, so the merge never runs as an initial checkout. \
            git itself refuses the merge where a stale path's index entry or \
            tracked file no longer holds the moved-from tip's content or an \
            untracked file is in the way; no --reset. What git's check passes \
            (an ignored file or directory where HEAD adds a path, a tracked \
            file deleted from disk) is checked immediately before the spawn. \
            Reached through the backend's Vcs method of the same name, whose \
            callers are all in check::refresh_drift: the files scope, run \
            through a copy of the checkout's index so the checkout's own index \
            is untouched, then the index-only scope over every stale path, \
            each passing only paths two readings of check::read_drift agree \
            are stale.",
    },
    Allowed {
        file: "git/carry_edits.rs",
        pattern: "\"update-ref\"",
        count: 1,
        callers: &[],
        justification: "set_back: points an attached HEAD, with --no-deref, at \
            the tip the checkout's files hold, the branch untouched and \
            named in the reflog message; index and files are not written. \
            Reached only from check::merge_edited_drift, after the drift \
            reading names that tip and, for the merge, after the working \
            copy and index are kept as a backup.",
    },
    Allowed {
        file: "git/carry_edits.rs",
        pattern: "\"checkout\"",
        count: 2,
        callers: &[],
        justification: "carry_edits_onto: `git checkout -m <branch>`, never \
            forced, from a checkout set back at the tip its files hold: git \
            merges each local change onto the branch and refuses, writing \
            nothing, where staged changes or an untracked file are in the \
            way. Reached from the merge flag of doctor --fix, consented to by \
            name. check_out_branch: a plain `git checkout <branch>`, never \
            forced, run by the set-aside flag once the held edits are \
            stashed: git carries what is left where the branch does not \
            change its path and refuses, writing nothing, where it would \
            have to merge.",
    },
    Allowed {
        file: "git/carry_edits.rs",
        pattern: "\"stash\"",
        count: 1,
        callers: &[],
        justification: "set_edits_aside: `git stash push -- <paths>` of the \
            files both moved and edited, from a checkout set back at the tip \
            its files hold, so the stash holds the edits alone; the report \
            prints the `git stash apply <stash>` that merges them back. \
            Never drop, pop or clear. Reached only from \
            --set-aside-edited-drift, consented to by name.",
    },
    Allowed {
        file: "git/backup.rs",
        pattern: "\"update-ref\"",
        count: 3,
        callers: &[],
        justification: "(1) write_backup_ref: creates \
            refs/rwv/backup/<branch>/<id> where no ref of that name exists \
            (or one already holds the same tip), so it adds a ref and moves \
            or removes none. Its only entry is Vcs::write_backup, reached \
            from Vcs::create_backup, the sole producer of a BackupWitness. \
            (2) drop_backup_ref: deletes one ref under refs/rwv/backup/, \
            the name having come from a listing of that namespace; the \
            caller's pin rule runs first, and the operator names the \
            repo (`rwv backup drop <repo>`) or runs `rwv backup prune`, \
            which drops only backups a branch already reaches in full. \
            (3) write_backup_ref, for an abort slot: points the fixed name \
            refs/rwv/backup/<branch>/aborted or .../aborted-working-copy at \
            the tip abort keeps, over what the slot held. The slot is one \
            deep by design, as the VCS's own record of the last tip a reset \
            moved off is; what it held stays in the ref's own log until \
            that expires.",
    },
    Allowed {
        file: "git/pick.rs",
        pattern: "\"update-ref\"",
        count: 1,
        callers: &[],
        justification: "copy_without_commit: writes CHERRY_PICK_HEAD, the \
            pseudo-ref naming the commit a copy in progress copies, after \
            `cherry-pick --no-commit` applied it to the index. It moves no \
            branch and no HEAD: it marks the checkout as stopped in a \
            cherry-pick, which `cherry-pick --abort` undoes and the next \
            commit ends.",
    },
    Allowed {
        file: "git/merge.rs",
        pattern: "\"update-ref\"",
        count: 2,
        callers: &[],
        justification: "(1) commit_staged_merge: moves HEAD (the branch it \
            names, or a detached HEAD) onto the merge commit just written \
            from the index, a descendant of the tip HEAD held. (2) \
            update_branch_ref: moves a local branch from the tip the caller \
            read to a new one by a compare-and-swap naming the expected old \
            value, so a branch another verb moved meanwhile is never \
            overwritten. Its callers are advance_branch_ref (refuses what is \
            not a fast-forward), reset_branch_ref (needs a BackupWitness for \
            the tip being left, checked against the repo and the tip) and \
            restore_branch_ref (puts back, for abort, a branch only where \
            it still sits at the tip the operation wrote).",
    },
    Allowed {
        file: "git/merge.rs",
        pattern: "\"worktree\", \"remove\"",
        count: 1,
        callers: &[],
        justification: "merge_commit_as_refs: removes the scratch worktree \
            it added moments before to merge in a release with no \
            merge-tree. It runs after the merge was aborted, so the \
            worktree is clean, and without --force: a worktree holding \
            anything is left standing and the error returned.",
    },
    Allowed {
        file: "git/records.rs",
        pattern: "\"update-ref\"",
        count: 1,
        callers: &[],
        justification: "advance_records_ref: moves the local records branch \
            from the tip the caller read to the commit it built on that tip \
            (or creates it where none exists), by a compare-and-swap that \
            names the expected old value, so a ref another verb moved \
            meanwhile is never overwritten and the call reports it as lost. \
            The records branch holds no weave state a checkout reads; its \
            only callers are the records store's writers, which merge the \
            current tips into what they write.",
    },
    Allowed {
        file: "git/replay.rs",
        pattern: "\"checkout\"",
        count: 1,
        callers: &[("checkout_rewritten", 1)],
        justification: "checkout_rewritten, the branch move of a re-map: \
            `checkout -B <branch> <to>` on the branch an AttachedRef witness \
            names, re-verified first, and only after HEAD was read to be \
            `from`, the tip the replay's record describes. `to` is a copy of \
            that replay the record names before the move, differing only in \
            the lock and in a staged rwv.toml the checkout already holds as \
            `to` does. The move is git's two-way merge, so other staged and \
            unstaged edits are kept as they are and one in a path the move \
            changes refuses it; no -f. The commits it leaves were never \
            published.",
    },
    Allowed {
        file: "git/replay.rs",
        pattern: "remove_file",
        count: 1,
        callers: &[],
        justification: "remove_replay_records: removes the files named `rwv-replayed-*` in the checkout's private \
            git directory, which only rwv's own rebases write, once the op \
            that reads them has ended; no ref, index or working-tree file.",
    },
    Allowed {
        file: "activate.rs",
        pattern: "symlink::remove(",
        count: 4,
        callers: &[],
        justification: "(1) activation-symlink cleanup: only symlinks that \
            are in the integration-owned set AND resolve into projects/. \
            (2) foreign-shared-name cleanup: only top-level symlinks whose \
            target is exactly projects/<other-project>/<same-name> — rwv's \
            own surfacing of a shared name out of a project the weave root \
            does not present — and only while surfacing the one it does. \
            Recoverable by re-surfacing that other project. \
            (3) unsurface_undeclared: links at names the presented project no \
            longer declares. This one is guarded by a TYPE rather than by a \
            checked condition at the site, which is why no caller enumeration \
            appears above for it and why a new caller cannot widen it. It \
            takes &[UndeclaredLink] and no root; UndeclaredLink has private \
            fields and no public constructor, so the only values in existence \
            are those undeclared_project_links minted on the branch where all \
            four conjuncts held — symlink read without following, rwv's own \
            surfacing shape for that path, resolving into the PRESENTED \
            project, and a name the project does not declare. A regular file \
            cannot be expressed as an argument, and neither can a path this \
            scan did not classify. \
            The consent is --remove-undeclared-links on `rwv materialize`, \
            minted only in cli::dispatch and named for what it destroys. \
            `rwv doctor` reports each link with its target and names that \
            command, and has NO --fix arm for it: on disk rwv cannot tell its \
            own residue from a link an operator made at the same shape, so the \
            loss list is shown and the choice is left. Recoverable without \
            rwv: unlinking destroys no content — the file each link pointed at \
            is untouched — and the finding prints the exact target, so the \
            link is reconstructable from the report that named it. \
            (4) SurfacingJournal::roll_back: unlinks only links the same \
            surface_symlinks call created — each recorded in the journal at \
            the moment symlink::create returned Ok — and only after that call \
            failed, ahead of re-creating every link the call had removed. The \
            journal is a private type with private fields, built and consumed \
            inside one call, so no other path can be put on its list. No \
            consent: the call is undoing its own act. Recoverable: what it \
            returns to is the weave root as the call found it, and re-running \
            the verb makes the link again.",
    },
    Allowed {
        file: "root_write.rs",
        pattern: "remove_file",
        count: 1,
        callers: &[],
        justification: "remove_entry, unlinking a real file at the weave-root path of a \
            lock an ecosystem tool writes through its link (Cargo.lock, package-lock.json, \
            pnpm-lock.yaml, uv.lock, go.work.sum). Two routes. (1) An adoption op: the \
            file's bytes have just been written to the project copy in the same journal — \
            the Write op precedes the Remove — or already equal it, and the verdict that \
            allowed it is that the project copy was absent, identical, or still the \
            generation the owned-digest ledger records; any other state is a conflict, \
            reported, and both files are left. Only a verb that may write reaches it \
            (SurfacingMode::Repair, or taking the outgoing project's file home on a \
            switch). (2) FileJournal::roll_back of a path the same pass created. No \
            consent: nothing removed here exists nowhere else. Recoverable: the project \
            copy holds the bytes, and the link now standing at the path reads them.",
    },
    Allowed {
        file: "root_write.rs",
        pattern: "symlink::remove(",
        count: 1,
        callers: &[],
        justification: "remove_entry's other arm, reached only from \
            FileJournal::roll_back: a failed pass putting a path back to what it held, \
            removing whatever stands there before restoring the prior file or link. The \
            adoption ops it undoes only ever remove regular files, so a link here is one \
            the same pass's rollback is replacing. Unlinking destroys no content.",
    },
    Allowed {
        file: "activate.rs",
        pattern: "remove_file",
        count: 3,
        callers: &[],
        justification: "deactivate's .rwv-active removal moved to \
            workspace.rs's clear_active_project, audited under state_file.rs; \
            the two symlink unlinks moved behind symlink::remove, audited \
            under that pattern. \
            (3) settle_arrived_drift's regenerate arm. Precondition: the file \
            is named by the owned-digest ledger AND its bytes differ from the \
            digest recorded when rwv accepted its generation — a fully-owned \
            generated file, whole-delete safe by the declaration that put it \
            there. Bare materialize REFUSES and lists every path it would \
            discard, so the override is informed by the run before it; the \
            consent is --regenerate-drifted, named for the discard, minted \
            only in cli::dispatch and unforgeable elsewhere. NOT recoverable \
            by rwv: the discarded bytes are content rwv never accepted, so no \
            digest, savepoint or copy of them exists — `rwv explain \
            materialize` says so at the flag. \
            (4) strip_disabled_integrations. Precondition: the integration is \
            disabled in rwv.toml AND the file is one its own \
            `owned_paths_on_disk` reports as WholeFile — rwv wrote all of it, \
            proven by the ownership marker on the workspace file that made rwv \
            the author, never by the file merely being present. A hybrid file \
            the operator co-owns is never removed here; it goes through the \
            integration's strip, which keeps their content. The consent is the \
            verb: `rwv doctor` reports the finding with every path it would \
            remove and has NO --fix arm, so `rwv materialize` is the operator \
            re-running against that list, and the strip names each path as it \
            acts. Recoverable by re-enabling the integration and running \
            `rwv doctor --fix`, which regenerates every artifact removed here \
            except the ecosystem lock, whose next generation is the ecosystem \
            tool's to produce. \
            (5) RestorePoint::restore, on an intent verb's regeneration that \
            failed before any install hook ran (ActivationFailure::BeforeHooks). \
            Precondition: the path was ABSENT when RestorePoint::take read it, \
            immediately before the verb wrote its manifest, and is a regular \
            file now; and it is rwv.toml, the owned-state ledger, or an owned \
            path under the manifest before or after the edit — all inside \
            projects/<project>/; the ledger's own unlink goes through \
            StatePath::remove, audited under state_file.rs. So the file is one \
            the failed regeneration created. No consent: the verb is undoing its own write, and the \
            error it returns says the change was put back. Recoverable: the \
            file is a derivation of the refused edit, and re-running the verb \
            once its cause is fixed regenerates it. The one case that removes \
            content rwv did not write is another writer creating that same \
            file inside the verb's own regeneration.",
    },
    Allowed {
        file: "state_file.rs",
        pattern: "remove_file",
        count: 1,
        callers: &[("clear_active_project", 3)],
        justification: "StatePath::remove, the one unlink of a state file. A \
            StatePath names only a file of the closed StateFile list, and only \
            StateFile::in_dir builds one, so what this removes is rwv-internal \
            bookkeeping, never user data, and the type gate binds a new caller \
            exactly as it binds an old one. Callers: clear_owner and \
            clear_lease (op_state), RestorePoint::restore (activate; audited \
            under activate.rs (5)), and clear_active_project (workspace). \
            clear_active_project removes the .rwv-active pointer. It is \
            unconditional here by design — the pointer's owner cannot know \
            which caller's precondition applies, so each caller establishes \
            its own in FRONT of the call, and the entry an auditor reads is \
            this one regardless of how many callers there are. \
            Every current caller is a doctor --fix arm. \
            (1) dangling pointer: the named project's directory \
            is gone, so the pointer selects nothing that exists. \
            (3) unreadable pointer: the file is empty, unreadable, or holds \
            text that is not a project name. rwv never writes such a file and \
            no verb acts on it — each refuses instead — so it selects nothing \
            and its removal discards no selection. \
            (2) weave-root identity conflict: the precondition \
            is evidence the tree does not contain. \
            classify_weave_root_identity emits the fixable RegisteredWorkweave \
            sub-kind only when the marker names THIS workspace's primary AND \
            that primary's .rwv-workweave-index records this exact directory \
            as one of its workweaves. Under that precondition the pointer is \
            provably redundant — the marker beside it and the registry entry \
            above it each already name the project, which is the pointer's \
            entire content — so the delete discards no fact that survives only \
            here. Every other shape (unreadable marker, foreign primary, \
            unregistered directory) takes the report-only Unwitnessed arm and \
            --fix touches neither file: with no external witness, deleting one \
            would be a guess, and the marker carries primary/parent values \
            that do NOT exist elsewhere. The marker is never the file deleted. \
            An absent pointer succeeds rather than erroring, which is what \
            lets a caller skip its own existence probe; it widens nothing, \
            since deleting nothing destroys nothing.",
    },
    Allowed {
        file: "liveness.rs",
        pattern: "remove_file",
        count: 1,
        callers: &[],
        justification: "release_unreachable: removes a short claim file \
            (.rwv-fetch.lock, rwv-workweave-index.lock, \
            .rwv-owned-digests.lock), rwv-internal bookkeeping and never user \
            data, only on `rwv doctor --fix --release-unreachable-claims`, \
            consent the operator gives by that flag's name, and only while \
            the file still records the holder the finding reported and the \
            filesystem still gives no lock to read its liveness from.",
    },
    Allowed {
        file: "durable_file.rs",
        pattern: "remove_file",
        count: 10,
        callers: &[],
        justification: "Temp-file cleanup for the two publish modes, never \
            able to name the published file itself, plus the claim release. \
            (1) staged_temp, when the \
            content write or its fsync failed. (2) replace, when the rename \
            failed. (3) create_new's always-runs cleanup after link(2), which \
            consumes the temp's role whether or not the link took. (4) \
            ClaimFile::remove, releasing a claim file \
            (.rwv-workweave-index.lock, .rwv-owned-digests.lock), which is \
            rwv-internal bookkeeping and never user data. The path is the \
            value's own, not a parameter, and claim() is the type's only \
            constructor and builds one only after it held that path, so a \
            ClaimFile that exists names a file this process held. \
            Unlinking is the whole release, and release reports a removal \
            that keeps failing rather than dropping it. (10) claim, taking \
            over a claim file on a filesystem with no advisory locks: removed \
            only once the file names a process on this host that no longer \
            runs. (5) \
            remove_named, the landing claim's takeover lock: called only on \
            a lock its own op created with create_new, or one `rwv abort` \
            of that op finds naming it, so it removes rwv-internal \
            bookkeeping of an op that is ending, never user data. (8) and \
            (9) refuse_without_exclusive_publish, removing the probe's own \
            temp and the temp-shaped name it published that temp onto, both \
            created by this call a moment earlier and never a published \
            file's name. (6) and \
            (7) remove_if_abandoned, unix and Windows: the sweep a publish \
            runs over temps beside its target named <a name beginning with \
            the target's>.tmp.<pid>.<16 hex digits>.<serial>, a spelling only \
            staged_temp makes. It removes one only once no writer holds it: \
            on unix after taking the advisory lock its writer holds for the \
            whole publish, which the kernel drops when the writer dies; on \
            Windows the delete itself fails while its writer holds it open \
            without sharing deletion. A writer whose temp a sweep met in the \
            instant before it held it stages it again. Sites (1)-(3): every \
            site unlinks only the sibling temp this call created moments \
            earlier, named <file>.tmp.<pid>.<key>.<serial>: the pid keeps \
            processes apart, a key drawn once per process keeps a process \
            that reuses a pid from spelling a dead one's temp, and a \
            process-local AtomicU64 keeps threads within one process apart, \
            so no other writer can be holding that name. Both components are \
            structural — an earlier revision used a nanosecond timestamp for \
            the intra-process half and two barrier-synchronized threads \
            collided on it, each unlinking the other's in-flight temp, so \
            \"unique\" here must NOT be re-derived from a clock. No waiver \
            needed: an orphan temp is scratch, and leaving it behind is the \
            worse outcome — a later reader trips over it.",
    },
    Allowed {
        file: "integrations/merge.rs",
        pattern: "remove_file",
        count: 1,
        callers: &[],
        justification: "strip_deactivate: marker-gated; file deleted only \
            when semantically empty after stripping rwv-owned keys — \
            user-held files (no marker) are never touched.",
    },
    Allowed {
        file: "integrations/vscode_workspace.rs",
        pattern: "remove_file",
        count: 1,
        callers: &[],
        justification: "strip_workspace_file: marker-gated; deletes the \
            .code-workspace only when the strip leaves nothing but rwv's own \
            seeded git.* settings still at their seeded values. A user-added \
            folder entry, an exclude key the marker does not claim, any other \
            block, or a changed git.* value keeps the file.",
    },
    Allowed {
        file: "integrations/uv_workspace.rs",
        pattern: "remove_file",
        count: 1,
        callers: &[],
        justification: "strip_workspace_sources: removes `[tool.uv.sources]` \
            entries whose value is `{ workspace = true }`, prunes the \
            emptied parent tables, and deletes pyproject.toml only when the \
            document is left with nothing at all. It writes the stripped \
            document back otherwise, so anything else in the file survives. \
            Marker-gated by its caller: deactivate reads has_our_marker \
            BEFORE strip_deactivate (which removes the marker) and calls this \
            only when we owned the file, so an unmarked, hand-authored \
            pyproject.toml keeps its workspace-true sources and is never \
            emptied or deleted. Before this gate the call was unconditional; \
            the guard is mutation-verified by the \
            deactivate_leaves_unmarked_* tests in uv_workspace.rs.",
    },
    Allowed {
        file: "integrations/npm_workspaces.rs",
        pattern: "remove_file",
        count: 1,
        callers: &[],
        justification: "package-lock.json removal on deactivate, gated on \
            rwv's ownership marker in package.json.",
    },
    Allowed {
        file: "integrations/gita.rs",
        pattern: "remove_file",
        count: 1,
        callers: &[],
        justification: "gita/repos.csv + groups.csv are fully rwv-owned \
            generated files; the directory itself survives if the user \
            added anything to it.",
    },
    Allowed {
        file: "integrations/cargo_workspace.rs",
        pattern: "remove_file",
        count: 1,
        callers: &[],
        justification: "prune_empty_cargo_config on deactivate: deletes \
            <root>/.cargo/config.toml ONLY when it's semantically empty \
            (parse-checked; unparseable content is left alone). The \
            strip_marked_patch_entries pass that runs just before is \
            marker-gated — only rwv-decorated `[patch.<reg>].<crate>` \
            entries are removed, so user-authored linker flags, per-target \
            settings, or hand-authored (unmarked) patch entries survive and \
            keep the file non-empty. The parent .cargo/ dir is pruned via \
            remove_dir (not remove_file) and only when empty.",
    },
    Allowed {
        file: "integrations/go_work.rs",
        pattern: "remove_file",
        count: 2,
        callers: &[],
        justification: "both sites unlink `<workspace_root>/go.work`, never \
            the canonical `<output_dir>/go.work`: the post-copy unlink sits \
            inside `if !same_file`, whose canonicalized comparison exists \
            precisely to keep a symlink-to-self out of the delete. In the \
            production layout output_dir != workspace_root, and the \
            workspace-root path is the framework's activation symlink rather \
            than a copy — so the error-path unlink (which is NOT behind the \
            same_file guard) removes a link, not content: unlinking a \
            symlink does not follow it, and the `go work` commands have \
            already written through it into the canonical file.",
    },
];

// Not built on tests/common/src_scan.rs: that scanner skips every
// #[cfg(test)]-gated item alike, but scan() below must keep reading inside
// one that is not a module (a real hook, like go_work.rs's test-only PATH
// override) and stop only at a #[cfg(test)] mod (a fixture) — a distinction
// the shared scanner does not draw.
fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Whether the next item in `rest` (skipping blank and comment lines) opens
/// an inline module BODY — i.e. whether the `#[cfg(test)]` just seen starts a
/// test region in this file.
///
/// The brace is what separates a region from a declaration. `mod <name>;`
/// gates a whole other file and opens nothing here, so reading it as a
/// boundary would put every line below it outside the scan.
fn next_item_opens_a_module_body(rest: &[&str]) -> bool {
    for line in rest {
        let t = line.trim_start();
        if t.is_empty() || t.starts_with("//") || t.starts_with("#[") {
            continue;
        }
        let Some((before_mod, after_mod)) = t.split_once("mod ") else {
            return false;
        };
        if !before_mod.split_whitespace().all(|w| w.starts_with("pub")) {
            return false;
        }
        return after_mod
            .trim_start()
            .trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_')
            .trim_start()
            .starts_with('{');
    }
    false
}

/// The lines of one source file this audit reads, left-trimmed: comment
/// lines dropped, and everything from an in-file test module onwards cut.
///
/// **Test modules are not call sites.** A `#[cfg(test)] mod` inside `src/`
/// is test code that happens to live next to what it tests, and its
/// fixtures legitimately run `git checkout` / `git branch -D` to build the
/// states the product code is asserted against. Counting them would force
/// every fixture into the allowlist alongside the sites it exists to test,
/// which is noise in exactly the place this file is trying to keep sharp.
///
/// The exclusion relies on the convention this crate follows everywhere: a
/// `#[cfg(test)] mod` body is the last item in its file, so scanning stops
/// at the first one. `#[cfg(test)]` on anything that opens no body — a
/// test-only hook like `integrations/go_work.rs`'s PATH override, or the
/// `mod <name>;` declaration `vcs.rs` gates its stub impl with — does not
/// stop the scan. Product code placed *after* a test module would go
/// unscanned — don't do that.
fn scanned_lines(text: &str) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    let mut kept = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[cfg(test)]") && next_item_opens_a_module_body(&lines[i + 1..]) {
            break;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        kept.push(trimmed);
    }
    kept
}

/// Count pattern hits per (relative file, pattern).
fn scan() -> BTreeMap<(String, &'static str), usize> {
    let src = src_dir();
    let mut files = Vec::new();
    rust_files(&src, &mut files);

    let mut counts: BTreeMap<(String, &'static str), usize> = BTreeMap::new();
    for file in files {
        let rel = file
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&file).expect("read source file");
        for trimmed in scanned_lines(&text) {
            for &pattern in TRACKED.iter().chain(FORBIDDEN) {
                if trimmed.contains(pattern) {
                    *counts.entry((rel.clone(), pattern)).or_insert(0) += 1;
                }
            }
        }
    }
    counts
}

/// Call sites of `name` on one already-trimmed source line: the bare name at
/// identifier boundaries, immediately followed by `(`.
///
/// That paren is what separates a call from a name quoted in a string, which
/// is followed by its closing quote; the `fn` before it is what separates a
/// call from the trait declaration, the impl, and the `vcs/testing.rs` stub.
fn calls_to(line: &str, name: &str) -> usize {
    let mut found = 0;
    let mut from = 0;
    while let Some(rel) = line[from..].find(name) {
        let at = from + rel;
        let before = &line[..at];
        let after = &line[at + name.len()..];
        let head = before.trim_end();
        let declares = head.strip_suffix("fn").is_some_and(|preceding| {
            !preceding.ends_with(|c: char| c.is_alphanumeric() || c == '_')
        });
        if after.starts_with('(')
            && !before.ends_with(|c: char| c.is_alphanumeric() || c == '_')
            && !declares
        {
            found += 1;
        }
        from = at + name.len();
    }
    found
}

/// Call sites in `src/` of every wrapper the allowlist names a caller count
/// for, keyed by wrapper name. Repo-wide, not per file: a destructive
/// wrapper's callers live wherever the verbs that reach it do.
fn scan_wrapper_callers() -> BTreeMap<&'static str, usize> {
    let names: BTreeSet<&'static str> = ALLOWLIST
        .iter()
        .flat_map(|a| a.callers.iter().map(|&(name, _)| name))
        .collect();

    let src = src_dir();
    let mut files = Vec::new();
    rust_files(&src, &mut files);

    let mut counts: BTreeMap<&'static str, usize> = names.iter().map(|&n| (n, 0)).collect();
    for file in files {
        let text = std::fs::read_to_string(&file).expect("read source file");
        for trimmed in scanned_lines(&text) {
            for (&name, found) in counts.iter_mut() {
                *found += calls_to(trimmed, name);
            }
        }
    }
    counts
}

#[test]
fn destructive_call_sites_match_audited_allowlist() {
    let actual = scan();

    let mut expected: BTreeMap<(String, &'static str), (usize, &'static str)> = BTreeMap::new();
    for a in ALLOWLIST {
        let prev = expected.insert((a.file.to_string(), a.pattern), (a.count, a.justification));
        assert!(
            prev.is_none(),
            "duplicate allowlist entry for ({}, {})",
            a.file,
            a.pattern
        );
    }

    let mut problems: Vec<String> = Vec::new();

    for ((file, pattern), &count) in &actual {
        if FORBIDDEN.contains(pattern) {
            problems.push(format!(
                "FORBIDDEN pattern {pattern} appears {count}x in src/{file} — this \
                 destruction vector has no audited use; remove it or promote it to \
                 TRACKED with an audited allowlist entry"
            ));
            continue;
        }
        match expected.get(&(file.clone(), *pattern)) {
            Some(&(want, _)) if want == count => {}
            Some(&(want, justification)) => problems.push(format!(
                "src/{file}: {pattern} found {count}x, allowlist says {want} — a \
                 destructive call site was added or removed; audit it against the \
                 policy in this file's header and update the allowlist.\n    \
                 existing sites: {justification}"
            )),
            None => problems.push(format!(
                "src/{file}: {pattern} found {count}x but has no allowlist entry — \
                 audit the new site (named-precondition-or-refuse, or an informed \
                 named override) and add an entry with the justification"
            )),
        }
    }

    for ((file, pattern), &(want, _)) in &expected {
        if !actual.contains_key(&(file.clone(), *pattern)) {
            problems.push(format!(
                "src/{file}: {pattern} expected {want}x but found none — if the \
                 site moved or was removed, update the allowlist so it stays \
                 accurate"
            ));
        }
    }

    assert!(
        problems.is_empty(),
        "destructive-op inventory drifted from the audited allowlist:\n  {}\n",
        problems.join("\n  ")
    );
}

/// Callers of the destructive wrappers whose justifications enumerate them.
///
/// The inventory above counts LITERALS, so a new caller of an
/// already-counted site is invisible to it: no literal moved, the count
/// still matches, and the justification's enumeration silently goes false —
/// while the caller itself is a fresh route into the destructive operation,
/// reaching it under conditions that enumeration never considered.
///
/// Scope: the same lines as the inventory — every `.rs` file under `src/`,
/// comment lines skipped, in-file test module bodies cut — and within them a
/// bare wrapper name at identifier boundaries followed by `(`. Invisible to
/// it: a route that reaches the wrapper under some other name, one a macro
/// expands to, and any caller inside a test module. Entries whose
/// justification rests on a type gate name no wrapper here and are measured
/// by nothing — deliberately, since the gate is what survives a new caller.
#[test]
fn enumerated_wrapper_callers_match_audited_counts() {
    let actual = scan_wrapper_callers();
    assert!(
        !actual.is_empty(),
        "no allowlist entry enumerates callers — this test asserts on nothing"
    );

    let mut problems: Vec<String> = Vec::new();
    for a in ALLOWLIST {
        for &(name, want) in a.callers {
            let found = actual[name];
            if found != want {
                problems.push(format!(
                    "src/{}: {} — {name} has {found} call sites in src/, this entry's \
                     justification accounts for {want}. A caller is a route into the \
                     destructive operation, so audit the new one against the policy in \
                     this file's header before extending the count.\n    \
                     audited callers: {}",
                    a.file, a.pattern, a.justification
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "destructive-wrapper callers drifted from the audited justifications:\n  {}\n",
        problems.join("\n  ")
    );
}

/// Both passes read `src/vcs.rs` below the `mod <name>;` it gates its stub
/// impl with.
///
/// That declaration sits in the file's first twenty lines, so reading it as
/// a test-module boundary would cut the scan there and put the whole typed
/// VCS seam outside every count in this file — the trait the destructive
/// wrappers are declared on, and `reset_attached_ref_keeping`, the sole caller
/// the `--hard` justification names.
#[test]
fn the_scan_reads_past_a_test_module_declaration() {
    let text = std::fs::read_to_string(src_dir().join("vcs.rs")).expect("read src/vcs.rs");
    assert!(
        scanned_lines(&text)
            .iter()
            .any(|l| l.starts_with("pub trait Vcs")),
        "src/vcs.rs was cut short of `pub trait Vcs`: a `#[cfg(test)] mod <name>;` \
         gates another file and opens no region here — the brace is what starts one"
    );
}
