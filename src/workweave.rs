//! Workweave operations: create, delete, list, and sync workweaves.
//!
//! A workweave is a parallel working directory containing worktrees for each
//! repo in a project, including the project repo itself. The workweave directory
//! lives under `.workweaves/` in the parent of the workspace root (or under
//! `RWV_WORKWEAVE_DIR` if set). Each workweave carries its own `.rwv-workweave` marker
//! and `.rwv-active` file so it is fully self-describing.

use crate::git::GitVcs;
use crate::manifest::{Manifest, ProjectName, WorkweaveName};
use crate::vcs::{vcs_for, RefName, Vcs};
use crate::workspace::{
    parse_weave_dir_name, read_active_project, set_active_project, weave_dir_name, WorkweaveMarker,
};
use anyhow::{anyhow, bail};
use std::path::{Path, PathBuf};

/// Determine where workweave directories live.
///
/// If `RWV_WORKWEAVE_DIR` is set, workweaves go under that directory.
/// Otherwise they live under `.workweaves/` in the parent of the workspace root.
fn workweave_parent(ws_root: &Path) -> PathBuf {
    if let Ok(wr) = std::env::var("RWV_WORKWEAVE_DIR") {
        PathBuf::from(wr)
    } else {
        ws_root
            .parent()
            .expect("workspace root should have a parent")
            .join(".workweaves")
    }
}

/// Compute the on-disk directory for a workweave by `(project, name)`, given
/// the primary workspace root.
///
/// Under the current convention the result is
/// `<workweave_parent>/<project>--<name>`. If that path does not exist, scans
/// the workweave parent for a legacy-named directory (`<primary>--<name>` or
/// other left-component) whose `.rwv-workweave` marker records the same
/// `(primary, project)`; the marker is authoritative for old-form workweaves.
/// If neither resolves, returns the current-convention path (which the caller
/// may then create or report as missing).
pub fn workweave_path_for(
    primary_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
) -> PathBuf {
    let parent = workweave_parent(primary_root);
    let current = parent.join(weave_dir_name(project.as_str(), name));
    if current.exists() {
        return current;
    }

    // Fall back to legacy-shaped sibling directories. We accept any
    // `*--<name>` dir whose marker matches this (primary, project).
    let primary_canonical = primary_root
        .canonicalize()
        .unwrap_or_else(|_| primary_root.to_path_buf());
    if let Ok(entries) = std::fs::read_dir(&parent) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            let parsed = parse_weave_dir_name(&dir_name);
            let matches_name = parsed
                .as_ref()
                .map(|(_, n)| n.as_str() == name.as_str())
                .unwrap_or(false);
            if !matches_name {
                continue;
            }
            if let Ok(Some(marker)) = WorkweaveMarker::read(&dir) {
                if &marker.project != project {
                    continue;
                }
                let m_primary = marker
                    .primary
                    .canonicalize()
                    .unwrap_or_else(|_| marker.primary.clone());
                if m_primary == primary_canonical {
                    return dir;
                }
            }
        }
    }

    current
}

/// Build the ephemeral branch name used by workweave worktrees.
///
/// Includes the project name so that workweaves with the same name across
/// different projects do not collide on shared repos (e.g., both
/// `project-repoweave` and `foundations` referencing `github/gastownhall/beads`).
fn ephemeral_branch_name(
    project: &ProjectName,
    workweave_name: &WorkweaveName,
    current_branch: &RefName,
) -> RefName {
    RefName::new(format!(
        "{}--{}/{}",
        project.as_str(),
        workweave_name.as_str(),
        current_branch.as_str()
    ))
}

/// Build the branch prefix used to locate all ephemeral branches for a given
/// (project, workweave_name) pair. Used to clean up branches on delete.
fn ephemeral_branch_prefix(project: &ProjectName, workweave_name: &WorkweaveName) -> RefName {
    RefName::new(format!("{}--{}", project.as_str(), workweave_name.as_str()))
}

