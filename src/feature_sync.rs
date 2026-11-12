//! `rwv sync`: a feature takes in the branch its record lands into, or any
//! weave branch merges another's history with `--from`.
//!
//! Every refusal is judged before anything moves. Then, under the op record,
//! each member takes in its merge source, the project branch takes in the
//! target's tip, and the relock records the three-way merge of pins with each
//! moved member at its new tip.

use crate::diagnosis::{ChoiceId, Fact, FactKind, Judged, Report, State};
use crate::lineage::{Lineage, PinHistory};
use crate::lock_merge::{merge_pins, Held, Parent, PinMerge, Reach};
use crate::manifest::{
    LockFile, Manifest, ProjectName, RepoEntry, RepoKey, RepoOperand, RepoPath, Role,
};
use crate::merge_upstream::Op;
use crate::op_state::{OpVerb, SavepointKey, SyncFrom, SyncStrategy, TakeInRecord};
use crate::parallel::OutputMode;
use crate::project_merge::{MemberMerging, MemberStop, Order, ProjectMerge, Sides};
use crate::records::{Mode, Record};
use crate::refusal::RefusalKind;
use crate::tip_of::{tip_of, TipOf};
use crate::vcs::{
    project_vcs, vcs_for, AttachedRef, BackupMark, BackupSource, CommitId, CommitSummary,
    ConflictOp, DerivedContentPolicy, HeadAttachment, RawRefName, Relation, ResolvedRevisionId,
    Vcs, VcsError,
};
use crate::workspace::{project_dir, project_rel_path, Checkout, WorkspaceContext};
use anyhow::Context;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// What `rwv sync` was asked to do.
#[derive(Debug, Clone)]
pub struct Args {
    pub from: Option<String>,
    pub target: Option<String>,
    pub reset_to_target: Vec<RepoOperand>,
    pub strategy: SyncStrategy,
    pub jobs: usize,
}

pub enum Run {
    Fresh(Args),
    Continue,
}

/// How one repo takes in its source.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Act {
    /// It already contains the source.
    Nothing,
    /// The branch moves forward to the source.
    FastForward(CommitId),
    /// A member with no new work moves to the source, which lacks it, after a
    /// backup.
    Follow(CommitId),
    /// The branch's commits are replayed onto the source.
    Rebase(CommitId),
    /// The source is merged in, always as a merge commit.
    Merge(CommitId),
    /// Named by `--reset-to-target`: the branch moves to the source after a
    /// backup.
    Reset(CommitId),
}

impl Act {
    fn rewrites(&self) -> bool {
        matches!(self, Self::Follow(_) | Self::Rebase(_) | Self::Reset(_))
    }
}

struct Member {
    path: RepoPath,
    dir: PathBuf,
    vcs: Box<dyn Vcs>,
    act: Act,
    /// Commits the receiving branch holds beyond the target's pin.
    beyond_pin: Vec<CommitSummary>,
    /// For a reset: the work the branch drops, and the tip it leaves.
    dropped: Vec<CommitSummary>,
    was: Option<CommitId>,
}

struct Sync<'a> {
    ctx: &'a WorkspaceContext,
    project: ProjectName,
    dir: PathBuf,
    vcs: Box<dyn Vcs>,
    branch: String,
    from: SyncFrom,
    /// The record's target, where the branch is a feature.
    target: Option<String>,
    strategy: SyncStrategy,
    jobs: usize,
}

impl Sync<'_> {
    fn label(&self) -> String {
        project_rel_path(&self.project)
    }

    fn source_name(&self) -> &str {
        match &self.from {
            SyncFrom::Target(name) | SyncFrom::From(name) => name,
        }
    }

    fn merging(&self) -> bool {
        matches!(self.from, SyncFrom::From(_))
    }
}

fn rev(commit: &CommitId) -> ResolvedRevisionId {
    ResolvedRevisionId::from(commit.clone())
}

pub fn run_sync(ctx: &WorkspaceContext, run: Run, output: OutputMode) -> anyhow::Result<()> {
    let mut members = Vec::new();
    let result = take(ctx, run, output, &mut members);
    if output.is_json() {
        let envelope = crate::sync::SyncJsonOutput {
            schema: crate::sync::SYNC_JSON_SCHEMA_URL.to_owned(),
            outcomes: Vec::new(),
            placements: Vec::new(),
            members: members.clone(),
            complete: result.is_ok(),
            refused: result
                .as_ref()
                .err()
                .map(crate::diagnosis::refused_output)
                .unwrap_or_default(),
            advisories: Vec::new(),
            resolution: Some(ctx.resolution()),
        };
        outln!(
            "{}",
            serde_json::to_string_pretty(&envelope).context("failed to serialize sync output")?
        );
    }
    result?;
    crate::sync::fail_on_refused_prunes(&members)
}

fn take(
    ctx: &WorkspaceContext,
    run: Run,
    output: OutputMode,
    members: &mut Vec<crate::sync::MemberOutput>,
) -> anyhow::Result<()> {
    let project = ctx.require_bound_project()?.clone();
    let dir = project_dir(ctx.active_path(), &project);
    let vcs = project_vcs();
    crate::workweave::refuse_symlinked_members(ctx.active_path(), &project)?;
    let Some(branch) = crate::branch::checked_out(vcs.as_ref(), &dir)? else {
        return Err(State::AlignmentProjectDetached
            .of(project_rel_path(&project))
            .project_repo()
            .checkout(&dir)
            .judge(vcs.as_ref())
            .refused());
    };
    match run {
        Run::Fresh(args) => {
            crate::op_state::check_no_op_in_progress(&[ctx.active_path()])?;
            crate::branch::require_recorded(ctx)?;
            fresh(ctx, project, dir, vcs, branch, args, output, members)
        }
        Run::Continue => {
            let (op, recorded, take_in) = Op::resume(ctx, OpVerb::Sync)?;
            anyhow::ensure!(
                recorded == project,
                "the op was started for project `{recorded}`, not `{project}`"
            );
            let owner = crate::op_state::read_owner(&op.workspace)?
                .context("the op record vanished while it was resumed")?;
            let from = take_in
                .sync_from
                .clone()
                .context("the op record names no branch the sync takes in")?;
            let target = feature_target(vcs.as_ref(), ctx, &dir, &branch);
            let sync = Sync {
                ctx,
                project,
                dir,
                vcs,
                target,
                branch,
                from,
                strategy: owner.strategy,
                jobs: 1,
            };
            act(&sync, &op, &take_in, output, members)
        }
    }
}

