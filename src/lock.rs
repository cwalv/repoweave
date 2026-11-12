//! Lock logic: record where member checkouts are as `rwv.lock`'s pins.

use crate::alignment::{Accounting, Aligner, Member, Mode, Pin, ProjectStanding, Target};
use crate::diagnosis::{Judged, Report, State};
use crate::manifest::{
    LockEntry, LockFile, Manifest, Project, RepoPath, ResolvedLockEntry, ResolvedLockFile,
};
use crate::repin::Naming;
use crate::selector::RepoFilter;
use crate::vcs::{
    project_vcs, vcs_for, HeadAttachment, RawRevisionId, Relation, ResolvedRevisionId, Vcs,
};
use crate::workspace::{project_dir, Checkout, WorkspaceContext};
use anyhow::Context;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Build a commit message summarizing which repos' lock entries changed or were dropped,
/// and naming `rwv.toml` when `records_manifest` says it is in the commit.
///
/// When `old_lock` is `None` (fresh lock), all repos appear in the list.
/// Otherwise, only repos whose version changed are listed.
fn build_commit_message(
    new_lock: &LockFile,
    old_lock: Option<&LockFile>,
    records_manifest: bool,
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
    let dropped: Vec<_> = old_lock
        .into_iter()
        .flat_map(|old| old.iter_entries())
        .filter(|(path, _)| new_lock.get_entry(path).is_none())
        .map(|(path, _)| path)
        .collect();

    let n = changed.len();
    let mut summary = Vec::new();
    if n > 0 {
        summary.push(format!(
            "refresh {} repo{}",
            n,
            if n == 1 { "" } else { "s" }
        ));
    }
    match dropped.as_slice() {
        [] => {}
        [only] => summary.push(format!("drop {only}")),
        many => summary.push(format!("drop {} repos", many.len())),
    }
    if records_manifest {
        summary.push(format!("record {}", Manifest::FILE_NAME));
    }
    if summary.is_empty() {
        return "lock: no changes".to_string();
    }

    let mut msg = format!("lock: {}", summary.join(", "));
    msg.push_str("\n\n");
    for (path, entry) in &changed {
        msg.push_str(&format!(
            "  - {}: {}\n",
            path,
            entry.version.short_display()
        ));
    }
    for path in &dropped {
        msg.push_str(&format!("  - {path}: dropped\n"));
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
/// When `dirty` is false, each repo is checked for uncommitted tracked changes
/// (untracked files cannot change HEAD) and an error is returned if any are found. When `dirty` is true, the check
/// is skipped.
pub fn generate_lock(
    manifest: &Manifest,
    workspace_root: &Path,
    workweave_dir: Option<&Path>,
    dirty: bool,
) -> anyhow::Result<ResolvedLockFile> {
    let mut repositories = BTreeMap::new();

    if !dirty {
        let mut report = Report::default();
        for (repo_path, entry) in &manifest.repositories {
            let checkout =
                crate::workspace::member_checkout_dir(repo_path, workspace_root, workweave_dir);
            let vcs = vcs_for(entry.vcs_type);
            let tracked = vcs.tracked_dirty_file_names(checkout.path())?;
            if !tracked.is_empty() {
                report.add(
                    State::EditsDirty
                        .of(repo_path)
                        .listing(tracked)
                        .checkout(checkout.path())
                        .judge(vcs.as_ref()),
                );
            }
        }
        report.into_result()?;
    }

    for (repo_path, entry) in &manifest.repositories {
        let checkout =
            crate::workspace::member_checkout_dir(repo_path, workspace_root, workweave_dir);
        let repo_dir = checkout.path();

        let vcs = vcs_for(entry.vcs_type);

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
            .map_err(|e| crate::untokened!(EnvironmentPassthrough, "{}: {:#}", repo_path, e))?
        {
            HeadAttachment::Attached(_) | HeadAttachment::Unborn(_) => None,
            HeadAttachment::Detached(d) => Some(d),
        };

        // Unborn HEAD: `head_revision` returns a `CommandFailed` whose stderr
        // contains "unborn HEAD" — return it as-is; the message already names
        // the repo path and the fix.
        let version = vcs
            .head_revision(repo_dir)
            .map_err(|e| crate::untokened!(EnvironmentPassthrough, "{}: {:#}", repo_path, e))?;

        // Detached HEAD: warn but do not refuse. Lock runs inside automation
        // (sync auto-relock) so a hard gate would break legitimate flows.
        // Warning text follows the house refusal pattern: name the state,
        // name the consequence, name the next verb. The SHA comes from the
        // witness, which is the value that established the state, rather than
        // from `version` — which may carry a tag as its display form.
        if let Some(d) = detached {
            let short = d.at().id().short();
            eprintln!(
                "warning: pinning detached HEAD {short} in {repo_path}: \
                 no branch names this commit; a later fetch materializes it attached to \
                 a branch at this pin, never detached. \
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

/// The warning for a lock entry pinned by name, spelled once for every verb
/// that reads a lock and proceeds on it.
pub(crate) fn pinned_by_name_warning(
    entry: impl std::fmt::Display,
    version: &crate::vcs::RawRevisionId,
) -> String {
    format!(
        "{entry}: lock pins `{version}`, a name rather than a commit id; it resolves to \
         wherever that name points now, and to nothing once it is gone. `rwv lock` \
         records the commit id"
    )
}

/// Write a lock file as JSON into `project_dir`.
///
/// Generic over any serializable lock form so callers can write either a
/// raw [`LockFile`] (round-trip) or a [`ResolvedLockFile`] (post-lock
/// generation). Both serialize as the same JSON shape — a single string
/// per `version`.
pub fn write_lock_in<L: serde::Serialize>(lock: &L, project_dir: &Path) -> anyhow::Result<()> {
    let mut json = serde_json::to_string_pretty(lock).context("failed to serialize lock file")?;
    json.push('\n');
    let path = LockFile::path_in(project_dir);
    path.publish(json.as_bytes())
        .with_context(|| format!("failed to write {path}"))
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
    naming: &Naming,
) -> anyhow::Result<bool> {
    stage_and_commit(vcs, project_dir, &[LockFile::FILE_NAME], message, naming)
}

/// Stage `paths` (relative to `project_dir`) and commit from `project_dir`.
///
/// Returns `Ok(false)` when nothing ended up staged, `Ok(true)` when a
/// commit was made. Every path must exist and be committable — callers
/// derive the list from `git status`, which reports neither absent nor
/// ignored files. A refused commit leaves `paths` unstaged again.
fn stage_and_commit(
    vcs: &dyn Vcs,
    project_dir: &Path,
    paths: &[&str],
    message: &str,
    naming: &Naming,
) -> anyhow::Result<bool> {
    vcs.stage_paths(project_dir, paths)
        .context("failed to stage paths for the lock commit")?;
    let staged = LockFile::from_state(&LockFile::path_in(project_dir)).ok();

    match crate::repin::commit(vcs, project_dir, paths, message, staged.as_ref(), naming) {
        Ok(committed) => Ok(committed),
        Err(refused) => {
            let refused = anyhow::Error::from(refused).context("failed to commit the lock");
            Err(match vcs.unstage_paths(project_dir, paths) {
                Ok(()) => refused,
                Err(e) => refused.context(format!(
                    "{} left staged: unstaging after the refused commit failed ({e:#})",
                    paths.join(", ")
                )),
            })
        }
    }
}

/// What a lock commit is recorded under.
pub enum CommitMessage {
    /// A summary of which repos' lock entries changed.
    Generated,
    Given(String),
}

impl CommitMessage {
    pub fn from_flag(message: Option<String>) -> Self {
        message.map_or(Self::Generated, Self::Given)
    }
}

/// The tracked files of the project repo with uncommitted changes outside
/// `rwv.lock`, `rwv.toml` and `authored`: edits no rwv commit carries.
pub(crate) fn unrelated_edits(
    vcs: &dyn Vcs,
    project_dir: &Path,
    authored: &BTreeSet<String>,
) -> anyhow::Result<Vec<String>> {
    Ok(vcs
        .tracked_dirty_file_names(project_dir)
        .context("failed to read project repo status")?
        .into_iter()
        .filter(|path| {
            path.as_str() != LockFile::FILE_NAME
                && path.as_str() != Manifest::FILE_NAME
                && !authored.contains(path.as_str())
        })
        .collect())
}

/// Say which edits a commit of the project repo left as they are.
pub(crate) fn report_left_edits(
    vcs: &dyn Vcs,
    project: &crate::manifest::ProjectName,
    project_dir: &Path,
    authored: &BTreeSet<String>,
) -> anyhow::Result<()> {
    let left = unrelated_edits(vcs, project_dir, authored)?;
    if left.is_empty() {
        return Ok(());
    }
    let staged = vcs
        .staged_paths(project_dir)
        .context("failed to read project repo status")?;
    let edited = vcs
        .unstaged_paths(project_dir)
        .context("failed to read project repo status")?;
    let named: Vec<String> = left
        .iter()
        .map(|path| {
            let how = match (staged.contains(path), edited.contains(path)) {
                (true, true) => "staged and edited",
                (true, false) => "staged",
                _ => "edited",
            };
            format!("{path} ({how})")
        })
        .collect();
    eprintln!(
        "{}: uncommitted files left as they are: {}",
        crate::workspace::project_rel_path(project),
        named.join(", ")
    );
    Ok(())
}

/// Commit `rwv.lock` from `project_dir`, together with a dirty `rwv.toml`
/// and whatever of `authored` the run actually rewrote.
///
/// The lock is derived from the manifest and the member tips, so a manifest
/// edit it reflects belongs in the same commit. `authored` is the owned path
/// set of an intent verb that regenerated managed content against the same
/// tips this lock records; the two changes are one change and belong in one
/// commit. It is empty for a verb that authors nothing.
///
/// Changes outside that set are left as they are, staged or not, and named.
fn commit_lock_file(
    vcs: &dyn Vcs,
    pending: &PendingLockCommit,
    authored: &BTreeSet<String>,
) -> anyhow::Result<()> {
    let project_dir = pending.project_dir.as_path();
    // Files omitted from the dirty set — absent, or ignored by operator
    // policy — are omitted from the commit.
    let dirty = vcs
        .dirty_file_names(project_dir)
        .context("failed to read project repo status")?;
    let carried = |path: &str| {
        path == LockFile::FILE_NAME || path == Manifest::FILE_NAME || authored.contains(path)
    };

    let mut paths = vec![LockFile::FILE_NAME];
    paths.extend(
        dirty
            .iter()
            .map(String::as_str)
            .filter(|path| *path != LockFile::FILE_NAME && carried(path)),
    );

    let message = match &pending.message {
        CommitMessage::Generated => build_commit_message(
            &pending.new_lock,
            pending.old_lock.as_ref(),
            paths.contains(&Manifest::FILE_NAME),
        ),
        CommitMessage::Given(message) => message.clone(),
    };
    if stage_and_commit(vcs, project_dir, &paths, &message, &pending.naming)? {
        eprintln!("Committed {}", paths.join(", "));
    } else {
        eprintln!("Lock unchanged, nothing to commit.");
    }
    report_left_edits(vcs, &pending.project_name, project_dir, authored)
}

/// Execute `rwv lock` for the current workspace context.
///
/// Records each member `acting` selects at the commit its checkout is at,
/// moving a pin forward only: a member with no pin keeps none, and one that
/// is behind or diverged from its pin is refused, `--repo` included.
/// Every member is judged before anything is written.
///
/// When `dirty` is true, the uncommitted-changes check is skipped.
/// When `commit` is set, the lock file is staged and committed after writing.
/// `ctx` is the already-resolved invocation context (with `--project` baked
/// in when passed). Handlers must not re-resolve.
///
/// Pure snapshot: no integration hooks fire here. Install/build hooks are
/// part of activation (`rwv activate`), since the trigger for
/// ecosystem-lockfile refresh is workspace membership change, not
/// cross-repo snapshot.
pub fn lock(
    ctx: &WorkspaceContext,
    acting: &RepoFilter,
    dirty: bool,
    commit: Option<CommitMessage>,
) -> anyhow::Result<()> {
    crate::branch::require_record(ctx)?;
    let relocking = Relocking {
        verb: "lock",
        acting,
        dirty,
        first_pins: FirstPins::Withheld,
        unaligned: Unaligned::Refused,
        whole: WholeFile::default(),
        op: OpHeld::None,
        by_name: None,
    };
    if commit.is_some() {
        if let Some(whole) = crate::lock_commit::plan(ctx, &relocking)? {
            return crate::lock_commit::run(ctx, &relocking, whole, commit);
        }
    }
    match relock(ctx, &relocking, commit)? {
        Some(pending) => commit_project_lock(&pending, &BTreeSet::new()),
        None => Ok(()),
    }
}

/// Whether a member with no pin is given one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FirstPins {
    /// It keeps none: only a verb that names a commit gives a first pin.
    Withheld,
    /// It takes the commit its checkout is at.
    Taken,
}

/// What a relock does with a member that is not where its plan puts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unaligned {
    /// The relock is refused.
    Refused,
    /// The member keeps its pin and the relock reports where it sits.
    Left,
}

/// One relock: the verb running it, the members it records and the rules it
/// records them under.
pub(crate) struct Relocking<'a> {
    pub(crate) verb: &'static str,
    pub(crate) acting: &'a RepoFilter,
    pub(crate) dirty: bool,
    pub(crate) first_pins: FirstPins,
    pub(crate) unaligned: Unaligned,
    pub(crate) whole: WholeFile<'a>,
    pub(crate) op: OpHeld,
    /// Members the verb names to re-pin, backward included.
    pub(crate) by_name: Option<&'a BTreeSet<RepoPath>>,
}

/// Whether the run committing the relock holds an op of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpHeld {
    None,
    /// The relock is a step of it, so its op is no other run's in flight.
    ByThisRun,
}

/// What a relock that commits the whole of `rwv.toml` adds to the plain one.
#[derive(Default)]
pub(crate) struct WholeFile<'a> {
    /// Members a setter's step moves first: judged and recorded by it.
    pub(crate) skipping: Option<&'a BTreeSet<RepoPath>>,
    /// Judge, write nothing.
    pub(crate) judge_only: bool,
}

/// How a member's new pin stands to its old one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum How {
    First,
    Same,
    Forward,
    ByName,
}

