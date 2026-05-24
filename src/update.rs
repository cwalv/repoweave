//! `rwv update` — advance each manifest repo to its branch HEAD and
//! re-snapshot `rwv.lock`.
//!
//! Semantically analogous to `cargo update` or `npm update`: this is the
//! verb that mutates the lock by pulling fresh tips from the network.
//! `rwv fetch` (default) reads the lock; `rwv lock` snapshots local tips;
//! `rwv update` advances and re-snapshots.

use crate::git::{git_command, GitVcs};
use crate::lock;
use crate::manifest::{Project, ProjectName, RepoEntry, RepoPath};
use crate::parallel::{run_in_parallel, run_subprocess_with_reporter, Reporter};
use crate::selector::RepoFilter;
use crate::vcs::{RefName, Vcs};
use crate::workspace::{WorkspaceContext, WorkspaceLocation};
use anyhow::Context;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Run `rwv update` for the current workspace context.
///
/// For each repo in the active project's manifest:
/// 1. `git fetch` the remote.
/// 2. Resolve the branch (`version:` in the manifest) on the remote — this
///    is the canonical "HEAD of the upstream branch" value.
/// 3. Checkout that revision in the local clone.
///
/// After all repos are advanced, regenerate `rwv.lock` from the new tips
/// and write it. The lock-write reuses `lock::generate_lock`, which carries
/// the dirty check; `dirty` here controls whether to bypass it.
///
/// When `commit` is true, the resulting lock is staged and committed in
/// the project repo (same semantics as `rwv lock --commit`).
/// When `project_override` is `Some`, that project is updated instead of
/// the active one (one-shot; does not change `.rwv-active`).
///
/// `jobs` is the resolved worker count (post-[`crate::parallel::resolve_jobs`]).
/// `jobs == 1` runs serially with no prefix; `jobs > 1` runs the per-repo
/// loop on a bounded worker pool, prefixing stdout/stderr lines with the
/// repo path. The lock write happens serially after all workers join.
pub fn run_update(
    cwd: &Path,
    dirty: bool,
    commit: bool,
    project_override: Option<ProjectName>,
    filter: &RepoFilter,
    jobs: usize,
) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, project_override.clone())?;

    let (project_name, workweave_name, workweave_dir) = match &ctx.location {
        WorkspaceLocation::Weave { .. } => {
            let name = ctx.require_active_project()?.clone();
            (name, None, None)
        }
        WorkspaceLocation::Workweave { name, dir, project } => {
            (project.clone(), Some(name.clone()), Some(dir.clone()))
        }
    };

    update_for_project(
        ctx.active_path(),
        ctx.primary_path(),
        &project_name,
        workweave_name.as_ref().zip(workweave_dir.as_deref()),
        dirty,
        commit,
        project_override,
        filter,
        jobs,
    )
}

/// Outcome of advancing a single repo. `Ok(())` means we ran fetch +
/// resolve + checkout cleanly; `Err(msg)` means one of those steps failed
/// and `msg` is the human-readable failure to surface in the aggregated
/// summary.
type RepoOutcome = Result<(), String>;

/// Internal: do the update for a specific project under `active_root`.
#[allow(clippy::too_many_arguments)]
fn update_for_project(
    active_root: &Path,
    primary_root: &Path,
    project_name: &ProjectName,
    workweave: Option<(&crate::manifest::WorkweaveName, &Path)>,
    dirty: bool,
    commit: bool,
    project_override: Option<ProjectName>,
    filter: &RepoFilter,
    jobs: usize,
) -> anyhow::Result<()> {
    let project_dir = active_root.join("projects").join(project_name.as_str());
    let project = Project::from_dir(&project_dir)
        .map_err(|e| anyhow::anyhow!("failed to load project '{}': {e}", project_name))?;

    let git = GitVcs;

    // Snapshot the repo list into a Vec so the parallel loop can index by
    // position. The BTreeMap iteration is deterministic, so the resulting
    // Vec mirrors the previous serial loop's order exactly.
    //
    // Apply the `--role` / `--repo` filter so only selected repos are
    // advanced. Empty filter is a no-op (every repo passes). The post-loop
    // lock re-snapshot below still walks the *full* manifest, so unfiltered
    // repos remain at their previous lock SHAs — see the comment by the
    // `lock::lock` call.
    let work_items: Vec<(RepoPath, RepoEntry)> = project
        .manifest
        .repositories
        .iter()
        .filter(|(rp, entry)| filter.matches(rp, entry.role))
        .map(|(rp, entry)| (rp.clone(), entry.clone()))
        .collect();

    let parallel = jobs > 1;
    let write_lock: Mutex<()> = Mutex::new(());

    let outcomes: Vec<RepoOutcome> = run_in_parallel(&work_items, jobs, |_idx, item| {
        let (repo_path, entry) = item;
        let reporter = if parallel {
            Reporter::parallel(repo_path.as_str().to_string(), &write_lock)
        } else {
            Reporter::serial()
        };
        advance_one(
            &git,
            repo_path,
            entry,
            primary_root,
            workweave.map(|(_, wd)| wd),
            &reporter,
        )
    });

    // Aggregate errors in input order — matches the existing serial shape.
    let mut errors: Vec<String> = Vec::new();
    let mut updated = 0usize;
    for outcome in outcomes {
        match outcome {
            Ok(()) => updated += 1,
            Err(msg) => errors.push(msg),
        }
    }

    if !errors.is_empty() {
        eprintln!("rwv update: {} repo(s) failed to update:", errors.len());
        for msg in &errors {
            eprintln!("  - {msg}");
        }
        anyhow::bail!(
            "update aborted with {} failure(s); lock not written",
            errors.len()
        );
    }

    println!("rwv update: advanced {updated} repo(s)");

    // Re-snapshot the lock to capture the new tips. Delegates to the same
    // `lock::lock` entry point so the commit/dirty handling, hook fire
    // policy, and error surface stay consistent. Pass the same override
    // through so the lock operates on the same project the update did.
    //
    // Critical: this happens AFTER the parallel worker pool has joined.
    // The lock file is shared project-wide state; concurrent writes would
    // race. Keeping it serial post-join is the natural fit for the
    // existing structure.
    //
    // Filter scope: the `--role` / `--repo` filter narrows the *advance*
    // loop above, not the lock snapshot. `lock::lock` walks the full
    // manifest and records HEAD of every repo on disk: filtered repos are
    // at their newly-advanced HEAD; unfiltered repos are at whatever HEAD
    // they were already on. This preserves the invariant that the lock
    // always describes the whole manifest. See fo-9kweo "Open questions"
    // — resolved as "filter narrows the loop, not the lock-shape" — and
    // the parallel decision for push in `src/push.rs`.
    let _ = workweave; // suppress unused warning if generate_lock signature changes
    lock::lock(active_root, dirty, commit, project_override)
        .context("failed to write lock after update")?;

    Ok(())
}