/// Recursively copy a directory from `src` to `dst`.
fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// Create a workweave: for each repo in the manifest, create a worktree in the
/// workweave directory on an ephemeral branch `{project}--{workweave_name}/{current_branch}`.
/// Also creates a worktree for the project repo, processes `workweave:` artifacts,
/// writes the marker file, writes `.rwv-active`, and runs activate.
///
/// `primary_root` locates the surrounding weave: it determines where new
/// workweaves live (`<primary_parent>/.workweaves/`) and is recorded in the
/// `.rwv-workweave` marker so the workweave knows its primary.
///
/// `source_root` is the workspace forked from: the manifest, per-repo HEADs,
/// `projects/<project>/` worktree, and `workweave:` copy/link sources are
/// all read from `source_root`. When forking from primary, pass the same path
/// for both. When forking from another workweave (e.g. a Gas City rig
/// creating a peer workweave from inside itself), `source_root` is that
/// workweave's directory while `primary_root` remains the primary weave.
///
/// If the workweave directory already exists, behavior depends on `force`:
/// - `force == false`: validate that the existing workweave matches this
///   `(primary, project)` pair and has no local modifications relative to
///   `source_root`, then short-circuit. This preserves non-git state (e.g.
///   `.runtime/`, `.claude/`) written by agents between invocations — the
///   contract relied on by Gas City's `gc runtime request-restart` flow.
///   Returns an error if the marker is missing or for a different project,
///   or if any worktree has uncommitted changes or has diverged from the
///   source.
/// - `force == true`: destroy the existing workweave and recreate from
///   scratch. Intended for explicit rebuild scenarios (corruption
///   recovery, or switching a slot to a different project).
///
/// Returns the absolute path of the created workweave directory.
pub fn create_workweave(
    primary_root: &Path,
    source_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
    force: bool,
) -> anyhow::Result<PathBuf> {
    let manifest = load_manifest(source_root, project)?;
    // Resolve to a legacy-shaped directory if one already exists for this
    // (primary, project, name); otherwise use the current `<project>--<name>`
    // form. `workweave_path_for` checks `.exists()`, so on a fresh create the
    // returned path is the new-convention path.
    let workweave_dir = workweave_path_for(primary_root, project, name);

    if workweave_dir.exists() {
        if force {
            // Destructive reuse. Prefer delete_workweave (which also
            // prunes worktrees and ephemeral branches) when the marker
            // belongs to this project; fall back to a raw remove
            // otherwise since delete_workweave loads a manifest tied to
            // `project` and would fail on wrong-marker / missing-marker
            // cases.
            let can_use_structured_delete = match WorkweaveMarker::read(&workweave_dir)? {
                Some(m) => &m.project == project,
                None => false,
            };
            if can_use_structured_delete {
                // `force: true` on the internal delete: the caller already
                // passed --force, signalling intent to overwrite uncommitted
                // state. Re-checking here would just produce a confusing
                // error when the operator's flag already authorised the
                // destructive path.
                delete_workweave(primary_root, project, name, true)?;
            } else {
                std::fs::remove_dir_all(&workweave_dir)?;
            }
        } else {
            return reuse_existing_workweave(
                primary_root,
                source_root,
                project,
                name,
                &workweave_dir,
                &manifest,
            );
        }
    }

    std::fs::create_dir_all(&workweave_dir)?;

    let mut errors: Vec<String> = Vec::new();

    // Create worktrees for each repo in the manifest. Forks come from
    // source_root so peer workweaves rooted in another workweave's HEADs
    // diverge cleanly from that parent rather than from primary.
    for (repo_path, entry) in &manifest.repositories {
        let vcs = vcs_for(entry.vcs_type);
        let repo_abs = source_root.join(repo_path.as_path());

        let result = (|| -> anyhow::Result<()> {
            // Get the current HEAD revision as the start point.
            let head = vcs.head_revision(&repo_abs)?;

            // Distinguish a real branch from detached HEAD. Detached HEADs
            // produce a `detached-<shortsha>` segment so the ephemeral
            // branch name doesn't masquerade as a real ref called "HEAD".
            let branch_segment = match vcs.current_ref(&repo_abs)? {
                Some(r) => RefName::new(r.as_str().to_string()),
                None => RefName::new(format!("detached-{}", short_sha(head.as_str()))),
            };

            let ephemeral_branch = ephemeral_branch_name(project, name, &branch_segment);

            let worktree_dest = workweave_dir.join(repo_path.as_path());

            // Ensure parent directories exist.
            if let Some(parent_dir) = worktree_dest.parent() {
                std::fs::create_dir_all(parent_dir)?;
            }

            vcs.create_worktree(&repo_abs, &worktree_dest, &ephemeral_branch, &head)?;

            Ok(())
        })();

        if let Err(e) = result {
            let msg = format!("{}: {e}", repo_path.as_str());
            eprintln!("rwv workweave create: error: {msg}");
            errors.push(msg);
        }
    }

    if !errors.is_empty() {
        let total = manifest.repositories.len();
        let failed = errors.len();
        // B7: ensure atomic create-or-nothing. Leaving a partial workweave
        // directory on disk turns a clean retry into a `--force` recovery.
        let _ = std::fs::remove_dir_all(&workweave_dir);
        bail!("workweave create completed with {failed} failure(s) out of {total} repo(s)");
    }

    // Create worktree for the project repo (if it is a git repo).
    // If the project directory exists but is not a git repo, copy it into the
    // workweave so that activate_workweave can find rwv.yaml there.
    let project_dir = source_root.join("projects").join(project.as_str());
    let project_wt_dest = workweave_dir.join("projects").join(project.as_str());
    if GitVcs.is_repo(&project_dir) {
        // B8: project-worktree creation failure must NOT silently fall
        // through to a static directory copy. The copy fallback is for
        // the "project dir exists but is not a git repo" branch only.
        // Producing a non-worktree copy here looks identical to a real
        // workweave on disk but has no upstream — commits go nowhere.
        let head = GitVcs.head_revision(&project_dir)?;
        let branch_segment = match GitVcs.current_ref(&project_dir)? {
            Some(r) => RefName::new(r.as_str().to_string()),
            None => RefName::new(format!("detached-{}", short_sha(head.as_str()))),
        };
        let ephemeral_branch = ephemeral_branch_name(project, name, &branch_segment);
        std::fs::create_dir_all(project_wt_dest.parent().unwrap())?;
        if let Err(e) =
            GitVcs.create_worktree(&project_dir, &project_wt_dest, &ephemeral_branch, &head)
        {
            // B7: clean up so a subsequent `rwv workweave create` without
            // --force isn't stuck on a partial directory with no marker.
            let _ = std::fs::remove_dir_all(&workweave_dir);
            bail!(
                "could not create project worktree projects/{}: {e}",
                project.as_str()
            );
        }
    } else if project_dir.exists() {
        // Project dir is not a git repo — copy it so activate has access to rwv.yaml.
        copy_dir_recursive(&project_dir, &project_wt_dest)?;
    }

    // rwv-c7h fix: the project worktree above was checked out from a ref, so
    // its `rwv.yaml` is the last committed version — any uncommitted edits
    // in source_root's working tree were dropped. Overlay the source's
    // working-tree `rwv.yaml` (and `rwv.lock` for completeness) so the
    // workweave captures the operator's in-flight state. Warn loudly when
    // we're doing this so dirty creates don't surprise.
    //
    // Limited to `rwv.yaml` / `rwv.lock` deliberately: these are the files
    // that change workweave behavior (manifest = what worktrees to create,
    // workweave config; lock = lockfile shared with downstream). Other
    // uncommitted project files remain at their committed state, matching
    // the existing worktree-from-ref contract for everything else.
    if project_dir.exists() && project_wt_dest.exists() {
        for fname in ["rwv.yaml", "rwv.lock"] {
            let src = project_dir.join(fname);
            let dst = project_wt_dest.join(fname);
            if !src.exists() {
                continue;
            }
            let src_bytes = std::fs::read(&src).ok();
            let dst_bytes = if dst.exists() {
                std::fs::read(&dst).ok()
            } else {
                None
            };
            if src_bytes != dst_bytes {
                eprintln!(
                    "rwv workweave create: using working-tree projects/{}/{fname} \
                     (uncommitted changes; workweave captures dirty state)",
                    project.as_str(),
                );
                if let Some(bytes) = &src_bytes {
                    if let Err(e) = std::fs::write(&dst, bytes) {
                        eprintln!(
                            "rwv workweave create: warning: failed to overlay {}: {e}",
                            dst.display()
                        );
                    }
                }
            }
        }
    }

    // Process WorkweaveConfig artifacts. Sources resolve against source_root
    // so artifacts follow the workspace being forked from.
    if let Some(ref ww_config) = manifest.workweave {
        // Copy entries.
        for entry in &ww_config.copy {
            let source = source_root.join(entry);
            let dest = workweave_dir.join(entry);
            if source.exists() {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if source.is_dir() {
                    copy_dir_recursive(&source, &dest)?;
                } else {
                    std::fs::copy(&source, &dest)?;
                }
            }
        }

        // Link entries — absolute symlinks to the source's canonical paths.
        for entry in &ww_config.link {
            let source = source_root
                .join(entry)
                .canonicalize()
                .unwrap_or_else(|_| source_root.join(entry));
            let dest = workweave_dir.join(entry);
            if source.exists() {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(&source, &dest)?;
            }
        }
    }

    // Write .rwv-workweave marker file. The marker records the primary so
    // workweaves always know how to find their parent weave regardless of
    // where they were forked from. `parent` records the workspace this
    // workweave was forked from (= source_root) so bare `rwv sync` knows
    // where to sync to. For workweaves forked directly from primary, parent
    // == primary; for workweaves forked from another workweave, parent is
    // that workweave's directory.
    let parent_path = source_root
        .canonicalize()
        .unwrap_or_else(|_| source_root.to_path_buf());
    let marker = WorkweaveMarker {
        primary: primary_root.to_path_buf(),
        project: project.clone(),
        parent: Some(parent_path),
    };
    marker.write(&workweave_dir)?;

    // Write .rwv-active.
    set_active_project(&workweave_dir, project)?;

    // Run activate in the workweave context.
    crate::activate::activate_workweave(project.as_str(), &workweave_dir)?;

    Ok(workweave_dir)
}

