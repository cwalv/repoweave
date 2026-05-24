//! `rwv update` — advance each manifest repo to its branch HEAD and
//! re-snapshot `rwv.lock`.
//!
//! Semantically analogous to `cargo update` or `npm update`: this is the
//! verb that mutates the lock by pulling fresh tips from the network.
//! `rwv fetch` (default) reads the lock; `rwv lock` snapshots local tips;
//! `rwv update` advances and re-snapshots.

use crate::git::{git_command, GitVcs};
use crate::lock;
use crate::manifest::{Project, ProjectName};
use crate::vcs::Vcs;
use crate::workspace::{WorkspaceContext, WorkspaceLocation};
use anyhow::Context;
use std::path::Path;

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
pub fn run_update(
    cwd: &Path,
    dirty: bool,
    commit: bool,
    project_override: Option<ProjectName>,
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
    )
}

/// Internal: do the update for a specific project under `active_root`.
fn update_for_project(
    active_root: &Path,
    primary_root: &Path,
    project_name: &ProjectName,
    workweave: Option<(&crate::manifest::WorkweaveName, &Path)>,
    dirty: bool,
    commit: bool,
    project_override: Option<ProjectName>,
) -> anyhow::Result<()> {
    let project_dir = active_root.join("projects").join(project_name.as_str());
    let project = Project::from_dir(&project_dir)
        .map_err(|e| anyhow::anyhow!("failed to load project '{}': {e}", project_name))?;

    let git = GitVcs;

    // Determine the on-disk root for each repo. In a workweave we look in
    // the workweave directory first, falling back to the primary root —
    // matching the existing convention in `lock::generate_lock`.
    let mut errors: Vec<String> = Vec::new();
    let mut updated = 0usize;

    for (repo_path, entry) in &project.manifest.repositories {
        let repo_dir = if let Some((_, wd)) = workweave {
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
            errors.push(format!(
                "{}: clone missing on disk; run `rwv fetch` first",
                repo_path.as_str()
            ));
            continue;
        }

        let branch = entry.version.as_str();

        // git fetch the remote(s). Run from the repo dir so default remote
        // selection applies.
        println!("rwv update: fetching {}", repo_path.as_str());
        let fetch_out = git_command()
            .args(["fetch", "--all", "--tags"])
            .current_dir(&repo_dir)
            .output();
        let fetch_ok = match fetch_out {
            Ok(o) => {
                if !o.status.success() {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    errors.push(format!(
                        "{}: git fetch failed: {}",
                        repo_path.as_str(),
                        stderr.trim()
                    ));
                    false
                } else {
                    true
                }
            }
            Err(e) => {
                errors.push(format!(
                    "{}: git fetch failed to spawn: {e}",
                    repo_path.as_str()
                ));
                false
            }
        };
        if !fetch_ok {
            continue;
        }

        // Resolve the branch HEAD as it now appears in the local clone
        // (post-fetch). Try `origin/<branch>` first (mirrors what
        // contributors usually want), then `upstream/<branch>` (used for
        // role=fork clones), then a bare branch reference.
        let candidates = [
            format!("origin/{branch}"),
            format!("upstream/{branch}"),
            branch.to_string(),
        ];

        let mut resolved_opt = None;
        for candidate in &candidates {
            if let Ok(resolved) = git.resolve_revision(&repo_dir, candidate) {
                resolved_opt = Some(resolved);
                break;
            }
        }

        let resolved = match resolved_opt {
            Some(r) => r,
            None => {
                errors.push(format!(
                    "{}: could not resolve branch '{branch}' on any remote (tried origin/{branch}, upstream/{branch}, {branch})",
                    repo_path.as_str()
                ));
                continue;
            }
        };

        if let Err(e) = git.checkout(&repo_dir, &resolved) {
            errors.push(format!(
                "{}: failed to checkout {}: {e}",
                repo_path.as_str(),
                resolved
            ));
            continue;
        }

        updated += 1;
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
    let _ = workweave; // suppress unused warning if generate_lock signature changes
    lock::lock(active_root, dirty, commit, project_override)
        .context("failed to write lock after update")?;

    Ok(())
}
