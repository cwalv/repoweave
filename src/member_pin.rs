//! `rwv member pin <repo> <rev>` — pin one member at a commit of the operator's
//! choosing, forward or backward, and align the member to it.

use crate::alignment::{Accounting, Aligner, Applied, DetachedBecause, Member, Pin, Target};
use crate::diagnosis::{Judged, Report, State};
use crate::lock::{self, CommitMessage};
use crate::manifest::{LockFile, Project, RepoPath};
use crate::refusal::RefusalKind;
use crate::vcs::{vcs_for, HeadAttachment};
use crate::workspace::{project_dir, WorkspaceContext};
use anyhow::Context;
use std::collections::BTreeSet;

/// What refuses pinning `member` where `candidate` pins it: a commit its
/// declared branch does not reach, and, where the pin moves the checkout
/// (`aligns`), a checkout that may not leave where it is. A detached HEAD
/// holding unpushed commits does not refuse: it is backed up first.
pub(crate) fn pin_refusals(
    aligner: &Aligner,
    member: &Member<'_>,
    candidate: &LockFile,
    aligns: bool,
) -> anyhow::Result<Vec<Judged>> {
    let mut lines = Vec::new();
    let plan = aligner.plan(member, Some(candidate))?;
    if let Target::Detached {
        why: DetachedBecause::NotOnDeclaredBranch { branch },
        ..
    } = &plan.target
    {
        lines.push(
            State::PinNotOnDeclaredBranch
                .of(member.path)
                .member()
                .subject(branch)
                .checkout(member.dir)
                .bind("<m>", member.path.as_str())
                .judge(member.vcs),
        );
    }
    let moving_detached_head = matches!(
        member.vcs.head_attachment(member.dir)?,
        HeadAttachment::Detached(_)
    );
    if aligns {
        for line in aligner.blocked(member, &plan, member.path)? {
            let held_for_backup = moving_detached_head
                && line.state() == State::AlignmentDetachedUnpushed
                && member.vcs.mid_operation(member.dir).is_none();
            if !held_for_backup {
                lines.push(line);
            }
        }
    }
    Ok(lines)
}