struct Recorded {
    path: RepoPath,
    dir: PathBuf,
    vcs_type: crate::manifest::VcsType,
    was: Option<RawRevisionId>,
    to: ResolvedRevisionId,
    how: How,
}

/// What judging one member of a relock found: the lines to print, and the
/// pin to record, if it records one.
struct Judgment {
    lines: Vec<Judged>,
    record: Option<Recorded>,
}

impl Judgment {
    fn kept(lines: Vec<Judged>) -> Self {
        Self {
            lines,
            record: None,
        }
    }
}

/// The line for a pin that would move to a commit not descending from the one
/// it holds.
///
/// `repin` is the revision `rwv member pin` would pin the member at, where it
/// would succeed as printed; with none, no choice is printed.
pub(crate) fn backwards_line(
    repo: &RepoPath,
    dir: &Path,
    vcs: &dyn Vcs,
    subject: impl std::fmt::Display,
    repin: Option<&str>,
) -> Judged {
    let judging = State::PinBackwards
        .of(repo)
        .member()
        .checkout(dir)
        .bind("<m>", repo.as_str())
        .subject(subject);
    match repin {
        Some(rev) => judging.bind("<rev>", rev),
        None => judging.only(&[]),
    }
    .judge(vcs)
}

/// How a commit that does not descend from the pin stands to it, in words.
pub(crate) fn backwards_subject(
    vcs: &dyn Vcs,
    dir: &Path,
    what: &str,
    head: &ResolvedRevisionId,
    pin: &ResolvedRevisionId,
    relation: Relation,
) -> String {
    match relation {
        Relation::Behind => {
            let by = vcs.count_commits_in_range(dir, head, pin).unwrap_or(0);
            format!(
                "{what} {} is {by} commit(s) behind the pin {}",
                head.id().short(),
                pin.id().short()
            )
        }
        _ => format!(
            "{what} {} has diverged from the pin {}",
            head.id().short(),
            pin.id().short()
        ),
    }
}