/// Per-repo worker: `git fetch --all --tags`, resolve the role-conventional
/// remote branch, then check out the resolved revision. Returns a flat
/// `Result<(), String>` so the caller can aggregate.
///
/// All user-facing output is routed through `reporter`, which prefixes
/// `[<repo>]` and serialises writes under `-j > 1`; under `-j 1` the
/// reporter is a no-prefix passthrough.
fn advance_one(
    git: &GitVcs,
    repo_path: &RepoPath,
    entry: &RepoEntry,
    primary_root: &Path,
    workweave_dir: Option<&Path>,
    reporter: &Reporter<'_>,
) -> RepoOutcome {
    let repo_dir: PathBuf = if let Some(wd) = workweave_dir {
        let candidate = wd.join(repo_path.as_path());
        if candidate.exists() {
            candidate
        } else {
            primary_root.join(repo_path.as_path())
        }
    } else {
        primary_root.join(repo_path.as_path())
    };

    if !repo_dir.exists() {
        return Err(format!(
            "{}: clone missing on disk; run `rwv fetch` first",
            repo_path.as_str()
        ));
    }

    let branch = entry.version.as_str();

    // git fetch the remote(s). Run from the repo dir so default remote
    // selection applies.
    reporter.out(&format!("rwv update: fetching {}", repo_path.as_str()));
    let mut cmd = git_command();
    cmd.args(["fetch", "--all", "--tags"])
        .current_dir(&repo_dir);
    let outcome = match run_subprocess_with_reporter(&mut cmd, reporter) {
        Ok(o) => o,
        Err(e) => {
            return Err(format!(
                "{}: git fetch failed to spawn: {e}",
                repo_path.as_str()
            ));
        }
    };
    if !outcome.status.success() {
        // Under serial mode `stderr_capture` carries git's stderr; under
        // parallel mode the stderr was already streamed through the
        // reporter, so the captured string is empty and we just say
        // "failed". The streamed lines remain on the terminal for the
        // user to read.
        let suffix = if outcome.stderr_capture.is_empty() {
            "failed".to_string()
        } else {
            format!("failed: {}", outcome.stderr_capture.trim())
        };
        return Err(format!("{}: git fetch {suffix}", repo_path.as_str()));
    }

    // Resolve the branch HEAD on the role-conventional remote. The VCS
    // layer owns the per-role naming policy (see fo-mb2y9), so this is
    // one call rather than a fallback chain. No bare-branch fallback —
    // missing-remote produces a clear error rather than silently
    // resolving to the local branch tip.
    let branch_ref = RefName::new(branch);
    let resolved = match git.resolve_branch_on_remote(&repo_dir, entry.role, &branch_ref) {
        Ok(r) => r,
        Err(e) => {
            return Err(format!(
                "{}: could not resolve branch '{branch}' on role-conventional remote: {e}",
                repo_path.as_str()
            ));
        }
    };

    if let Err(e) = git.checkout(&repo_dir, &resolved) {
        return Err(format!(
            "{}: failed to checkout {}: {e}",
            repo_path.as_str(),
            resolved
        ));
    }

    Ok(())
}