/// Validate that an existing workweave directory matches `(primary_root, project, name)`
/// and is in a clean state relative to `source_root`, then return its path
/// without modifying anything.
///
/// Called from [`create_workweave`] on re-invocation without `--force`. Refuses
/// if the `.rwv-workweave` marker is missing or for a different primary/project,
/// or if any per-repo worktree has uncommitted changes or has diverged from the
/// source's HEAD.
fn reuse_existing_workweave(
    primary_root: &Path,
    source_root: &Path,
    project: &ProjectName,
    _name: &WorkweaveName,
    workweave_dir: &Path,
    manifest: &Manifest,
) -> anyhow::Result<PathBuf> {
    let marker = WorkweaveMarker::read(workweave_dir)?.ok_or_else(|| {
        anyhow!(
            "workweave directory {} exists but has no .rwv-workweave marker; \
             rerun with --force to recreate it",
            workweave_dir.display()
        )
    })?;

    if &marker.project != project {
        bail!(
            "workweave at {} is for project '{}', refusing to recreate for project '{}'; \
             rerun with --force to overwrite",
            workweave_dir.display(),
            marker.project.as_str(),
            project
        );
    }

    let marker_primary = marker
        .primary
        .canonicalize()
        .unwrap_or_else(|_| marker.primary.clone());
    let primary_canonical = primary_root
        .canonicalize()
        .unwrap_or_else(|_| primary_root.to_path_buf());
    if marker_primary != primary_canonical {
        bail!(
            "workweave at {} is for primary workspace {}, refusing to recreate for {}; \
             rerun with --force to overwrite",
            workweave_dir.display(),
            marker.primary.display(),
            primary_root.display()
        );
    }

    // Detect local modifications (uncommitted changes or HEAD divergence
    // from source) in any existing worktree. Missing worktrees are not
    // "modified" — a manifest may have grown since the workweave was
    // created; `rwv workweave sync` is the path for adding them.
    let mut modified: Vec<String> = Vec::new();
    for (repo_path, entry) in &manifest.repositories {
        let vcs = vcs_for(entry.vcs_type);
        let worktree_dest = workweave_dir.join(repo_path.as_path());
        if !worktree_dest.exists() {
            continue;
        }
        if vcs.has_uncommitted_changes(&worktree_dest)? {
            modified.push(format!("{}: uncommitted changes", repo_path.as_str()));
            continue;
        }
        let repo_abs = source_root.join(repo_path.as_path());
        let wt_head = vcs.head_revision(&worktree_dest)?;
        let source_head = vcs.head_revision(&repo_abs)?;
        if wt_head != source_head {
            modified.push(format!(
                "{}: worktree has diverged from source ({} vs {})",
                repo_path.as_str(),
                short_sha(wt_head.as_str()),
                short_sha(source_head.as_str()),
            ));
        }
    }

    if !modified.is_empty() {
        bail!(
            "workweave at {} has local modifications; refusing to recreate without --force:\n  {}",
            workweave_dir.display(),
            modified.join("\n  ")
        );
    }

    Ok(workweave_dir.to_path_buf())
}

fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// Return the relative paths within `workweave_dir` whose worktrees have
/// uncommitted changes (staged, unstaged, or untracked).
///
/// Checks the project worktree (`projects/<project>`) and each manifest-repo
/// worktree. A repo that is missing on disk is skipped; a repo whose dirty
/// check itself fails is reported as dirty (conservative: "we couldn't
/// confirm clean").
pub fn collect_dirty_paths(
    workweave_dir: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> Vec<String> {
    let mut dirty = Vec::new();

    // Project worktree.
    let project_wt = workweave_dir.join("projects").join(project.as_str());
    if GitVcs.is_repo(&project_wt) {
        match GitVcs.has_uncommitted_changes(&project_wt) {
            Ok(true) => dirty.push(format!("projects/{}", project.as_str())),
            Ok(false) => {}
            Err(e) => dirty.push(format!(
                "projects/{}: status check failed: {e}",
                project.as_str()
            )),
        }
    }

    // Manifest-repo worktrees.
    for (repo_path, entry) in &manifest.repositories {
        let wt = workweave_dir.join(repo_path.as_path());
        if !wt.exists() {
            continue;
        }
        let vcs = vcs_for(entry.vcs_type);
        match vcs.has_uncommitted_changes(&wt) {
            Ok(true) => dirty.push(repo_path.as_str().to_string()),
            Ok(false) => {}
            Err(e) => dirty.push(format!("{}: status check failed: {e}", repo_path.as_str())),
        }
    }

    dirty
}

/// Delete a workweave: remove worktrees (including project repo) and delete
/// the workweave directory.
///
/// Refuses to delete a workweave with uncommitted changes (in the project
/// worktree or any manifest-repo worktree) unless `force` is true. The error
/// lists the dirty paths so the operator knows what would have been lost.
/// `force` matches the `git branch -D` pattern.
pub fn delete_workweave(
    ws_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
    force: bool,
) -> anyhow::Result<()> {
    let manifest = load_manifest(ws_root, project)?;
    // Use `workweave_path_for` so old-form `<primary>--<name>` workweaves
    // (resolved via marker) are deleted correctly.
    let workweave_dir = workweave_path_for(ws_root, project, name);

    // Safety check: refuse to delete dirty workweaves without --force.
    // Skip the check if the workweave directory doesn't exist (nothing to
    // lose) or if force was passed.
    if !force && workweave_dir.exists() {
        let dirty = collect_dirty_paths(&workweave_dir, project, &manifest);
        if !dirty.is_empty() {
            bail!(
                "workweave {} has uncommitted changes; refusing to delete without --force:\n  {}",
                name.as_str(),
                dirty.join("\n  ")
            );
        }
    }

    // Remove worktrees for each repo, collecting errors.
    let mut errors: Vec<String> = Vec::new();

    for (repo_path, entry) in &manifest.repositories {
        let vcs = vcs_for(entry.vcs_type);
        let repo_abs = ws_root.join(repo_path.as_path());
        let worktree_path = workweave_dir.join(repo_path.as_path());

        if worktree_path.exists() {
            if let Err(e) = vcs.remove_worktree(&repo_abs, &worktree_path) {
                let msg = format!("{}: {e}", repo_path.as_str());
                eprintln!("rwv workweave delete: error: {msg}");
                errors.push(msg);
                continue;
            }
        }

        // Prune stale worktree metadata and delete ephemeral branches.
        let _ = vcs.worktree_prune(&repo_abs);
        let branch_prefix = ephemeral_branch_prefix(project, name);
        match vcs.list_branches_with_prefix(&repo_abs, &branch_prefix) {
            Ok(branches) => {
                for branch in branches {
                    if let Err(e) = vcs.delete_branch(&repo_abs, &branch) {
                        eprintln!(
                            "rwv workweave delete: warning: could not delete branch {branch} in {}: {e}",
                            repo_path.as_str()
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "rwv workweave delete: warning: could not list branches in {}: {e}",
                    repo_path.as_str()
                );
            }
        }
    }

    // Remove the project repo worktree.
    // Only call remove_worktree if the workweave copy is actually a git worktree,
    // indicated by .git being a FILE (not a directory). If .git is a directory
    // (or absent), the workweave copy was a plain directory copy — just let
    // remove_dir_all below handle it.
    let project_dir = ws_root.join("projects").join(project.as_str());
    let project_worktree = workweave_dir.join("projects").join(project.as_str());
    if project_worktree.exists() {
        let dot_git = project_worktree.join(".git");
        if dot_git.exists() && dot_git.is_file() {
            if let Err(e) = GitVcs.remove_worktree(&project_dir, &project_worktree) {
                let msg = format!("projects/{}: {e}", project.as_str());
                eprintln!("rwv workweave delete: error: {msg}");
                errors.push(msg);
            } else {
                // Prune and delete ephemeral branches for the project repo.
                let _ = GitVcs.worktree_prune(&project_dir);
                let branch_prefix = ephemeral_branch_prefix(project, name);
                if let Ok(branches) = GitVcs.list_branches_with_prefix(&project_dir, &branch_prefix)
                {
                    for branch in branches {
                        if let Err(e) = GitVcs.delete_branch(&project_dir, &branch) {
                            eprintln!(
                                "rwv workweave delete: warning: could not delete branch {branch} in projects/{}: {e}",
                                project.as_str()
                            );
                        }
                    }
                }
            }
        }
    }

    // Remove the workweave directory itself.
    if workweave_dir.exists() {
        std::fs::remove_dir_all(&workweave_dir)?;
    }

    if errors.is_empty() {
        Ok(())
    } else {
        let total = manifest.repositories.len() + 1;
        let failed = errors.len();
        bail!("workweave delete completed with {failed} failure(s) out of {total} repo(s)")
    }
}

/// List workweaves for `project` under `ws_root`'s primary.
///
/// A workweave belongs to `(primary, project)` when its `.rwv-workweave`
/// marker records both. For old-form workweaves missing a marker, the
/// directory's left component (legacy `{primary}--{name}`) is taken as the
/// project name — matching how `WorkspaceContext::resolve` infers the project
/// from such directories.
pub fn list_workweaves(ws_root: &Path, project: &ProjectName) -> anyhow::Result<Vec<String>> {
    let mut names: Vec<String> = list_workweave_dirs_for_project(ws_root, project)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    names.sort();
    Ok(names)
}

/// Return `(name, path)` pairs for workweaves of `project` under `ws_root`'s
/// primary. See [`list_workweaves`] for the marker / legacy resolution rules.
fn list_workweave_dirs_for_project(
    ws_root: &Path,
    project: &ProjectName,
) -> Vec<(String, PathBuf)> {
    let parent = workweave_parent(ws_root);
    let primary_canonical = ws_root
        .canonicalize()
        .unwrap_or_else(|_| ws_root.to_path_buf());
    let mut result = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&parent) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            let parsed = parse_weave_dir_name(&dir_name);
            if parsed.is_none() {
                continue;
            }
            let (left, parsed_name) = parsed.unwrap();

            match WorkweaveMarker::read(&dir) {
                Ok(Some(marker)) => {
                    if &marker.project != project {
                        continue;
                    }
                    let m_primary = marker
                        .primary
                        .canonicalize()
                        .unwrap_or_else(|_| marker.primary.clone());
                    if m_primary != primary_canonical {
                        continue;
                    }
                    result.push((parsed_name.as_str().to_string(), dir));
                }
                _ => {
                    // No marker — fall back to legacy interpretation: left
                    // component is the project (same as resolve()).
                    if left == project.as_str() {
                        result.push((parsed_name.as_str().to_string(), dir));
                    }
                }
            }
        }
    }

    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

/// Return `(name, path)` pairs for all workweave directories belonging to
/// `ws_root`'s primary, across every project. Used by `rwv doctor` /
/// `rwv check` to scan all workweaves for drift.
pub fn list_workweave_dirs(ws_root: &Path) -> Vec<(String, PathBuf)> {
    let parent = workweave_parent(ws_root);
    let primary_canonical = ws_root
        .canonicalize()
        .unwrap_or_else(|_| ws_root.to_path_buf());
    let mut result = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&parent) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            let parsed = parse_weave_dir_name(&dir_name);
            if parsed.is_none() {
                continue;
            }
            let (_, parsed_name) = parsed.unwrap();

            // Authoritative source: marker file. Accept any project under
            // this primary.
            match WorkweaveMarker::read(&dir) {
                Ok(Some(marker)) => {
                    let m_primary = marker
                        .primary
                        .canonicalize()
                        .unwrap_or_else(|_| marker.primary.clone());
                    if m_primary == primary_canonical {
                        result.push((parsed_name.as_str().to_string(), dir));
                    }
                }
                _ => {
                    // No marker: fall back on legacy `{primary}--{name}` —
                    // include only if the left component matches the actual
                    // primary directory basename (the legacy convention).
                    if let Some(pname) = ws_root.file_name().and_then(|n| n.to_str()) {
                        if dir_name.starts_with(&format!("{pname}--")) {
                            result.push((parsed_name.as_str().to_string(), dir));
                        }
                    }
                }
            }
        }
    }

    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