fn feature_target(
    vcs: &dyn Vcs,
    ctx: &WorkspaceContext,
    dir: &Path,
    branch: &str,
) -> Option<String> {
    let project = ctx.require_bound_project().ok()?;
    let name = crate::branch::named(vcs, project, dir, branch).ok()?;
    match crate::branch::recorded_value(vcs, project, dir, &name).ok()? {
        Record::Feature { target, .. } => Some(target.to_string()),
        Record::Declared => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn fresh(
    ctx: &WorkspaceContext,
    project: ProjectName,
    dir: PathBuf,
    vcs: Box<dyn Vcs>,
    branch: String,
    args: Args,
    output: OutputMode,
    members: &mut Vec<crate::sync::MemberOutput>,
) -> anyhow::Result<()> {
    let name = crate::branch::named(vcs.as_ref(), &project, &dir, &branch)?;
    let record = crate::branch::recorded_value(vcs.as_ref(), &project, &dir, &name)?;
    if let Some(line) = crate::alignment::unseen_record_change(
        vcs.as_ref(),
        &project,
        &dir,
        &name,
        &record,
        "sync",
    )? {
        return Err(line.refused());
    }
    let from = match (&args.from, &record, &args.target) {
        (Some(from), _, None) => SyncFrom::From(from.clone()),
        (Some(_), _, Some(_)) => crate::refuse!(
            RefusalKind::InapplicableFlag,
            "`--from` merges the branch it names, so `--target` has nothing to set; pass one"
        ),
        (None, Record::Feature { .. }, Some(target)) => SyncFrom::Target(target.clone()),
        (None, Record::Feature { target, .. }, None) => SyncFrom::Target(target.to_string()),
        (None, Record::Declared, _) => {
            return Err(State::TargetNoTarget
                .of(project_rel_path(&project))
                .project_repo()
                .subject(&branch)
                .bind("<W>", &branch)
                .checkout(&dir)
                .by_verb("sync")
                .judge(vcs.as_ref())
                .refused())
        }
    };
    let as_mode = match &record {
        Record::Feature { as_mode, .. } => Some(*as_mode),
        Record::Declared => None,
    };
    if matches!(ctx.checkout, Checkout::Primary { .. }) {
        crate::shared_members::refuse_moving(ctx.primary_path(), &project, &|_, _| true)?;
    }
    let fetched = crate::fetch::fetch_repos(
        ctx,
        &crate::member_selection::MemberSelection::new(
            crate::selector::RepoFilter::parse(&[], &[])?,
            false,
        ),
        args.jobs,
        OutputMode::Json,
    )?;
    if fetched.unconfirmed > 0 {
        crate::refuse!(
            RefusalKind::PartialRunAborted,
            "sync could not complete the fetch of {} repo(s); nothing was taken in",
            fetched.unconfirmed
        );
    }
    let sync = Sync {
        ctx,
        project,
        dir,
        vcs,
        branch,
        from,
        target: match &record {
            Record::Feature { target, .. } => Some(target.to_string()),
            Record::Declared => None,
        },
        strategy: args.strategy,
        jobs: args.jobs,
    };
    let planned = plan(&sync, as_mode, &args.reset_to_target)?;

    let mut take_in = TakeInRecord {
        sync_from: Some(sync.from.clone()),
        reset_to_target: args
            .reset_to_target
            .iter()
            .map(ToString::to_string)
            .collect(),
        ..TakeInRecord::default()
    };
    take_in
        .upstream_tips
        .insert(RepoKey::Project, planned.source.clone());
    for member in &planned.members {
        if let Some(source) = member.source() {
            take_in
                .upstream_tips
                .insert(RepoKey::Member(member.path.clone()), source.clone());
        }
    }
    let nothing =
        planned.project == Act::Nothing && planned.members.iter().all(|m| m.act == Act::Nothing);
    if nothing {
        say(
            output,
            &format!("{}: already contains {}", sync.label(), sync.source_name()),
        );
        return Ok(());
    }
    let repos = savepoint_repos(&sync);
    let op = Op::begin(
        ctx.active_path(),
        &sync.project,
        OpVerb::Sync,
        take_in.clone(),
        &repos,
    )?;
    {
        let mut owner = crate::op_state::read_owner(&op.workspace)?
            .context("the op record vanished as it began")?;
        owner.strategy = sync.strategy;
        crate::op_state::write_owner(&op.workspace, &owner)?;
    }
    for member in &planned.members {
        for line in member.report_lines() {
            say(output, &line);
        }
    }
    act(&sync, &op, &take_in, output, members)
}

impl Member {
    fn source(&self) -> Option<&CommitId> {
        match &self.act {
            Act::Nothing => None,
            Act::FastForward(to)
            | Act::Follow(to)
            | Act::Rebase(to)
            | Act::Merge(to)
            | Act::Reset(to) => Some(to),
        }
    }

    fn report_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if !self.beyond_pin.is_empty() {
            lines.push(format!(
                "{}: the branch it lands on holds {} commit(s) beyond the target's pin, which the \
                 landing will pin: {}",
                self.path,
                self.beyond_pin.len(),
                listed(&self.beyond_pin)
            ));
        }
        if let (Act::Reset(_), Some(was)) = (&self.act, &self.was) {
            if !self.dropped.is_empty() {
                let checkout = self.dir.display().to_string();
                let ids: Vec<&str> = self.dropped.iter().rev().map(|c| c.id.as_str()).collect();
                let pick = self
                    .vcs
                    .hint(crate::vcs::VcsAction::CherryPick)
                    .replace("<checkout>", &checkout)
                    .replace("<ids>", &ids.join(" "));
                let merge = self
                    .vcs
                    .hint(crate::vcs::VcsAction::MergeNoFastForward)
                    .replace("<checkout>", &checkout)
                    .replace("<rev>", was.as_str());
                lines.push(format!(
                    "{}: new work the reset leaves in the backup: {}\n  copy these commits onto \
                     the branch: {pick}\n  or merge the backup in as a merge commit: {merge}",
                    self.path,
                    listed(&self.dropped),
                ));
            }
        }
        lines
    }
}

fn listed(commits: &[CommitSummary]) -> String {
    commits
        .iter()
        .map(|c| format!("{} {}", c.id.short(), c.subject))
        .collect::<Vec<_>>()
        .join("; ")
}

fn say(output: OutputMode, line: &str) {
    if output == OutputMode::Text {
        eprintln!("rwv sync: {line}");
    }
}

fn savepoint_repos(sync: &Sync<'_>) -> Vec<(PathBuf, Box<dyn Vcs>)> {
    let mut repos: Vec<(PathBuf, Box<dyn Vcs>)> = vec![(sync.dir.clone(), project_vcs())];
    if let Ok(manifest) = Manifest::from_path(&sync.dir.join(Manifest::FILE_NAME)) {
        for (path, entry) in manifest.iter_entries() {
            let member = sync.ctx.active_path().join(path.as_path());
            if member.exists() {
                repos.push((member, vcs_for(entry.vcs_type)));
            }
        }
    }
    repos
}

struct Planned {
    source: CommitId,
    project: Act,
    members: Vec<Member>,
}

fn manifest_at(vcs: &dyn Vcs, dir: &Path, commit: &CommitId) -> Option<Manifest> {
    let text = vcs
        .read_file_at_revision(dir, &rev(commit), Path::new(Manifest::FILE_NAME))
        .ok()?;
    Manifest::from_toml_str(&text).ok()
}

fn lock_at(vcs: &dyn Vcs, dir: &Path, commit: &CommitId) -> Option<LockFile> {
    let text = vcs
        .read_file_at_revision(dir, &rev(commit), Path::new(LockFile::FILE_NAME))
        .ok()?;
    LockFile::from_json_str(&text).ok()
}

/// The branch a member's work lands on: the member's branch named after the
/// target when the target is itself a feature, else the branch the merged
/// entries declare; `None` where the two sides changed that entry
/// differently.
fn receiving_branch(
    path: &RepoPath,
    target: &str,
    as_mode: Option<Mode>,
    ours: &Manifest,
    theirs: Option<&Manifest>,
    base: Option<&Manifest>,
) -> Option<RawRefName> {
    if as_mode == Some(Mode::Feature) {
        return Some(RawRefName::new(target));
    }
    let declared = |m: &Manifest| {
        m.get_entry(path)
            .map(|e| e.branch.local_counterpart().as_str().to_owned())
    };
    let (mine, theirs, was) = (
        declared(ours),
        theirs.and_then(declared),
        base.and_then(declared),
    );
    let chosen = match (mine, theirs) {
        (Some(mine), Some(theirs)) if mine == theirs => mine,
        (Some(mine), Some(theirs)) => {
            if Some(&mine) == was.as_ref() {
                theirs
            } else if Some(&theirs) == was.as_ref() {
                mine
            } else {
                return None;
            }
        }
        (Some(only), None) | (None, Some(only)) => only,
        (None, None) => return None,
    };
    Some(RawRefName::new(chosen))
}

fn plan(sync: &Sync<'_>, as_mode: Option<Mode>, resets: &[RepoOperand]) -> anyhow::Result<Planned> {
    let vcs = sync.vcs.as_ref();
    let dir = &sync.dir;
    let label = sync.label();
    let mut report = Report::default();
    let url_remote = vcs.project_url_remote(dir).ok().flatten();
    let source_branch = RawRefName::new(sync.source_name());
    let source_tip = tip_of(vcs, dir, &source_branch, url_remote.as_ref())?;
    let source = match source_tip.newer() {
        Ok(Some(tip)) => tip.id().clone(),
        Ok(None) => {
            let state = match sync.from {
                SyncFrom::Target(_) => State::TargetGone,
                SyncFrom::From(_) => State::NameNoSuchBranch,
            };
            return Err(state
                .of(&label)
                .project_repo()
                .subject(sync.source_name())
                .bind("<W>", &sync.branch)
                .bind("<T>", sync.source_name())
                .checkout(dir)
                .by_verb("sync")
                .judge(vcs)
                .refused());
        }
        Err(diverged) => {
            let (ahead, behind) =
                vcs.ahead_behind(dir, diverged.upstream.id(), diverged.local.id());
            return Err(State::PushBranchDiverged
                .of(&label)
                .project_repo()
                .subject(format!(
                    "{} and its upstream have diverged ({ahead} ahead, {behind} behind); take \
                     theirs in where it is checked out",
                    sync.source_name()
                ))
                .bind("<repo>", &label)
                .checkout(dir)
                .judge(vcs)
                .refused());
        }
    };
    let head = vcs.head_revision(dir)?.id().clone();
    let ours = Manifest::from_path(&dir.join(Manifest::FILE_NAME))?;
    let our_lock = lock_at(vcs, dir, &head);
    let theirs = manifest_at(vcs, dir, &source);
    let their_lock = lock_at(vcs, dir, &source);
    let base_commit = vcs.merge_bases(dir, &head, &source)?.into_iter().next();
    let base_manifest = base_commit.as_ref().and_then(|b| manifest_at(vcs, dir, b));
    if theirs.is_none() {
        return Err(State::UnparseableProjectMalformed
            .of(&label)
            .project_repo()
            .subject(format!(
                "{}'s rwv.toml at {} does not parse",
                sync.source_name(),
                source.short()
            ))
            .checkout(dir)
            .judge(vcs)
            .refused());
    }
    let history = match &sync.from {
        SyncFrom::Target(_) => PinHistory::of(vcs, dir, &source)?,
        SyncFrom::From(_) => PinHistory::default(),
    };

    for named in resets {
        let known = match named {
            RepoOperand::Project(p) => *p == sync.project,
            RepoOperand::Member(path) => ours.contains_repo(path),
        };
        if !known || sync.merging() {
            crate::refuse!(
                RefusalKind::InapplicableFlag,
                "`--reset-to-target {named}` names no repo this sync takes its target into"
            );
        }
    }
    let resets_project = resets.iter().any(|r| matches!(r, RepoOperand::Project(_)));

    let aligner = crate::alignment::Aligner::open(
        sync.ctx.active_path(),
        &sync.project,
        crate::alignment::Accounting::default(),
    )?;
    let accounting = crate::alignment::Accounting::scan(sync.ctx.primary_path());
    let dependents = crate::merge_upstream::dependents_of(vcs, dir, &sync.branch)?;
    let mut members = Vec::new();
    for (path, entry) in ours.iter_entries() {
        let mdir = sync.ctx.active_path().join(path.as_path());
        let mvcs = vcs_for(entry.vcs_type);
        if !mvcs.is_repo(&mdir) {
            continue;
        }
        let checked = crate::alignment::Member {
            path,
            entry,
            dir: &mdir,
            vcs: mvcs.as_ref(),
        };
        let aligned = aligner.plan(&checked, our_lock.as_ref())?;
        if let Some(off) = aligner.refusal(&checked, &aligned, path)? {
            report.add(off);
            continue;
        }
        let member = plan_member(
            sync,
            path,
            entry,
            mdir,
            mvcs,
            PlanInputs {
                as_mode,
                ours: &ours,
                theirs: theirs.as_ref(),
                base: base_manifest.as_ref(),
                their_lock: their_lock.as_ref(),
                history: &history,
                reset: resets.contains(&RepoOperand::Member(path.clone())),
                dependents: !dependents.is_empty(),
            },
            &mut report,
        )?;
        if let Some(member) = member {
            let on = match member.vcs.head_attachment(&member.dir)? {
                HeadAttachment::Attached(attached) => Some(attached.to_string()),
                _ => None,
            };
            if let (true, Some(on)) = (member.act.rewrites(), on) {
                let declarers = accounting.declarers(path, &on);
                if !declarers.is_empty() {
                    report.add(
                        State::ProtectedDeclared
                            .of(path)
                            .member()
                            .subject(&on)
                            .listing(declarers)
                            .bind("<m>", path)
                            .by_verb("sync")
                            .judge(member.vcs.as_ref()),
                    );
                }
            }
            members.push(member);
        }
    }

    let attached = match vcs.head_attachment(dir)? {
        HeadAttachment::Attached(attached) => attached,
        _ => anyhow::bail!("the project repo left its branch while sync judged it"),
    };
    let project_act = if resets_project {
        report.add_opt(judge_reset_site(sync, vcs, dir, &label, &dependents));
        Act::Reset(source.clone())
    } else if vcs.is_ancestor(dir, &rev(&source), &rev(&head))? {
        Act::Nothing
    } else if vcs.is_ancestor(dir, &rev(&head), &rev(&source))? {
        Act::FastForward(source.clone())
    } else if sync.merging() {
        Act::Merge(source.clone())
    } else {
        match sync.strategy {
            SyncStrategy::Ff => {
                report.note(format!(
                    "{label}: '{}' holds commits {} lacks, so it cannot fast-forward to it; \
                     rerun with `--strategy rebase`",
                    sync.branch,
                    sync.source_name()
                ));
                Act::Nothing
            }
            SyncStrategy::Rebase => {
                judge_merge_in_range(vcs, dir, &label, &head, &source, sync, &mut report)?;
                Act::Rebase(source.clone())
            }
        }
    };
    if project_act != Act::Nothing {
        report.extend(crate::diagnosis::judge_edits(
            vcs,
            &label,
            dir,
            vcs.tracked_dirty_file_names(dir)?,
            None,
        ));
    }
    if !resets_project && !sync.merging() {
        let receiving = [source.clone()];
        judge_push_branch(vcs, dir, &label, &attached, &receiving, true, &mut report)?;
    }
    let rewrites = project_act.rewrites() || members.iter().any(|m| m.act.rewrites());
    if rewrites && !dependents.is_empty() {
        report.add(
            State::TargetDependents
                .of(&label)
                .project_repo()
                .checkout(dir)
                .bind("<W>", &sync.branch)
                .bind("<T>", sync.source_name())
                .listing(dependents.iter().map(|d| {
                    format!(
                        "{d} lands into {}, which this sync would rewrite",
                        sync.branch
                    )
                }))
                .judge(vcs),
        );
    }
    if report.lines().is_empty() && !report.notes().is_empty() {
        crate::refuse!(
            RefusalKind::NotFastForwardable,
            "{}",
            report.notes().join("\n")
        );
    }
    report.into_result()?;
    Ok(Planned {
        source,
        project: project_act,
        members,
    })
}

trait ReportExt {
    fn add_opt(&mut self, judged: Option<Judged>);
}

impl ReportExt for Report {
    fn add_opt(&mut self, judged: Option<Judged>) {
        self.extend(judged);
    }
}

fn judge_reset_site(
    _sync: &Sync<'_>,
    vcs: &dyn Vcs,
    dir: &Path,
    label: &str,
    dependents: &[String],
) -> Option<Judged> {
    if !dependents.is_empty() {
        return Some(
            State::TargetDependents
                .of(label)
                .project_repo()
                .checkout(dir)
                .listing(
                    dependents
                        .iter()
                        .map(|d| format!("{d} lands into this branch")),
                )
                .judge(vcs),
        );
    }
    crate::diagnosis::judge_edits(
        vcs,
        label,
        dir,
        vcs.dirty_file_names(dir).unwrap_or_default(),
        None,
    )
}

/// A range holding a merge is never rebased: the rebase would flatten the
/// merge's second parent, which is how another lineage's work entered.
fn judge_merge_in_range(
    vcs: &dyn Vcs,
    dir: &Path,
    repo: &str,
    tip: &CommitId,
    onto: &CommitId,
    sync: &Sync<'_>,
    report: &mut Report,
) -> anyhow::Result<()> {
    let base = vcs.merge_bases(dir, tip, onto)?.into_iter().next();
    let range = vcs.pushed_range(dir, &rev(tip), base.as_ref().map(rev).as_ref())?;
    if let Some(merge) = range.iter().find(|c| c.parents.len() > 1) {
        report.note(format!(
            "{repo}: the commits a rebase would replay hold the merge {}, which a rebase would \
             flatten; take {} in by a merge instead: `rwv sync --from {}`",
            merge.id.short(),
            sync.source_name(),
            sync.source_name()
        ));
    }
    Ok(())
}

struct PlanInputs<'a> {
    as_mode: Option<Mode>,
    ours: &'a Manifest,
    theirs: Option<&'a Manifest>,
    base: Option<&'a Manifest>,
    their_lock: Option<&'a LockFile>,
    history: &'a PinHistory,
    reset: bool,
    dependents: bool,
}

