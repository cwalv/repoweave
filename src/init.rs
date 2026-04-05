//! Project initialization: `rwv init`.
//!
//! Creates a new project directory under `projects/`, runs `git init`,
//! writes an empty `rwv.yaml`, and auto-activates the project. Optionally
//! configures a git remote when `--provider` is given.
//!
//! `rwv init --adopt SOURCE` clones an existing repo as a project. The source
//! can be a URL or a shorthand (`owner/repo` or `registry/owner/repo`). The
//! cloned repo is placed under `projects/{name}/`, an `rwv.yaml` is written if
//! missing, and the project is activated.

use crate::git::GitVcs;
use crate::registry::{builtin_registries, resolve_to_clone_info, RepoId};
use crate::vcs::Vcs;
use crate::workspace::WorkspaceContext;
use std::path::Path;
use std::process::Command;

/// Initialize a new project in the workspace.
///
/// - Resolves the workspace root from `cwd`.
/// - Creates `projects/{name}/`.
/// - Runs `git init` in the new directory.
/// - Writes an empty `rwv.yaml` (`repositories: {}`).
/// - If `provider` is given (e.g., `"github/owner"`), configures a git remote.
/// - Activates the project (writes `.rwv-active` and generates ecosystem files).
pub fn init(name: &str, provider: Option<&str>, cwd: &Path) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let project_dir = ctx.root.join("projects").join(name);

    // Collision check
    if project_dir.exists() {
        anyhow::bail!(
            "project '{}' already exists at {}",
            name,
            project_dir.display()
        );
    }

    // Create directory
    std::fs::create_dir_all(&project_dir)
        .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", project_dir.display()))?;

    // git init
    let status = Command::new("git")
        .args(["init"])
        .current_dir(&project_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run git init: {e}"))?;
    if !status.success() {
        anyhow::bail!("git init failed in {}", project_dir.display());
    }

    // Write empty rwv.yaml
    std::fs::write(project_dir.join("rwv.yaml"), "repositories: {}\n")
        .map_err(|e| anyhow::anyhow!("failed to write rwv.yaml: {e}"))?;

    // Set up remote from --provider
    if let Some(provider_str) = provider {
        let (registry_name, owner) = provider_str.split_once('/').ok_or_else(|| {
            anyhow::anyhow!(
                "invalid --provider format '{}', expected 'registry/owner' (e.g., 'github/myorg')",
                provider_str
            )
        })?;

        // Look up the registry to get the clone URL pattern
        let registries = builtin_registries();
        let registry = registries
            .iter()
            .find(|r| r.name().0 == registry_name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown registry '{}'. Known registries: github, gitlab, bitbucket",
                    registry_name
                )
            })?;

        let repo_id = RepoId {
            owner: owner.to_string(),
            repo: name.to_string(),
        };

        let url = registry.clone_url(&repo_id).ok_or_else(|| {
            anyhow::anyhow!("registry '{}' does not support clone URLs", registry_name)
        })?;

        let status = Command::new("git")
            .args(["remote", "add", "origin", &url])
            .current_dir(&project_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| anyhow::anyhow!("failed to run git remote add: {e}"))?;
        if !status.success() {
            anyhow::bail!("git remote add failed in {}", project_dir.display());
        }
    }

    eprintln!(
        "Initialized project '{}' at {}",
        name,
        project_dir.display()
    );

    // Auto-activate the newly created project.
    crate::activate::activate(name, cwd)?;

    Ok(())
}

/// Adopt an existing repo as a project.
///
/// `source` is a URL or shorthand (`owner/repo` or `registry/owner/repo`).
/// The function:
/// 1. Resolves the workspace root from `cwd`.
/// 2. Determines the clone URL and project name from `source`.
/// 3. Clones the repo to `projects/{name}/` (skips if already exists).
/// 4. Writes an empty `rwv.yaml` if the clone does not already contain one.
/// 5. Activates the project.
pub fn init_adopt(source: &str, cwd: &Path) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let root = &ctx.root;

    // Resolve the source to a clone URL and project name.
    let (clone_url, project_name) = resolve_adopt_source(source)?;

    let project_dir = root.join("projects").join(&project_name);

    // Collision check
    if project_dir.exists() {
        anyhow::bail!(
            "project '{}' already exists at {}",
            project_name,
            project_dir.display()
        );
    }

    // Clone the repo
    let git = GitVcs;
    eprintln!("Cloning {} into {}", clone_url, project_dir.display());
    git.clone_repo(&clone_url, &project_dir)
        .map_err(|e| anyhow::anyhow!("failed to clone {}: {e}", clone_url))?;

    // Write rwv.yaml if missing
    let manifest_path = project_dir.join("rwv.yaml");
    if !manifest_path.exists() {
        std::fs::write(&manifest_path, "repositories: {}\n")
            .map_err(|e| anyhow::anyhow!("failed to write rwv.yaml: {e}"))?;
    }

    eprintln!(
        "Adopted project '{}' at {}",
        project_name,
        project_dir.display()
    );

    // Activate the project
    crate::activate::activate(&project_name, cwd)?;

    Ok(())
}

/// Resolve an adopt source (URL or shorthand) into a clone URL and project name.
///
/// For full URLs, the project name is derived from the last path segment.
/// For shorthands, the registry is used to construct the clone URL and the
/// repo name becomes the project name.
fn resolve_adopt_source(source: &str) -> anyhow::Result<(String, String)> {
    let (clone_url, _registry_name, repo_id) = resolve_to_clone_info(source)?;
    Ok((clone_url, repo_id.repo))
}
