//! Activate and deactivate projects.
//!
//! `rwv activate PROJECT` sets the active project by:
//! 1. Running integrations with `output_dir = project_dir` and
//!    `workspace_root = root`, collecting `generated_files` from each.
//! 2. Removing old symlinks (from a previous activation).
//! 3. Creating new symlinks at the workspace root pointing to generated files
//!    in the project directory.
//! 4. Writing `.rwv-active`.
//!
//! `deactivate` removes the symlinks and `.rwv-active`.

use std::path::Path;

use crate::git::GitVcs;
use crate::integration::{is_enabled, Integration, IntegrationContext, Severity};
use crate::integration_runner::{run_activations, IntegrationContextBase};
use crate::integrations::builtin_integrations;
use crate::manifest::{IntegrationConfig, Manifest, ProjectName};
use crate::registry::builtin_registries;
use crate::workspace::{scan_repos_on_disk, set_active_project, WorkspaceContext};

/// Run `rwv activate PROJECT` from the given working directory.
pub fn activate(project: &str, cwd: &Path) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let root = &ctx.root;

    let project_name = ProjectName::new(project);
    let project_dir = root.join("projects").join(project);
    let manifest_path = project_dir.join("rwv.yaml");
    let manifest = Manifest::from_path(&manifest_path)?;

    // Discover repos on disk and project paths (needed by IntegrationContext).
    let registries = builtin_registries();
    let git = GitVcs;
    let all_repos_on_disk = scan_repos_on_disk(root, &registries, &git);
    let all_project_paths = discover_project_paths(root);

    let builtin = builtin_integrations();
    let integrations: Vec<&dyn Integration> = builtin.iter().map(|b| b.as_ref()).collect();

    // 1. Run integrations with output_dir = project_dir.
    let ctx_base = IntegrationContextBase {
        output_dir: &project_dir,
        workspace_root: root,
        project: &project_name,
        all_repos_on_disk: &all_repos_on_disk,
        all_project_paths: &all_project_paths,
    };

    let issues = run_activations(&integrations, &manifest, &ctx_base);

    // Report errors but don't abort — partial activation is better than none.
    for issue in &issues {
        let prefix = match issue.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        eprintln!("[{prefix}] {}: {}", issue.integration, issue.message);
    }

    // 2. Collect generated files from all enabled integrations.
    let default_config = IntegrationConfig::default();
    let mut new_generated: Vec<String> = Vec::new();

    for integration in &integrations {
        let config = manifest
            .integrations
            .get(integration.name())
            .unwrap_or(&default_config);

        if !is_enabled(*integration, config) {
            continue;
        }

        let int_ctx = IntegrationContext {
            output_dir: &project_dir,
            workspace_root: root,
            project: &project_name,
            repos: &manifest.repositories,
            config,
            all_repos_on_disk: &all_repos_on_disk,
            all_project_paths: &all_project_paths,
        };

        new_generated.extend(integration.generated_files(&int_ctx));
    }

    // 3. Remove old symlinks from a previous activation.
    //    We check every file at the workspace root that is a symlink whose
    //    target points into `projects/`. This avoids needing to remember
    //    exactly which files were created by the previous project.
    remove_activation_symlinks(root)?;

    // 4. Create new symlinks at workspace root pointing to project_dir files.
    //    Failures are collected as warnings so that partial symlink creation
    //    does not prevent .rwv-active from being written.
    for file in &new_generated {
        let source = project_dir.join(file);
        let link = root.join(file);

        // Create symlink even if the target doesn't exist yet — lock files
        // (Cargo.lock, package-lock.json, etc.) are populated by ecosystem
        // tools on first build/install, writing through the dangling symlink.

        // Ensure parent directory exists for nested files (e.g., gita/repos.csv).
        if let Some(parent) = link.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    eprintln!(
                        "[warning] symlink: failed to create parent directory {}: {e}",
                        parent.display()
                    );
                    continue;
                }
            }
        }

        // Compute a relative symlink target from the link location to the
        // source in the project directory. For top-level files this is just
        // `projects/{project}/{file}`. For nested files like `gita/repos.csv`
        // we need to prepend `../` for each directory level.
        let file_path = Path::new(file);
        let depth = file_path.parent().map(|p| p.components().count()).unwrap_or(0);
        let mut relative_target = std::path::PathBuf::new();
        for _ in 0..depth {
            relative_target.push("..");
        }
        relative_target.push("projects");
        relative_target.push(project);
        relative_target.push(file);

        #[cfg(unix)]
        if let Err(e) = std::os::unix::fs::symlink(&relative_target, &link) {
            eprintln!(
                "[warning] symlink: failed to create {} -> {}: {e}",
                link.display(),
                relative_target.display()
            );
        }

        #[cfg(windows)]
        if let Err(e) = std::os::windows::fs::symlink_file(&relative_target, &link) {
            eprintln!(
                "[warning] symlink: failed to create {} -> {}: {e}",
                link.display(),
                relative_target.display()
            );
        }
    }

    // 5. Write .rwv-active.
    set_active_project(root, &project_name)?;

    Ok(())
}

/// Remove activation symlinks at the workspace root (recursively).
///
/// A symlink is considered an activation symlink if its target (resolved
/// relative to its location) starts with `projects/`. Directories that were
/// created solely to hold nested symlinks are cleaned up if they become empty.
fn remove_activation_symlinks(root: &Path) -> anyhow::Result<()> {
    remove_activation_symlinks_in(root, root)
}

