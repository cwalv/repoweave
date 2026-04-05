//! `rwv add` and `rwv remove` — manage repos in a project manifest.

use crate::activate::activate;
use crate::git::GitVcs;
use crate::manifest::{Manifest, RepoEntry, RepoPath, Role, VcsType};
use crate::registry::{builtin_registries, resolve_url, Registry};
use crate::vcs::Vcs;
use crate::workspace::{WorkspaceContext, WorkspaceLocation};
use anyhow::{bail, Context};
use std::path::{Path, PathBuf};

/// Find the active project directory from the workspace context.
/// If no project is auto-detected (e.g., CWD is the workspace root),
/// look for a single project under `projects/` and use that.
fn find_project_dir(ctx: &WorkspaceContext) -> anyhow::Result<std::path::PathBuf> {
    let project_name = match &ctx.location {
        WorkspaceLocation::Weave { project } => project.clone(),
        WorkspaceLocation::Workweave { project, .. } => Some(project.clone()),
    };

    if let Some(name) = project_name {
        return Ok(ctx.root.join("projects").join(name.as_str()));
    }

    // No project auto-detected. Scan projects/ for a single project.
    let projects_dir = ctx.root.join("projects");
    if !projects_dir.is_dir() {
        bail!("no projects/ directory found in workspace");
    }

    let mut project_dirs: Vec<std::path::PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&projects_dir).context("failed to read projects/")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() && path.join("rwv.yaml").exists() {
            project_dirs.push(path);
        }
    }

    match project_dirs.len() {
        0 => bail!("no projects with rwv.yaml found under projects/"),
        1 => Ok(project_dirs.into_iter().next().unwrap()),
        _ => bail!("multiple projects found; run from inside a project directory or use --project"),
    }
}

/// Execute `rwv add URL [--role=ROLE]`.
pub fn run_add(url: &str, role: Role, cwd: &Path) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let project_dir = find_project_dir(&ctx)?;
    let manifest_path = project_dir.join("rwv.yaml");

    // Check if the argument is a local path (no URL scheme and directory exists
    // relative to workspace root).
    if !url.contains("://") {
        let candidate = ctx.root.join(url);
        if candidate.is_dir() {
            run_add_from_local_path(url, &candidate, role, &manifest_path)?;
            let project_name = project_dir
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| anyhow::anyhow!("could not determine project name from path"))?;
            return activate(project_name, cwd);
        }
    }

    // Resolve the URL through registries to get a canonical local path.
    let owned_registries = builtin_registries();
    let registry_refs: Vec<&dyn Registry> = owned_registries.iter().map(|r| r.as_ref()).collect();

    let local_path =
        if let Some((_registry_name, _repo_id, path)) = resolve_url(url, &registry_refs) {
            path
        } else {
            // No registry matched — try to derive a path from the URL.
            derive_local_path_from_url(url).ok_or_else(|| {
                anyhow::anyhow!("Error: unrecognized URL '{url}' — could not derive a local path")
            })?
        };

    let repo_path = RepoPath::new(local_path.to_string_lossy().to_string());

    // Load and check existing manifest.
    let mut manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

    if manifest.repositories.contains_key(&repo_path) {
        eprintln!(
            "Repository already exists in manifest at '{}'",
            repo_path.as_str()
        );
        return Ok(());
    }

    // Clone the repo if it doesn't exist on disk.
    let dest = ctx.root.join(repo_path.as_path());
    if dest.exists() {
        eprintln!(
            "Directory already exists at '{}', skipping clone",
            dest.display()
        );
    } else {
        // Create parent directories.
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }
        let git = GitVcs;
        git.clone_repo(url, &dest)
            .with_context(|| format!("failed to clone '{}' into {}", url, dest.display()))?;
    }

    let git = GitVcs;
    let default_branch = git.default_branch(&dest)?;

    // Add entry to manifest.
    let entry = RepoEntry {
        vcs_type: VcsType::Git,
        url: url.to_string(),
        version: default_branch,
        role,
    };
    manifest.repositories.insert(repo_path.clone(), entry);

    // Write back the manifest.
    write_manifest(&manifest_path, &manifest)?;

    eprintln!("Added '{}' to manifest", repo_path.as_str());

    // Re-run activation so ecosystem files (Cargo.toml, package.json, etc.) are updated.
    let project_name = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("could not determine project name from path"))?;
    activate(project_name, cwd)?;

    Ok(())
}

