//! Weave operations: create, delete, list, and sync weaves.
//!
//! A weave is a parallel working directory containing worktrees for each repo
//! in a project. The weave directory is named `{primary}--{name}` and sits as
//! a sibling of the workspace root (or under `WEAVEROOT` if set).

use crate::git::GitVcs;
use crate::manifest::{Manifest, RepoPath, VcsType, WeaveName};
use crate::vcs::Vcs;
use crate::workspace::{parse_weave_dir_name, weave_dir_name, WorkspaceContext};
use anyhow::bail;
use std::path::{Path, PathBuf};

/// Determine where weave directories live. If `WEAVEROOT` is set, weaves go
/// under that directory; otherwise they are siblings of the workspace root.
fn weave_parent(ws_root: &Path) -> PathBuf {
    if let Ok(wr) = std::env::var("WEAVEROOT") {
        PathBuf::from(wr)
    } else {
        ws_root
            .parent()
            .expect("workspace root should have a parent")
            .to_path_buf()
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

/// Build the ephemeral branch name used by weave worktrees.
fn ephemeral_branch_name(weave_name: &WeaveName, current_branch: &str) -> String {
    format!("{}/{}", weave_name.as_str(), current_branch)
}

/// Create a weave: for each repo in the manifest, create a worktree in the
/// weave directory on an ephemeral branch `{weave_name}/{current_branch}`.
pub fn create_weave(
    ws_root: &Path,
    project: &str,
    name: &WeaveName,
) -> anyhow::Result<()> {
    let manifest = load_manifest(ws_root, project)?;
    let pname = primary_name(ws_root);
    let parent = weave_parent(ws_root);
    let weave_dir = parent.join(weave_dir_name(&pname, name));

    std::fs::create_dir_all(&weave_dir)?;

    let mut errors: Vec<String> = Vec::new();

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

            let worktree_dest = weave_dir.join(repo_path.as_path());

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
            eprintln!("rwv weave create: error: {msg}");
            errors.push(msg);
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        let total = manifest.repositories.len();
        let failed = errors.len();
        bail!(
            "weave create completed with {failed} failure(s) out of {total} repo(s)"
        )
    }
}

/// Delete a weave: remove worktrees and delete the weave directory.
pub fn delete_weave(
    ws_root: &Path,
    project: &str,
    name: &WeaveName,
) -> anyhow::Result<()> {
    let manifest = load_manifest(ws_root, project)?;
    let pname = primary_name(ws_root);
    let parent = weave_parent(ws_root);
    let weave_dir = parent.join(weave_dir_name(&pname, name));

    // Remove worktrees for each repo, collecting errors.
    let mut errors: Vec<String> = Vec::new();

    for (repo_path, entry) in &manifest.repositories {
        let vcs = vcs_for(entry.vcs_type);
        let repo_abs = ws_root.join(repo_path.as_path());
        let worktree_path = weave_dir.join(repo_path.as_path());

        if worktree_path.exists() {
            if let Err(e) = vcs.remove_worktree(&repo_abs, &worktree_path) {
                let msg = format!("{}: {e}", repo_path.as_str());
                eprintln!("rwv weave delete: error: {msg}");
                errors.push(msg);
            }
        }
    }

    // Remove the weave directory itself.
    if weave_dir.exists() {
        std::fs::remove_dir_all(&weave_dir)?;
    }

    if errors.is_empty() {
        Ok(())
    } else {
        let total = manifest.repositories.len();
        let failed = errors.len();
        bail!(
            "weave delete completed with {failed} failure(s) out of {total} repo(s)"
        )
    }
}

/// List weaves: scan for sibling directories matching `{primary}--*`.
pub fn list_weaves(ws_root: &Path) -> anyhow::Result<Vec<String>> {
    let pname = primary_name(ws_root);
    let parent = weave_parent(ws_root);

    let mut names = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&parent) {
        for entry in entries.flatten() {
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            if let Some((primary, weave_name)) = parse_weave_dir_name(&dir_name) {
                if primary == pname {
                    names.push(weave_name.as_str().to_string());
                }
            }
        }
    }

    names.sort();
    Ok(names)
}

/// Sync a weave: add worktrees for repos that are in the manifest but missing
/// from the weave, and report any extra worktrees.
pub fn sync_weave(
    ws_root: &Path,
    project: &str,
    name: &WeaveName,
) -> anyhow::Result<()> {
    let manifest = load_manifest(ws_root, project)?;
    let pname = primary_name(ws_root);
    let parent = weave_parent(ws_root);
    let weave_dir = parent.join(weave_dir_name(&pname, name));

    let mut errors: Vec<String> = Vec::new();

    for (repo_path, entry) in &manifest.repositories {
        let vcs = vcs_for(entry.vcs_type);
        let repo_abs = ws_root.join(repo_path.as_path());
        let worktree_dest = weave_dir.join(repo_path.as_path());

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
            eprintln!("rwv weave sync: error: {msg}");
            errors.push(msg);
        }
    }

    // Report extra worktrees (dirs in weave that aren't in manifest).
    // Walk the weave dir looking for git repos not listed in manifest.
    report_extras(&weave_dir, &manifest)?;

    if errors.is_empty() {
        Ok(())
    } else {
        let total = manifest.repositories.len();
        let failed = errors.len();
        bail!(
            "weave sync completed with {failed} failure(s) out of {total} repo(s)"
        )
    }
}

/// Walk the weave directory and report repos not in the manifest.
fn report_extras(weave_dir: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    let vcs = vcs_for(VcsType::Git);
    walk_for_repos(weave_dir, weave_dir, vcs.as_ref(), manifest)?;
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
