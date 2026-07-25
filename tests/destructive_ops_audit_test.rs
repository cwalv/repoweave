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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One audited (file, pattern) entry: how many call sites are allowed and
/// why each is safe.
struct Allowed {
    /// Path relative to `src/`.
    file: &'static str,
    pattern: &'static str,
    count: usize,
    justification: &'static str,
}

/// Patterns that identify destructive call sites. Substring match against
/// non-comment source lines.
const TRACKED: &[&str] = &[
    "\"--hard\"",               // git reset --hard
    "remove_dir_all",           // recursive directory deletion
    "remove_file",              // file deletion
    "\"-D\"",                   // git branch force-delete
    "\"worktree\", \"remove\"", // git worktree remove
    "push(\"--force\")",        // git push --force
    "\"checkout\"",             // git checkout (worktree overwrite when forced)
    "\"update-ref\"",           // ref surgery
];

/// Patterns that must not appear at all. Each introduces a destruction
/// vector this codebase has no audited use for. If you need one, add it to
/// TRACKED with an allowlist entry instead.
const FORBIDDEN: &[&str] = &[
    "\"clean\"",            // git clean: deletes untracked files
    "\"stash\"",            // stash drop/clear loses work; stash flows hide it
    "\"filter-branch\"",    // history rewrite
    "\"checkout\", \"-f\"", // force-checkout bypasses git's dirty refusal
    "\"reflog\"",           // expire/delete cuts the last recovery path
];