/// Handle `rwv add <local-path>` where the argument is a relative path to an
/// existing directory under the workspace root.  Infers the URL by reading the
/// clone's `origin` remote.
fn run_add_from_local_path(
    path_arg: &str,
    clone_dir: &Path,
    role: Role,
    manifest_path: &Path,
) -> anyhow::Result<()> {
    // Read the origin URL from the existing clone.
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(clone_dir)
        .output()
        .with_context(|| format!("failed to run git in {}", clone_dir.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "could not determine origin URL for '{}': {}",
            clone_dir.display(),
            stderr.trim()
        );
    }

    let raw_url = String::from_utf8(output.stdout)
        .map_err(|e| anyhow::anyhow!("git remote get-url origin produced non-UTF-8 output: {e}"))?
        .trim()
        .to_string();

    // Normalise bare absolute paths to file:// URLs so manifests are
    // consistent regardless of how the clone was created.
    let origin_url = if raw_url.starts_with('/') {
        format!("file://{raw_url}")
    } else {
        raw_url
    };

    let repo_path = RepoPath::new(path_arg);

    // Load and check existing manifest.
    let mut manifest = Manifest::from_path(manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

    if manifest.repositories.contains_key(&repo_path) {
        eprintln!(
            "Repository already exists in manifest at '{}'",
            repo_path.as_str()
        );
        return Ok(());
    }

    let git = GitVcs;
    let default_branch = git.default_branch(clone_dir)?;

    // Add entry to manifest using the inferred origin URL.
    let entry = RepoEntry {
        vcs_type: VcsType::Git,
        url: origin_url.clone(),
        version: default_branch,
        role,
    };
    manifest.repositories.insert(repo_path.clone(), entry);

    write_manifest(manifest_path, &manifest)?;

    eprintln!(
        "Added '{}' to manifest (url: {})",
        repo_path.as_str(),
        origin_url
    );
    Ok(())
}

/// Execute `rwv remove PATH [--delete] [--force]`.
pub fn run_remove(path: &str, delete: bool, force: bool, cwd: &Path) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let project_dir = find_project_dir(&ctx)?;
    let manifest_path = project_dir.join("rwv.yaml");

    let repo_path = RepoPath::new(path);

    // Load existing manifest.
    let mut manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

    if manifest.repositories.remove(&repo_path).is_none() {
        bail!("Error: path '{}' not found in manifest", repo_path.as_str());
    }

    // Before writing anything, check for cross-project references when --delete
    // is requested.  If another project references the repo and --force is not
    // set, bail early so the manifest is left untouched.
    if delete {
        let repo_dir = ctx.root.join(repo_path.as_path());
        if repo_dir.exists() {
            let referencing_projects =
                find_other_projects_referencing(&ctx.root, &project_dir, &repo_path);

            if !referencing_projects.is_empty() {
                for proj in &referencing_projects {
                    eprintln!("warning: repo also referenced by project '{proj}'");
                }
                if !force {
                    anyhow::bail!(
                        "refusing to delete '{}': referenced by other projects (use --force to override)",
                        repo_path.as_str()
                    );
                }
            }
        }
    }

    // Write back the manifest (after all pre-flight checks pass).
    write_manifest(&manifest_path, &manifest)?;

    eprintln!("Removed '{}' from manifest", repo_path.as_str());

    // Re-run activation so ecosystem files (Cargo.toml, package.json, etc.) are updated.
    let project_name = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("could not determine project name from path"))?;
    activate(project_name, cwd)?;

    // Optionally delete the clone directory.
    if delete {
        let repo_dir = ctx.root.join(repo_path.as_path());
        if repo_dir.exists() {
            std::fs::remove_dir_all(&repo_dir)
                .with_context(|| format!("failed to delete directory {}", repo_dir.display()))?;
            eprintln!("Deleted '{}'", repo_dir.display());
        }
    }

    Ok(())
}

