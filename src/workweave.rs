//! Workweave operations: create, delete, list, and sync workweaves.
//!
//! A workweave is a parallel working directory containing worktrees for each
//! repo in a project, including the project repo itself. The workweave directory
//! lives under `.workweaves/` in the parent of the workspace root (or under
//! `WEAVEROOT` if set). Each workweave carries its own `.rwv-workweave` marker
//! and `.rwv-active` file so it is fully self-describing.

use crate::git::GitVcs;
use crate::manifest::{Manifest, ProjectName, RepoPath, VcsType, WorkweaveName};
use crate::vcs::Vcs;
use crate::workspace::{parse_weave_dir_name, set_active_project, weave_dir_name, WorkweaveMarker};
use anyhow::bail;
use std::path::{Path, PathBuf};

/// Determine where workweave directories live.
///
/// If `WEAVEROOT` is set, workweaves go under that directory.
/// Otherwise they live under `.workweaves/` in the parent of the workspace root.
fn workweave_parent(ws_root: &Path) -> PathBuf {
    if let Ok(wr) = std::env::var("WEAVEROOT") {
        PathBuf::from(wr)
    } else {
        ws_root
            .parent()
            .expect("workspace root should have a parent")
            .join(".workweaves")
    }
}

/// The primary directory name (last component of workspace root).
fn primary_name(ws_root: &Path) -> String {
    ws_root
        .file_name()
        .expect("workspace root should have a file name")
        .to_str()
        .expect("workspace root name should be valid UTF-8")
        .to_string()
}

/// Resolve a VCS implementation from a `VcsType`.
fn vcs_for(vcs_type: VcsType) -> Box<dyn Vcs> {
    match vcs_type {
        VcsType::Git => Box::new(GitVcs),
    }
}