const ALLOWLIST: &[Allowed] = &[
    Allowed {
        file: "sync.rs",
        pattern: "remove_dir_all",
        count: 2,
        justification: "prune_dropped_repo, both arms behind an \
            uncommitted-changes refusal plus a unique-commits refusal \
            (worktree-divergence check in workweaves, local-only-branch \
            scan in primary), all failing safe on git errors. The primary \
            arm is a DESTROY-STORE (branch-model.md §3.2): `dest` there IS \
            the canonical store, so the delete would take the object \
            database and the worktree administration of every live \
            workweave checkout of that repo with it. R4 gates it via \
            check_store_unclaimed — no worktree still registered against \
            the store, no ownership receipt still keyed to it — in FRONT of \
            the delete. The local-only refusal is NOT relaxed in exchange: \
            it is incidentally what has been keeping this call off a live \
            workweave's store, so recorded rwv refs stay inside its \
            predicate (§5, prune_dropped_repo row). The workweave arm's \
            call is not a store destroy: it removes a checkout whose refdb \
            lives in a canonical store that arm has already established is \
            gone.",
    },
    Allowed {
        file: "workweave.rs",
        pattern: "remove_dir_all",
        count: 4,
        justification: "(1) CreateRollbackGuard::drop: removes the \
            partially-built workweave of a failed create. \
            (2) CreateRollbackGuard::rollback_and_collect_failures: same \
            intent as (1) but for explicit bail! paths so cleanup failures \
            can be appended to the returned error; defuses Drop to prevent \
            double-rollback. (3) create --replace-existing raw replace: \
            behind the dirty-scan refusal. (4) delete_workweave: behind the \
            dirty + unmerged-commits refusals unless --discard-uncommitted / \
            --discard-unmerged-commits, which list what is lost first.",
    },
    Allowed {
        file: "workweave.rs",
        pattern: "remove_file",
        count: 1,
        justification: "delete_workweave: unlinks a reference-repo SYMLINK \
            (classify_checkout == ReferenceAlias) before any git call. \
            remove_file removes the link itself, never following it, so the \
            shared canonical store the symlink aliases is never touched — \
            making explicit the safety the old code only got accidentally \
            (is_lone_canonical + remove_dir_all not following symlinks). No \
            waiver needed: removing a read-only alias destroys no work.",
    },
    Allowed {
        file: "add_remove.rs",
        pattern: "remove_dir_all",
        count: 1,
        justification: "rwv remove --delete on the canonical clone; \
            refuses while other projects reference the repo unless \
            --delete-shared-clone.",
    },
    Allowed {
        file: "git.rs",
        pattern: "\"-D\"",
        count: 3,
        justification: "(1) create_worktree retry: deletes a stale \
            ephemeral branch (project--workweave/branch namespace) left by \
            a previous failed create. (2) delete_branch: called only from \
            workweave.rs — delete_workweave, behind its refusals, and the \
            create-rollback guard, which deletes only branches the failed \
            create itself recorded creating. doctor no longer reaches it: \
            its stale-ephemeral --fix now destroys through \
            delete_owned_ref (3) instead, so `doctor --fix` cannot delete a \
            ref rwv holds no receipt for. (3) destroy_local_ref: the branch-model DESTROY \
            primitive (branch-model.md §3.2, §4.3). Reachable only through \
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
        justification: "remove_worktree: every caller (delete_workweave, \
            prune_dropped_repo, create-rollback pruning) checks for \
            uncommitted changes and unique commits first. delete_workweave \
            also resolves each worktree's actual canonical-store parent \
            (Vcs::resolve_canonical_store) and refuses on \
            no-canonical-store-with-foreign-dependents — the tier-0 \
            topology precondition (joints/clone-topology.md), not \
            bypassable by any waiver.",
    },
    Allowed {
        file: "git.rs",
        pattern: "push(\"--force\")",
        count: 2,
        justification: "(1) push_with_role: force only when the operator \
            passed rwv push --force; lock-freshness and branch \
            preconditions run first. (2) push_ref: same flag, same \
            preconditions; the branch-model form takes the ref to publish \
            as a parameter (PublishRef) instead of reading whatever branch \
            the checkout happens to be on, so the choice is made at one \
            site in push.rs rather than inside the VCS impl \
            (branch-model.md §4.3; Q6 decides what that site passes).",
    },
    Allowed {
        file: "git.rs",
        pattern: "\"checkout\"",
        count: 5,
        justification: "(1) checkout(): no -f flag, so git itself refuses \
            when the switch would overwrite a modified path. The verbs that \
            realign a checkout no longer reach it — fetch and update route \
            through the branch-model primitives below, which classify what \
            HEAD is before writing anything (branch-model.md §5) — so its \
            remaining callers are the ones .10 sweeps. \
            (2) refresh_working_tree_to_head_if_safe: \
            restores files from HEAD only after verifying every on-disk \
            blob is reachable from recent history — live edits are never \
            clobbered (relocated from sync.rs). \
            (3) set_detached_head: the branch-model ATTACH/MOVE primitive \
            for a HEAD that names no branch. No -f, so git's own refusal to \
            overwrite modified paths still applies. Reachable only through \
            detach_head, which requires a DetachConsent minted from \
            --detach-checkouts and re-verifies the attachment witness \
            first, or through advance_detached_head, which is a MOVE of an \
            already-detached HEAD and refuses when the repo is mid-op \
            (branch-model.md §3.6 — including mid-bisect). \
            (4) attach_head_to: reattaches to an EXISTING local branch. \
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
            (5) clone_attached_at: the second half of a birth, and it can \
            only ever run against a repo the first half of the same call \
            just created — there is no signature that points it at an \
            existing one. `-B` therefore repositions a ref git minted \
            moments earlier with no working tree hanging off it and no \
            observer, which is what makes it a birth rather than a MOVE \
            (branch-model.md §5, `fetch` (absent clone)). With an explicit \
            start point there is no checkout.guess and no tag lookup, and \
            the -- terminator keeps a path-shaped name out of the pathspec \
            list; the clone runs --no-checkout, so no working tree is ever \
            materialized at the remote tip for this to overwrite.",
    },
    Allowed {
        file: "git.rs",
        pattern: "\"--hard\"",
        count: 3,
        justification: "(1) hard_reset(): the operation's intent is to \
            discard divergent commits; the sole sync caller (--force Phase \
            1') gates on a clean-project precondition and creates a \
            refs/rwv/pre-op savepoint first so discarded commits stay \
            recoverable via `rwv abort`. (2) restore_savepoint(): restoring \
            the pre-op state is the operation's contract; any dirt at \
            abort time is churn from the failed op being rolled back \
            (relocated from sync.rs). (3) reset_and_drop_savepoint(): \
            shared helper factored from verified_restore_savepoint(); called \
            only from the mid-op, intent, and converged branches — each \
            gated on their respective attributable-tip precondition before \
            the helper is reached (design § 5; fo-jsbr3i.4, fo-6rysot.3, \
            fo-wbbqof.9).",
    },
    Allowed {
        file: "git.rs",
        pattern: "\"update-ref\"",
        count: 3,
        justification: "(1) savepoint create, (2) savepoint drop, both \
            namespaced under refs/rwv/pre-op/<op-id> (relocated from \
            sync.rs). (3) create_pre_abort_ref(): writes \
            refs/rwv/pre-abort/<op-id> at HEAD before any abort-time \
            restore; information-preserving rail (design § 5; \
            fo-jsbr3i.4) — abort is itself undoable via this ref and the \
            ref is never deleted by abort cleanup. None touch user refs.",
    },
    Allowed {
        file: "check.rs",
        pattern: "\"checkout\"",
        count: 1,
        justification: "restore_working_tree_to_head: doctor --fix path, \
            called only after classify_working_tree_drift proves every \
            on-disk blob is committed content.",
    },
    Allowed {
        file: "check.rs",
        pattern: "remove_file",
        count: 1,
        justification: "doctor --fix removing a dangling .rwv-active \
            pointer (rwv-internal state, target project missing).",
    },
    Allowed {
        file: "activate.rs",
        pattern: "remove_file",
        count: 2,
        justification: "(1) activation-symlink cleanup: only symlinks that \
            are in the integration-owned set AND resolve into projects/. \
            (2) deactivate removing .rwv-active (rwv-internal state).",
    },
    Allowed {
        file: "op_state.rs",
        pattern: "remove_file",
        count: 4,
        justification: "(1) clear_owner: removing the .rwv-op owner record (rwv-internal). \
            (2) clear_lease: removing the .rwv-op-lease thin lease (rwv-internal). \
            (3)+(4) atomic_write_new temp-file cleanup: unlinks the sibling temp file \
            used to publish op-state atomically via link(2) — both on the write-error \
            path and on the always-runs post-link cleanup. The temp file is created by \
            atomic_write_new itself with a PID+ns-unique name, so nothing else on disk \
            can be named that. All four sites operate on rwv-internal bookkeeping, \
            never user data.",
    },
    Allowed {
        file: "workweave_index.rs",
        pattern: "remove_file",
        count: 2,
        justification: "write_durably temp-file cleanup, on the two error \
            paths (content write failed; rename failed). Unlinks only the \
            sibling temp this call created moments earlier, named \
            <INDEX_FILENAME>.tmp.<pid>.<serial> with a process-local counter \
            — the same structural uniqueness op_state::atomic_write_new \
            uses, so no other writer, thread or process can be holding that \
            name. Never reached on the success path (the rename consumes \
            the temp) and never able to name the index itself. No waiver \
            needed: an orphan temp is scratch, and leaving it behind is the \
            worse outcome — a later reader trips over it.",
    },
    Allowed {
        file: "integrations/merge.rs",
        pattern: "remove_file",
        count: 1,
        justification: "strip_deactivate: marker-gated; file deleted only \
            when semantically empty after stripping rwv-owned keys — \
            user-held files (no marker) are never touched.",
    },
    Allowed {
        file: "integrations/vscode_workspace.rs",
        pattern: "remove_file",
        count: 1,
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
        justification: "strip_workspace_sources: deletes pyproject.toml \
            only when nothing user-authored remains after the marker-gated \
            strip.",
    },
    Allowed {
        file: "integrations/npm_workspaces.rs",
        pattern: "remove_file",
        count: 1,
        justification: "package-lock.json removal on deactivate, gated on \
            rwv's ownership marker in package.json.",
    },
    Allowed {
        file: "integrations/gita.rs",
        pattern: "remove_file",
        count: 1,
        justification: "gita/repos.csv + groups.csv are fully rwv-owned \
            generated files; the directory itself survives if the user \
            added anything to it.",
    },
    Allowed {
        file: "integrations/cargo_workspace.rs",
        pattern: "remove_file",
        count: 1,
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
        justification: "cleanup of rwv's own temporary go.work copy \
            (error path and post-copy); the canonical file is preserved.",
    },
];

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

