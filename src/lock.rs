//! Lock logic: snapshot repo HEADs into `rwv.lock`.

use crate::git::GitVcs;
use crate::manifest::{LockEntry, LockFile, Manifest, Project, WorkweaveName};
use crate::vcs::{vcs_for, RevisionId};
use crate::workspace::{WorkspaceContext, WorkspaceLocation};
use std::collections::BTreeMap;
use std::path::Path;

/// Generate a [`LockFile`] for a project, resolving HEAD revisions from the
/// workspace (weave or workweave).
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
    workweave: Option<(&WorkweaveName, &Path)>,
    dirty: bool,
) -> anyhow::Result<LockFile> {
    let mut repositories = BTreeMap::new();

    for (repo_path, entry) in &manifest.repositories {
        // Determine the actual on-disk path for this repo.
        // In a workweave, repos live under the workweave directory; in primary, under root.
        let repo_dir = if let Some((_, wd)) = workweave {
            let candidate = wd.join(repo_path.as_path());
            if candidate.exists() {
                candidate
            } else {
                // Fall back to primary if the repo doesn't exist in the workweave
                workspace_root.join(repo_path.as_path())
            }
        } else {
            workspace_root.join(repo_path.as_path())
        };

        let vcs = vcs_for(entry.vcs_type);

        // Check for uncommitted changes unless --dirty is set.
        if !dirty {
            if vcs.has_uncommitted_changes(&repo_dir)? {
                anyhow::bail!(
                    "repo {} has uncommitted changes; commit or use --dirty to override",
                    repo_path
                );
            }
        }

        // Prefer tag name at HEAD over raw SHA.
        let version = if let Some(tag) = vcs.tag_at_head(&repo_dir)? {
            RevisionId::new(tag)
        } else {
            vcs.head_revision(&repo_dir)?
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
        workweave: workweave.map(|(name, _)| name.clone()),
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
    use crate::integration_runner::run_lock_hooks;
    use crate::integrations::builtin_integrations;

    let ctx = WorkspaceContext::resolve(cwd, None)?;

    let (project_name, workweave_name, workweave_dir) = match &ctx.location {
        WorkspaceLocation::Weave { project } => {
            let name = project
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no active project found; run from a project directory or use --project"))?;
            (name.clone(), None, None)
        }
        WorkspaceLocation::Workweave {
            name,
            dir,
            project,
        } => (project.clone(), Some(name.clone()), Some(dir.clone())),
    };

    // Load the project manifest from the primary workspace.
    let project_dir = ctx.root.join("projects").join(project_name.as_str());
    let project = Project::from_dir(&project_dir)
        .map_err(|e| anyhow::anyhow!("failed to load project '{}': {e}", project_name))?;

    let workweave_pair = workweave_name
        .as_ref()
        .zip(workweave_dir.as_deref());
    let lock = generate_lock(
        &project.manifest,
        &ctx.root,
        workweave_pair,
        dirty,
    )?;

    let lock_path = project_dir.join("rwv.lock");
    write_lock(&lock, &lock_path)?;

    eprintln!("Wrote {}", lock_path.display());

    // Run integration lock hooks after writing the lock file.
    let session = crate::workspace::WorkspaceSession::new(&ctx.root);

    let output_dir = workweave_dir.as_deref().unwrap_or(&ctx.root);
    let ctx_base = session.context_base(output_dir, &project_name);

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