/// Build the ephemeral branch name used by workweave worktrees.
fn ephemeral_branch_name(workweave_name: &WorkweaveName, current_branch: &str) -> String {
    format!("{}/{}", workweave_name.as_str(), current_branch)
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
/// workweave directory on an ephemeral branch `{workweave_name}/{current_branch}`.
/// Also creates a worktree for the project repo, processes `workweave:` artifacts,
/// writes the marker file, writes `.rwv-active`, and runs activate.
///
/// Returns the absolute path of the created workweave directory.
pub fn create_workweave(
    ws_root: &Path,
    project: &str,
    name: &WorkweaveName,
) -> anyhow::Result<PathBuf> {
    let manifest = load_manifest(ws_root, project)?;
    let pname = primary_name(ws_root);
    let parent = workweave_parent(ws_root);
    let workweave_dir = parent.join(weave_dir_name(&pname, name));

    std::fs::create_dir_all(&workweave_dir)?;

    let mut errors: Vec<String> = Vec::new();

    // Create worktrees for each repo in the manifest.
    for (repo_path, entry) in &manifest.repositories {
        let vcs = vcs_for(entry.vcs_type);
        let repo_abs = ws_root.join(repo_path.as_path());

        let result = (|| -> anyhow::Result<()> {
            // Get the current branch (or fall back to "HEAD").
            let current_branch = vcs
                .current_ref(&repo_abs)?
                .map(|r| r.as_str().to_string())
                .unwrap_or_else(|| "HEAD".to_string());

            let ephemeral_branch = ephemeral_branch_name(name, &current_branch);

            // Get the current HEAD revision as the start point.
            let head = vcs.head_revision(&repo_abs)?;

            let worktree_dest = workweave_dir.join(repo_path.as_path());

            // Ensure parent directories exist.
            if let Some(parent_dir) = worktree_dest.parent() {
                std::fs::create_dir_all(parent_dir)?;
            }

            vcs.create_worktree(
                &repo_abs,
                &worktree_dest,
                &ephemeral_branch,
                head.as_str(),
            )?;

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
        bail!(
            "workweave create completed with {failed} failure(s) out of {total} repo(s)"
        );
    }

    // Create worktree for the project repo (if it is a git repo).
    // If the project directory exists but is not a git repo, copy it into the
    // workweave so that activate_workweave can find rwv.yaml there.
    let project_dir = ws_root.join("projects").join(project);
    let project_wt_dest = workweave_dir.join("projects").join(project);
    if GitVcs.is_repo(&project_dir) {
        let result = (|| -> anyhow::Result<()> {
            let current_branch = GitVcs
                .current_ref(&project_dir)?
                .map(|r| r.as_str().to_string())
                .unwrap_or_else(|| "HEAD".to_string());
            let ephemeral_branch = ephemeral_branch_name(name, &current_branch);
            let head = GitVcs.head_revision(&project_dir)?;
            std::fs::create_dir_all(project_wt_dest.parent().unwrap())?;
            GitVcs.create_worktree(&project_dir, &project_wt_dest, &ephemeral_branch, head.as_str())?;
            Ok(())
        })();

        if let Err(e) = result {
            eprintln!("rwv workweave create: warning: could not create project worktree projects/{project}: {e}");
            // Non-fatal: fall through so we still create the directory copy below
            if !project_wt_dest.exists() {
                if project_dir.exists() {
                    copy_dir_recursive(&project_dir, &project_wt_dest)?;
                }
            }
        }
    } else if project_dir.exists() {
        // Project dir is not a git repo — copy it so activate has access to rwv.yaml.
        copy_dir_recursive(&project_dir, &project_wt_dest)?;
    }

    // Process WorkweaveConfig artifacts.
    if let Some(ref ww_config) = manifest.workweave {
        // Copy entries.
        for entry in &ww_config.copy {
            let source = ws_root.join(entry);
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

        // Link entries — absolute symlinks to primary's canonical paths.
        for entry in &ww_config.link {
            let source = ws_root.join(entry).canonicalize()
                .unwrap_or_else(|_| ws_root.join(entry));
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

    // Write .rwv-workweave marker file.
    let marker = WorkweaveMarker {
        primary: ws_root.to_path_buf(),
        project: ProjectName::new(project),
    };
    marker.write(&workweave_dir)?;

    // Write .rwv-active.
    set_active_project(&workweave_dir, &ProjectName::new(project))?;

    // Run activate in the workweave context.
    crate::activate::activate_workweave(project, &workweave_dir)?;

    Ok(workweave_dir)
}

/// Delete a workweave: remove worktrees (including project repo) and delete the workweave directory.
pub fn delete_workweave(
    ws_root: &Path,
    project: &str,
    name: &WorkweaveName,
) -> anyhow::Result<()> {
    let manifest = load_manifest(ws_root, project)?;
    let pname = primary_name(ws_root);
    let parent = workweave_parent(ws_root);
    let workweave_dir = parent.join(weave_dir_name(&pname, name));

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
            }
        }
    }

    // Remove the project repo worktree (only if the primary project dir is a git repo).
    let project_dir = ws_root.join("projects").join(project);
    let project_worktree = workweave_dir.join("projects").join(project);
    if project_worktree.exists() && GitVcs.is_repo(&project_dir) {
        if let Err(e) = GitVcs.remove_worktree(&project_dir, &project_worktree) {
            let msg = format!("projects/{project}: {e}");
            eprintln!("rwv workweave delete: error: {msg}");
            errors.push(msg);
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
        bail!(
            "workweave delete completed with {failed} failure(s) out of {total} repo(s)"
        )
    }
}

/// List workweaves: scan for directories matching the legacy `{primary}--*` convention.
pub fn list_workweaves(ws_root: &Path) -> anyhow::Result<Vec<String>> {
    let pname = primary_name(ws_root);
    let parent = workweave_parent(ws_root);

    let mut names = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&parent) {
        for entry in entries.flatten() {
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            if let Some((primary, workweave_name)) = parse_weave_dir_name(&dir_name) {
                if primary == pname {
                    names.push(workweave_name.as_str().to_string());
                }
            }
        }
    }

    names.sort();
    Ok(names)
}

/// Sync a workweave: add worktrees for repos that are in the manifest but missing
/// from the workweave, and report any extra worktrees.
pub fn sync_workweave(
    ws_root: &Path,
    project: &str,
    name: &WorkweaveName,
) -> anyhow::Result<()> {
    let manifest = load_manifest(ws_root, project)?;
    let pname = primary_name(ws_root);
    let parent = workweave_parent(ws_root);
    let workweave_dir = parent.join(weave_dir_name(&pname, name));

    let mut errors: Vec<String> = Vec::new();

    for (repo_path, entry) in &manifest.repositories {
        let vcs = vcs_for(entry.vcs_type);
        let repo_abs = ws_root.join(repo_path.as_path());
        let worktree_dest = workweave_dir.join(repo_path.as_path());

        if worktree_dest.exists() {
            continue; // already present
        }

        let result = (|| -> anyhow::Result<()> {
            // Get the current branch (or fall back to "HEAD").
            let current_branch = vcs
                .current_ref(&repo_abs)?
                .map(|r| r.as_str().to_string())
                .unwrap_or_else(|| "HEAD".to_string());

            let ephemeral_branch = ephemeral_branch_name(name, &current_branch);
            let head = vcs.head_revision(&repo_abs)?;

            if let Some(parent_dir) = worktree_dest.parent() {
                std::fs::create_dir_all(parent_dir)?;
            }

            vcs.create_worktree(
                &repo_abs,
                &worktree_dest,
                &ephemeral_branch,
                head.as_str(),
            )?;

            eprintln!("added: {}", repo_path.as_str());
            Ok(())
        })();

        if let Err(e) = result {
            let msg = format!("{}: {e}", repo_path.as_str());
            eprintln!("rwv workweave sync: error: {msg}");
            errors.push(msg);
        }
    }

    // Report extra worktrees (dirs in workweave that aren't in manifest).
    // Walk the workweave dir looking for git repos not listed in manifest.
    report_extras(&workweave_dir, &manifest)?;

    if errors.is_empty() {
        Ok(())
    } else {
        let total = manifest.repositories.len();
        let failed = errors.len();
        bail!(
            "workweave sync completed with {failed} failure(s) out of {total} repo(s)"
        )
    }
}

/// Walk the workweave directory and report repos not in the manifest.
fn report_extras(workweave_dir: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    let vcs = vcs_for(VcsType::Git);
    walk_for_repos(workweave_dir, workweave_dir, vcs.as_ref(), manifest)?;
    Ok(())
}

fn walk_for_repos(
    base: &Path,
    dir: &Path,
    vcs: &dyn Vcs,
    manifest: &Manifest,
) -> anyhow::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }

    // If this directory is a git repo, check if it's in the manifest.
    if vcs.is_repo(dir) {
        if let Ok(rel) = dir.strip_prefix(base) {
            let rel_str = rel.to_string_lossy().to_string();
            let repo_path = RepoPath::new(&rel_str);
            if !manifest.repositories.contains_key(&repo_path) {
                eprintln!("extra: {}", rel_str);
            }
        }
        return Ok(()); // Don't recurse into repos.
    }

    // Recurse into subdirectories.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                walk_for_repos(base, &entry.path(), vcs, manifest)?;
            }
        }
    }

    Ok(())
}

/// Load the project manifest from the workspace.
fn load_manifest(ws_root: &Path, project: &str) -> anyhow::Result<Manifest> {
    let manifest_path = ws_root.join("projects").join(project).join("rwv.yaml");
    Manifest::from_path(&manifest_path)
}