/// Where a branch stands against its push branch, when they have diverged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Diverged {
    /// It holds commits no remote-tracking branch and no superseded tip
    /// reaches.
    Unpushed(Vec<CommitSummary>),
    /// Everything it holds beyond its push branch is on the branches it
    /// lands on: a reset or a landing.
    Landed,
    /// Everything it holds beyond its upstream is reached only by a
    /// superseded tip of it: the upstream was rewritten.
    Rewritten,
    /// Neither: commits other remote branches reach.
    Other,
}

/// How a branch that has diverged from its push branch `upstream` stands,
/// `receiving` being the tips it lands on.
pub(crate) fn diverged_standing(
    vcs: &dyn Vcs,
    dir: &Path,
    branch: &RawRefName,
    local: &ResolvedRevisionId,
    upstream: &ResolvedRevisionId,
    receiving: &[CommitId],
) -> Result<Diverged, VcsError> {
    let unpushed = vcs.unpushed_commits(dir, branch)?;
    if !unpushed.is_empty() {
        return Ok(Diverged::Unpushed(unpushed));
    }
    let mut landed = receiving.to_vec();
    landed.push(upstream.id().clone());
    if vcs
        .commits_excluding(dir, local, &landed, false)?
        .is_empty()
    {
        return Ok(Diverged::Landed);
    }
    if vcs
        .commits_excluding(dir, local, std::slice::from_ref(upstream.id()), true)?
        .is_empty()
    {
        return Ok(Diverged::Rewritten);
    }
    Ok(Diverged::Other)
}