/// Judge one member of a relock: what its checkout allows, and what it records.
#[allow(clippy::too_many_arguments)]
fn judge_member(
    relocking: &Relocking<'_>,
    aligner: &Aligner,
    manifest: &Manifest,
    old: Option<&LockFile>,
    path: &RepoPath,
    entry: &crate::manifest::RepoEntry,
    dir: &Path,
    aligns: bool,
    refusals: &mut Report,
) -> anyhow::Result<Judgment> {
    let vcs = vcs_for(entry.vcs_type);
    let member = Member {
        path,
        entry,
        dir,
        vcs: vcs.as_ref(),
    };
    let plan = aligner
        .plan(&member, old)
        .map_err(|e| crate::untokened!(EnvironmentPassthrough, "{path}: {e:#}"))?;
    let mut lines: Vec<Judged> = Vec::new();

    if !relocking.dirty {
        let tracked = vcs.tracked_dirty_file_names(dir)?;
        if !tracked.is_empty() {
            refusals.add(
                State::EditsDirty
                    .of(path)
                    .member()
                    .listing(tracked)
                    .checkout(dir)
                    .by_verb(relocking.verb)
                    .judge(vcs.as_ref()),
            );
        }
    }

    if let Some(off) = aligner.refusal(&member, &plan, path)? {
        match relocking.unaligned {
            Unaligned::Refused => refusals.add(off),
            Unaligned::Left => {
                lines.push(off);
                return Ok(Judgment::kept(lines));
            }
        }
    }

    let was = old
        .and_then(|lock| lock.get_entry(path))
        .map(|e| e.version.clone());
    if let HeadAttachment::Unborn(_) = vcs.head_attachment(dir)? {
        lines.extend(Aligner::pin_note(&member, &plan, path));
        return Ok(Judgment::kept(lines));
    }
    let head = vcs
        .head_revision(dir)
        .map_err(|e| crate::untokened!(EnvironmentPassthrough, "{path}: {e:#}"))?;

    let how = match &plan.pin {
        Pin::None
            if relocking.first_pins == FirstPins::Withheld
                || matches!(plan.target, Target::AsFound) =>
        {
            lines.extend(aligner.status_lines(&member, old, path)?);
            lines.extend(Aligner::pin_note(&member, &plan, path));
            return Ok(Judgment::kept(lines));
        }
        Pin::None => How::First,
        Pin::Unreachable | Pin::At(_) => {
            let backwards =
                match &plan.pin {
                    Pin::At(pin) => match vcs.relation(dir, &head, pin)? {
                        Relation::Even => None,
                        Relation::Ahead => None,
                        relation @ (Relation::Behind | Relation::Diverged) => Some(
                            backwards_subject(vcs.as_ref(), dir, "HEAD", &head, pin, relation),
                        ),
                    },
                    _ => Some(format!(
                        "the pin {} is in no ref here",
                        was.as_ref().map_or("", |v| v.short_display())
                    )),
                };
            match backwards {
                None if matches!(&plan.pin, Pin::At(pin) if *pin == head) => How::Same,
                None => How::Forward,
                Some(_) if relocking.by_name.is_some_and(|named| named.contains(path)) => {
                    How::ByName
                }
                Some(subject) => {
                    let candidate = with_pins(manifest, old, [(path, &head)]);
                    let repin =
                        crate::member_pin::pin_refusals(aligner, &member, &candidate, aligns)?
                            .is_empty()
                            .then_some("HEAD");
                    refusals.add(backwards_line(path, dir, vcs.as_ref(), subject, repin));
                    return Ok(Judgment::kept(lines));
                }
            }
        }
    };
    Ok(Judgment {
        lines,
        record: Some(Recorded {
            path: path.clone(),
            dir: dir.to_path_buf(),
            vcs_type: entry.vcs_type,
            was,
            to: head,
            how,
        }),
    })
}