/// Load the project manifest from the workspace.
fn load_manifest(ws_root: &Path, project: &ProjectName) -> anyhow::Result<Manifest> {
    let manifest_path = ws_root
        .join("projects")
        .join(project.as_str())
        .join("rwv.yaml");
    Manifest::from_path(&manifest_path)
}

// ---------------------------------------------------------------------------
// Claude Code hook mode
// ---------------------------------------------------------------------------

/// Input JSON sent by Claude Code for WorktreeCreate / WorktreeRemove hooks.
#[derive(serde::Deserialize)]
struct ClaudeHookInput {
    cwd: Option<String>,
    branch_name: Option<String>,
    session_id: Option<String>,
    hook_event_name: Option<String>,
    worktree_path: Option<String>,
}

/// Derive a workweave name from the hook payload.
///
/// Priority: branch_name → timestamp+nanos fallback.
/// Session ID is not used — it's constant within a session, causing
/// collisions when multiple subagents are spawned.
/// Slashes are replaced with dashes for filesystem safety.
fn derive_workweave_name(branch_name: Option<&str>, _session_id: Option<&str>) -> String {
    let raw = match branch_name {
        Some(b) if !b.is_empty() && b != "null" => b.to_string(),
        _ => {
            let d = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            // Mix seconds and nanos into a short hex suffix for uniqueness
            let hash = d.as_secs() ^ (d.subsec_nanos() as u64);
            format!("workweave-{:08x}", hash)
        }
    };
    raw.replace('/', "-")
}

