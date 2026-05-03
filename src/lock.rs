//! Lock logic: snapshot repo HEADs into `rwv.lock`.

use crate::manifest::{LockEntry, LockFile, Manifest, Project, WorkweaveName};
use crate::vcs::vcs_for;
use crate::workspace::{WorkspaceContext, WorkspaceLocation};
use std::collections::BTreeMap;
use std::path::Path;

/// Build a commit message summarising which repos' lock entries changed.
///
/// When `old_lock` is `None` (fresh lock), all repos appear in the list.
/// Otherwise, only repos whose version advanced are listed. Versions are
/// compared by canonical SHA and by display form to correctly handle
/// tag-pinned entries that survived deserialization as raw strings.
fn build_commit_message(new_lock: &LockFile, old_lock: Option<&LockFile>) -> String {
    let changed: Vec<_> = new_lock
        .repositories
        .iter()
        .filter(|(path, new_entry)| {
            old_lock.is_none_or(|old| {
                old.repositories.get(*path).is_none_or(|old_entry| {
                    old_entry.version != new_entry.version
                        && old_entry.version.display_str() != new_entry.version.display_str()
                })
            })
        })
        .collect();

    let n = changed.len();
    if n == 0 {
        return "lock: no changes".to_string();
    }

    let mut msg = format!("lock: refresh {} repo{}", n, if n == 1 { "" } else { "s" });
    msg.push_str("\n\n");
    for (path, entry) in &changed {
        let ver = entry.version.display_str();
        let abbrev = if ver.len() == 40 && ver.chars().all(|c| c.is_ascii_hexdigit()) {
            &ver[..7]
        } else {
            ver
        };
        msg.push_str(&format!("  - {}: {}\n", path, abbrev));
    }
    msg.trim_end().to_string()
}

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
        if !dirty && vcs.has_uncommitted_changes(&repo_dir)? {
            anyhow::bail!(
                "repo {} has uncommitted changes; commit or use --dirty to override",
                repo_path
            );
        }

        // `head_revision` resolves to the canonical SHA and, when a tag
        // points at HEAD, also fills in the tag display form — so the lock
        // serializes as the tag name when available and the canonical SHA
        // otherwise.
        let version = vcs.head_revision(&repo_dir)?;

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

/// Stage `rwv.lock` and commit from `project_dir`.
///
/// Generic helper: caller supplies the message. Returns `Ok(false)` when
/// the staged change is empty (lock unchanged), `Ok(true)` when a commit
/// was made.
///
/// Runs git from `project_dir` so the operation works for both the
/// project-as-its-own-git-repo model (where `project_dir` is itself a
/// git repo or worktree) and the workspace-root-as-single-repo model
/// (where `project_dir` is a sub-directory inside a larger git repo).
pub(crate) fn commit_lock_file_with_message(
    project_dir: &Path,
    message: &str,
) -> anyhow::Result<bool> {
    use crate::git::git_command;

    let add_out = git_command()
        .args(["add", "rwv.lock"])
        .current_dir(project_dir)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git add: {e}"))?;

    if !add_out.status.success() {
        let stderr = String::from_utf8_lossy(&add_out.stderr);
        anyhow::bail!("git add failed: {}", stderr.trim());
    }

    // `git diff --cached --quiet` exits 0 when nothing is staged.
    let nothing_staged = git_command()
        .args(["diff", "--cached", "--quiet"])
        .current_dir(project_dir)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to check staged changes: {e}"))?
        .status
        .success();

    if nothing_staged {
        return Ok(false);
    }

    let commit_out = git_command()
        .args(["commit", "-m", message])
        .current_dir(project_dir)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git commit: {e}"))?;

    if !commit_out.status.success() {
        let stderr = String::from_utf8_lossy(&commit_out.stderr);
        anyhow::bail!("git commit failed: {}", stderr.trim());
    }

    Ok(true)
}

/// Commit `rwv.lock` from `project_dir` with a multi-repo summary message.
///
/// Refuses if the project repo has uncommitted changes outside the lock
/// file — the auto-commit must not bundle unrelated work-in-progress.
fn commit_lock_file(
    project_dir: &Path,
    new_lock: &LockFile,
    old_lock: Option<&LockFile>,
) -> anyhow::Result<()> {
    use crate::git::git_command;

    // Dirty check (scoped to the project repo).
    let status_out = git_command()
        .args(["status", "--porcelain"])
        .current_dir(project_dir)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git status: {e}"))?;
    if !status_out.status.success() {
        let stderr = String::from_utf8_lossy(&status_out.stderr);
        anyhow::bail!("git status failed: {}", stderr.trim());
    }
    let status_str = String::from_utf8_lossy(&status_out.stdout);
    let has_other_changes = status_str.lines().any(|line| {
        if line.starts_with("??") {
            return false; // untracked files are never committed
        }
        // Porcelain format: "XY path" — path starts at byte 3.
        let path = line.get(3..).unwrap_or("").trim();
        path != "rwv.lock"
    });
    if has_other_changes {
        anyhow::bail!(
            "project repo has uncommitted changes outside rwv.lock; \
             commit or stash them before using --commit"
        );
    }

    let message = build_commit_message(new_lock, old_lock);
    if commit_lock_file_with_message(project_dir, &message)? {
        eprintln!("Committed rwv.lock");
    } else {
        eprintln!("Lock unchanged, nothing to commit.");
    }
    Ok(())
}

/// Execute `rwv lock` for the current workspace context.
///
/// When `dirty` is true, the uncommitted-changes check is skipped.
/// When `commit` is true, the lock file is staged and committed after writing.
pub fn lock(cwd: &Path, dirty: bool, commit: bool) -> anyhow::Result<()> {
    use crate::integration::Severity;
    use crate::integration_runner::run_lock_hooks;
    use crate::integrations::builtin_integrations;

    let ctx = WorkspaceContext::resolve(cwd, None)?;

    let (project_name, workweave_name, workweave_dir) = match &ctx.location {
        WorkspaceLocation::Weave { project } => {
            let name = project.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "no active project found; run from a project directory or use --project"
                )
            })?;
            (name.clone(), None, None)
        }
        WorkspaceLocation::Workweave { name, dir, project } => {
            (project.clone(), Some(name.clone()), Some(dir.clone()))
        }
    };

    let project_dir = ctx
        .active_path()
        .join("projects")
        .join(project_name.as_str());
    let project = Project::from_dir(&project_dir)
        .map_err(|e| anyhow::anyhow!("failed to load project '{}': {e}", project_name))?;

    let workweave_pair = workweave_name.as_ref().zip(workweave_dir.as_deref());
    let lock = generate_lock(&project.manifest, ctx.primary_path(), workweave_pair, dirty)?;

    let lock_path = project_dir.join("rwv.lock");
    // Read old lock before overwriting so the commit message can list what changed.
    let old_lock = if commit {
        LockFile::from_path(&lock_path).ok()
    } else {
        None
    };
    write_lock(&lock, &lock_path)?;

    eprintln!("Wrote {}", lock_path.display());

    // Run integration lock hooks after writing the lock file.
    let session = crate::workspace::WorkspaceSession::new(ctx.active_path());

    let output_dir = ctx.active_path();
    let detection_cache = crate::integration_runner::build_detection_cache(
        ctx.active_path(),
        &project.manifest.repositories,
    );
    let ctx_base = session.context_base(output_dir, &project_name, &detection_cache);

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

    if commit {
        commit_lock_file(&project_dir, &lock, old_lock.as_ref())?;
    }

    Ok(())
}