/// The project the invocation acts on, and the workweave directory when it
/// runs in one.
pub(crate) fn scope(
    ctx: &WorkspaceContext,
) -> anyhow::Result<(crate::manifest::ProjectName, Option<PathBuf>)> {
    Ok(match &ctx.checkout {
        Checkout::Primary { .. } => (ctx.require_active_project_on_disk()?.clone(), None),
        Checkout::Workweave { dir, project, .. } => {
            (project.clone(), Some(dir.as_path().to_path_buf()))
        }
    })
}

/// The lock as it stands: `None` where there is none, and where it does not
/// parse, which pins nothing.
pub(crate) fn read_old_lock(
    lock_path: &crate::state_file::StatePath,
    verb: &str,
) -> Option<LockFile> {
    if !lock_path.exists() {
        return None;
    }
    match LockFile::from_state(lock_path) {
        Ok(lock) => Some(lock),
        Err(e) => {
            eprintln!(
                "rwv {verb}: {lock_path} does not parse, so it pins nothing and is written \
                 afresh: {e:#}"
            );
            None
        }
    }
}

/// Refuse a detached workspace or project repo: there is no branch to record
/// the pins on.
pub(crate) fn refuse_detached(
    aligner: &Aligner,
    _project: &crate::manifest::ProjectName,
) -> anyhow::Result<()> {
    if aligner.standing != ProjectStanding::Detached && aligner.mode != Mode::Detached {
        return Ok(());
    }
    Err(aligner
        .project_detached()?
        .judge(project_vcs().as_ref())
        .refused())
}

