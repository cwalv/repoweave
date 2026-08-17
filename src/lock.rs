//! Lock logic: snapshot repo HEADs into `rwv.lock`.

use crate::manifest::{LockFile, Manifest, Project, ResolvedLockEntry, ResolvedLockFile};
use crate::vcs::{project_vcs, vcs_for, HeadAttachment, ResolvedRevisionId, Vcs};
use crate::workspace::{project_dir, Checkout, WorkspaceContext};
use anyhow::Context;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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
/// version instead of the raw SHA — unless the project has asked to forgo
/// tag names, in which case every entry records the commit id. See
/// [`crate::manifest::LockConfig`].
pub fn generate_lock(
    manifest: &Manifest,
    workspace_root: &Path,
    workweave_dir: Option<&Path>,
    dirty: bool,
) -> anyhow::Result<ResolvedLockFile> {
    let mut repositories = BTreeMap::new();

    for (repo_path, entry) in &manifest.repositories {
        let checkout =
            crate::workspace::member_checkout_dir(repo_path, workspace_root, workweave_dir);
        let repo_dir = checkout.path();

        let vcs = vcs_for(entry.vcs_type);

        // Check for uncommitted changes unless --dirty is set.
        if !dirty && vcs.has_uncommitted_changes(repo_dir)? {
            anyhow::bail!(
                "repo {} has uncommitted changes; commit or use --dirty to override",
                repo_path
            );
        }

        // Ask what HEAD *is* before asking what it resolves to.
        //
        // "On no branch" is four situations, not one: detached, unborn,
        // not-a-repo, and an unreadable ref database. `head_attachment` is
        // total over the three that are states and returns the two that are
        // errors as errors, so the `Err` arm below must refuse and name the
        // repo. Collapsing it to a `.ok()` would leave an operator whose
        // member directory is not a repo at all indistinguishable from one
        // who detached on purpose.
        //
        // Unborn deliberately warns about nothing: an unborn HEAD cannot be
        // pinned at all, and `head_revision` refuses it by name two lines
        // down (that refusal is keyed off this same classification). Warning
        // about the branch first would print advice ahead of the error that
        // says the commit does not exist.
        let detached = match vcs
            .head_attachment(repo_dir)
            .map_err(|e| anyhow::anyhow!("{}: {}", repo_path, e))?
        {
            HeadAttachment::Attached(_) | HeadAttachment::Unborn(_) => None,
            HeadAttachment::Detached(d) => Some(d),
        };

        // `head_revision` resolves to the canonical SHA and, when a tag
        // points at HEAD, also fills in the tag display form — so the lock
        // serializes as the tag name when available and the canonical SHA
        // otherwise.
        //
        // Unborn HEAD: `head_revision` returns a `CommandFailed` whose stderr
        // contains "unborn HEAD" — return it as-is; the message already names
        // the repo path and the fix.
        let version = vcs
            .head_revision(repo_dir)
            .map_err(|e| anyhow::anyhow!("{}: {}", repo_path, e))?;

        // Applied here rather than inside `head_revision`: which form to
        // record is a property of the project keeping the lock, not of the
        // VCS being asked what HEAD is.
        let version = if manifest.forgo_tag_names() {
            ResolvedRevisionId::from_canonical(version.as_str(), None)
        } else {
            version
        };

        // Detached HEAD: warn but do not refuse. Lock runs inside automation
        // (sync auto-relock) so a hard gate would break legitimate flows.
        // Warning text follows the house refusal pattern: name the state,
        // name the consequence, name the next verb. The SHA comes from the
        // witness, which is the value that established the state, rather than
        // from `version` — which may carry a tag as its display form.
        if let Some(d) = detached {
            let at = d.at().as_str();
            let short = &at[..at.len().min(7)];
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

    Ok(ResolvedLockFile { repositories })
}

/// Write a lock file as JSON into `project_dir`.
///
/// Generic over any serializable lock form so callers can write either a
/// raw [`LockFile`] (round-trip) or a [`ResolvedLockFile`] (post-lock
/// generation). Both serialize as the same JSON shape — a single string
/// per `version`.
pub fn write_lock<L: serde::Serialize>(lock: &L, path: &Path) -> anyhow::Result<()> {
    let mut json = serde_json::to_string_pretty(lock).context("failed to serialize lock file")?;
    json.push('\n');
    crate::state_file::StateFile::ProjectLock
        .publish_at(path, json.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))
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
    vcs: &dyn Vcs,
    project_dir: &Path,
    message: &str,
) -> anyhow::Result<bool> {
    stage_and_commit(vcs, project_dir, &[LockFile::FILE_NAME], message)
}

/// Stage `paths` (relative to `project_dir`) and commit from `project_dir`.
///
/// Returns `Ok(false)` when nothing ended up staged, `Ok(true)` when a
/// commit was made. Every path must exist and be committable — callers
/// derive the list from `git status`, which reports neither absent nor
/// ignored files.
fn stage_and_commit(
    vcs: &dyn Vcs,
    project_dir: &Path,
    paths: &[&str],
    message: &str,
) -> anyhow::Result<bool> {
    vcs.stage_paths(project_dir, paths)
        .context("failed to stage paths for the lock commit")?;

    if !vcs
        .has_staged_changes(project_dir)
        .context("failed to check staged changes")?
    {
        return Ok(false);
    }

    vcs.commit(project_dir, message)
        .context("failed to commit the lock")?;

    Ok(true)
}

/// Commit `rwv.lock` from `project_dir` with a multi-repo summary message,
/// together with whatever of `authored` the run actually rewrote.
///
/// `authored` is the owned path set of an intent verb that regenerated
/// managed content against the same tips this lock records; the two changes
/// are one change and belong in one commit. It is empty for a verb that
/// authors nothing, which reduces this to a lock-only commit.
///
/// Refuses if the project repo has uncommitted changes outside that set —
/// the auto-commit must not bundle unrelated work-in-progress.
fn commit_lock_file(
    vcs: &dyn Vcs,
    project_dir: &Path,
    new_lock: &ResolvedLockFile,
    old_lock: Option<&ResolvedLockFile>,
    authored: &BTreeSet<String>,
) -> anyhow::Result<()> {
    // Two questions, two answers, one predicate between them: everything
    // dirty says which owned paths to stage, and everything dirty *and
    // tracked* says whether anything unrelated is in the way. Files omitted
    // from the dirty set — absent, or ignored by operator policy — are
    // omitted from the commit for the same reason.
    let dirty = vcs
        .dirty_file_names(project_dir)
        .context("failed to read project repo status")?;
    let tracked_dirty = vcs
        .tracked_dirty_file_names(project_dir)
        .context("failed to read project repo status")?;

    let mut authored_paths: Vec<&str> = Vec::new();
    for path in &dirty {
        if path == LockFile::FILE_NAME {
            continue;
        }
        if let Some(owned) = authored.get(path.as_str()) {
            authored_paths.push(owned.as_str());
        }
    }
    let has_other_changes = tracked_dirty
        .iter()
        .any(|path| path != LockFile::FILE_NAME && !authored.contains(path.as_str()));
    if has_other_changes {
        anyhow::bail!(
            "project repo has uncommitted changes outside rwv.lock; \
             commit or stash them before using --commit"
        );
    }

    let mut paths = vec![LockFile::FILE_NAME];
    paths.extend(authored_paths);

    let message = build_commit_message(new_lock, old_lock);
    if stage_and_commit(vcs, project_dir, &paths, &message)? {
        eprintln!("Committed {}", paths.join(", "));
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
    match write_project_lock(ctx, dirty, commit)? {
        Some(pending) => commit_project_lock(&pending, &BTreeSet::new()),
        None => Ok(()),
    }
}

/// A written `rwv.lock` and what committing it needs.
///
/// [`lock`] is the whole of `rwv lock`; a verb that also authors managed
/// content splits it, because the authoring pass has to run between the
/// write and the commit for both changes to land together.
pub(crate) struct PendingLockCommit {
    project_dir: PathBuf,
    new_lock: ResolvedLockFile,
    old_lock: Option<ResolvedLockFile>,
}

/// Regenerate and write `rwv.lock` for the current workspace context.
///
/// Returns the pending commit when `commit` is set — the caller finishes
/// with [`commit_project_lock`], after any authoring pass of its own.
pub(crate) fn write_project_lock(
    ctx: &WorkspaceContext,
    dirty: bool,
    commit: bool,
) -> anyhow::Result<Option<PendingLockCommit>> {
    // Cross-verb mutex (Correction 1, COVERAGE), scoped to `--commit`. Writing
    // the working-tree `rwv.lock` (plain `rwv lock`) is benign — it is the
    // auto-relock's own input and the carve-out in Correction 3 treats a dirty
    // `rwv.lock` as non-dirt. But `--commit` writes a NEW lock commit into the
    // project repo, mutating the same history a mid-op replay is reconciling;
    // refuse while an op involves this workspace, via the shared op-state guard.
    if commit {
        crate::op_state::check_no_op_in_progress(&[ctx.active_path()])?;
    }

    let (project_name, workweave_dir) = match &ctx.checkout {
        Checkout::Primary { .. } => {
            let name = ctx.require_active_project_on_disk()?.clone();
            (name, None)
        }
        Checkout::Workweave { dir, project, .. } => (project.clone(), Some(dir.clone())),
    };

    let project_dir = project_dir(ctx.active_path(), project_name.as_str());
    // Use the parse-free loader: `rwv.lock` is derived state and must be
    // regenerable *over* a corrupted or conflict-markered existing lock.
    // `from_dir` hard-parses `rwv.lock` and errors on conflict markers,
    // which turned the naive recovery sequence (`rwv lock; git add; git
    // rebase --continue`) into a footgun that silently committed the
    // markers. `lock()` never reads `project.lock`
    // (it regenerates from manifest tips and reads the old lock
    // tolerantly further down via `.ok()`), so skipping the parse
    // costs nothing and closes the recovery gap.
    let project = Project::from_dir_skip_lock(&project_dir, project_name.clone())
        .with_context(|| format!("failed to load project '{}'", project_name))?;

    let lock = generate_lock(
        &project.manifest,
        ctx.primary_path(),
        workweave_dir.as_deref(),
        dirty,
    )?;

    let lock_path = project_dir.join(LockFile::FILE_NAME);
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

    Ok(commit.then_some(PendingLockCommit {
        project_dir,
        new_lock: lock,
        old_lock,
    }))
}

/// Commit a written `rwv.lock`, carrying whatever of `authored` the caller
/// rewrote against the same tips.
pub(crate) fn commit_project_lock(
    pending: &PendingLockCommit,
    authored: &BTreeSet<String>,
) -> anyhow::Result<()> {
    commit_lock_file(
        project_vcs().as_ref(),
        &pending.project_dir,
        &pending.new_lock,
        pending.old_lock.as_ref(),
        authored,
    )
}
