//! Lock logic: snapshot repo HEADs into `rwv.lock`.

use crate::manifest::{
    LockFile, Manifest, Project, ResolvedLockEntry, ResolvedLockFile, WorkweaveName,
};
use crate::vcs::vcs_for;
use crate::workspace::{Checkout, WorkspaceContext};
use anyhow::Context;
use std::collections::BTreeMap;
use std::path::Path;

/// Build a commit message summarising which repos' lock entries changed.
///
/// When `old_lock` is `None` (fresh lock), all repos appear in the list.
/// Otherwise, only repos whose version advanced are listed. Both sides
/// are [`ResolvedLockFile`]s, so the comparison is a straightforward
/// canonical-SHA equality (no raw-vs-resolved ambiguity).
fn build_commit_message(
    new_lock: &ResolvedLockFile,
    old_lock: Option<&ResolvedLockFile>,
) -> String {
    let changed: Vec<_> = new_lock
        .iter_entries()
        .filter(|(path, new_entry)| {
            old_lock.is_none_or(|old| {
                old.get_entry(path)
                    .is_none_or(|old_entry| old_entry.version != new_entry.version)
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

/// Generate a [`ResolvedLockFile`] for a project, resolving HEAD revisions
/// from the workspace (weave or workweave).
///
/// Returns a [`ResolvedLockFile`] (not a raw [`LockFile`]) because each
/// entry's version comes from [`crate::vcs::Vcs::head_revision`], which
/// already carries the canonical SHA — there is no parse step in this
/// path that could leave the value unresolved.
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
) -> anyhow::Result<ResolvedLockFile> {
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
        //
        // Unborn HEAD: `head_revision` returns a `CommandFailed` whose stderr
        // contains "unborn HEAD" — return it as-is; the message already names
        // the repo path and the fix.
        let version = vcs
            .head_revision(&repo_dir)
            .map_err(|e| anyhow::anyhow!("{}: {}", repo_path, e))?;

        // Detached HEAD: warn but do not refuse. Lock runs inside automation
        // (sync auto-relock) so a hard gate would break legitimate flows.
        // Warning text follows the house refusal pattern: name the state,
        // name the consequence, name the next verb.
        if vcs.current_ref(&repo_dir).ok().flatten().is_none() {
            let short = &version.as_str()[..version.as_str().len().min(7)];
            eprintln!(
                "warning: pinning detached HEAD {short} in {repo_path}: \
                 no branch names this commit; a later fetch will materialize detached. \
                 Create/checkout a branch if this is unintended."
            );
        }

        repositories.insert(
            repo_path.clone(),
            ResolvedLockEntry {
                vcs_type: entry.vcs_type,
                url: entry.url.clone(),
                version,
            },
        );
    }

    Ok(ResolvedLockFile {
        workweave: workweave.map(|(name, _)| name.clone()),
        repositories,
    })
}

/// Write a lock file as YAML to the given path.
///
/// Generic over any serializable lock form so callers can write either a
/// raw [`LockFile`] (round-trip) or a [`ResolvedLockFile`] (post-lock
/// generation). Both serialize as the same YAML shape — a single scalar
/// per `version`.
pub fn write_lock<L: serde::Serialize>(lock: &L, path: &Path) -> anyhow::Result<()> {
    let yaml = serde_yaml::to_string(lock).context("failed to serialize lock file")?;
    std::fs::write(path, &yaml).with_context(|| format!("failed to write {}", path.display()))?;
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
        .context("failed to run git add")?;

    if !add_out.status.success() {
        let stderr = String::from_utf8_lossy(&add_out.stderr);
        anyhow::bail!("git add failed: {}", stderr.trim());
    }

    // `git diff --cached --quiet` exits 0 when nothing is staged.
    let nothing_staged = git_command()
        .args(["diff", "--cached", "--quiet"])
        .current_dir(project_dir)
        .output()
        .context("failed to check staged changes")?
        .status
        .success();

    if nothing_staged {
        return Ok(false);
    }

    let commit_out = git_command()
        .args(["commit", "-m", message])
        .current_dir(project_dir)
        .output()
        .context("failed to run git commit")?;

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
    new_lock: &ResolvedLockFile,
    old_lock: Option<&ResolvedLockFile>,
) -> anyhow::Result<()> {
    use crate::git::git_command;

    // Dirty check (scoped to the project repo).
    let status_out = git_command()
        .args(["status", "--porcelain"])
        .current_dir(project_dir)
        .output()
        .context("failed to run git status")?;
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
/// `ctx` is the already-resolved invocation context (with `--project` baked
/// in when passed). Handlers must not re-resolve.
///
/// Pure git SHA snapshot — no integration hooks fire here. Install/build
/// hooks are part of activation (`rwv activate`), since the trigger for
/// ecosystem-lockfile refresh is workspace membership change, not
/// cross-repo snapshot.
pub fn lock(ctx: &WorkspaceContext, dirty: bool, commit: bool) -> anyhow::Result<()> {
    // Cross-verb mutex (Correction 1, COVERAGE), scoped to `--commit`. Writing
    // the working-tree `rwv.lock` (plain `rwv lock`) is benign — it is the
    // auto-relock's own input and the carve-out in Correction 3 treats a dirty
    // `rwv.lock` as non-dirt. But `--commit` writes a NEW lock commit into the
    // project repo, mutating the same history a mid-op replay is reconciling;
    // refuse while an op involves this workspace, via the shared op-state guard.
    if commit {
        crate::op_state::check_no_op_in_progress(&[ctx.active_path()])?;
    }

    let (project_name, workweave_name, workweave_dir) = match &ctx.checkout {
        Checkout::Primary { .. } => {
            let name = ctx.require_active_project_on_disk()?.clone();
            (name, None, None)
        }
        Checkout::Workweave { name, dir, project } => {
            (project.clone(), Some(name.clone()), Some(dir.clone()))
        }
    };

    let project_dir = ctx
        .active_path()
        .join("projects")
        .join(project_name.as_str());
    // Use the parse-free loader: `rwv.lock` is derived state and must be
    // regenerable *over* a corrupted or conflict-markered existing lock.
    // `from_dir` hard-parses `rwv.lock` and errors on conflict markers,
    // which turned the naive recovery sequence (`rwv lock; git add; git
    // rebase --continue`) into a footgun that silently committed the
    // markers — see fo-yk0rlj bug 2. `lock()` never reads `project.lock`
    // (it regenerates from manifest tips and reads the old lock
    // tolerantly further down via `.ok()`), so skipping the parse
    // costs nothing and closes the recovery gap.
    let project = Project::from_dir_skip_lock(&project_dir)
        .with_context(|| format!("failed to load project '{}'", project_name))?;

    let workweave_pair = workweave_name.as_ref().zip(workweave_dir.as_deref());
    let lock = generate_lock(&project.manifest, ctx.primary_path(), workweave_pair, dirty)?;

    let lock_path = project_dir.join("rwv.lock");
    // Read old lock before overwriting so the commit message can list what
    // changed. Resolve the raw lock against on-disk repos so the diff
    // computed by `commit_lock_file` is a canonical-SHA comparison rather
    // than a string comparison of possibly-tag-form values.
    let old_lock = if commit {
        LockFile::from_path(&lock_path)
            .ok()
            .map(|raw| raw.resolve_versions(ctx.primary_path()).0)
    } else {
        None
    };
    write_lock(&lock, &lock_path)?;

    eprintln!("Wrote {}", lock_path.display());

    if commit {
        commit_lock_file(&project_dir, &lock, old_lock.as_ref())?;
    }

    Ok(())
}
