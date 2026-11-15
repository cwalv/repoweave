//! `rwv sync`: a feature takes in the branch its record lands into, or any
//! weave branch merges another's history with `--from`.
//!
//! Every refusal is judged before anything moves. Then, under the op record,
//! each member takes in its merge source, the project branch takes in the
//! target's tip, and the relock records the three-way merge of pins with each
//! moved member at its new tip.

use crate::diagnosis::{ChoiceId, Fact, FactKind, Form, Judged, Judging, Report, State};
use crate::lineage::{Lineage, PinHistory, Verdict};
use crate::lock_merge::{merge_pins, Held, Parent, PinMerge, Reach};
use crate::manifest::{
    History, LockFile, Manifest, ProjectName, RepoEntry, RepoKey, RepoOperand, RepoPath, Role,
};
use crate::merge_upstream::Op;
use crate::op_state::{OpVerb, SavepointKey, SyncFrom, TakeInRecord};
use crate::parallel::OutputMode;
use crate::project_merge::{MemberMerging, MemberStop, Order, ProjectMerge, Sides};
use crate::records::{Mode, Record};
use crate::refusal::RefusalKind;
use crate::set_history::Integration;
use crate::vcs::{
    project_vcs, vcs_for, AttachedRef, BackupSource, CommitId, CommitSummary, ConflictOp,
    DerivedContentPolicy, HeadAttachment, RawRefName, Relation, ResolvedRevisionId, Vcs, VcsError,
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
    /// How a repo whose target records no `history` integrates, for this run.
    pub history: Option<History>,
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

struct Member {
    path: RepoPath,
    dir: PathBuf,
    vcs: Box<dyn Vcs>,
    act: Act,
    /// Commits the receiving branch holds beyond the target's pin.
    beyond_pin: Vec<CommitSummary>,
    /// For a reset: its new work, every commit the backup holds that the
    /// branch no longer reaches, and the tip it leaves.
    new_work: Vec<CommitSummary>,
    dropped: Vec<CommitSummary>,
    was: Option<CommitId>,
    /// The merge its range holds, where that range is integrated by a merge
    /// commit under a rebasing setting.
    merged: Option<CommitId>,
    /// Pins of the target's history the lineage rule considered that this
    /// clone still lacks after the sync's fetch.
    absent: Vec<crate::lineage::HistoryPin>,
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
    history: Option<History>,
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

/// The branch and its upstream each hold commits the other lacks.
struct DivergedTips {
    local: ResolvedRevisionId,
    upstream: ResolvedRevisionId,
}

/// The newer of `branch` and its upstream; `None` where it is nowhere.
fn newer(
    vcs: &dyn Vcs,
    repo: &Path,
    branch: &RawRefName,
    home: Option<&crate::vcs::RemoteHandle>,
) -> Result<Result<Option<ResolvedRevisionId>, DivergedTips>, VcsError> {
    use crate::push_branch::NewerOfBranchAndUpstream as Newer;
    Ok(
        match crate::push_branch::newer_of_branch_and_upstream(vcs, repo, branch, home)? {
            Newer::Tip(tip) => Ok(Some(tip)),
            Newer::Nowhere => Ok(None),
            Newer::Diverged {
                local,
                upstream_tip,
                ..
            } => Err(DivergedTips {
                local,
                upstream: upstream_tip,
            }),
        },
    )
}

fn rev(commit: &CommitId) -> ResolvedRevisionId {
    ResolvedRevisionId::from(commit.clone())
}

/// What a run reports beyond its refusal.
#[derive(Default)]
struct Reported {
    members: Vec<crate::sync::MemberOutput>,
    placements: Vec<crate::placement::PlacementOutput>,
    advisories: Vec<crate::workspace::AdvisoryOutput>,
    reports: Vec<crate::op_state::LandingReport>,
}

/// Journal `report` in the op record and keep it for the run's report; the
/// text line is said only the first time the record takes it.
fn report_integration(
    output: OutputMode,
    workspace: &Path,
    reported: &mut Reported,
    report: crate::op_state::LandingReport,
    line: Option<String>,
) -> anyhow::Result<()> {
    if crate::op_state::journal_report(workspace, &report)? {
        if let Some(line) = line {
            say(output, &line);
        }
        reported.reports.push(report);
    }
    Ok(())
}

fn replay_reports(output: OutputMode, reported: &mut Reported, take_in: &TakeInRecord) {
    for report in &take_in.reports {
        let line = match report {
            crate::op_state::LandingReport::Merged { repo, merge } => {
                crate::set_history::merged_line(repo, merge)
            }
            crate::op_state::LandingReport::Rebased { repo, was, backup } => format!(
                "{repo}: rebased; the tip it left, {}, is kept in the rebased backup {backup} \
                 while the operation runs",
                was.short()
            ),
            crate::op_state::LandingReport::HistoryPinAbsent {
                repo: RepoKey::Member(member),
                lock,
                pin,
            } => crate::lineage::absent_line(member, lock, pin, crate::lineage::Fetch::Made),
            _ => continue,
        };
        say(output, &line);
        reported.reports.push(report.clone());
    }
}

pub fn run_sync(ctx: &WorkspaceContext, run: Run, output: OutputMode) -> anyhow::Result<()> {
    let mut reported = Reported::default();
    let result = take(ctx, run, output, &mut reported);
    let Reported {
        members,
        placements,
        advisories,
        reports,
    } = reported;
    if output.is_json() {
        let envelope = crate::sync::SyncJsonOutput {
            schema: crate::sync::SYNC_JSON_SCHEMA_URL.to_owned(),
            outcomes: Vec::new(),
            placements,
            members: members.clone(),
            complete: result.is_ok(),
            refused: result
                .as_ref()
                .err()
                .map(crate::diagnosis::refused_output)
                .unwrap_or_default(),
            advisories,
            reports: reports
                .iter()
                .map(crate::sync::LandingReportOutput::of)
                .collect(),
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
    reported: &mut Reported,
) -> anyhow::Result<()> {
    let project = ctx.require_bound_project()?.clone();
    let dir = project_dir(ctx.active_path(), &project);
    let vcs = project_vcs();
    if let Run::Fresh(_) = run {
        crate::workweave::refuse_symlinked_members(ctx.active_path(), &project)?;
        crate::diagnosis::settle_project_drift(&project, &dir)?;
    }
    let held = match run {
        Run::Fresh(_) => None,
        Run::Continue => branch_left_by_a_stop(ctx.active_path())?,
    };
    let Some(branch) = crate::branch::checked_out(vcs.as_ref(), &dir)?.or(held) else {
        return Err(
            crate::alignment::detached_project(vcs.as_ref(), &dir, &project)
                .judge(vcs.as_ref())
                .refused(),
        );
    };
    match run {
        Run::Fresh(args) => {
            crate::branch::require_recorded(ctx)?;
            fresh(ctx, project, dir, vcs, branch, args, output, reported)
        }
        Run::Continue => {
            let (op, recorded, take_in) = Op::resume(ctx, OpVerb::Sync)?;
            if recorded != project {
                return Err(crate::untokened!(
                    Owed,
                    "the op was started for project `{recorded}`, not `{project}`"
                ));
            }
            let pre_head = vcs
                .resolve_savepoint(&dir, &SavepointKey::Op(op.id.clone()))
                .context("the op's savepoint of the project repo is gone")?;
            let pre_manifest = manifest_at(vcs.as_ref(), &dir, pre_head.id())
                .context("the project's rwv.toml did not parse when the sync began")?;
            crate::workweave::refuse_symlinked_members_named(
                ctx.active_path(),
                &project,
                &pre_manifest,
            )?;
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
                history: owner.history,
                jobs: 1,
            };
            replay_reports(output, reported, &take_in);
            act(&sync, &op, &take_in, output, reported)
        }
    }
}

/// The branch a stopped sync's project checkout is put back on once the
/// replay that stopped there ends: a carry's, or the sync's own rebase of
/// it, each begun on that branch and left detached by the stop.
fn branch_left_by_a_stop(workspace: &Path) -> anyhow::Result<Option<String>> {
    let Some(take_in) = crate::op_state::read_owner(workspace)?.and_then(|owner| owner.take_in)
    else {
        return Ok(None);
    };
    Ok(take_in
        .carries
        .iter()
        .find(|c| c.repo == RepoKey::Project && c.composed.is_none())
        .map(|c| c.checkout_on.clone())
        .or_else(|| {
            take_in
                .rebases
                .iter()
                .find(|r| r.repo == RepoKey::Project)
                .map(|r| r.branch.clone())
        }))
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
    reported: &mut Reported,
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
    if args.from.is_some() && args.history.is_some() {
        crate::refuse!(
            RefusalKind::InapplicableFlag,
            "`--from` merges the branch it names whatever the setting, so `--history` has nothing to set; pass one"
        );
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
        let causes: String = fetched
            .records
            .iter()
            .filter(|record| record.status == crate::fetch::FetchStatus::Unconfirmed)
            .map(|record| {
                format!(
                    "\n  {}: {}",
                    record.path,
                    record.message.as_deref().unwrap_or("no answer")
                )
            })
            .collect();
        crate::refuse!(
            RefusalKind::PartialRunAborted,
            "sync could not complete the fetch of {} repo(s); nothing was taken in{causes}",
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
        history: args.history,
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
    if matches!(planned.project, Act::Merge(_)) && !sync.merging() {
        take_in.merged.push(RepoKey::Project);
    }
    for member in &planned.members {
        if let Some(source) = member.source() {
            take_in
                .upstream_tips
                .insert(RepoKey::Member(member.path.clone()), source.clone());
        }
        if matches!(member.act, Act::Merge(_)) && !sync.merging() {
            take_in.merged.push(RepoKey::Member(member.path.clone()));
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
        sync.history,
        take_in.clone(),
        &repos,
    )?;
    if let Some(merge) = &planned.merged {
        report_integration(
            output,
            &op.workspace,
            reported,
            crate::op_state::LandingReport::Merged {
                repo: RepoKey::Project,
                merge: merge.clone(),
            },
            Some(crate::set_history::merged_line(&sync.label(), merge)),
        )?;
    }
    for member in &planned.members {
        if let Some(merge) = &member.merged {
            report_integration(
                output,
                &op.workspace,
                reported,
                crate::op_state::LandingReport::Merged {
                    repo: RepoKey::Member(member.path.clone()),
                    merge: merge.clone(),
                },
                Some(crate::set_history::merged_line(&member.path, merge)),
            )?;
        }
        for absent in &member.absent {
            report_integration(
                output,
                &op.workspace,
                reported,
                crate::op_state::LandingReport::HistoryPinAbsent {
                    repo: RepoKey::Member(member.path.clone()),
                    lock: absent.lock.clone(),
                    pin: absent.pin.clone(),
                },
                Some(absent.absent_line(&member.path, crate::lineage::Fetch::Made)),
            )?;
        }
        for line in member.report_lines() {
            say(output, &line);
        }
    }
    act(&sync, &op, &take_in, output, reported)
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
                let merge = self
                    .vcs
                    .hint(crate::vcs::VcsAction::MergeNoFastForward)
                    .replace("<checkout>", &checkout)
                    .replace("<pin>", was.as_str());
                let mut text = if self.new_work.is_empty() {
                    format!(
                        "{}: the reset leaves in the backup only commits a pin of the target's \
                         history reaches: {}\n  merge",
                        self.path,
                        listed(&self.dropped),
                    )
                } else {
                    let ids: Vec<&str> =
                        self.new_work.iter().rev().map(|c| c.id.as_str()).collect();
                    let pick = self
                        .vcs
                        .hint(crate::vcs::VcsAction::CherryPick)
                        .replace("<checkout>", &checkout)
                        .replace("<ids>", &ids.join(" "));
                    format!(
                        "{}: new work the reset leaves in the backup: {}\n  copy these commits \
                         onto the branch: {pick}\n  or merge",
                        self.path,
                        listed(&self.new_work),
                    )
                };
                text.push_str(&format!(
                    " the backup in as a merge commit, bringing {}: {merge}",
                    listed(&self.dropped)
                ));
                lines.push(text);
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
    /// The merge the project branch's range holds, where that range is
    /// integrated by a merge commit under a rebasing setting.
    merged: Option<CommitId>,
    members: Vec<Member>,
}

/// The workspace whose project checkout `checkout` is.
fn workspace_holding(checkout: &Path, project: &ProjectName) -> Option<PathBuf> {
    let root = checkout.parent()?.parent()?;
    (project_dir(root, project) == checkout).then(|| root.to_path_buf())
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
pub(crate) fn receiving_branch(
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
    let source = match newer(vcs, dir, &source_branch, url_remote.as_ref())? {
        Ok(Some(tip)) => tip.id().clone(),
        Ok(None) => {
            let judging = match &sync.from {
                SyncFrom::Target(target) => crate::branch::target_gone(
                    vcs,
                    &sync.project,
                    dir,
                    &crate::branch::named(vcs, &sync.project, dir, &sync.branch)?,
                    &crate::branch::named(vcs, &sync.project, dir, target)?,
                )?,
                SyncFrom::From(_) => State::NameNoSuchBranch
                    .of(&label)
                    .project_repo()
                    .subject(sync.source_name())
                    .bind("<W>", &sync.branch)
                    .bind("<T>", sync.source_name())
                    .checkout(dir),
            };
            return Err(judging.by_verb("sync").judge(vcs).refused());
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
    if !resets.contains(&RepoOperand::Project(sync.project.clone())) {
        let named: Vec<RepoPath> = resets
            .iter()
            .filter_map(|reset| match reset {
                RepoOperand::Member(path) => Some(path.clone()),
                RepoOperand::Project(_) => None,
            })
            .collect();
        crate::sync::judge_source_pins(
            sync.ctx.active_path(),
            &crate::manifest::Project::from_dir(dir, sync.project.clone())?,
            "sync",
            &named,
            &mut report,
        );
    }
    let theirs = match vcs
        .read_file_at_revision(dir, &rev(&source), Path::new(Manifest::FILE_NAME))
        .map_err(anyhow::Error::from)
        .and_then(|text| Manifest::from_toml_str(&text))
    {
        Ok(theirs) => Some(theirs),
        Err(e) => {
            let mut judging = State::UnparseableProjectAt
                .of(&label)
                .project_repo()
                .subject(sync.source_name())
                .listing([format!("{e:#}")])
                .bind("<b>", sync.source_name())
                .doctor_fix_for(
                    vcs.read_file_at_revision(dir, &rev(&source), Path::new(Manifest::FILE_NAME))
                        .ok()
                        .as_deref(),
                    sync.source_name(),
                )
                .by_verb("sync");
            if let Some(there) = vcs
                .checkouts_of_branch(dir, &source_branch)?
                .into_iter()
                .find_map(|checkout| workspace_holding(&checkout, &sync.project))
            {
                judging = judging.bind_place("<b-checkout>", &there);
            }
            return Err(judging.judge(vcs).refused());
        }
    };
    let their_lock = lock_at(vcs, dir, &source);
    let base_commit = vcs.merge_bases(dir, &head, &source)?.into_iter().next();
    let base_manifest = base_commit
        .as_ref()
        .and_then(|b| crate::land::base_manifest_at(vcs, dir, b));
    let history = match &sync.from {
        SyncFrom::Target(_) => PinHistory::of(vcs, dir, &source)?.since(&head),
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
            members.push(member);
        }
    }

    let attached = match vcs.head_attachment(dir)? {
        HeadAttachment::Attached(attached) => attached,
        _ => {
            return Err(crate::untokened!(
                EnvironmentPassthrough,
                "the project repo left its branch while sync judged it"
            ))
        }
    };
    let project_scoped = |state: State| {
        state
            .of(&label)
            .project_repo()
            .checkout(dir)
            .bind("<T>", sync.source_name())
            .by_verb("sync")
    };
    let project_recorded = theirs.as_ref().and_then(|m| m.history);
    let mut project_merged = None;
    let project_act = if let Some(judged) =
        crate::set_history::contradicted(project_recorded, sync.history, false, project_scoped, vcs)
    {
        report.add(judged);
        Act::Nothing
    } else if resets_project {
        judge_reset_site(vcs, dir, &label, &sync.branch, &mut report)?;
        Act::Reset(source.clone())
    } else if vcs.is_ancestor(dir, &rev(&source), &rev(&head))? {
        Act::Nothing
    } else if vcs.is_ancestor(dir, &rev(&head), &rev(&source))? {
        Act::FastForward(source.clone())
    } else if sync.merging() {
        Act::Merge(source.clone())
    } else {
        let scoped = |state: State| {
            state
                .of(&label)
                .project_repo()
                .checkout(dir)
                .bind("<T>", sync.source_name())
                .by_verb("sync")
        };
        match crate::set_history::governing(
            theirs.as_ref().and_then(|m| m.history),
            sync.history,
            false,
            scoped,
            vcs,
        ) {
            Ok(history) => {
                let in_range = crate::set_history::merge_in_range(vcs, dir, &head, &source)?;
                let (how, merged) = crate::set_history::integration(history, in_range);
                project_merged = merged;
                match how {
                    Integration::Rebase => Act::Rebase(source.clone()),
                    Integration::Merge => Act::Merge(source.clone()),
                }
            }
            Err(judged) => {
                report.add(*judged);
                Act::Nothing
            }
        }
    };
    if project_act != Act::Nothing {
        let authored = crate::activate::owned_paths(sync.ctx.active_path(), &sync.project, &ours);
        let dirty: Vec<String> = vcs
            .tracked_dirty_file_names(dir)?
            .into_iter()
            .filter(|path| !authored.contains(path))
            .collect();
        report.extend(crate::diagnosis::judge_edits(vcs, &label, dir, dirty, None));
    }
    if !resets_project && !sync.merging() {
        let receiving = [source.clone()];
        judge_push_branch(vcs, dir, &label, &attached, &receiving, true, &mut report)?;
    }
    let carried_along = |act: &Act| matches!(act, Act::Follow(_) | Act::Rebase(_));
    let mut moved = Vec::new();
    if carried_along(&project_act) {
        moved.push((RepoKey::Project, vcs.head_revision(dir)?.id().clone()));
    }
    for member in &members {
        if carried_along(&member.act) {
            moved.push((
                RepoKey::Member(member.path.clone()),
                member.vcs.head_revision(&member.dir)?.id().clone(),
            ));
        }
    }
    if !moved.is_empty() {
        let manifest = Manifest::from_path(&dir.join(Manifest::FILE_NAME))?;
        let repos = crate::stack::Repos {
            root: sync.ctx.active_path(),
            primary: sync.ctx.primary_path(),
            project: &sync.project,
            manifest: &manifest,
        };
        let branch = crate::records::BranchName::parse(vcs, &sync.branch)
            .map_err(|e| crate::untokened!(EnvironmentPassthrough, "{}: {e:?}", sync.branch))?;
        let stack = crate::stack::walk(&repos, &branch, &moved, &[])?;
        crate::stack::judge(
            &stack,
            &repos,
            sync.source_name(),
            "sync",
            None,
            &mut report,
        )?;
    }
    report.into_result()?;
    Ok(Planned {
        source,
        project: project_act,
        merged: project_merged,
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
    vcs: &dyn Vcs,
    dir: &Path,
    label: &str,
    branch: &str,
    report: &mut Report,
) -> anyhow::Result<()> {
    crate::merge_upstream::judge_dependents(vcs, dir, branch, report)?;
    report.add_opt(crate::diagnosis::judge_edits(
        vcs,
        label,
        dir,
        vcs.dirty_file_names(dir).unwrap_or_default(),
        None,
    ));
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
                Diverged::Unpushed(_) => {
                    let judging = scoped(State::PushBranchDiverged)
                        .subject(format!("'{branch}': {ahead} ahead, {behind} behind"));
                    report.add(
                        if crate::backup::rebased_holding(vcs, dir, &branch, tip.id()) {
                            judging.by_verb("sync")
                        } else {
                            judging
                        }
                        .judge(vcs),
                    );
                }
                Diverged::Other => report.add(
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
        absent: Vec::new(),
        new_work: Vec::new(),
        dropped: Vec::new(),
        was: Some(head.clone()),
        merged: None,
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
    let source = match newer(mvcs, &mdir, &receiving, home.as_ref())?
        .map(|tip| tip.map(|t| t.id().clone()).unwrap_or_else(|| pin.clone()))
    {
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

    let recorded = inputs.theirs.and_then(|m| m.history_for(path));
    let member_scoped = |state: State| {
        state
            .of(path)
            .member()
            .checkout(&mdir)
            .bind("<m>", path)
            .bind("<T>", sync.source_name())
            .by_verb("sync")
    };
    if let Some(judged) =
        crate::set_history::contradicted(recorded, sync.history, true, member_scoped, mvcs)
    {
        report.add(judged);
        return Ok(Some(member));
    }
    if inputs.reset {
        let live = crate::pins::live_pins(sync.ctx.primary_path())?;
        let branch = crate::branch_ref::BranchRef::Local(RawRefName::new(attached.to_string()));
        let (was, now) = (rev(&head), rev(&source));
        let change = crate::pins::RefChange {
            branch: &branch,
            was: &was,
            now: Some(&now),
            was_backed_up: true,
        };
        let held = live.get(path).map(Vec::as_slice).unwrap_or_default();
        let stranded = crate::pins::stranded_by(mvcs, &mdir, held, &change, |_| false)?;
        if !stranded.is_empty() {
            report.add(
                State::PinWouldBeUnreachable
                    .of(path)
                    .member()
                    .checkout(&mdir)
                    .bind("<m>", path)
                    .listing(stranded.iter().map(|s| s.line(&change)))
                    .judge(mvcs),
            );
        }
        member.new_work = crate::lineage::new_work(mvcs, &mdir, &head, Some(&source), &pins)?;
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
        let scoped = |state: State| {
            state
                .of(path)
                .member()
                .checkout(&mdir)
                .bind("<m>", path)
                .bind("<T>", sync.source_name())
                .by_verb("sync")
        };
        match crate::set_history::governing(
            inputs.theirs.and_then(|m| m.history_for(path)),
            sync.history,
            true,
            scoped,
            mvcs,
        ) {
            Ok(history) => {
                let in_range = crate::set_history::merge_in_range(mvcs, &mdir, &head, &source)?;
                let (how, merged) = crate::set_history::integration(history, in_range);
                member.merged = merged;
                match how {
                    Integration::Rebase => Act::Rebase(source.clone()),
                    Integration::Merge => Act::Merge(source.clone()),
                }
            }
            Err(judged) => {
                report.add(*judged);
                Act::Nothing
            }
        }
    };
    if !follows || (inputs.dependents && !contained) {
        let judged = crate::lineage::judge_by_history(
            project_vcs().as_ref(),
            inputs.history,
            path,
            mvcs,
            &mdir,
            &head,
            &source,
        )?;
        match judged {
            Verdict::Passes { absent, .. } => member.absent = absent,
            Verdict::Off { base } => {
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
        }
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
    reported: &mut Reported,
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
    let branch = crate::records::BranchName::parse(sync.vcs.as_ref(), &sync.branch)
        .map_err(|e| crate::untokened!(EnvironmentPassthrough, "{}: {e:?}", sync.branch))?;
    let carrier = crate::stack::Carrier {
        repos: crate::stack::Repos {
            root: sync.ctx.active_path(),
            primary: sync.ctx.primary_path(),
            project: &sync.project,
            manifest: &pre_manifest,
        },
        op: &op.id,
        workspace: &op.workspace,
        verb: OpVerb::Sync,
        w: &branch,
        t: sync.source_name(),
    };
    carrier.finish_stopped()?;

    let mut moved: BTreeMap<RepoPath, CommitId> = BTreeMap::new();
    for (path, entry) in pre_manifest.iter_entries() {
        let Some(member_source) = take_in.upstream_tips.get(&RepoKey::Member(path.clone())) else {
            continue;
        };
        let dir = sync.ctx.active_path().join(path.as_path());
        let vcs = vcs_for(entry.vcs_type);
        crate::sync::hold_on_drift(vcs.as_ref(), path, &dir, Judging::member, OpVerb::Sync)?;
        let reset = resets.contains(&RepoOperand::Member(path.clone()));
        let how = if take_in.merged.contains(&RepoKey::Member(path.clone())) {
            Integration::Merge
        } else {
            Integration::Rebase
        };
        let tip = take_member(
            sync,
            op,
            path,
            &dir,
            vcs.as_ref(),
            member_source,
            reset,
            how,
            output,
            reported,
        )?;
        let before = vcs.resolve_savepoint(&dir, &key).map(|r| r.id().clone());
        if before.as_ref() != Some(&tip) || reset {
            moved.insert(path.clone(), tip);
        }
    }
    for (path, entry) in pre_manifest.iter_entries() {
        let dir = sync.ctx.active_path().join(path.as_path());
        let vcs = vcs_for(entry.vcs_type);
        if vcs.is_repo(&dir) {
            crate::sync::refresh_drift_after("sync", path, vcs.as_ref(), &dir);
        }
    }

    let resets_project = resets.iter().any(|r| matches!(r, RepoOperand::Project(_)));
    let vcs = sync.vcs.as_ref();
    let head = vcs.head_revision(&sync.dir)?.id().clone();
    let project_merges = sync.merging()
        || take_in.merged.contains(&RepoKey::Project)
        || (!resets_project
            && vcs.mid_op(&sync.dir).is_none()
            && !vcs.is_ancestor(&sync.dir, &rev(&source), &rev(&head))?
            && !vcs.is_ancestor(&sync.dir, &rev(&head), &rev(&source))?
            && {
                let (now, merged) =
                    integration_now(sync, op, None, &sync.dir, vcs, &head, &source)?;
                if let Some(merge) = merged {
                    report_integration(
                        output,
                        &op.workspace,
                        reported,
                        crate::op_state::LandingReport::Merged {
                            repo: RepoKey::Project,
                            merge: merge.clone(),
                        },
                        Some(crate::set_history::merged_line(&sync.label(), &merge)),
                    )?;
                }
                now == Integration::Merge
            });
    if project_merges {
        if !vcs.is_ancestor(&sync.dir, &rev(&source), &rev(&head))?
            || vcs.merge_in_flight(&sync.dir)?.is_some()
        {
            let (commit, pruning) = ProjectMerge {
                ctx: sync.ctx,
                project: &sync.project,
                verb: "sync",
                resume: crate::op_state::resume_command_at(OpVerb::Sync, sync.ctx.active_path()),
                opts: crate::activate::ActivateOptions::default(),
                message: format!("Merge {} into {}", sync.source_name(), sync.branch),
                head: Parent::First,
                members: MemberMerging::Done(moved.clone()),
                rebases: journaled_rebases(sync)?,
            }
            .run(&Sides {
                first: head.clone(),
                second: source.clone(),
            })?;
            for line in pruning
                .iter()
                .filter_map(crate::sync::MemberOutput::prune_line)
            {
                say(output, &line);
            }
            reported.members.extend(pruning);
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
            relock(sync, &pre_head, &source, &moved, false, &resets)?;
        }
    } else {
        set_aside_authored(sync, &pre_manifest)?;
        take_project(sync, &head, &source, resets_project, output, reported)?;
        let fix = crate::project_merge::settle_replayed_manifest(
            vcs,
            sync.label(),
            &sync.dir,
            sync.ctx.active_path(),
            OpVerb::Sync,
        )?;
        remap(sync, op, output, fix.as_deref())?;
        relock(sync, &pre_head, &source, &moved, resets_project, &resets)?;
    }
    let carries = carrier.carry()?;
    for landed in &carries.landed {
        say(output, &landed.line());
    }
    for line in &carries.held {
        say(output, line);
    }
    for carried in carries.carried {
        say(
            output,
            &format!(
                "{}: {}",
                carried.label,
                crate::stack::carried_line(
                    &carried.branch,
                    &carried.was,
                    &carried.now,
                    &carried.backup,
                    carried.outlives_op
                )
            ),
        );
        for moved in &carried.remapped {
            say(
                output,
                &format!(
                    "{}: re-pinned {} to {} in the carried '{}': the rebase dropped the commit \
                     it pinned, already on {}",
                    moved.member,
                    moved.was.short(),
                    moved.now.short(),
                    carried.branch,
                    sync.source_name()
                ),
            );
        }
    }
    settle(sync, &pre_manifest, reported, output)?;
    let hits = delivered_materialized_inputs(sync, &key);
    if !hits.is_empty() {
        say(
            output,
            &format!(
                "delivered changes touch materialized inputs ({}); run `rwv materialize` to \
                 bring the generated ecosystem state up to date",
                hits.iter()
                    .map(|(repo, file)| format!("{repo}: {file}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        reported.advisories.push(crate::workspace::AdvisoryOutput {
            kind: crate::workspace::AdvisoryKindOutput::DerivedStateStale,
            remedy: "rwv materialize".to_owned(),
            inputs: hits
                .iter()
                .map(|(repo, file)| match repo {
                    RepoKey::Project => format!("{}/{file}", sync.label()),
                    RepoKey::Member(path) => format!("{path}/{file}"),
                })
                .collect(),
        });
    }
    let mut repos = savepoint_repos(sync);
    for (path, entry) in pre_manifest.iter_entries() {
        let store = sync.ctx.primary_path().join(path.as_path());
        if !sync.ctx.active_path().join(path.as_path()).exists() && store.exists() {
            repos.push((store, vcs_for(entry.vcs_type)));
        }
    }
    let kept = crate::op_state::read_owner(&op.workspace)?
        .and_then(|owner| owner.take_in)
        .map(|take_in| take_in.backups)
        .unwrap_or_default();
    crate::landing_claim::LandingClaim::release_held_by(sync.vcs.as_ref(), &sync.dir, &op.id)?;
    op.finish(&repos);
    crate::backup::drop_rebased(
        sync.ctx.primary_path(),
        sync.ctx.active_path(),
        &sync.project,
        kept.iter().filter(|k| !k.outlives_op),
    );
    align(sync, reported, output)?;
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
    let paths = vcs.unmerged_paths(dir).unwrap_or_default();
    let workspace = sync.ctx.active_path();
    let resume = crate::op_state::resume_command_at(OpVerb::Sync, workspace);
    let entries = if dir == sync.dir {
        crate::manifest_conflict::enter_stop(vcs, dir, workspace, &resume)
    } else {
        Vec::new()
    };
    crate::refusal::refusal(
        RefusalKind::OpParked,
        format!(
            "rwv sync: the {} of {repo} onto {} stopped on a conflict ({}) at {}.\n{}{}then run \
             `{resume}`, or `{}` to put everything back as it was, keeping first as a \
             backup each edit and commit it would discard",
            match op {
                ConflictOp::Rebase => "rebase",
                _ => "merge",
            },
            sync.source_name(),
            paths.join(", "),
            dir.display(),
            crate::manifest_conflict::resolution_hint(vcs, op, &paths, &entries),
            entries
                .iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>(),
            crate::op_state::abort_command_at(workspace),
        ),
    )
}

fn rebase_refusal(
    sync: &Sync<'_>,
    repo: &str,
    dir: &Path,
    vcs: &dyn Vcs,
    error: VcsError,
) -> anyhow::Error {
    if matches!(error, VcsError::RebaseConflict { .. }) {
        return conflict_refusal(sync, repo, dir, vcs, ConflictOp::Rebase);
    }
    let gone = vcs
        .branch_under_rebase(dir)
        .ok()
        .flatten()
        .filter(|branch| matches!(vcs.resolve_local_branch_tip(dir, branch), Ok(None)));
    if let Some(branch) = gone {
        return crate::refusal::refusal(
            RefusalKind::OpParked,
            format!(
                "rwv sync: the rebase of {repo} onto {} cannot finish at {}: the branch it moves, '{branch}', is gone. Abort puts everything back as it was, '{branch}' included:\n{}",
                sync.source_name(),
                dir.display(),
                crate::op_state::abort_command_at(sync.ctx.active_path()),
            ),
        );
    }
    crate::refusal::refusal(
        RefusalKind::OpParked,
        format!(
            "rwv sync: the rebase of {repo} onto {} failed at {}: {error:#}\nfix the cause, then \
             run `{}`, or `{}` to put everything back as it was",
            sync.source_name(),
            dir.display(),
            crate::op_state::resume_command_at(OpVerb::Sync, sync.ctx.active_path()),
            crate::op_state::abort_command_at(sync.ctx.active_path()),
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
    how: Integration,
    output: OutputMode,
    reported: &mut Reported,
) -> anyhow::Result<CommitId> {
    let workspace = &op.workspace;
    if matches!(vcs.mid_op(dir), Some(ConflictOp::Rebase)) {
        vcs.rebase_continue(dir, DerivedContentPolicy::keep_target_side())
            .map_err(|e| rebase_refusal(sync, path.as_str(), dir, vcs, e))?;
        if let Some(replayed) = vcs.resolve_savepoint(dir, &SavepointKey::Op(op.id.clone())) {
            report_dropped_derived(path.as_str(), dir, vcs, &rev(source), &replayed);
        }
    }
    let attached = match vcs.head_attachment(dir)? {
        HeadAttachment::Attached(attached) => attached,
        _ => {
            return Err(crate::untokened!(
                EnvironmentPassthrough,
                "{path}: left its branch during the sync"
            ))
        }
    };
    let head = vcs.head_revision(dir)?;
    let branch = RawRefName::new(attached.to_string());
    let src = rev(source);
    if reset {
        if head.id() != source {
            let kept =
                vcs.create_backup(dir, &BackupSource::Branch(branch.clone()), &head, None)?;
            vcs.reset_attached_ref_keeping(&attached, &src, &kept)?;
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
    let mut merges = sync.merging() || how == Integration::Merge;
    let mut follows = false;
    if !merges && !vcs.is_ancestor(dir, &head, &src)? {
        let pins = PinHistory::of(
            sync.vcs.as_ref(),
            &sync.dir,
            take_in_tip(op)?.as_ref().unwrap_or(source),
        )
        .map(|h| h.of_member(path))
        .unwrap_or_default();
        follows = crate::lineage::new_work(vcs, dir, head.id(), Some(source), &pins)?.is_empty();
        if !follows {
            let (now, merged) = integration_now(sync, op, Some(path), dir, vcs, head.id(), source)?;
            merges = now == Integration::Merge;
            if let Some(merge) = merged {
                report_integration(
                    output,
                    workspace,
                    reported,
                    crate::op_state::LandingReport::Merged {
                        repo: RepoKey::Member(path.clone()),
                        merge: merge.clone(),
                    },
                    Some(crate::set_history::merged_line(path, &merge)),
                )?;
            }
        }
    }
    if merges {
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
            crate::vcs::DerivedContentPolicy::keep_incoming_side(),
        ) {
            Ok(joined) => {
                let tip = joined.tip().cloned().unwrap_or_else(|| head.id().clone());
                say(output, &format!("{path}: merged {}", source.short()));
                Ok(tip)
            }
            Err(MemberStop::Conflict(conflicted)) => Err(crate::project_merge::conflict_stop(
                "sync",
                sync.ctx.active_path(),
                &crate::op_state::resume_command_at(OpVerb::Sync, sync.ctx.active_path()),
                &conflicted,
                vcs,
                &[],
            )),
            Err(MemberStop::Refused(e)) => Err(e),
        };
    }
    if vcs.is_ancestor(dir, &head, &src)? {
        vcs.advance_attached_ref(&attached, &src)?;
        say(
            output,
            &format!("{path}: fast-forwarded to {}", source.short()),
        );
        return Ok(source.clone());
    }
    if follows {
        crate::op_state::journal_rebasing(
            workspace,
            crate::op_state::Rebasing {
                repo: RepoKey::Member(path.clone()),
                branch: branch.as_str().to_owned(),
                base: head.id().clone(),
                from: head.id().clone(),
                onto: source.clone(),
            },
        )?;
        let kept = vcs.create_backup(dir, &BackupSource::Branch(branch.clone()), &head, None)?;
        vcs.reset_attached_ref_keeping(&attached, &src, &kept)?;
        crate::test_pause::point("sync-member-followed");
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
    crate::op_state::journal_rebasing(
        workspace,
        crate::op_state::Rebasing {
            repo: RepoKey::Member(path.clone()),
            branch: branch.as_str().to_owned(),
            base: source.clone(),
            from: head.id().clone(),
            onto: source.clone(),
        },
    )?;
    let backup = crate::backup::keep_rebased(
        vcs,
        dir,
        &branch,
        &head,
        workspace,
        RepoKey::Member(path.clone()),
    )?;
    report_integration(
        output,
        workspace,
        reported,
        crate::op_state::LandingReport::Rebased {
            repo: RepoKey::Member(path.clone()),
            was: head.id().clone(),
            backup,
        },
        None,
    )?;
    vcs.rebase(dir, &src, &src, DerivedContentPolicy::keep_target_side())
        .map_err(|e| rebase_refusal(sync, path.as_str(), dir, vcs, e))?;
    report_dropped_derived(path.as_str(), dir, vcs, &src, &head);
    let tip = vcs.head_revision(dir)?.id().clone();
    say(output, &format!("{path}: rebased onto {}", source.short()));
    Ok(tip)
}

/// Name the declared derived paths a rebase onto `onto` resolved to its
/// side, so what `replayed` carried there is not what landed.
fn report_dropped_derived(
    repo: &str,
    dir: &Path,
    vcs: &dyn Vcs,
    onto: &ResolvedRevisionId,
    replayed: &ResolvedRevisionId,
) {
    let dropped = vcs
        .head_revision(dir)
        .and_then(|landed| vcs.derived_content_dropped_by_replay(dir, onto, replayed, &landed));
    match dropped {
        Ok(dropped) if dropped.is_empty() => {}
        Ok(dropped) => eprintln!(
            "rwv sync: {repo}: the rebase kept the target's version of these declared derived \
             paths, so what was replayed is not what landed:\n    {}\n  regenerate them from \
             their source of record in {repo} and commit; until then the tree is stale at those \
             paths",
            dropped.join("\n    ")
        ),
        Err(e) => eprintln!(
            "rwv sync: {repo}: could not read which declared derived paths the rebase resolved: \
             {e:#}"
        ),
    }
}

/// How a repo whose branch and source have each moved takes the source in,
/// judged from the repository as it stands, so a `--continue` re-judges what
/// a stop let the operator change: the target's setting at the tip taken in
/// (the member's entry, else the top level), else the run's `--history`; a
/// range holding a merge is merged; a rebase refuses a declared branch, and
/// carries the branch's dependents along, judging them again as it does.
/// `repo` is the member, `None` for the project.
fn integration_now(
    sync: &Sync<'_>,
    op: &Op,
    repo: Option<&RepoPath>,
    dir: &Path,
    vcs: &dyn Vcs,
    head: &CommitId,
    source: &CommitId,
) -> anyhow::Result<(Integration, Option<CommitId>)> {
    let label = repo.map_or_else(|| sync.label(), ToString::to_string);
    let scoped = |state: State| {
        let judging = state.of(&label);
        let judging = match repo {
            Some(path) => judging.member().bind("<m>", path),
            None => judging.project_repo(),
        };
        judging
            .checkout(dir)
            .bind("<T>", sync.source_name())
            .by_verb("sync")
    };
    let theirs = take_in_tip(op)?.and_then(|tip| manifest_at(sync.vcs.as_ref(), &sync.dir, &tip));
    let recorded = theirs.and_then(|m| match repo {
        Some(path) => m.history_for(path),
        None => m.history,
    });
    let history =
        crate::set_history::governing(recorded, sync.history, repo.is_some(), scoped, vcs)
            .map_err(|judged| judged.refused())?;
    let in_range = crate::set_history::merge_in_range(vcs, dir, head, source)?;
    let (how, merged) = crate::set_history::integration(history, in_range);
    if how == Integration::Merge {
        return Ok((how, merged));
    }
    let mut report = Report::default();
    let manifest = Manifest::from_path(&sync.dir.join(Manifest::FILE_NAME))?;
    let repos = crate::stack::Repos {
        root: sync.ctx.active_path(),
        primary: sync.ctx.primary_path(),
        project: &sync.project,
        manifest: &manifest,
    };
    let branch = crate::records::BranchName::parse(sync.vcs.as_ref(), &sync.branch)
        .map_err(|e| crate::untokened!(EnvironmentPassthrough, "{}: {e:?}", sync.branch))?;
    let key = repo.map_or(RepoKey::Project, |path| RepoKey::Member(path.clone()));
    let stack = crate::stack::walk(&repos, &branch, &[(key, head.clone())], &[])?;
    crate::stack::judge(
        &stack,
        &repos,
        sync.source_name(),
        "sync",
        None,
        &mut report,
    )?;
    report.into_result()?;
    Ok((how, merged))
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
    reported: &mut Reported,
) -> anyhow::Result<()> {
    let vcs = sync.vcs.as_ref();
    let dir = &sync.dir;
    let label = sync.label();
    if matches!(vcs.mid_op(dir), Some(ConflictOp::Rebase)) {
        vcs.rebase_continue(dir, DerivedContentPolicy::rewritten_after())
            .map_err(|e| rebase_refusal(sync, &label, dir, vcs, e))?;
        return Ok(());
    }
    let attached = match vcs.head_attachment(dir)? {
        HeadAttachment::Attached(attached) => attached,
        _ => {
            crate::project_merge::settle_replayed_manifest(
                vcs,
                &label,
                dir,
                sync.ctx.active_path(),
                OpVerb::Sync,
            )?;
            return Err(crate::untokened!(
                EnvironmentPassthrough,
                "{label}: left its branch during the sync"
            ));
        }
    };
    let branch = RawRefName::new(attached.to_string());
    let src = rev(source);
    let here = rev(head);
    if reset {
        if head != source {
            let kept =
                vcs.create_backup(dir, &BackupSource::Branch(branch.clone()), &here, None)?;
            vcs.reset_attached_ref_keeping(&attached, &src, &kept)?;
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
    crate::op_state::journal_rebasing(
        sync.ctx.active_path(),
        crate::op_state::Rebasing {
            repo: RepoKey::Project,
            branch: branch.as_str().to_owned(),
            base: source.clone(),
            from: head.clone(),
            onto: source.clone(),
        },
    )?;
    let backup = crate::backup::keep_rebased(
        vcs,
        dir,
        &branch,
        &here,
        sync.ctx.active_path(),
        RepoKey::Project,
    )?;
    report_integration(
        output,
        sync.ctx.active_path(),
        reported,
        crate::op_state::LandingReport::Rebased {
            repo: RepoKey::Project,
            was: head.clone(),
            backup,
        },
        None,
    )?;
    vcs.rebase(dir, &src, &src, DerivedContentPolicy::rewritten_after())
        .map_err(|e| rebase_refusal(sync, &label, dir, vcs, e))?;
    say(output, &format!("{label}: rebased onto {}", source.short()));
    Ok(())
}

/// Pin each replayed project commit at what the member rebases made of the
/// commits it pinned.
fn remap(sync: &Sync<'_>, op: &Op, output: OutputMode, fix: Option<&str>) -> anyhow::Result<()> {
    let rebases = crate::op_state::read_owner(&op.workspace)?
        .and_then(|owner| owner.take_in)
        .map(|take_in| take_in.rebases)
        .unwrap_or_default();
    let remapped = crate::remap::remap_rebased_project(
        sync.vcs.as_ref(),
        &sync.dir,
        sync.ctx.active_path(),
        &rebases,
        &op.workspace,
        fix,
    )?
    .unwrap_or_default();
    if let Some(dropped) = &remapped.emptied {
        say(output, &format!("{}: {}", sync.label(), dropped));
    }
    for moved in remapped.remapped {
        say(
            output,
            &format!(
                "{}: re-pinned {} to {}: the rebase dropped the commit it pinned, already on {}",
                moved.member,
                moved.was.short(),
                moved.now.short(),
                sync.source_name()
            ),
        );
    }
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

    fn named(&self, member: &RepoPath, name: &str) -> Option<CommitId> {
        let entry = self.manifest.get_entry(member)?;
        Some(
            vcs_for(entry.vcs_type)
                .resolve_revision(&self.root.join(member.as_path()), name)
                .ok()?
                .id()
                .clone(),
        )
    }
}

/// Discard the uncommitted versions of the files the integrations author in
/// the project repo: the project branch is about to move under them, and
/// settling regenerates each from the members it leaves.
fn set_aside_authored(sync: &Sync<'_>, manifest: &Manifest) -> anyhow::Result<()> {
    let vcs = sync.vcs.as_ref();
    let authored = crate::activate::owned_paths(sync.ctx.active_path(), &sync.project, manifest);
    for path in vcs.tracked_dirty_file_names(&sync.dir)? {
        if !authored.contains(&path) {
            continue;
        }
        let file = sync.dir.join(&path);
        let captured = std::fs::read(&file).unwrap_or_default();
        vcs.discard_captured_change(&sync.dir, Path::new(&path), &captured)?;
    }
    Ok(())
}

/// The members `--reset-to-target` names: the lock commit re-pins them by
/// name.
fn journaled_rebases(sync: &Sync<'_>) -> anyhow::Result<Vec<crate::op_state::Rebasing>> {
    Ok(crate::op_state::read_owner(sync.ctx.active_path())?
        .and_then(|owner| owner.take_in)
        .map(|take_in| take_in.rebases)
        .unwrap_or_default())
}

fn by_name_resets(resets: &[RepoOperand]) -> crate::repin::Naming {
    crate::repin::Naming::by_name(
        "sync",
        resets.iter().filter_map(|r| match r {
            RepoOperand::Member(path) => Some(path.clone()),
            RepoOperand::Project(_) => None,
        }),
    )
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
    resets: &[RepoOperand],
) -> anyhow::Result<()> {
    let vcs = sync.vcs.as_ref();
    let dir = &sync.dir;
    let manifest = Manifest::from_path(&dir.join(Manifest::FILE_NAME))?;
    let reach = MemberReach {
        root: sync.ctx.active_path(),
        manifest: &manifest,
    };
    let held = |commit: &CommitId| crate::unmerged::held_at(vcs, dir, commit, &reach);
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
            &reach,
        );
        merged.pins
    };
    let resolved: Vec<(RepoPath, ResolvedRevisionId)> =
        pins.iter().map(|(p, c)| (p.clone(), rev(c))).collect();
    let lock = crate::lock::with_pins(&manifest, None, resolved.iter().map(|(p, c)| (p, c)));
    crate::lock::write_lock_in(&lock, dir)?;
    let named = by_name_resets(resets);
    let jumps = crate::remap::rebase_jumps(
        sync.ctx.active_path(),
        &manifest,
        &journaled_rebases(sync)?,
        &held(vcs.head_revision(dir)?.id()).pins,
        &pins,
        sync.ctx.active_path(),
    )?;
    let jumps = jumps
        .into_iter()
        .filter(|jump| match &named {
            crate::repin::Naming::ByName { members, .. } => !members.contains(&jump.member),
            crate::repin::Naming::Unnamed => true,
        })
        .map(|jump| jump.annotation())
        .collect();
    let committed = crate::lock::commit_lock_file_annotated(
        vcs,
        dir,
        &format!("lock: relock after sync from {}", sync.source_name()),
        &named,
        jumps,
    )?;
    if committed {
        crate::test_pause::point("sync-relock-committed");
    }
    Ok(())
}

/// Align the workspace to the branch as the sync left it: members the new
/// entries drop are pruned, missing ones cloned, and the rest aligned.
fn settle(
    sync: &Sync<'_>,
    before: &Manifest,
    reported: &mut Reported,
    output: OutputMode,
) -> anyhow::Result<()> {
    let members = &mut reported.members;
    let now = Manifest::from_path(&sync.dir.join(Manifest::FILE_NAME))?;
    for (path, entry) in before.iter_entries() {
        let checkout = sync.ctx.active_path().join(path.as_path());
        let judged = members.iter().any(|m| match m {
            crate::sync::MemberOutput::Pruned { path: p, .. }
            | crate::sync::MemberOutput::PruneRefused { path: p, .. } => p == path.as_str(),
            _ => false,
        });
        if !now.contains_repo(path) && checkout.exists() && !judged {
            let member = match crate::sync::prune_dropped_repo(
                vcs_for(entry.vcs_type).as_ref(),
                sync.ctx,
                path,
            ) {
                Ok(()) => crate::sync::MemberOutput::pruned(path, &checkout),
                Err(e) => crate::sync::MemberOutput::prune_refused(path, &checkout, &e),
            };
            if let Some(line) = member.prune_line() {
                say(output, &line);
            }
            members.push(member);
        }
    }
    Ok(())
}

/// Align the workspace once the sync's record is gone: the members its
/// project branch now names are made and moved only then, so `rwv abort`
/// never meets a checkout the sync made.
fn align(sync: &Sync<'_>, reported: &mut Reported, output: OutputMode) -> anyhow::Result<()> {
    crate::test_pause::point("aligning-source");
    let run = crate::update::align_after_sync(sync.ctx, sync.jobs, output)?;
    reported.placements.extend(run.envelope.placements);
    run.tail
}

/// The inputs of the materialized project the sync changed, each with the
/// repo it is in: the project's `rwv.toml` and `rwv.lock`, and each member's
/// detection manifest for an integration the project enables. Empty where the
/// weave root does not present the project, since nothing is materialized
/// there to go stale.
fn delivered_materialized_inputs(sync: &Sync<'_>, key: &SavepointKey) -> Vec<(RepoKey, String)> {
    use crate::integration::Integration;
    let root = sync.ctx.active_path();
    let presented =
        crate::workspace::observe_root(root).and_then(|obs| obs.presented_project().cloned());
    if presented.as_ref() != Some(&sync.project) {
        return Vec::new();
    }
    let Ok(project) = crate::manifest::Project::from_dir_skip_lock(&sync.dir, sync.project.clone())
    else {
        return Vec::new();
    };
    let builtin = crate::integrations::builtin_integrations();
    let integrations: Vec<&dyn Integration> = builtin.iter().map(|b| b.as_ref()).collect();
    let config = crate::manifest::IntegrationConfig::default();
    let member_inputs: BTreeSet<&str> =
        crate::integration_runner::enabled_integrations(&integrations, &project.manifest, &config)
            .flat_map(|(integration, _)| integration.detection_manifests().iter().copied())
            .collect();
    let changed = |vcs: &dyn Vcs, dir: &Path| -> Vec<String> {
        let Some(pre) = vcs.resolve_savepoint(dir, key) else {
            return Vec::new();
        };
        match vcs.head_revision(dir) {
            Ok(head) if head != pre => vcs
                .changed_paths_between(dir, &pre, &head)
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    };
    let mut hits = Vec::new();
    let in_project = changed(sync.vcs.as_ref(), &sync.dir);
    for file in [Manifest::FILE_NAME, LockFile::FILE_NAME] {
        if in_project.iter().any(|p| p == file) {
            hits.push((RepoKey::Project, file.to_owned()));
        }
    }
    for (path, entry) in project.manifest.iter_entries() {
        let dir = root.join(path.as_path());
        if !dir.exists() {
            continue;
        }
        for file in changed(vcs_for(entry.vcs_type).as_ref(), &dir)
            .into_iter()
            .filter(|p| member_inputs.contains(p.as_str()))
        {
            hits.push((RepoKey::Member(path.clone()), file));
        }
    }
    hits
}

/// Where `rwv sync --reset-to-target <m>` stands for a feature's member: the
/// tip of the branch it lands on, and whether that spelling, run here, would
/// plan without a refusal.
pub(crate) struct ResetOffer {
    pub(crate) landing_tip: CommitId,
    pub(crate) runs: bool,
}

impl ResetOffer {
    /// `pin` is on the branch the member lands on.
    pub(crate) fn landed(&self, vcs: &dyn Vcs, dir: &Path, pin: &CommitId) -> bool {
        vcs.is_ancestor(dir, &rev(pin), &rev(&self.landing_tip))
            .unwrap_or(false)
    }

    /// The `patch-equivalent` fact for `unlanded` against where it lands.
    pub(crate) fn patch_fact(
        &self,
        vcs: &dyn Vcs,
        dir: &Path,
        unlanded: &CommitId,
    ) -> Option<Fact> {
        patch_fact(vcs, dir, unlanded, &self.landing_tip)
    }
}

/// The reset offer for `path` in the feature `ctx` has checked out; `None`
/// where the branch is no feature or the member lands nowhere.
pub(crate) fn reset_offer(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    path: &RepoPath,
) -> Option<ResetOffer> {
    let dir = project_dir(ctx.active_path(), project);
    let vcs = project_vcs();
    let branch = crate::branch::checked_out(vcs.as_ref(), &dir).ok()??;
    let name = crate::branch::named(vcs.as_ref(), project, &dir, &branch).ok()?;
    let Record::Feature { target, as_mode } =
        crate::branch::recorded_value(vcs.as_ref(), project, &dir, &name).ok()?
    else {
        return None;
    };
    let url_remote = vcs.project_url_remote(&dir).ok().flatten();
    let tip = newer(
        vcs.as_ref(),
        &dir,
        &RawRefName::new(target.to_string()),
        url_remote.as_ref(),
    )
    .ok()?
    .ok()??;
    let ours = Manifest::from_path(&dir.join(Manifest::FILE_NAME)).ok()?;
    let entry = ours.get_entry(path)?;
    let theirs = manifest_at(vcs.as_ref(), &dir, tip.id());
    let receiving = receiving_branch(
        path,
        target.as_str(),
        Some(as_mode),
        &ours,
        theirs.as_ref(),
        None,
    )?;
    let mdir = ctx.active_path().join(path.as_path());
    let mvcs = vcs_for(entry.vcs_type);
    let landing_tip = newer(mvcs.as_ref(), &mdir, &receiving, None)
        .ok()?
        .ok()??
        .id()
        .clone();
    let sync = Sync {
        ctx,
        project: project.clone(),
        dir,
        vcs,
        branch,
        from: SyncFrom::Target(target.to_string()),
        target: Some(target.to_string()),
        history: None,
        jobs: 1,
    };
    let runs = plan(&sync, Some(as_mode), &[RepoOperand::Member(path.clone())]).is_ok();
    Some(ResetOffer { landing_tip, runs })
}

/// What the lineage rule reads for a feature's members: its target's pins
/// and entries.
pub(crate) struct FeatureLineage {
    target: String,
    as_mode: Mode,
    pins: Box<PinHistory>,
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
    let Ok(Some(tip)) = newer(
        vcs.as_ref(),
        &dir,
        &RawRefName::new(target.to_string()),
        url_remote.as_ref(),
    )?
    else {
        return Ok(None);
    };
    Ok(Some(FeatureLineage {
        target: target.to_string(),
        as_mode,
        pins: Box::new(PinHistory::of(vcs.as_ref(), &dir, tip.id())?),
        theirs: manifest_at(vcs.as_ref(), &dir, tip.id()),
    }))
}

/// Which `--merge-upstream` merges a member: `update --remote` with the
/// member's upstream, or `pull` with the pin the project branch's upstream
/// holds for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MergeUpstreamBy {
    UpdateRemote,
    Pull { project: String },
}

impl FeatureLineage {
    /// Judge each member a project merge merges, its checkout's commit taking
    /// the pin the other side holds, by the lineage rule.
    pub(crate) fn judge_member_merges(
        &self,
        root: &Path,
        merged: &Manifest,
        merging: &[crate::project_merge::MemberMerge],
        project: &str,
        report: &mut Report,
    ) -> anyhow::Result<()> {
        for member in merging {
            let dir = root.join(member.path.as_path());
            let vcs = vcs_for(member.entry.vcs_type);
            if !vcs.is_repo(&dir) {
                continue;
            }
            let local = vcs.head_revision(&dir)?.id().clone();
            report.extend(self.judge_merge_upstream(
                merged,
                &member.path,
                &dir,
                vcs.as_ref(),
                &local,
                &member.first,
                MergeUpstreamBy::Pull {
                    project: project.to_owned(),
                },
            )?);
        }
        Ok(())
    }

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
        by: MergeUpstreamBy,
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
            (Some(their_base), Some(receiving)) => match newer(vcs, dir, &receiving, None)? {
                Ok(Some(u)) => vcs.is_ancestor(dir, &rev(&their_base), &u)?,
                _ => false,
            },
            _ => false,
        };
        let judging = State::PinOffLineage
            .of(path)
            .checkout(dir)
            .bind("<m>", path);
        let judging = match by {
            MergeUpstreamBy::UpdateRemote => judging
                .member()
                .bind("<repo>", path)
                .subject(format!(
                    "its base {} is not on its upstream ({})",
                    base.short(),
                    upstream.short()
                ))
                .by_verb("update")
                .only_forms(&[if on_receiving {
                    Form::C3UpdateRemote
                } else {
                    Form::C27
                }]),
            MergeUpstreamBy::Pull { project } => judging
                .bind("<repo>", project)
                .subject(format!(
                    "its base {} is not on the pin the upstream holds ({})",
                    base.short(),
                    upstream.short()
                ))
                .by_verb("pull")
                .only_forms(&[if on_receiving {
                    Form::C3Pull
                } else {
                    Form::C27
                }]),
        };
        Ok(Some(judging.judge(vcs)))
    }
}
