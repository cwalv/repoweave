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
    /// justification rests on a type gate instead — a receipt parameter or a
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
    "push(\"--force\")",        // git push --force
    "\"checkout\"",             // git checkout (worktree overwrite when forced)
    "\"update-ref\"",           // ref surgery
];

/// Patterns that must not appear at all. Each introduces a destruction
/// vector this codebase has no audited use for. If you need one, add it to
/// TRACKED with an allowlist entry instead.
///
/// `-M` is here because the branch model has one rename
/// ([`Vcs::rename_local_ref`]) and the uppercase form would let it rename
/// *over* an existing branch — destroying
/// that branch's ref with neither receipt nor warrant, and doing it silently,
/// since git stops refusing. The lowercase `-m` refuses, which is what makes
/// a leftover in the way a reported obstacle instead of a casualty.
const FORBIDDEN: &[&str] = &[
    "\"clean\"",            // git clean: deletes untracked files
    "\"stash\"",            // stash drop/clear loses work; stash flows hide it
    "\"filter-branch\"",    // history rewrite
    "\"checkout\", \"-f\"", // force-checkout bypasses git's dirty refusal
    "\"reflog\"",           // expire/delete cuts the last recovery path
    "\"-M\"",               // git branch -M: renames OVER an existing branch
];

const ALLOWLIST: &[Allowed] = &[
    Allowed {
        file: "sync.rs",
        pattern: "remove_dir_all",
        count: 2,
        callers: &[],
        justification: "prune_dropped_repo, both arms behind the \
            uncommitted-changes refusal at the top of the function (fails \
            safe: unwrap_or(true) refuses when git cannot be asked). \
            Each arm additionally establishes WHAT it is about to delete \
            before deleting it, because the two removals are different \
            operations and the preconditions that gate them do not \
            transfer. \
            (1) PRIMARY arm — a DESTROY-STORE: \
            `dest` there IS the canonical store, so the delete would take \
            the object database and the worktree administration of every \
            live workweave checkout of that repo with it. Behind the \
            local-only-branch scan AND, in FRONT of the delete, R4 via \
            check_store_unclaimed — no worktree still registered against \
            the store, no ownership receipt still keyed to it. The \
            local-only refusal is NOT relaxed in exchange: it is \
            incidentally what has been keeping this call off a live \
            workweave's store, so recorded rwv refs stay inside its \
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
            at remove_worktree; the remove_dir_all is in the `else`, where \
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
        count: 4,
        callers: &[],
        justification: "(1) CreateRollbackGuard::drop: removes the \
            partially-built workweave of a failed create. \
            (2) CreateRollbackGuard::rollback_and_collect_failures: same \
            intent as (1) but for explicit bail! paths so cleanup failures \
            can be appended to the returned error; defuses Drop to prevent \
            double-rollback. (3) create --replace-existing raw replace: \
            behind the dirty-scan refusal. (4) delete_workweave: behind the \
            dirty refusal unless --discard-uncommitted, and behind the \
            unmerged-commits refusal unless the caller holds a \
            DiscardUnmergedConsent — the token minted only from \
            --discard-unmerged-commits at CLI dispatch. \
            Both list what is lost first. The per-ref DESTROYs inside \
            the same verb each additionally hold their own warrant (R3); \
            this entry covers the directory removal only.",
    },
    Allowed {
        file: "workweave.rs",
        pattern: "symlink::remove(",
        count: 1,
        callers: &[],
        justification: "delete_workweave: unlinks a reference-repo SYMLINK \
            (classify_checkout == ReferenceAlias) before any git call. \
            symlink::remove unlinks the link itself by its Windows type, \
            never following it, so the shared canonical store the symlink \
            aliases is never touched — making explicit the safety the old \
            code only got accidentally (is_lone_canonical + remove_dir_all \
            not following symlinks). No waiver needed: removing a read-only \
            alias destroys no work.",
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
        file: "add_remove.rs",
        pattern: "remove_dir_all",
        count: 1,
        callers: &[],
        justification: "rwv remove --delete on the canonical clone. A \
            DESTROY-STORE: it removes an entire ref \
            store and object database at once, so no ref-level rule can gate \
            it and none is read as permitting it. Gated by \
            refuse_claimed_store, which is R4 — refuses while any live \
            worktree is registered against the store (git worktree list \
            reporting anything beyond the store itself) or while any \
            ownership receipt keyed to it still stands, checked across every \
            project on disk because a clone is shared by path. On top of \
            that: refuses while other projects reference the repo unless \
            --delete-shared-clone. Unreadable worktree registrations refuse \
            rather than being assumed unclaimed — with one measured caveat: \
            that refusal is itself conditioned on is_repo(), which is \
            `rev-parse --git-dir` and returns false on ANY git failure, so a \
            store git cannot be run against at all skips the worktree half. \
            The receipt half still bails on any Err, so the call as a whole \
            stays fail-closed. The \
            verb-level dirty/unpushed preconditions are Q11, narrowed and \
            still open.",
    },
    Allowed {
        file: "git.rs",
        pattern: "\"-D\"",
        count: 1,
        callers: &[],
        justification: "destroy_local_ref, and nothing else — the two \
            unguarded sites this entry used to cover are gone. \
            create_worktree's force-delete-and-retry (whose \"deletes a STALE \
            branch\" claim was measured FALSE) and delete_branch \
            (which independently lost its last caller) were both DELETED \
            along with the rest of the old Vcs surface. The \
            force-delete of a ref rwv holds no receipt for is now \
            unreachable because the code that could do it does not exist. \
            destroy_local_ref: the branch-model DESTROY \
            primitive. Reachable only through \
            Vcs::delete_owned_ref, which takes a persisted receipt \
            (OwnedRef — R2, ownership by record, never by name shape) AND a \
            DeletionWarrant (R3), which is an opaque struct over a private \
            enum whose only constructors RUN the check they certify: \
            unmoved (tip still equals the recorded tip), merged (tip is an \
            ancestor of a named baseline), operator_discarded \
            (--discard-unmerged-commits). The receipt carries the store, so \
            it cannot authorise a delete in a different refdb. Force \
            semantics are correct here: the warrant already established the \
            safety `-d` would re-derive, and `-d` would additionally refuse \
            the operator_discarded case the flag exists to permit.",
    },
    Allowed {
        file: "git.rs",
        pattern: "\"worktree\", \"remove\"",
        count: 1,
        callers: &[("remove_worktree", 5), ("prune_orphan_worktrees_for", 3)],
        justification: "remove_worktree. NOTE the `--force`: git will remove \
            a DIRTY worktree here, so what protects each caller has to be \
            named per caller — the previous blanket \"every caller checks \
            for uncommitted changes and unique commits first\" was measured \
            false for the rollback callers, which check neither. \
            (1) delete_workweave (manifest repos, and the project repo): \
            behind the dirty refusal unless --discard-uncommitted and the \
            unmerged refusal unless DiscardUnmergedConsent (see the \
            workweave.rs/remove_dir_all entry). It also resolves each \
            worktree's actual canonical-store parent \
            (Vcs::resolve_canonical_store) and refuses on \
            no-canonical-store-with-foreign-dependents — the tier-0 \
            topology precondition (joints/clone-topology.md), not \
            bypassable by any waiver. \
            (2) prune_dropped_repo: behind the uncommitted-changes refusal, \
            and on the only arm that reaches remove_worktree also behind the \
            divergence refusal (that arm is the `canonical.exists()` one). \
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
            (5) create --replace-existing's orphan prune: behind that \
            branch's at_risk dirty-walk refusal.",
    },
    Allowed {
        file: "git.rs",
        pattern: "push(\"--force\")",
        count: 1,
        callers: &[],
        justification: "push_ref: force only when the operator passed \
            rwv push --force; lock-freshness and branch preconditions run \
            first. The branch-model form takes the ref to publish as a \
            parameter (PublishRef) instead of reading whatever branch the \
            checkout happens to be on, so the choice belongs to the caller \
            rather than to the VCS impl, and no caller can publish a ref it \
            did not name (Q6 decides what push.rs passes). Its predecessor \
            push_with_role — which read `current_ref` inside the impl — was \
            deleted, so there is no longer a publish path \
            that force-pushes a ref nobody chose.",
    },
    Allowed {
        file: "git.rs",
        pattern: "\"checkout\"",
        count: 4,
        callers: &[("restore_paths_from_head", 2)],
        justification: "The bare `checkout()` that used to head this list is \
            DELETED: fetch and update had already moved to \
            the branch-model primitives below, which classify what HEAD is \
            before writing anything, and with its last \
            caller gone the method went with the rest of the old surface. \
            What remains is four sites, none of which can reposition a \
            checkout without first saying which kind of ref write it is \
            performing. \
            (1) restore_paths_from_head: the one write both restore \
            paths share. Its callers each establish the same precondition \
            before reaching it — refresh_working_tree_to_head_if_safe acts \
            only on a WorkingTreeState::RestorableFromHead, and check's \
            restore_working_tree_to_head is the doctor --fix path, called \
            only after classify_working_tree_drift has proved the same \
            thing. That state is reached only when every on-disk blob is \
            reachable from recent history, so live edits are never \
            clobbered. \
            (2) set_detached_head: the branch-model ATTACH/MOVE primitive \
            for a HEAD that names no branch. No -f, so git's own refusal to \
            overwrite modified paths still applies. Reachable only through \
            detach_head, which requires a DetachConsent minted from \
            --detach-checkouts and re-verifies the attachment witness \
            first, or through advance_detached_head, which is a MOVE of an \
            already-detached HEAD and refuses when the repo is mid-op \
            (including mid-bisect). \
            (3) attach_head_to: reattaches to an EXISTING local branch. \
            Omitting -b does NOT make git refuse an absent branch — it \
            invents one from a remote-tracking ref (checkout.guess), or \
            detaches when the name is a tag's, or treats the name as a \
            pathspec and reverts uncommitted edits to that path, all \
            exiting 0. So the refusal is rwv's: the branch tip is resolved \
            through refs/heads/<name> first and an absent branch is an \
            error before git runs, with --no-guess and the -- terminator as \
            defence in depth for the window after the check. No -f, so \
            git's own refusal to overwrite modified paths still applies. \
            Reachable only through reattach_head, which requires a \
            ReattachConsent minted from --reattach-checkouts and refuses \
            when the observed HEAD state differs from the one the caller \
            planned against. \
            (4) clone_attached_at: the second half of a birth, and it can \
            only ever run against a repo the first half of the same call \
            just created — there is no signature that points it at an \
            existing one. `-B` therefore repositions a ref git minted \
            moments earlier with no working tree hanging off it and no \
            observer, which is what makes it a birth rather than a MOVE \
            (`fetch` (absent clone) is the other birth site). With an explicit \
            start point there is no checkout.guess and no tag lookup, and \
            the -- terminator keeps a path-shaped name out of the pathspec \
            list; the clone runs --no-checkout, so no working tree is ever \
            materialized at the remote tip for this to overwrite.",
    },
    Allowed {
        file: "git.rs",
        pattern: "\"--hard\"",
        count: 2,
        callers: &[("reset_and_drop_savepoint", 4), ("hard_reset", 1)],
        justification: "(1) hard_reset(): the operation's intent is to \
            discard divergent commits. It has NO caller outside the trait \
            any more — the only one is Vcs::reset_attached_ref, so the \
            rewind is now type-gated rather than gated by a check the author \
            remembered to write: reset_attached_ref takes an AttachedRef \
            witness (which a detached or unborn HEAD cannot produce), \
            re-verifies that witness against the repo, requires a \
            DiscardWarrant (which cannot be constructed without a \
            SavepointRef, so a rewind with no recovery path is \
            unrepresentable), and refuses a savepoint taken in a different \
            repo. Its sole production entry point is sync's \
            rewind_project_repo under --discard-local-commits, which \
            resolves the refs/rwv/pre-op savepoint first and refuses if \
            there is none — so discarded commits stay recoverable via \
            `rwv abort`. (The previous text named a \"--force Phase 1'\" \
            caller and a clean-project precondition; the flag is spelled \
            --discard-local-commits and the guard is the warrant.) \
            (2) reset_and_drop_savepoint(): \
            shared helper factored from verified_restore_savepoint(); called \
            from the mid-op, intent, and converged branches — each gated on \
            their respective attributable-tip precondition before the helper \
            is reached (design § 5) — and from the foreign-tip branch under \
            ForeignTipPolicy::Abandon, which is the operator's \
            --abandon-foreign-tip naming that one repo. That fourth branch \
            waives the attributability precondition and no other: it is \
            still gated on the pre-abort ref already resolving to the \
            observed tip, so the commits it moves the branch off remain \
            reachable and the flag consents to abandonment rather than \
            loss. The unverified \
            restore_savepoint() that used to sit between these two — a bare \
            `reset --hard` on the public trait, gated by nothing — was \
            deleted, so verified_restore_savepoint is the only way to reach \
            a savepoint-driven rewind.",
    },
    Allowed {
        file: "git.rs",
        pattern: "\"update-ref\"",
        count: 3,
        callers: &[],
        justification: "(1) savepoint create, (2) savepoint drop, both \
            namespaced under refs/rwv/pre-op/<op-id> (relocated from \
            sync.rs). (3) create_pre_abort_ref(): writes \
            refs/rwv/pre-abort/<op-id> at HEAD before any abort-time \
            restore; information-preserving rail (design § 5) — abort is \
            itself undoable via this ref and the \
            ref is never deleted by abort cleanup. The same line covers \
            both first-write and the first-write-wins-along-divergence \
            advance (under --abandon-foreign-tip, an existing capture that \
            is an ancestor of HEAD is advanced to HEAD; ancestry keeps the \
            earlier capture reachable). None touch user refs.",
    },
    Allowed {
        file: "workspace.rs",
        pattern: "remove_file",
        count: 1,
        callers: &[("clear_active_project", 2)],
        justification: "clear_active_project: the one site that removes the \
            .rwv-active pointer (rwv-internal state). Unconditional here by \
            design — the pointer's owner cannot know which caller's \
            precondition applies, so each caller establishes its own in \
            FRONT of the call, and the entry an auditor reads is this one \
            regardless of how many callers there are. \
            Both current callers are doctor --fix arms. \
            (1) dangling pointer: the named project's directory \
            is gone, so the pointer selects nothing that exists. \
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
        file: "activate.rs",
        pattern: "symlink::remove(",
        count: 3,
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
            link is reconstructable from the report that named it.",
    },
    Allowed {
        file: "activate.rs",
        pattern: "remove_file",
        count: 2,
        callers: &[],
        justification: "deactivate's .rwv-active removal moved to \
            workspace.rs's clear_active_project, audited above; the two \
            symlink unlinks moved behind symlink::remove, audited under \
            that pattern. \
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
            tool's to produce.",
    },
    Allowed {
        file: "op_state.rs",
        pattern: "remove_file",
        count: 2,
        callers: &[],
        justification: "(1) clear_owner: removing the .rwv-op owner record (rwv-internal). \
            (2) clear_lease: removing the .rwv-op-lease thin lease (rwv-internal). \
            Both operate on rwv-internal bookkeeping, never user data. The temp-file \
            cleanup that used to sit here moved to durable_file.rs, audited below.",
    },
    Allowed {
        file: "owned_state.rs",
        pattern: "remove_file",
        count: 1,
        callers: &[],
        justification: "LedgerClaim::drop: releasing the \
            .rwv-owned-digests.lock claim this value's construction created, \
            which is rwv-internal bookkeeping and never user data. The name \
            is not a parameter — it is joined from the claim's own directory \
            and the one constant — so the site cannot be pointed at anything \
            else, and the type has no public constructor, so a value that \
            exists is a claim this process won. Unlinking is the whole \
            release: left behind, it is the wedge every later stamp in that \
            directory refuses on.",
    },
    Allowed {
        file: "durable_file.rs",
        pattern: "remove_file",
        count: 3,
        callers: &[],
        justification: "Temp-file cleanup for the two publish modes, and never \
            able to name the published file itself. (1) staged_temp, when the \
            content write or its fsync failed. (2) replace, when the rename \
            failed. (3) create_new's always-runs cleanup after link(2), which \
            consumes the temp's role whether or not the link took. Every site \
            unlinks only the sibling temp this call created moments earlier, \
            named <file>.tmp.<pid>.<serial>: the pid keeps processes apart and \
            a process-local AtomicU64 keeps threads within one process apart, \
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
/// wrappers are declared on, and `reset_attached_ref`, the sole caller the
/// `--hard` justification names.
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
