//! Project initialization: `rwv init`.
//!
//! Creates a new project directory under `projects/`, runs `git init`, and
//! writes an empty `rwv.yaml`. Optionally configures a git remote when
//! `--provider` is given.

use crate::registry::{builtin_registries, RepoId};
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
///
/// Does NOT activate the project (no `.rwv-active` update).
pub fn init(name: &str, provider: Option<&str>, cwd: &Path) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let project_dir = ctx.root.join("projects").join(name);

    // Collision check
    if project_dir.exists() {
        anyhow::bail!("project '{}' already exists at {}", name, project_dir.display());
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
        let (registry_name, owner) = provider_str
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!(
                "invalid --provider format '{}', expected 'registry/owner' (e.g., 'github/myorg')",
                provider_str
            ))?;

        // Look up the registry to get the clone URL pattern
        let registries = builtin_registries();
        let registry = registries
            .iter()
            .find(|r| r.name().0 == registry_name)
            .ok_or_else(|| anyhow::anyhow!(
                "unknown registry '{}'. Known registries: github, gitlab, bitbucket",
                registry_name
            ))?;

        let repo_id = RepoId {
            owner: owner.to_string(),
            repo: name.to_string(),
        };

        let url = registry
            .clone_url(&repo_id)
            .ok_or_else(|| anyhow::anyhow!(
                "registry '{}' does not support clone URLs",
                registry_name
            ))?;

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

    eprintln!("Initialized project '{}' at {}", name, project_dir.display());
    Ok(())
}