/// Judge the branch `on` against its push branch: behind refuses, and a
/// divergence by what [`diverged_standing`] finds.
fn judge_push_branch(
    vcs: &dyn Vcs,
    dir: &Path,
    repo: &str,
    on: &AttachedRef,
    receiving: &[CommitId],
    project: bool,
    report: &mut Report,
) -> anyhow::Result<()> {
    let branch = RawRefName::new(on.to_string());
    let Some(upstream) = vcs.upstream_of(dir, &branch)? else {
        return Ok(());
    };
    let Some(tip) = vcs
        .remote_copies(dir, &upstream.branch)?
        .into_iter()
        .find(|c| c.remote == upstream.remote)
        .map(|c| c.tip)
    else {
        return Ok(());
    };
    let local = vcs.head_revision(dir)?;
    let scoped = |state: State| {
        let judging = state
            .of(repo)
            .checkout(dir)
            .bind("<repo>", repo)
            .bind("<m>", repo);
        if project {
            judging.project_repo()
        } else {
            judging.member()
        }
    };
    match vcs.relation(dir, &local, &tip)? {
        Relation::Even | Relation::Ahead => {}
        Relation::Behind => report.add(
            scoped(State::PushBranchBehind)
                .subject(format!("'{branch}' is behind its push branch"))
                .by_verb("sync")
                .judge(vcs),
        ),
        Relation::Diverged => {
            let (ahead, behind) = vcs.ahead_behind(dir, tip.id(), local.id());
            match diverged_standing(vcs, dir, &branch, &local, &tip, receiving)? {
                Diverged::Landed => eprintln!(
                    "rwv sync: {repo}: '{branch}' and its push branch have diverged ({ahead} \
                     ahead, {behind} behind); what it holds beyond the push branch is on the \
                     branch it lands on (landed)"
                ),
                Diverged::Rewritten => report.add(
                    scoped(State::PushBranchDiverged)
                        .subject(format!(
                            "'{branch}': {ahead} ahead, {behind} behind; what it holds beyond its \
                             upstream only a superseded tip reaches"
                        ))
                        .fact(Fact {
                            kind: FactKind::Rewritten,
                            about: None,
                        })
                        .judge(vcs),
                ),
                Diverged::Unpushed(_) | Diverged::Other => report.add(
                    scoped(State::PushBranchDiverged)
                        .subject(format!("'{branch}': {ahead} ahead, {behind} behind"))
                        .judge(vcs),
                ),
            }
        }
    }
    Ok(())
}