/// Execute `rwv add PATH --new`.
///
/// Instead of cloning from a URL, this creates a new repo at the canonical
/// path by running `git init`. The URL is inferred from the path convention
/// via registries (e.g., `github/owner/repo` → `https://github.com/owner/repo.git`).
/// The repo is added to the manifest with role `primary`.
pub fn run_add_new(path_arg: &str, cwd: &Path) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let project_dir = find_project_dir(&ctx)?;
    let manifest_path = project_dir.join("rwv.yaml");

    // Validate that the argument looks like a path (registry/owner/repo).
    let segments: Vec<&str> = path_arg.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 3 {
        bail!(
            "Error: '{}' does not look like a valid repo path (expected registry/owner/repo)",
            path_arg
        );
    }

    // Try to infer the URL from the path via registries.
    let owned_registries = builtin_registries();
    let registry_refs: Vec<&dyn Registry> = owned_registries.iter().map(|r| r.as_ref()).collect();

    let url = infer_url_from_path(path_arg, &registry_refs).ok_or_else(|| {
        anyhow::anyhow!(
            "Error: could not infer a URL from path '{}' — no matching registry",
            path_arg
        )
    })?;

    let repo_path = RepoPath::new(path_arg);

    // Load and check existing manifest.
    let mut manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;

    if manifest.repositories.contains_key(&repo_path) {
        eprintln!(
            "Repository already exists in manifest at '{}'",
            repo_path.as_str()
        );
        return Ok(());
    }

    // Create the directory and run git init.
    let dest = ctx.root.join(repo_path.as_path());
    if dest.exists() {
        eprintln!(
            "Directory already exists at '{}', skipping init",
            dest.display()
        );
    } else {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }
        let git = GitVcs;
        git.init_repo(&dest)
            .with_context(|| format!("failed to init repo at {}", dest.display()))?;
    }

    let git = GitVcs;
    let default_branch = git.default_branch(&dest)?;

    // Add entry to manifest with role primary.
    let entry = RepoEntry {
        vcs_type: VcsType::Git,
        url,
        version: default_branch,
        role: Role::Primary,
    };
    manifest.repositories.insert(repo_path.clone(), entry);

    // Write back the manifest.
    write_manifest(&manifest_path, &manifest)?;

    eprintln!("Added new repo '{}' to manifest", repo_path.as_str());

    // Re-run activation so ecosystem files (Cargo.toml, package.json, etc.) are updated.
    let project_name = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("could not determine project name from path"))?;
    activate(project_name, cwd)?;

    Ok(())
}

/// Scan `projects/*/rwv.yaml` (excluding `active_project_dir`) and return the
/// names of any projects that reference `repo_path`.
fn find_other_projects_referencing(
    workspace_root: &Path,
    active_project_dir: &Path,
    repo_path: &RepoPath,
) -> Vec<String> {
    let projects_dir = workspace_root.join("projects");
    let mut referencing: Vec<String> = Vec::new();

    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(_) => return referencing,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Skip the active project.
        if path == active_project_dir {
            continue;
        }
        let manifest_path = path.join("rwv.yaml");
        if let Ok(manifest) = Manifest::from_path(&manifest_path) {
            if manifest.repositories.contains_key(repo_path) {
                // Derive a human-readable project name from the directory name.
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                referencing.push(name);
            }
        }
    }

    referencing
}

/// Infer a clone URL from a local path by matching the first segment against
/// known registries.
///
/// For example, `github/owner/repo` matches the GitHub registry and produces
/// `https://github.com/owner/repo.git`.
fn infer_url_from_path(path: &str, registries: &[&dyn Registry]) -> Option<String> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 3 {
        return None;
    }

    let registry_name = segments[0];
    let owner = segments[1];
    let repo = segments[2];

    let id = crate::registry::RepoId {
        owner: owner.to_string(),
        repo: repo.to_string(),
    };

    for reg in registries {
        if reg.name().0 == registry_name {
            return reg.clone_url(&id);
        }
    }

    None
}

/// Derive a local path from a URL by taking its last two path segments.
///
/// For example, `file:///tmp/foo/bar/remote.git` → `bar/remote`
/// (stripping `.git` suffix from the repo name).
fn derive_local_path_from_url(url: &str) -> Option<PathBuf> {
    // Strip scheme
    let path_str = if let Some(rest) = url.strip_prefix("file://") {
        rest
    } else if url.contains("://") {
        url.split("://").nth(1)?
    } else {
        return None;
    };

    let trimmed = path_str.trim_end_matches('/');
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 {
        return None;
    }

    let repo = segments[segments.len() - 1];
    let owner = segments[segments.len() - 2];
    let repo = repo.strip_suffix(".git").unwrap_or(repo);

    if owner.is_empty() || repo.is_empty() {
        return None;
    }

    Some(PathBuf::from(owner).join(repo))
}