/// Record the members `relocking` selects in `rwv.lock`, every other pin as it
/// was. Every member is judged first, and the relock refuses with all that it
/// found before anything is written.
///
/// Returns the pending commit when `commit` is set; the caller finishes with
/// [`commit_project_lock`], after any authoring pass of its own.
pub(crate) fn relock(
    ctx: &WorkspaceContext,
    relocking: &Relocking<'_>,
    commit: Option<CommitMessage>,
) -> anyhow::Result<Option<PendingLockCommit>> {
    // `--commit` writes a new lock commit into the history a mid-op replay is
    // reconciling; a plain lock write is the auto-relock's own input and is
    // not refused.
    if commit.is_some() && relocking.op == OpHeld::None {
        crate::op_state::check_no_op_in_progress(&[ctx.active_path()])?;
    }

    let (project_name, workweave_dir) = scope(ctx)?;

    let project_dir = project_dir(ctx.active_path(), &project_name);
    // The lock is read below, tolerantly: one that does not parse must not
    // stop the verb that writes it afresh.
    let project = Project::from_dir_skip_lock(&project_dir, project_name.clone())
        .with_context(|| format!("failed to load project '{}'", project_name))?;
    let lock_path = LockFile::path_in(&project_dir);
    let old = read_old_lock(&lock_path, relocking.verb);

    let aligner = Aligner::open(
        ctx.active_path(),
        &project_name,
        Accounting::scan(ctx.primary_path())?,
    )?;
    refuse_detached(&aligner, &project_name)?;

    let mut refusals = Report::default();
    let mut failures: Vec<String> = Vec::new();
    let mut recorded: Vec<Recorded> = Vec::new();
    let mut lines: Vec<Judged> = Vec::new();
    let mut acted = 0usize;
    for (path, entry) in project.manifest.iter_entries() {
        if !relocking.acting.matches(path, entry.role) {
            continue;
        }
        acted += 1;
        if relocking
            .whole
            .skipping
            .is_some_and(|skipping| skipping.contains(path))
        {
            continue;
        }
        let checkout = crate::workspace::member_checkout_dir(
            path,
            ctx.primary_path(),
            workweave_dir.as_deref(),
        );
        if !checkout.path().exists() {
            failures.push(format!(
                "{path}: clone missing on disk; run `rwv update` to re-materialize it, then \
                 re-run `rwv {}`",
                relocking.verb
            ));
            continue;
        }
        let aligns = workweave_dir.is_none() || checkout.is_workweave_slot();
        match judge_member(
            relocking,
            &aligner,
            &project.manifest,
            old.as_ref(),
            path,
            entry,
            checkout.path(),
            aligns,
            &mut refusals,
        ) {
            Ok(judgment) => {
                lines.extend(judgment.lines);
                recorded.extend(judgment.record);
            }
            Err(e) => failures.push(format!("{e:#}")),
        }
    }
    if acted == 0 && !relocking.acting.is_empty() {
        crate::refuse!(
            crate::refusal::RefusalKind::RepoNotInManifest,
            "--repo selects no member of the manifest"
        );
    }
    if !failures.is_empty() {
        if refusals.is_empty() {
            return Err(crate::untokened!(Tally, "{}", failures.join("\n")));
        }
        for failure in &failures {
            refusals.note(failure);
        }
    }
    refusals.into_result()?;
    if relocking.whole.judge_only {
        return Ok(None);
    }

    let new = with_pins(
        &project.manifest,
        old.as_ref(),
        recorded.iter().map(|one| (&one.path, &one.to)),
    );
    write_lock_in(&new, &project_dir)?;

    eprintln!("Wrote {lock_path}");
    report_recorded(relocking, ctx, &project_name, &recorded);
    crate::entry_report::report(
        relocking.verb,
        ctx,
        &aligner,
        &project_dir,
        &project.manifest,
        false,
    );
    if !lines.is_empty() {
        let mut report = Report::default();
        for line in lines {
            report.add(line);
        }
        eprint!("{}", report.render(relocking.verb));
    }

    Ok(commit.map(|message| PendingLockCommit {
        project_name,
        project_dir,
        new_lock: new,
        old_lock: old,
        message,
        naming: Naming::Unnamed,
    }))
}