/// Execute `rwv member pin <repo> <rev> [--commit]`.
///
/// The pin is written by name, so it may move backward. Where the member
/// follows a declared branch, the commit must be one that branch or its
/// upstream reaches. A member left detached with commits no branch holds is
/// backed up before it moves. Every refusal is reported before the backup is
/// taken, the lock is written or the member moves.
///
/// `ctx` is the already-resolved invocation context (with `--project` baked
/// in when passed). Handlers must not re-resolve.
pub fn run_pin(
    repo: &str,
    rev: &str,
    commit: Option<CommitMessage>,
    ctx: &WorkspaceContext,
) -> anyhow::Result<()> {
    crate::branch::require_recorded(ctx)?;
    let (project_name, workweave_dir) = lock::scope(ctx)?;
    let dir_of_project = project_dir(ctx.active_path(), &project_name);
    crate::diagnosis::settle_project_drift(&project_name, &dir_of_project)?;
    let project = Project::from_dir_skip_lock(&dir_of_project, project_name.clone())
        .with_context(|| format!("failed to load project '{project_name}'"))?;
    let path = RepoPath::new(repo)?;
    let Some(entry) = project.manifest.get_entry(&path) else {
        crate::refuse!(
            RefusalKind::RepoNotInManifest,
            "path '{}' not found in manifest",
            path.as_str()
        );
    };

    let lock_path = LockFile::path_in(&dir_of_project);
    let old = lock::read_old_lock(&lock_path, "member pin");
    let aligner = Aligner::open(
        ctx.active_path(),
        &project_name,
        Accounting::scan(ctx.primary_path())?,
    )?;
    lock::refuse_detached(&aligner, &project_name)?;

    let checkout =
        crate::workspace::member_checkout_dir(&path, ctx.primary_path(), workweave_dir.as_deref());
    let dir = checkout.path();
    let aligns = workweave_dir.is_none() || checkout.is_workweave_slot();
    if !dir.exists() {
        return Err(crate::untokened!(
            Owed,
            "{path}: clone missing on disk; run `rwv update` to re-materialize it, then re-run \
         `rwv member pin`"
        ));
    }
    let vcs = vcs_for(entry.vcs_type);
    let member = Member {
        path: &path,
        entry,
        dir,
        vcs: vcs.as_ref(),
    };

    let mut refusals = Report::default();
    let Ok(to) = vcs.resolve_revision(dir, rev) else {
        return Err(State::NameNoSuchBranch
            .of(&path)
            .member()
            .subject(rev)
            .checkout(dir)
            .judge(vcs.as_ref())
            .refused());
    };
    let candidate = lock::with_pins(&project.manifest, old.as_ref(), [(&path, &to)]);
    for line in pin_refusals(&aligner, &member, &candidate, aligns)? {
        refusals.add(line);
    }
    refusals.into_result()?;

    if let Some(witness) = aligns
        .then(|| vcs.backup_unpreserved_head(dir))
        .transpose()?
        .flatten()
    {
        eprintln!(
            "rwv member pin: {path}: the commits its detached HEAD holds are kept as {}",
            witness.backup().name
        );
    }

    let was = old
        .as_ref()
        .and_then(|lock| lock.get_entry(&path))
        .map(|e| e.version.clone());
    let new = lock::with_pins(&project.manifest, old.as_ref(), [(&path, &to)]);
    lock::write_lock_in(&new, &dir_of_project)?;
    eprintln!("Wrote {lock_path}");
    eprintln!(
        "rwv member pin: {path}: pinned at {} (was {})",
        to.id().short(),
        was.as_ref().map_or("none", |was| was.short_display())
    );
    if let Pin::At(was) = crate::alignment::pin_of(vcs.as_ref(), dir, old.as_ref(), &path)? {
        lock::report_backward_repin(vcs.as_ref(), dir, "member pin", &path, &was, &to)?;
    }

    if aligns {
        let plan = aligner.plan(&member, Some(&new))?;
        match aligner.apply(&member, &plan, &path)? {
            Applied::Aligned { facts, notes, .. } => {
                for fact in &facts {
                    eprintln!(
                        "rwv member pin: {path}: {}",
                        crate::diagnosis::fact_text(fact)
                    );
                }
                report_lines(notes);
            }
            Applied::Left { judged } => report_lines(judged),
        }
        report_standing(ctx, &path, dir, vcs.as_ref(), &to)?;
    } else {
        eprintln!(
            "rwv member pin: {path}: this workspace holds no checkout of it; `rwv update` \
             creates one at the pin"
        );
    }

    if let Some(message) = commit {
        let pending = lock::PendingLockCommit::new(project_name, dir_of_project, new, old, message)
            .by_name("member pin", [path]);
        lock::commit_project_lock(&pending, &BTreeSet::new())?;
    }
    Ok(())
}

fn report_lines(lines: Vec<Judged>) {
    if lines.is_empty() {
        return;
    }
    let mut report = Report::default();
    for line in lines {
        report.add(line);
    }
    eprint!("{}", report.render("member pin"));
}

/// Where the checkout stands against the pin it was just given, when that is
/// not at it: a pin behind the branch's tip leaves the checkout at the tip.
fn report_standing(
    ctx: &WorkspaceContext,
    path: &RepoPath,
    dir: &std::path::Path,
    vcs: &dyn crate::vcs::Vcs,
    pin: &crate::vcs::ResolvedRevisionId,
) -> anyhow::Result<()> {
    let head = vcs.head_revision(dir)?;
    let live = crate::pins::live_pins(ctx.primary_path())?;
    let held = live.get(path).map(Vec::as_slice).unwrap_or_default();
    let standing = crate::pins::standing(vcs, dir, &head, pin, held)?;
    if let Some(line) = crate::pins::judge(path, dir, vcs, &standing, (&head, pin), None) {
        report_lines(vec![line]);
    }
    Ok(())
}
