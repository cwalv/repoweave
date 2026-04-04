//! Lock logic: snapshot repo HEADs into `rwv.lock`.

use crate::git::GitVcs;
use crate::manifest::{LockEntry, LockFile, Manifest, Project, VcsType, WeaveName};
use crate::vcs::{RevisionId, Vcs};
use crate::workspace::{WorkspaceContext, WorkspaceLocation};
use std::collections::BTreeMap;
use std::path::Path;

/// Generate a [`LockFile`] for a project, resolving HEAD revisions from the
/// workspace (primary or weave).
///
/// When `dirty` is false, each repo is checked for uncommitted changes and
/// an error is returned if any are found. When `dirty` is true, the check
/// is skipped.
///
/// If a tag points at HEAD for a given repo, the tag name is used as the
/// version instead of the raw SHA.
pub fn generate_lock(
    manifest: &Manifest,
    workspace_root: &Path,
    weave: Option<&WeaveName>,
    weave_dir: Option<&Path>,
    dirty: bool,
) -> anyhow::Result<LockFile> {
    let git = GitVcs;
    let mut repositories = BTreeMap::new();

    for (repo_path, entry) in &manifest.repositories {
        // Determine the actual on-disk path for this repo.
        // In a weave, repos live under the weave directory; in primary, under root.
        let repo_dir = if let Some(wd) = weave_dir {
            let candidate = wd.join(repo_path.as_path());
            if candidate.exists() {
                candidate
            } else {
                // Fall back to primary if the repo doesn't exist in the weave
                workspace_root.join(repo_path.as_path())
            }
        } else {
            workspace_root.join(repo_path.as_path())
        };

        // Check for uncommitted changes unless --dirty is set.
        if !dirty {
            match entry.vcs_type {
                VcsType::Git => {
                    if git.has_uncommitted_changes(&repo_dir)? {
                        anyhow::bail!(
                            "repo {} has uncommitted changes; commit or use --dirty to override",
                            repo_path
                        );
                    }
                }
            }
        }

        // Prefer tag name at HEAD over raw SHA.
        let version = match entry.vcs_type {
            VcsType::Git => {
                if let Some(tag) = git.tag_at_head(&repo_dir)? {
                    RevisionId::new(tag)
                } else {
                    git.head_revision(&repo_dir)?
                }
            }
        };

        repositories.insert(
            repo_path.clone(),
            LockEntry {
                vcs_type: entry.vcs_type,
                url: entry.url.clone(),
                version,
            },
        );
    }

    Ok(LockFile {
        weave: weave.cloned(),
        repositories,
    })
}

/// Write a lock file as YAML to the given path.
pub fn write_lock(lock: &LockFile, path: &Path) -> anyhow::Result<()> {
    let yaml = serde_yaml::to_string(lock)
        .map_err(|e| anyhow::anyhow!("failed to serialize lock file: {e}"))?;
    std::fs::write(path, &yaml)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))?;
    Ok(())
}

/// Execute `rwv lock` for the current workspace context.
///
/// When `dirty` is true, the uncommitted-changes check is skipped.
pub fn lock(cwd: &Path, dirty: bool) -> anyhow::Result<()> {
    use crate::integration::Severity;
    use crate::integration_runner::{run_lock_hooks, IntegrationContextBase};
    use crate::integrations::builtin_integrations;
    use crate::registry::builtin_registries;

    let ctx = WorkspaceContext::resolve(cwd, None)?;

    let (project_name, weave_name, weave_dir) = match &ctx.location {
        WorkspaceLocation::Primary { project } => {
            let name = project
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no active project found; run from a project directory or use --project"))?;
            (name.clone(), None, None)
        }
        WorkspaceLocation::Weave {
            name,
            dir,
            project,
        } => (project.clone(), Some(name.clone()), Some(dir.clone())),
    };

    // Load the project manifest from the primary workspace.
    let project_dir = ctx.root.join("projects").join(project_name.as_str());
    let project = Project::from_dir(&project_dir)
        .map_err(|e| anyhow::anyhow!("failed to load project '{}': {e}", project_name))?;

    let lock = generate_lock(
        &project.manifest,
        &ctx.root,
        weave_name.as_ref(),
        weave_dir.as_deref(),
        dirty,
    )?;

    let lock_path = project_dir.join("rwv.lock");
    write_lock(&lock, &lock_path)?;

    eprintln!("Wrote {}", lock_path.display());

    // Run integration lock hooks after writing the lock file.
    let registries = builtin_registries();
    let git = GitVcs;
    let repos_on_disk = crate::workspace::scan_repos_on_disk(&ctx.root, &registries, &git);

    let projects_dir = ctx.root.join("projects");
    let project_paths: Vec<String> = if projects_dir.is_dir() {
        std::fs::read_dir(&projects_dir)
            .ok()
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir() && e.path().join("rwv.yaml").exists())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let output_dir = weave_dir.as_deref().unwrap_or(&ctx.root);
    let ctx_base = IntegrationContextBase {
        output_dir,
        workspace_root: &ctx.root,
        project: &project_name,
        all_repos_on_disk: &repos_on_disk,
        all_project_paths: &project_paths,
    };

    let builtin = builtin_integrations();
    let integrations: Vec<&dyn crate::integration::Integration> =
        builtin.iter().map(|b| b.as_ref()).collect();

    let issues = run_lock_hooks(&integrations, &project.manifest, &ctx_base);
    for issue in &issues {
        let prefix = match issue.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        eprintln!("[{prefix}] {}: {}", issue.integration, issue.message);
    }

    Ok(())
}