fn remove_activation_symlinks_in(dir: &Path, root: &Path) -> anyhow::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match path.symlink_metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        if meta.file_type().is_symlink() {
            if let Ok(target) = std::fs::read_link(&path) {
                // An activation symlink's target always passes through the
                // `projects/` directory. For top-level files the target is
                // `projects/{proj}/{file}`; for nested files it's
                // `../projects/{proj}/{dir}/{file}`. We check whether any
                // component of the target path is `projects`.
                let is_activation = target
                    .components()
                    .any(|c| c.as_os_str() == "projects");
                if is_activation {
                    std::fs::remove_file(&path)?;
                }
            }
        } else if meta.file_type().is_dir() {
            // Skip well-known workspace directories to avoid unnecessary recursion.
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name == "projects" || name == "github" || name == "gitlab"
                    || name == "bitbucket" || name == ".git"
                {
                    continue;
                }
            }
            remove_activation_symlinks_in(&path, root)?;

            // Clean up empty directories that we may have created.
            if dir != root {
                let _ = std::fs::remove_dir(dir); // ignore error if not empty
            }
        }
    }

    // If we're in a subdirectory, try to remove it if empty.
    if dir != root {
        let _ = std::fs::remove_dir(dir);
    }

    Ok(())
}

/// Discover all project names under `projects/`.
fn discover_project_paths(root: &Path) -> Vec<String> {
    let projects_dir = root.join("projects");
    let mut names = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&projects_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    names.sort();
    names
}

/// Run activation in a workweave directory without calling resolve.
///
/// This is used by `create_workweave` after the workweave is fully set up.
/// Unlike `activate`, it does not call `WorkspaceContext::resolve` (which would
/// return the primary root via the `.rwv-workweave` marker). Instead it works
/// directly against the workweave directory.
pub fn activate_workweave(project: &str, workweave_dir: &Path) -> anyhow::Result<()> {
    let project_name = ProjectName::new(project);
    let project_dir = workweave_dir.join("projects").join(project);
    let manifest_path = project_dir.join("rwv.yaml");
    let manifest = Manifest::from_path(&manifest_path)?;

    let registries = builtin_registries();
    let git = GitVcs;
    let all_repos_on_disk = scan_repos_on_disk(workweave_dir, &registries, &git);
    let all_project_paths = discover_project_paths(workweave_dir);

    let builtin = builtin_integrations();
    let integrations: Vec<&dyn Integration> = builtin.iter().map(|b| b.as_ref()).collect();

    // 1. Run integrations with output_dir = project_dir, workspace_root = workweave_dir.
    let ctx_base = IntegrationContextBase {
        output_dir: &project_dir,
        workspace_root: workweave_dir,
        project: &project_name,
        all_repos_on_disk: &all_repos_on_disk,
        all_project_paths: &all_project_paths,
    };

    let issues = run_activations(&integrations, &manifest, &ctx_base);

    for issue in &issues {
        let prefix = match issue.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        eprintln!("[{prefix}] {}: {}", issue.integration, issue.message);
    }

    // 2. Collect generated files.
    let default_config = IntegrationConfig::default();
    let mut new_generated: Vec<String> = Vec::new();

    for integration in &integrations {
        let config = manifest
            .integrations
            .get(integration.name())
            .unwrap_or(&default_config);

        if !is_enabled(*integration, config) {
            continue;
        }

        let int_ctx = IntegrationContext {
            output_dir: &project_dir,
            workspace_root: workweave_dir,
            project: &project_name,
            repos: &manifest.repositories,
            config,
            all_repos_on_disk: &all_repos_on_disk,
            all_project_paths: &all_project_paths,
        };

        new_generated.extend(integration.generated_files(&int_ctx));
    }

    // 3. Remove old symlinks from a previous activation in the workweave.
    remove_activation_symlinks(workweave_dir)?;

    // 4. Create new symlinks at workweave root pointing to project_dir files.
    for file in &new_generated {
        let source = project_dir.join(file);
        let link = workweave_dir.join(file);

        if !source.exists() {
            continue;
        }

        if let Some(parent) = link.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    eprintln!(
                        "[warning] symlink: failed to create parent directory {}: {e}",
                        parent.display()
                    );
                    continue;
                }
            }
        }

        let file_path = Path::new(file);
        let depth = file_path.parent().map(|p| p.components().count()).unwrap_or(0);
        let mut relative_target = std::path::PathBuf::new();
        for _ in 0..depth {
            relative_target.push("..");
        }
        relative_target.push("projects");
        relative_target.push(project);
        relative_target.push(file);

        #[cfg(unix)]
        if let Err(e) = std::os::unix::fs::symlink(&relative_target, &link) {
            eprintln!(
                "[warning] symlink: failed to create {} -> {}: {e}",
                link.display(),
                relative_target.display()
            );
        }

        #[cfg(windows)]
        if let Err(e) = std::os::windows::fs::symlink_file(&relative_target, &link) {
            eprintln!(
                "[warning] symlink: failed to create {} -> {}: {e}",
                link.display(),
                relative_target.display()
            );
        }
    }

    // 5. Write .rwv-active.
    set_active_project(workweave_dir, &project_name)?;

    Ok(())
}

/// Deactivate the current project: remove symlinks and `.rwv-active`.
#[allow(dead_code)]
pub fn deactivate(cwd: &Path) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let root = &ctx.root;

    remove_activation_symlinks(root)?;

    let active_file = root.join(".rwv-active");
    if active_file.exists() {
        std::fs::remove_file(&active_file)?;
    }

    Ok(())
}