/// `old` restricted to the members of `manifest`, each of `pins` recorded at
/// its commit under the url the manifest gives it.
pub(crate) fn with_pins<'a>(
    manifest: &Manifest,
    old: Option<&LockFile>,
    pins: impl IntoIterator<Item = (&'a RepoPath, &'a ResolvedRevisionId)>,
) -> LockFile {
    let mut new = LockFile {
        repositories: BTreeMap::new(),
    };
    for (path, _) in manifest.iter_entries() {
        if let Some(held) = old.and_then(|lock| lock.get_entry(path)) {
            new.insert_entry(path.clone(), held.clone());
        }
    }
    for (path, to) in pins {
        let entry = manifest
            .get_entry(path)
            .expect("a pinned member is a member of the manifest");
        new.insert_entry(
            path.clone(),
            LockEntry {
                vcs_type: entry.vcs_type,
                url: entry.url.clone(),
                version: RawRevisionId::new(to.id().as_str()),
            },
        );
    }
    new
}

/// The line for a pin moved by name from `was` to `to`.
fn repinned_line(verb: &str, path: &RepoPath, was: &str, to: &str) -> String {
    format!("rwv {verb}: {path}: re-pinned {was} to {to}, by name")
}

/// Report a pin moved by name from `was` to `to` where `to` does not descend
/// from `was`.
pub(crate) fn report_backward_repin(
    vcs: &dyn Vcs,
    dir: &Path,
    verb: &str,
    path: &RepoPath,
    was: &ResolvedRevisionId,
    to: &ResolvedRevisionId,
) -> anyhow::Result<()> {
    if was != to
        && matches!(
            vcs.relation(dir, to, was)?,
            Relation::Behind | Relation::Diverged
        )
    {
        eprintln!(
            "{}",
            repinned_line(verb, path, was.id().short(), to.id().short())
        );
    }
    Ok(())
}