fn plan_member(
    sync: &Sync<'_>,
    path: &RepoPath,
    entry: &RepoEntry,
    dir: PathBuf,
    vcs: Box<dyn Vcs>,
    inputs: PlanInputs<'_>,
    report: &mut Report,
) -> anyhow::Result<Option<Member>> {
    if !matches!(entry.role, Role::Owned | Role::Fork) {
        return Ok(None);
    }
    let attached = match vcs.head_attachment(&dir)? {
        HeadAttachment::Attached(attached) => attached,
        _ => return Ok(None),
    };
    let head = vcs.head_revision(&dir)?.id().clone();
    let mut member = Member {
        path: path.clone(),
        dir,
        vcs,
        act: Act::Nothing,
        beyond_pin: Vec::new(),
        dropped: Vec::new(),
        was: Some(head.clone()),
    };
    let mvcs = member.vcs.as_ref();
    let mdir = member.dir.clone();
    let pin = inputs
        .their_lock
        .and_then(|lock| lock.get_entry(path))
        .and_then(|e| e.version.as_commit_id());
    let Some(pin) = pin else {
        return Ok(Some(member));
    };
    let unreachable = |what: String| {
        State::PinUnreachable
            .of(path)
            .member()
            .checkout(&mdir)
            .bind("<m>", path)
            .subject(what)
            .judge(mvcs)
    };
    if mvcs.resolve_revision(&mdir, pin.as_str()).is_err() {
        report.add(unreachable(format!(
            "{} pins {} for it, which this clone lacks",
            sync.source_name(),
            pin.short()
        )));
        return Ok(Some(member));
    }

    if let SyncFrom::From(_) = sync.from {
        let declared = |m: &Manifest| {
            m.get_entry(path)
                .map(|e| e.branch.local_counterpart().as_str().to_owned())
        };
        let theirs = inputs.theirs.and_then(declared);
        if sync.target.is_none() && theirs.is_some() && theirs != declared(inputs.ours) {
            report.add(
                State::PinNotOnDeclaredBranch
                    .of(path)
                    .member()
                    .checkout(&mdir)
                    .bind("<m>", path)
                    .bind("<b>", theirs.unwrap_or_default())
                    .subject(format!(
                        "{} declares it on another branch; declare that branch here first",
                        sync.source_name()
                    ))
                    .only(&[ChoiceId::C11])
                    .judge(mvcs),
            );
            return Ok(Some(member));
        }
        if !mvcs.is_ancestor(&mdir, &rev(&pin), &rev(&head))? {
            member.act = Act::Merge(pin);
            judge_member_moves(&member, &attached, report)?;
        }
        return Ok(Some(member));
    }

    let receiving = receiving_branch(
        path,
        sync.source_name(),
        inputs.as_mode,
        inputs.ours,
        inputs.theirs,
        inputs.base,
    );
    let Some(receiving) = receiving else {
        return Ok(Some(member));
    };
    let home = crate::registry::resolve_to_clone_info(&entry.url)
        .ok()
        .and_then(|info| mvcs.url_remote(&mdir, &info.url).ok())
        .and_then(|found| match found {
            crate::vcs::UrlRemote::Present(remote) => Some(remote),
            _ => None,
        });
    let u = tip_of(mvcs, &mdir, &receiving, home.as_ref())?;
    let source = match receiving_source(&u, &pin) {
        Ok(source) => source,
        Err(diverged) => {
            let (ahead, behind) =
                mvcs.ahead_behind(&mdir, diverged.upstream.id(), diverged.local.id());
            report.add(
                State::PushBranchDiverged
                    .of(path)
                    .member()
                    .checkout(&mdir)
                    .bind("<m>", path)
                    .bind("<repo>", path)
                    .subject(format!(
                        "'{receiving}', where it lands, and its upstream have diverged ({ahead} \
                         ahead, {behind} behind)"
                    ))
                    .judge(mvcs),
            );
            return Ok(Some(member));
        }
    };
    if source != pin {
        member.beyond_pin = mvcs.commits_beyond(&mdir, &rev(&source), &rev(&pin))?;
    }
    let pins = inputs.history.of_member(path);

    if inputs.reset {
        member.dropped =
            crate::lineage::new_work(mvcs, &mdir, &head, Some(&source), &BTreeSet::new())?;
        member.act = Act::Reset(source);
        judge_member_moves(&member, &attached, report)?;
        return Ok(Some(member));
    }
    let new_work = crate::lineage::new_work(mvcs, &mdir, &head, Some(&source), &pins)?;
    let follows = new_work.is_empty();
    let contained = mvcs.is_ancestor(&mdir, &rev(&source), &rev(&head))?;
    member.act = if contained {
        Act::Nothing
    } else if follows {
        if mvcs.is_ancestor(&mdir, &rev(&head), &rev(&source))? {
            Act::FastForward(source.clone())
        } else {
            Act::Follow(source.clone())
        }
    } else {
        match sync.strategy {
            SyncStrategy::Ff => {
                report.note(format!(
                    "{path}: '{}' holds new work and {} has moved, so it cannot fast-forward; \
                     rerun with `--strategy rebase`",
                    sync.branch, receiving
                ));
                Act::Nothing
            }
            SyncStrategy::Rebase => Act::Rebase(source.clone()),
        }
    };
    if !follows || (inputs.dependents && !contained) {
        match crate::lineage::judge(mvcs, &mdir, &head, &pins, &source)? {
            Lineage::Passes { .. } => {}
            Lineage::Off { base } => {
                let mut judging = State::PinOffLineage
                    .of(path)
                    .member()
                    .checkout(&mdir)
                    .bind("<m>", path)
                    .bind("<b>", &receiving)
                    .subject(format!(
                        "its base {} is not on '{receiving}' ({})",
                        base.short(),
                        source.short()
                    ));
                if let Some(fact) = patch_fact(mvcs, &mdir, &head, &source) {
                    judging = judging.fact(fact);
                }
                report.add(judging.judge(mvcs));
            }
            Lineage::Unreachable { absent } => report.add(unreachable(format!(
                "{} pinned {} for it, which this clone lacks, so its base cannot be read here",
                sync.source_name(),
                absent
                    .iter()
                    .map(|c| c.short().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }
    if let Act::Rebase(onto) = &member.act {
        let onto = onto.clone();
        judge_merge_in_range(mvcs, &mdir, path.as_str(), &head, &onto, sync, report)?;
    }
    judge_push_branch(
        mvcs,
        &mdir,
        path.as_str(),
        &attached,
        std::slice::from_ref(&source),
        false,
        report,
    )?;
    if member.act != Act::Nothing {
        judge_member_moves(&member, &attached, report)?;
    }
    Ok(Some(member))
}

/// The tip of the branch a member lands on, or the target's pin where that
/// branch exists nowhere.
fn receiving_source(u: &TipOf, pin: &CommitId) -> Result<CommitId, crate::tip_of::Diverged> {
    Ok(u.newer()?
        .map(|tip| tip.id().clone())
        .unwrap_or_else(|| pin.clone()))
}

/// The `patch-equivalent` fact for a member's unlanded commits, where the
/// backend names any.
pub(crate) fn patch_fact(
    vcs: &dyn Vcs,
    dir: &Path,
    unlanded: &CommitId,
    onto: &CommitId,
) -> Option<Fact> {
    let found = vcs.patch_equivalents(dir, unlanded, onto)?;
    if !found.names_any() {
        return None;
    }
    let mut about: Vec<String> = found
        .matched
        .iter()
        .map(|(mine, theirs)| format!("{} matches {}", mine.short(), theirs.short()))
        .collect();
    if let Some(combined) = &found.combined {
        about.push(format!("the whole change matches {}", combined.short()));
    }
    if !found.unmatched.is_empty() && found.combined.is_none() {
        about.push(format!(
            "no match for {}",
            found
                .unmatched
                .iter()
                .map(|c| c.short().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Some(Fact {
        kind: FactKind::PatchEquivalent,
        about: Some(about.join("; ")),
    })
}

/// What a member that moves needs judged: no edits, and its branch checked
/// out nowhere else.
fn judge_member_moves(
    member: &Member,
    on: &AttachedRef,
    report: &mut Report,
) -> anyhow::Result<()> {
    let vcs = member.vcs.as_ref();
    let dir = &member.dir;
    report.extend(crate::diagnosis::judge_edits(
        vcs,
        &member.path,
        dir,
        vcs.tracked_dirty_file_names(dir)?,
        None,
    ));
    let branch = RawRefName::new(on.to_string());
    let own = crate::path_spelling::CanonicalPath::of(dir);
    let elsewhere: Vec<PathBuf> = vcs
        .checkouts_of_branch(dir, &branch)?
        .into_iter()
        .filter(|other| crate::path_spelling::CanonicalPath::of(other) != own)
        .collect();
    if !elsewhere.is_empty() {
        report.add(
            State::CheckoutInAnotherWorkspace
                .of(&member.path)
                .member()
                .checkout(dir)
                .subject(format!("'{branch}'"))
                .listing(elsewhere.iter().map(|p| p.display().to_string()))
                .judge(vcs),
        );
    }
    Ok(())
}

/// Carry out what was planned, or resume it: every step finds what an
/// earlier run already did and leaves it.
fn act(
    sync: &Sync<'_>,
    op: &Op,
    take_in: &TakeInRecord,
    output: OutputMode,
    members: &mut Vec<crate::sync::MemberOutput>,
) -> anyhow::Result<()> {
    let key = SavepointKey::Op(op.id.clone());
    let source = take_in
        .upstream_tips
        .get(&RepoKey::Project)
        .context("the op record holds no tip of the branch the sync takes in")?
        .clone();
    let resets: Vec<RepoOperand> = take_in
        .reset_to_target
        .iter()
        .filter_map(|r| r.parse().ok())
        .collect();
    let pre_head = sync
        .vcs
        .resolve_savepoint(&sync.dir, &key)
        .context("the op's savepoint of the project repo is gone")?
        .id()
        .clone();
    let pre_manifest = manifest_at(sync.vcs.as_ref(), &sync.dir, &pre_head)
        .context("the project's rwv.toml did not parse when the sync began")?;

    let mut moved: BTreeMap<RepoPath, CommitId> = BTreeMap::new();
    for (path, entry) in pre_manifest.iter_entries() {
        let Some(member_source) = take_in.upstream_tips.get(&RepoKey::Member(path.clone())) else {
            continue;
        };
        let dir = sync.ctx.active_path().join(path.as_path());
        let vcs = vcs_for(entry.vcs_type);
        let reset = resets.contains(&RepoOperand::Member(path.clone()));
        let tip = take_member(
            sync,
            op,
            path,
            &dir,
            vcs.as_ref(),
            member_source,
            reset,
            output,
        )?;
        let before = vcs.resolve_savepoint(&dir, &key).map(|r| r.id().clone());
        if before.as_ref() != Some(&tip) || reset {
            vcs.refresh_index_to_head_if_safe(&dir);
            vcs.refresh_working_tree_to_head_if_safe(&dir);
            moved.insert(path.clone(), tip);
        }
    }

    let resets_project = resets.iter().any(|r| matches!(r, RepoOperand::Project(_)));
    let vcs = sync.vcs.as_ref();
    let head = vcs.head_revision(&sync.dir)?.id().clone();
    if sync.merging() {
        if !vcs.is_ancestor(&sync.dir, &rev(&source), &rev(&head))?
            || vcs.merge_in_flight(&sync.dir)?.is_some()
        {
            let (commit, pruning) = ProjectMerge {
                ctx: sync.ctx,
                project: &sync.project,
                verb: "sync",
                resume: crate::op_state::resume_command(OpVerb::Sync),
                jobs: sync.jobs,
                opts: crate::activate::ActivateOptions::default(),
                message: format!("Merge {} into {}", sync.source_name(), sync.branch),
                head: Parent::First,
                members: MemberMerging::Done(moved.clone()),
            }
            .run(&Sides {
                first: head.clone(),
                second: source.clone(),
            })?;
            members.extend(pruning);
            say(
                output,
                &format!(
                    "{}: merged {} into {}",
                    sync.label(),
                    sync.source_name(),
                    commit.short()
                ),
            );
        } else if !moved.is_empty() {
            relock(sync, &pre_head, &source, &moved, false)?;
        }
    } else {
        take_project(sync, &head, &source, resets_project, output)?;
        relock(sync, &pre_head, &source, &moved, resets_project)?;
    }
    settle(sync, &pre_manifest, members, output)?;
    let repos = savepoint_repos(sync);
    op.finish(&repos);
    say(output, &format!("took in {}", sync.source_name()));
    Ok(())
}

fn conflict_refusal(
    sync: &Sync<'_>,
    repo: &str,
    dir: &Path,
    vcs: &dyn Vcs,
    op: ConflictOp,
) -> anyhow::Error {
    crate::refusal::refusal(
        RefusalKind::OpParked,
        format!(
            "rwv sync: the {} of {repo} onto {} stopped on a conflict at {}.\n{}\nthen run `{}`, \
             or `rwv abort` to put everything back as it was",
            match op {
                ConflictOp::Rebase => "rebase",
                _ => "merge",
            },
            sync.source_name(),
            dir.display(),
            vcs.conflict_resolution_hint(op),
            crate::op_state::resume_command(OpVerb::Sync),
        ),
    )
}

#[allow(clippy::too_many_arguments)]
fn take_member(
    sync: &Sync<'_>,
    op: &Op,
    path: &RepoPath,
    dir: &Path,
    vcs: &dyn Vcs,
    source: &CommitId,
    reset: bool,
    output: OutputMode,
) -> anyhow::Result<CommitId> {
    let workspace = &op.workspace;
    if matches!(vcs.mid_op(dir), Some(ConflictOp::Rebase)) {
        vcs.rebase_continue(dir, DerivedContentPolicy::keep_target_side())
            .map_err(|_| conflict_refusal(sync, path.as_str(), dir, vcs, ConflictOp::Rebase))?;
    }
    let attached = match vcs.head_attachment(dir)? {
        HeadAttachment::Attached(attached) => attached,
        _ => anyhow::bail!("{path}: left its branch during the sync"),
    };
    let head = vcs.head_revision(dir)?;
    let branch = RawRefName::new(attached.to_string());
    let src = rev(source);
    let journal = |tip: &CommitId| {
        crate::op_state::journal_tip(workspace, RepoKey::Member(path.clone()), tip)
    };
    if reset {
        if head.id() != source {
            let kept =
                vcs.create_backup(dir, &BackupSource::Branch(branch.clone()), &head, None)?;
            vcs.reset_attached_ref_keeping(&attached, &src, &kept)?;
            journal(source)?;
            say(
                output,
                &format!(
                    "{path}: reset '{branch}' to {}, keeping {} in backup {}",
                    source.short(),
                    head.id().short(),
                    kept.backup().name
                ),
            );
        }
        return Ok(source.clone());
    }
    if vcs.is_ancestor(dir, &src, &head)? {
        return Ok(head.id().clone());
    }
    if sync.merging() {
        let message = format!(
            "Merge {}'s pin of {path} into {}",
            sync.source_name(),
            sync.branch
        );
        return match crate::project_merge::merge_member(
            false,
            workspace,
            path,
            &sync_entry(sync, path)?,
            dir,
            source,
            if sync.target.as_deref() == Some(sync.source_name()) {
                Order::HeadFirst
            } else {
                Order::HeadFirstAlwaysMerge
            },
            &message,
        ) {
            Ok(joined) => {
                let tip = joined.tip().cloned().unwrap_or_else(|| head.id().clone());
                say(output, &format!("{path}: merged {}", source.short()));
                Ok(tip)
            }
            Err(MemberStop::Conflict(conflicted)) => Err(crate::project_merge::conflict_stop(
                "sync",
                &crate::op_state::resume_command(OpVerb::Sync),
                &conflicted,
                vcs,
                &[],
            )),
            Err(MemberStop::Refused(e)) => Err(e),
        };
    }
    if vcs.is_ancestor(dir, &head, &src)? {
        vcs.advance_attached_ref(&attached, &src)?;
        journal(source)?;
        say(
            output,
            &format!("{path}: fast-forwarded to {}", source.short()),
        );
        return Ok(source.clone());
    }
    let pins = PinHistory::of(
        sync.vcs.as_ref(),
        &sync.dir,
        take_in_tip(op)?.as_ref().unwrap_or(source),
    )
    .map(|h| h.of_member(path))
    .unwrap_or_default();
    let new_work = crate::lineage::new_work(vcs, dir, head.id(), Some(source), &pins)?;
    if new_work.is_empty() {
        let kept = vcs.create_backup(dir, &BackupSource::Branch(branch.clone()), &head, None)?;
        vcs.reset_attached_ref_keeping(&attached, &src, &kept)?;
        journal(source)?;
        say(
            output,
            &format!(
                "{path}: has no new work, so it follows {} (was {}, kept in backup {})",
                source.short(),
                head.id().short(),
                kept.backup().name
            ),
        );
        return Ok(source.clone());
    }
    vcs.create_backup(
        dir,
        &BackupSource::Branch(branch.clone()),
        &head,
        Some(BackupMark::Rebased),
    )?;
    vcs.rebase(dir, &src, &src, DerivedContentPolicy::keep_target_side())
        .map_err(|_| conflict_refusal(sync, path.as_str(), dir, vcs, ConflictOp::Rebase))?;
    let tip = vcs.head_revision(dir)?.id().clone();
    journal(&tip)?;
    say(output, &format!("{path}: rebased onto {}", source.short()));
    Ok(tip)
}

fn take_in_tip(op: &Op) -> anyhow::Result<Option<CommitId>> {
    let owner = crate::op_state::read_owner(&op.workspace)?;
    Ok(owner
        .and_then(|o| o.take_in)
        .and_then(|t| t.upstream_tips.get(&RepoKey::Project).cloned()))
}

fn sync_entry(sync: &Sync<'_>, path: &RepoPath) -> anyhow::Result<RepoEntry> {
    let manifest = Manifest::from_path(&sync.dir.join(Manifest::FILE_NAME))?;
    manifest
        .get_entry(path)
        .cloned()
        .with_context(|| format!("{path} is no member of the project"))
}

fn take_project(
    sync: &Sync<'_>,
    head: &CommitId,
    source: &CommitId,
    reset: bool,
    output: OutputMode,
) -> anyhow::Result<()> {
    let vcs = sync.vcs.as_ref();
    let dir = &sync.dir;
    let label = sync.label();
    if matches!(vcs.mid_op(dir), Some(ConflictOp::Rebase)) {
        vcs.rebase_continue(dir, DerivedContentPolicy::keep_target_side())
            .map_err(|_| conflict_refusal(sync, &label, dir, vcs, ConflictOp::Rebase))?;
        return Ok(());
    }
    let attached = match vcs.head_attachment(dir)? {
        HeadAttachment::Attached(attached) => attached,
        _ => anyhow::bail!("{label}: left its branch during the sync"),
    };
    let branch = RawRefName::new(attached.to_string());
    let src = rev(source);
    let here = rev(head);
    if reset {
        if head != source {
            let kept =
                vcs.create_backup(dir, &BackupSource::Branch(branch.clone()), &here, None)?;
            vcs.reset_attached_ref_keeping(&attached, &src, &kept)?;
            crate::op_state::journal_tip(sync.ctx.active_path(), RepoKey::Project, source)?;
            say(
                output,
                &format!(
                    "{label}: reset '{branch}' to {} with its lock, keeping {} in backup {}",
                    source.short(),
                    head.short(),
                    kept.backup().name
                ),
            );
        }
        return Ok(());
    }
    if vcs.is_ancestor(dir, &src, &here)? {
        return Ok(());
    }
    if vcs.is_ancestor(dir, &here, &src)? {
        vcs.advance_attached_ref(&attached, &src)?;
        say(
            output,
            &format!("{label}: fast-forwarded to {}", source.short()),
        );
        return Ok(());
    }
    vcs.create_backup(
        dir,
        &BackupSource::Branch(branch),
        &here,
        Some(BackupMark::Rebased),
    )?;
    vcs.rebase(dir, &src, &src, DerivedContentPolicy::keep_target_side())
        .map_err(|_| conflict_refusal(sync, &label, dir, vcs, ConflictOp::Rebase))?;
    say(output, &format!("{label}: rebased onto {}", source.short()));
    Ok(())
}

struct MemberReach<'a> {
    root: &'a Path,
    manifest: &'a Manifest,
}

impl Reach for MemberReach<'_> {
    fn reaches(&self, member: &RepoPath, ancestor: &CommitId, of: &CommitId) -> Option<bool> {
        let entry = self.manifest.get_entry(member)?;
        vcs_for(entry.vcs_type)
            .is_ancestor(&self.root.join(member.as_path()), &rev(ancestor), &rev(of))
            .ok()
    }
}

/// The lock a sync leaves: the three-way merge of pins against the merge
/// base's lock, each moved member at its new tip; with the project repo reset,
/// the target's lock with the moved members at theirs.
fn relock(
    sync: &Sync<'_>,
    pre_head: &CommitId,
    source: &CommitId,
    moved: &BTreeMap<RepoPath, CommitId>,
    reset_project: bool,
) -> anyhow::Result<()> {
    let vcs = sync.vcs.as_ref();
    let dir = &sync.dir;
    let manifest = Manifest::from_path(&dir.join(Manifest::FILE_NAME))?;
    let held = |commit: &CommitId| crate::unmerged::held_at(vcs, dir, commit);
    let theirs = held(source);
    let pins = if reset_project {
        let mut pins = theirs.pins.clone();
        pins.extend(moved.clone());
        pins
    } else {
        let base: Option<Held> = vcs
            .merge_bases(dir, pre_head, source)?
            .into_iter()
            .next()
            .map(|b| held(&b));
        let ours = held(pre_head);
        let merged_declared: BTreeMap<RepoPath, String> = manifest
            .iter_entries()
            .map(|(path, entry)| {
                (
                    path.clone(),
                    entry.branch.local_counterpart().as_str().to_owned(),
                )
            })
            .collect();
        let merged = merge_pins(
            &PinMerge {
                base: base.as_ref(),
                first: &ours,
                second: &theirs,
                merged_declared: &merged_declared,
                member_merges: moved,
            },
            &MemberReach {
                root: sync.ctx.active_path(),
                manifest: &manifest,
            },
        );
        merged.pins
    };
    let resolved: Vec<(RepoPath, ResolvedRevisionId)> =
        pins.iter().map(|(p, c)| (p.clone(), rev(c))).collect();
    let lock = crate::lock::with_pins(&manifest, None, resolved.iter().map(|(p, c)| (p, c)));
    crate::lock::write_lock_in(&lock, dir)?;
    crate::lock::commit_lock_file_with_message(
        vcs,
        dir,
        &format!("lock: relock after sync from {}", sync.source_name()),
    )?;
    Ok(())
}

/// Align the workspace to the branch as the sync left it: members the new
/// entries drop are pruned, missing ones cloned, and the rest aligned.
fn settle(
    sync: &Sync<'_>,
    before: &Manifest,
    members: &mut Vec<crate::sync::MemberOutput>,
    output: OutputMode,
) -> anyhow::Result<()> {
    let now = Manifest::from_path(&sync.dir.join(Manifest::FILE_NAME))?;
    for (path, entry) in before.iter_entries() {
        let checkout = sync.ctx.active_path().join(path.as_path());
        let judged = members.iter().any(|m| match m {
            crate::sync::MemberOutput::Pruned { path: p, .. }
            | crate::sync::MemberOutput::PruneRefused { path: p, .. } => p == path.as_str(),
            _ => false,
        });
        if !now.contains_repo(path) && checkout.exists() && !judged {
            members.push(
                match crate::sync::prune_dropped_repo(
                    vcs_for(entry.vcs_type).as_ref(),
                    sync.ctx,
                    path,
                ) {
                    Ok(()) => crate::sync::MemberOutput::pruned(path, &checkout),
                    Err(e) => crate::sync::MemberOutput::prune_refused(path, &checkout, &e),
                },
            );
        }
    }
    crate::update::update_members(
        sync.ctx,
        &crate::member_selection::MemberSelection::new(
            crate::selector::RepoFilter::parse(&[], &[])?,
            false,
        ),
        sync.jobs,
        output,
        crate::activate::ActivateOptions::default(),
    )?
    .tail
}

/// What the lineage rule reads for a feature's members: its target's pins
/// and entries.
pub(crate) struct FeatureLineage {
    target: String,
    as_mode: Mode,
    pins: PinHistory,
    theirs: Option<Manifest>,
}

/// The lineage a feature checked out in `ctx` lands on; `None` for a branch
/// that is no feature, or whose target exists nowhere.
pub(crate) fn feature_lineage(
    ctx: &WorkspaceContext,
    project: &ProjectName,
) -> anyhow::Result<Option<FeatureLineage>> {
    let dir = project_dir(ctx.active_path(), project);
    let vcs = project_vcs();
    let Some(branch) = crate::branch::checked_out(vcs.as_ref(), &dir)? else {
        return Ok(None);
    };
    let name = crate::branch::named(vcs.as_ref(), project, &dir, &branch)?;
    let Record::Feature { target, as_mode } =
        crate::branch::recorded_value(vcs.as_ref(), project, &dir, &name)?
    else {
        return Ok(None);
    };
    let url_remote = vcs.project_url_remote(&dir).ok().flatten();
    let tip = tip_of(
        vcs.as_ref(),
        &dir,
        &RawRefName::new(target.to_string()),
        url_remote.as_ref(),
    )?;
    let Ok(Some(tip)) = tip.newer() else {
        return Ok(None);
    };
    Ok(Some(FeatureLineage {
        target: target.to_string(),
        as_mode,
        pins: PinHistory::of(vcs.as_ref(), &dir, tip.id())?,
        theirs: manifest_at(vcs.as_ref(), &dir, tip.id()),
    }))
}

impl FeatureLineage {
    /// Judge a merge of `upstream` into the member's work `local` by the
    /// lineage rule: refused where the upstream lacks the work's base, with
    /// C3 where the upstream's own base is on the branch the member lands
    /// on, else C27.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn judge_merge_upstream(
        &self,
        ours: &Manifest,
        path: &RepoPath,
        dir: &Path,
        vcs: &dyn Vcs,
        local: &CommitId,
        upstream: &CommitId,
    ) -> anyhow::Result<Option<Judged>> {
        let pins = self.pins.of_member(path);
        let Lineage::Off { base } = crate::lineage::judge(vcs, dir, local, &pins, upstream)? else {
            return Ok(None);
        };
        let (present, _) = crate::lineage::split_present(vcs, dir, &pins);
        let their_base = crate::lineage::base_of(vcs, dir, upstream, &present)?;
        let receiving = receiving_branch(
            path,
            &self.target,
            Some(self.as_mode),
            ours,
            self.theirs.as_ref(),
            None,
        );
        let on_receiving = match (their_base, receiving) {
            (Some(their_base), Some(receiving)) => {
                match tip_of(vcs, dir, &receiving, None)?.newer() {
                    Ok(Some(u)) => vcs.is_ancestor(dir, &rev(&their_base), u)?,
                    _ => false,
                }
            }
            _ => false,
        };
        let choice = if on_receiving {
            ChoiceId::C3
        } else {
            ChoiceId::C27
        };
        Ok(Some(
            State::PinOffLineage
                .of(path)
                .member()
                .checkout(dir)
                .bind("<m>", path)
                .bind("<repo>", path)
                .subject(format!(
                    "its base {} is not on its upstream ({})",
                    base.short(),
                    upstream.short()
                ))
                .by_verb("update")
                .only(&[choice])
                .judge(vcs),
        ))
    }
}