/// Whether the next item in `rest` (skipping blank and comment lines) is a
/// module declaration — i.e. whether the `#[cfg(test)]` just seen opens a
/// test module rather than gating a single test-only item.
fn next_item_is_a_module(rest: &[&str]) -> bool {
    for line in rest {
        let t = line.trim_start();
        if t.is_empty() || t.starts_with("//") || t.starts_with("#[") {
            continue;
        }
        return t.starts_with("mod ")
            || t.starts_with("pub mod ")
            || t.starts_with("pub(crate) mod ");
    }
    false
}

/// Count pattern hits per (relative file, pattern), skipping comment lines
/// and in-file test modules.
///
/// **Test modules are not call sites.** A `#[cfg(test)] mod` inside `src/`
/// is test code that happens to live next to what it tests, and its
/// fixtures legitimately run `git checkout` / `git branch -D` to build the
/// states the product code is asserted against. Counting them would force
/// every fixture into the allowlist alongside the sites it exists to test,
/// which is noise in exactly the place this file is trying to keep sharp.
///
/// The exclusion relies on the convention this crate follows everywhere: a
/// `#[cfg(test)] mod` is the last item in its file, so scanning stops at
/// the first one. `#[cfg(test)]` on anything that is *not* a module (the
/// test hooks in `integrations/go_work.rs`, for instance) does not stop the
/// scan. Product code placed *after* a test module would go unscanned —
/// don't do that.
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
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("#[cfg(test)]") && next_item_is_a_module(&lines[i + 1..]) {
                break;
            }
            if trimmed.starts_with("//") {
                continue;
            }
            for &pattern in TRACKED.iter().chain(FORBIDDEN) {
                if trimmed.contains(pattern) {
                    *counts.entry((rel.clone(), pattern)).or_insert(0) += 1;
                }
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