/// Say what the relock did that the lock file does not show: a first pin, a
/// pin moved backward by name, and a forward move over commits another
/// project's lock does not pin.
fn report_recorded(
    relocking: &Relocking<'_>,
    ctx: &WorkspaceContext,
    project: &crate::manifest::ProjectName,
    recorded: &[Recorded],
) {
    let verb = relocking.verb;
    let mut live = None;
    for one in recorded {
        let path = &one.path;
        let to = one.to.id().short();
        match one.how {
            How::Same => {}
            How::First => eprintln!("rwv {verb}: {path}: pinned at {to}; it had no pin"),
            How::ByName => eprintln!(
                "{}",
                repinned_line(
                    verb,
                    path,
                    one.was.as_ref().map_or("", |was| was.short_display()),
                    to
                )
            ),
            How::Forward => {
                let vcs = vcs_for(one.vcs_type);
                let Some(was) = one
                    .was
                    .as_ref()
                    .and_then(|was| vcs.resolve_revision(&one.dir, was.as_str()).ok())
                else {
                    continue;
                };
                let Ok(unpushed) = vcs.unpushed_beyond(&one.dir, &one.to, &was) else {
                    continue;
                };
                if unpushed.is_empty() {
                    continue;
                }
                let live = live.get_or_insert_with(|| {
                    crate::pins::live_pins(ctx.primary_path()).unwrap_or_default()
                });
                let others: BTreeSet<String> = live
                    .get(path)
                    .into_iter()
                    .flatten()
                    .filter(|pin| &pin.project != project)
                    .map(|pin| pin.project.to_string())
                    .collect();
                if others.is_empty() {
                    continue;
                }
                eprintln!(
                    "rwv {verb}: {path}: pins {} unpushed commit(s) beyond {}, which the lock \
                     of project {} does not pin: {}",
                    unpushed.len(),
                    was.id().short(),
                    others.into_iter().collect::<Vec<_>>().join(", "),
                    unpushed
                        .iter()
                        .map(|c| c.id.short())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }
}

/// A written `rwv.lock` and what committing it needs.
///
/// [`lock`] is the whole of `rwv lock`; a verb that also authors managed
/// content splits it, because the authoring pass has to run between the
/// write and the commit for both changes to land together.
pub(crate) struct PendingLockCommit {
    project_name: crate::manifest::ProjectName,
    project_dir: PathBuf,
    new_lock: LockFile,
    old_lock: Option<LockFile>,
    message: CommitMessage,
    naming: Naming,
}

impl PendingLockCommit {
    pub(crate) fn new(
        project_name: crate::manifest::ProjectName,
        project_dir: PathBuf,
        new_lock: LockFile,
        old_lock: Option<LockFile>,
        message: CommitMessage,
    ) -> Self {
        Self {
            project_name,
            project_dir,
            new_lock,
            old_lock,
            message,
            naming: Naming::Unnamed,
        }
    }

    /// The commit re-pins `members` by name, as `verb`.
    pub(crate) fn by_name(
        mut self,
        verb: &'static str,
        members: impl IntoIterator<Item = RepoPath>,
    ) -> Self {
        self.naming = Naming::by_name(verb, members);
        self
    }
}

/// Commit a written `rwv.lock`, carrying whatever of `authored` the caller
/// rewrote against the same tips.
pub(crate) fn commit_project_lock(
    pending: &PendingLockCommit,
    authored: &BTreeSet<String>,
) -> anyhow::Result<()> {
    commit_lock_file(project_vcs().as_ref(), pending, authored)
}