/// Serialize and write a manifest to disk, preserving YAML format.
fn write_manifest(path: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    let yaml = serde_yaml::to_string(manifest)
        .map_err(|e| anyhow::anyhow!("failed to serialize manifest: {e}"))?;
    std::fs::write(path, &yaml)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{DomainRegistry, RegistryName};
    use std::path::PathBuf;

    fn github_reg() -> DomainRegistry {
        DomainRegistry {
            registry_name: RegistryName("github".into()),
            domain: "github.com".into(),
        }
    }

    fn gitlab_reg() -> DomainRegistry {
        DomainRegistry {
            registry_name: RegistryName("gitlab".into()),
            domain: "gitlab.com".into(),
        }
    }

    // -----------------------------------------------------------------------
    // infer_url_from_path
    // -----------------------------------------------------------------------

    #[test]
    fn infer_url_github_three_segments() {
        let gh = github_reg();
        let registries: Vec<&dyn Registry> = vec![&gh];
        let url = infer_url_from_path("github/owner/repo", &registries).unwrap();
        assert_eq!(url, "https://github.com/owner/repo.git");
    }

    #[test]
    fn infer_url_gitlab_three_segments() {
        let gl = gitlab_reg();
        let registries: Vec<&dyn Registry> = vec![&gl];
        let url = infer_url_from_path("gitlab/org/project", &registries).unwrap();
        assert_eq!(url, "https://gitlab.com/org/project.git");
    }

    #[test]
    fn infer_url_first_matching_registry_wins() {
        let gh = github_reg();
        let gl = gitlab_reg();
        let registries: Vec<&dyn Registry> = vec![&gh, &gl];
        let url = infer_url_from_path("github/alice/widgets", &registries).unwrap();
        assert_eq!(url, "https://github.com/alice/widgets.git");
    }

    #[test]
    fn infer_url_unknown_registry_returns_none() {
        let gh = github_reg();
        let registries: Vec<&dyn Registry> = vec![&gh];
        assert!(infer_url_from_path("unknown/owner/repo", &registries).is_none());
    }

    #[test]
    fn infer_url_two_segments_returns_none() {
        let gh = github_reg();
        let registries: Vec<&dyn Registry> = vec![&gh];
        assert!(infer_url_from_path("owner/repo", &registries).is_none());
    }

    #[test]
    fn infer_url_single_segment_returns_none() {
        let gh = github_reg();
        let registries: Vec<&dyn Registry> = vec![&gh];
        assert!(infer_url_from_path("repo", &registries).is_none());
    }

    #[test]
    fn infer_url_empty_string_returns_none() {
        let gh = github_reg();
        let registries: Vec<&dyn Registry> = vec![&gh];
        assert!(infer_url_from_path("", &registries).is_none());
    }

    #[test]
    fn infer_url_empty_registries_returns_none() {
        let registries: Vec<&dyn Registry> = vec![];
        assert!(infer_url_from_path("github/owner/repo", &registries).is_none());
    }

    #[test]
    fn infer_url_extra_segments_uses_first_three() {
        let gh = github_reg();
        let registries: Vec<&dyn Registry> = vec![&gh];
        let url = infer_url_from_path("github/owner/repo/extra/path", &registries).unwrap();
        assert_eq!(url, "https://github.com/owner/repo.git");
    }

    #[test]
    fn infer_url_leading_slash_ignored() {
        let gh = github_reg();
        let registries: Vec<&dyn Registry> = vec![&gh];
        let url = infer_url_from_path("/github/owner/repo", &registries).unwrap();
        assert_eq!(url, "https://github.com/owner/repo.git");
    }

    // -----------------------------------------------------------------------
    // derive_local_path_from_url
    // -----------------------------------------------------------------------

    #[test]
    fn derive_path_from_file_url() {
        let path = derive_local_path_from_url("file:///tmp/foo/bar/remote.git").unwrap();
        assert_eq!(path, PathBuf::from("bar/remote"));
    }

    #[test]
    fn derive_path_strips_git_suffix() {
        let path = derive_local_path_from_url("file:///srv/repos/owner/repo.git").unwrap();
        assert_eq!(path, PathBuf::from("owner/repo"));
    }

    #[test]
    fn derive_path_no_git_suffix() {
        let path = derive_local_path_from_url("file:///srv/repos/owner/repo").unwrap();
        assert_eq!(path, PathBuf::from("owner/repo"));
    }

    #[test]
    fn derive_path_https_url() {
        let path = derive_local_path_from_url("https://github.com/owner/repo.git").unwrap();
        assert_eq!(path, PathBuf::from("owner/repo"));
    }

    #[test]
    fn derive_path_trailing_slash() {
        let path = derive_local_path_from_url("file:///srv/repos/owner/repo/").unwrap();
        assert_eq!(path, PathBuf::from("owner/repo"));
    }

    #[test]
    fn derive_path_single_segment_returns_none() {
        assert!(derive_local_path_from_url("file:///repo").is_none());
    }

    #[test]
    fn derive_path_no_scheme_returns_none() {
        assert!(derive_local_path_from_url("/some/path").is_none());
    }

    #[test]
    fn derive_path_empty_returns_none() {
        assert!(derive_local_path_from_url("").is_none());
    }
}