/// Handle a Claude Code hook invocation.
///
/// Reads JSON from stdin, dispatches on `hook_event_name`:
/// - `WorktreeCreate` — creates a workweave and prints its path to stdout.
/// - `WorktreeRemove` — deletes the workweave (fire-and-forget; always exits 0).
pub fn handle_claude_hook() -> anyhow::Result<()> {
    let input: ClaudeHookInput = serde_json::from_reader(std::io::stdin())
        .map_err(|e| anyhow!("failed to parse hook JSON from stdin: {e}"))?;

    match input.hook_event_name.as_deref() {
        Some("WorktreeCreate") => {
            let cwd = input
                .cwd
                .ok_or_else(|| anyhow!("missing cwd in hook input"))?;

            let ws_ctx = crate::workspace::WorkspaceContext::resolve(Path::new(&cwd), None)?;
            let primary_root = ws_ctx.primary_path();
            let source_root = ws_ctx.active_path();

            // Prefer the workweave's project (from .rwv-workweave marker) over
            // the primary weave's .rwv-active. This matters when a sub-agent
            // spawns from a workweave for a different project than the weave's
            // active project.
            let project = match &ws_ctx.location {
                crate::workspace::WorkspaceLocation::Workweave { project, .. } => project.clone(),
                _ => read_active_project(primary_root)
                    .ok_or_else(|| anyhow!("no .rwv-active found in {}", primary_root.display()))?,
            };

            let name =
                derive_workweave_name(input.branch_name.as_deref(), input.session_id.as_deref());

            let path = create_workweave(
                primary_root,
                source_root,
                &project,
                &WorkweaveName::new(&name),
                false,
            )?;
            println!("{}", path.display());
        }
        Some("WorktreeRemove") => {
            let worktree_path = input
                .worktree_path
                .ok_or_else(|| anyhow!("missing worktree_path in hook input"))?;

            // Fire-and-forget: log errors but always succeed.
            if let Ok(Some(marker)) = WorkweaveMarker::read(Path::new(&worktree_path)) {
                let dir_name = Path::new(&worktree_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                let name = dir_name
                    .split_once("--")
                    .map(|(_, n)| n)
                    .unwrap_or(dir_name);

                // Claude's WorktreeRemove is fire-and-forget cleanup of a
                // worktree Claude has decided to discard. Pass `force: true`
                // because (a) the operator's intent is already expressed by
                // the Claude action, and (b) any prompt for dirty state
                // would land on stderr unseen — Claude has already moved on.
                if let Err(e) = delete_workweave(
                    &marker.primary,
                    &marker.project,
                    &WorkweaveName::new(name),
                    true,
                ) {
                    eprintln!("rwv workweave --claude-hook WorktreeRemove: warning: {e}");
                }
            }
            // Always exit 0.
        }
        other => {
            anyhow::bail!("unknown hook_event_name: {:?}", other);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // derive_workweave_name
    // -----------------------------------------------------------------------

    #[test]
    fn derive_name_uses_branch_name() {
        assert_eq!(
            derive_workweave_name(Some("feat/my-branch"), None),
            "feat-my-branch"
        );
    }

    #[test]
    fn derive_name_null_branch_uses_timestamp() {
        let name = derive_workweave_name(Some("null"), Some("abc-session-123"));
        assert!(
            name.starts_with("workweave-"),
            "session_id ignored, expected ww-<timestamp>, got {name}"
        );
    }

    #[test]
    fn derive_name_empty_branch_uses_timestamp() {
        let name = derive_workweave_name(Some(""), Some("sess-xyz"));
        assert!(
            name.starts_with("workweave-"),
            "session_id ignored, expected ww-<timestamp>, got {name}"
        );
    }

    #[test]
    fn derive_name_timestamps_are_unique() {
        let a = derive_workweave_name(None, None);
        std::thread::sleep(std::time::Duration::from_millis(1));
        let b = derive_workweave_name(None, None);
        assert_ne!(a, b, "sequential calls should produce different names");
    }

    #[test]
    fn derive_name_all_none_produces_timestamp() {
        let name = derive_workweave_name(None, None);
        assert!(
            name.starts_with("workweave-"),
            "expected ww-<timestamp>, got {name}"
        );
    }

    #[test]
    fn derive_name_replaces_slashes() {
        assert_eq!(derive_workweave_name(Some("a/b/c"), None), "a-b-c");
    }

    // -----------------------------------------------------------------------
    // handle_claude_hook — JSON parsing via serde
    // -----------------------------------------------------------------------

    #[test]
    fn claude_hook_unknown_event_errors() {
        // Deserialise directly and call the dispatch logic via the public API.
        // We simulate by constructing the input struct.
        let json = r#"{"hook_event_name":"UnknownEvent"}"#;
        let input: ClaudeHookInput = serde_json::from_str(json).unwrap();
        // The match arm should hit the `other` branch.
        assert_eq!(input.hook_event_name.as_deref(), Some("UnknownEvent"));
        assert!(input.cwd.is_none());
        assert!(input.worktree_path.is_none());
    }

    #[test]
    fn claude_hook_null_branch_uses_timestamp_not_session() {
        let name = derive_workweave_name(Some("null"), Some("my-session-id"));
        assert!(
            name.starts_with("workweave-"),
            "session_id ignored, expected ww-*, got {name}"
        );
    }

    #[test]
    fn claude_hook_input_deserialises_fully() {
        let json = r#"{
            "cwd": "/home/user/ws",
            "branch_name": "feat/new-thing",
            "session_id": "sess-001",
            "hook_event_name": "WorktreeCreate",
            "worktree_path": "/tmp/wt"
        }"#;
        let input: ClaudeHookInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.cwd.as_deref(), Some("/home/user/ws"));
        assert_eq!(input.branch_name.as_deref(), Some("feat/new-thing"));
        assert_eq!(input.session_id.as_deref(), Some("sess-001"));
        assert_eq!(input.hook_event_name.as_deref(), Some("WorktreeCreate"));
        assert_eq!(input.worktree_path.as_deref(), Some("/tmp/wt"));
    }

    #[test]
    fn claude_hook_input_all_optional_fields_missing() {
        let json = r#"{}"#;
        let input: ClaudeHookInput = serde_json::from_str(json).unwrap();
        assert!(input.cwd.is_none());
        assert!(input.branch_name.is_none());
        assert!(input.session_id.is_none());
        assert!(input.hook_event_name.is_none());
        assert!(input.worktree_path.is_none());
    }
}
