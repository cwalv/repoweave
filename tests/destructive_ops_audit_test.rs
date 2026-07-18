//! Destructive-operation tripwire.
//!
//! Policy lives in `docs/contributing/destructive-operations.md` —
//! satisfy-the-precondition-or-stop, informed `--force`, discards stay
//! recoverable. Read that before editing the allowlist below; this
//! header carries the enforcement-mechanics summary only.
//!
//! Enforcement: this test inventories every destructive call site in
//! `src/` by scanning for the patterns in `TRACKED` (and refusing the
//! patterns in `FORBIDDEN` outright). Adding, moving, or removing a
//! tracked site fails the build here until the `ALLOWLIST` below is
//! updated with the new count and a justification that names which
//! precondition guards the site, what `--force` consent looks like,
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
            scan in primary), all failing safe on git errors.",
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
            double-rollback. (3) create --force raw replace: behind the \
            dirty-scan refusal. (4) delete_workweave: behind the dirty + \
            unmerged-commits refusals unless --force, which lists what is \
            lost first.",
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
            --force needed: removing a read-only alias destroys no work.",
    },
    Allowed {
        file: "add_remove.rs",
        pattern: "remove_dir_all",
        count: 1,
        justification: "rwv remove --delete on the canonical clone; \
            refuses while other projects reference the repo unless \
            --force.",
    },
    Allowed {
        file: "git.rs",
        pattern: "\"-D\"",
        count: 2,
        justification: "(1) create_worktree retry: deletes a stale \
            ephemeral branch (project--workweave/branch namespace) left by \
            a previous failed create. (2) delete_branch: only called with \
            ephemeral-prefix branch names from delete_workweave, behind \
            its refusals.",
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
            bypassable by --force.",
    },
    Allowed {
        file: "git.rs",
        pattern: "push(\"--force\")",
        count: 1,
        justification: "push_with_role: force only when the operator \
            passed rwv push --force; lock-freshness and branch \
            preconditions run first.",
    },
    Allowed {
        file: "git.rs",
        pattern: "\"checkout\"",
        count: 2,
        justification: "(1) checkout(): no -f flag, so git itself refuses to \
            overwrite dirty trees; callers check out lock-pinned revisions \
            or fresh clones. (2) refresh_working_tree_to_head_if_safe: \
            restores files from HEAD only after verifying every on-disk \
            blob is reachable from recent history — live edits are never \
            clobbered (relocated from sync.rs).",
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
        file: "integrations/merge.rs",
        pattern: "remove_file",
        count: 1,
        justification: "strip_deactivate: marker-gated; file deleted only \
            when semantically empty after stripping rwv-owned keys — \
            user-held files (no marker) are never touched.",
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

/// Count pattern hits per (relative file, pattern), skipping comment lines.
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
        for line in text.lines() {
            let trimmed = line.trim_start();
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
                 audit the new site (named-precondition-or-refuse, or informed \
                 --force) and add an entry with the justification"
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
