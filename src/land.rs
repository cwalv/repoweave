//! `rwv land`: where a feature lands, and what refuses its landing.
//!
//! [`resolve`] reads the record of the checked-out feature `W` and finds its
//! target `T`, `T`'s tip and the workspace `T` is checked out in. [`judge`]
//! plans each member's receiving branch `U` and judges every refusal the
//! landing owes before anything moves; the phase machine in `crate::sync`
//! composes in the source and fast-forwards the receiving branches.

use crate::diagnosis::{Form, Judged, Report, State};
use crate::lineage::{Lineage, PinHistory};
use crate::manifest::{
    LockFile, Manifest, ProjectName, RepoEntry, RepoKey, RepoOperand, RepoPath, Role,
};
use crate::op_state::{CreatedRef, LeftBehind, Moves};
use crate::records::{BranchName, Mode, Record, Store};
use crate::refusal::RefusalKind;
use crate::vcs::{
    project_vcs, vcs_for, CommitId, DestroyProof, HeadAttachment, RawRefName, ResolvedRevisionId,
    Site, Vcs, VcsError,
};
use crate::workspace::{project_dir, project_rel_path, Checkout, WorkspaceContext};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A landing of the checked-out feature into its target, as resolved before
/// the op begins.
#[derive(Debug, Clone)]
pub struct Landing {
    pub project: ProjectName,
    /// The checked-out feature, `W`.
    pub feature: BranchName,
    /// The branch it lands into, `T`.
    pub target: BranchName,
    /// The workspace that has `T` checked out.
    pub target_workspace: PathBuf,
    /// The newer of `T` and its upstream.
    pub target_tip: CommitId,
    /// `T`'s mode as its record holds it now.
    pub target_mode: Mode,
    /// Refusals judged while resolving, reported with the rest.
    pub judged: Report,
    /// The repos whose entries in `T` the landing may change, as
    /// `--allow-member-change` names them.
    pub allow_member_change: Vec<RepoOperand>,
}

/// What [`judge`] planned.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Each member's receiving branch: the one map of every branch the
    /// landing composes onto, moves, creates or follows.
    pub receiving: BTreeMap<RepoPath, Receiving>,
    /// For each named declared-branch change, the commits `T`'s pin reaches
    /// that the new receiving branch lacks.
    pub left_behind: Vec<LeftBehind>,
    /// The commits each moved member's receiving branch holds beyond `T`'s
    /// pin, which the landing pins into `T` with the member's work.
    pub beyond_pin: BTreeMap<RepoPath, Vec<CommitSummary>>,
}

impl Plan {
    /// The facts the landing reports before it composes, one line each.
    pub fn fact_lines(&self, target: &BranchName) -> Vec<String> {
        self.beyond_pin
            .iter()
            .filter(|(_, commits)| !commits.is_empty())
            .map(|(path, commits)| {
                format!(
                    "{path}: the branch it lands on holds {} commit(s) beyond '{target}''s pin, \
                     which the landing pins into '{target}': {}",
                    commits.len(),
                    commits
                        .iter()
                        .map(|c| format!("{} {}", c.id.short(), c.subject))
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            })
            .collect()
    }
}

/// A receiving branch `U`, its tip as the landing read it (the newer of it
/// and its upstream; for a branch the landing creates, where it starts), and
/// how the landing moves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receiving {
    pub branch: RawRefName,
    pub tip: CommitId,
    pub moves: Moves,
}

impl Plan {
    /// The members the landing composes onto their receiving branch's tip.
    pub fn composed(&self) -> impl Iterator<Item = (&RepoPath, &Receiving)> {
        self.receiving
            .iter()
            .filter(|(_, r)| matches!(r.moves, Moves::InCheckout | Moves::AsRef { .. }))
    }
}

/// How the landing composes a branch that does not contain the tip it lands
/// on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Composing {
    /// Rebased onto that tip.
    Rebase,
    /// Not composed: such a branch refuses.
    FastForwardOnly,
}

/// A range a rebase would replay that holds a merge is never rebased: the
/// one judgment `sync` makes, made here for the landing so it joins the
/// landing's one report.
#[allow(clippy::too_many_arguments)]
fn judge_merge_in_range(
    composing: Composing,
    vcs: &dyn Vcs,
    dir: &Path,
    repo: &str,
    head: &CommitId,
    onto: &CommitId,
    target: &BranchName,
    report: &mut Report,
) -> anyhow::Result<()> {
    if composing == Composing::Rebase && !vcs.is_ancestor(dir, &rev(onto), &rev(head))? {
        crate::feature_sync::judge_merge_in_range(
            vcs,
            dir,
            repo,
            head,
            onto,
            target.as_str(),
            report,
        )?;
    }
    Ok(())
}

fn rev(commit: &CommitId) -> ResolvedRevisionId {
    ResolvedRevisionId::from(commit.clone())
}

/// Resolve the landing the checked-out feature makes: `--target` for one run,
/// else the record's. A refusal that leaves nothing to land into refuses here,
/// with every refusal judged before it.
pub fn resolve(
    ctx: &WorkspaceContext,
    target_flag: Option<&str>,
    allow_member_change: Vec<RepoOperand>,
) -> anyhow::Result<Landing> {
    let project = ctx.require_bound_project()?.clone();
    let dir = project_dir(ctx.active_path(), &project);
    let vcs = project_vcs();
    let vcs = vcs.as_ref();
    let label = project_rel_path(&project);
    let Some(checked_out) = crate::branch::checked_out(vcs, &dir)? else {
        return Err(State::AlignmentProjectDetached
            .of(&label)
            .project_repo()
            .checkout(&dir)
            .judge(vcs)
            .refused());
    };
    let feature = crate::branch::named(vcs, &project, &dir, &checked_out)?;
    let record = crate::branch::recorded_value(vcs, &project, &dir, &feature)?;
    let mut judged = Report::default();
    judged.extend(crate::alignment::unseen_record_change(
        vcs, &project, &dir, &feature, &record, "land",
    )?);
    let (target, as_mode) = match (&record, target_flag) {
        (Record::Declared, _) => {
            judged.add(
                State::TargetNoTarget
                    .of(&label)
                    .project_repo()
                    .subject(&feature)
                    .bind("<W>", &feature)
                    .checkout(&dir)
                    .by_verb("land")
                    .judge(vcs),
            );
            return Err(refused_before_target(ctx, &feature, None, judged));
        }
        (Record::Feature { .. }, Some(flag)) => {
            (crate::branch::named(vcs, &project, &dir, flag)?, None)
        }
        (Record::Feature { target, as_mode }, None) => (target.clone(), Some(*as_mode)),
    };
    let target_record = crate::branch::recorded_value(vcs, &project, &dir, &target)?;
    if let Some(recorded_as) = as_mode {
        let now = target_record.mode();
        if now != recorded_as {
            judged.add(
                State::TargetModeChanged
                    .of(&label)
                    .project_repo()
                    .subject(format!(
                        "'{target}' was {} when '{feature}''s record was written; it is {} now",
                        recorded_as.token(),
                        now.token()
                    ))
                    .bind("<W>", &feature)
                    .bind("<T>", &target)
                    .bind("<as>", recorded_as.token())
                    .checkout(&dir)
                    .by_verb("land")
                    .only_forms(match recorded_as {
                        Mode::Declared => &[Form::C12Feature, Form::C12RestoreTarget],
                        Mode::Feature => &[Form::C12Feature],
                    })
                    .judge(vcs),
            );
        }
    }
    let target_ref = RawRefName::new(target.as_str());
    if target_flag.is_none() {
        if let Some(gone) = judge_unrelated_target(vcs, &project, &dir, &feature, &target)? {
            judged.add(gone);
            return Err(refused_before_target(ctx, &feature, Some(&target), judged));
        }
    }
    let url_remote = vcs.project_url_remote(&dir).ok().flatten();
    use crate::push_branch::NewerOfBranchAndUpstream as Newer;
    let target_tip = match crate::push_branch::newer_of_branch_and_upstream(
        vcs,
        &dir,
        &target_ref,
        url_remote.as_ref(),
    )? {
        Newer::Tip(tip) => tip.id().clone(),
        Newer::Nowhere => {
            judged.add(
                crate::branch::target_gone(vcs, &project, &dir, &feature, &target)?
                    .by_verb("land")
                    .judge(vcs),
            );
            return Err(refused_before_target(ctx, &feature, Some(&target), judged));
        }
        Newer::Diverged {
            local,
            upstream_tip,
            ..
        } => {
            let (ahead, behind) = vcs.ahead_behind(&dir, upstream_tip.id(), local.id());
            judged.add(
                State::PushBranchDiverged
                    .of(&label)
                    .project_repo()
                    .subject(format!(
                        "'{target}', which this lands into, and its upstream have diverged \
                             ({ahead} ahead, {behind} behind)"
                    ))
                    .bind("<repo>", &label)
                    .checkout(&dir)
                    .by_verb("land")
                    .judge(vcs),
            );
            local.id().clone()
        }
    };
    let target_workspace = match target_checkout(ctx, &project, &dir, &target, &label)? {
        Ok(workspace) => workspace,
        Err(refused) => {
            judged.add(refused);
            return Err(refused_before_target(ctx, &feature, Some(&target), judged));
        }
    };
    Ok(Landing {
        project,
        feature,
        target,
        target_workspace,
        target_tip,
        target_mode: target_record.mode(),
        judged,
        allow_member_change,
    })
}

/// The refusal of a landing with nothing to land into: what refused it, and
/// beside it every refusal the source owes on its own.
fn refused_before_target(
    ctx: &WorkspaceContext,
    feature: &BranchName,
    target: Option<&BranchName>,
    mut judged: Report,
) -> anyhow::Error {
    if let Err(e) = judge_source(ctx, feature, target, &mut judged) {
        judged.note(format!("{e:#}"));
    }
    refusal(judged)
}

/// The refusals the source owes whatever it lands into: its edits, where its
/// checkouts stand to its lock and to the branch they are aligned for, and
/// the weave branches landing into `feature`.
fn judge_source(
    ctx: &WorkspaceContext,
    feature: &BranchName,
    target: Option<&BranchName>,
    report: &mut Report,
) -> anyhow::Result<()> {
    crate::sync::judge_landing_source(ctx, report)?;
    let project = ctx.require_bound_project()?;
    judge_source_members(ctx.active_path(), project, feature, target, report)?;
    Ok(())
}

/// The members of the source refused for their alignment, each judged into
/// `report`, after the weave branches that land into `feature`.
fn judge_source_members(
    source_root: &Path,
    project: &ProjectName,
    feature: &BranchName,
    target: Option<&BranchName>,
    report: &mut Report,
) -> anyhow::Result<BTreeSet<RepoPath>> {
    let vcs = project_vcs();
    let vcs = vcs.as_ref();
    let dir = project_dir(source_root, project);
    let label = project_rel_path(project);
    for dependent in crate::merge_upstream::dependents_of(vcs, &dir, feature.as_str())? {
        let mut judging = State::TargetDependents
            .of(&label)
            .project_repo()
            .subject(format!("{dependent} lands into {feature}"))
            .bind("<dep>", &dependent)
            .bind("<W>", &dependent)
            .checkout(&dir)
            .by_verb("land");
        if let Some(target) = target {
            judging = judging.bind("<T>", target);
        }
        report.add(judging.judge(vcs));
    }
    let ours = Manifest::from_path(&dir.join(Manifest::FILE_NAME))?;
    let head = vcs.head_revision(&dir)?.id().clone();
    let our_lock = lock_at(vcs, &dir, &head);
    let aligner = crate::alignment::Aligner::open(
        source_root,
        project,
        crate::alignment::Accounting::default(),
    )?;
    let mut refused = BTreeSet::new();
    for (path, entry) in ours.iter_entries() {
        let mdir = source_root.join(path.as_path());
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
            refused.insert(path.clone());
        }
    }
    Ok(refused)
}

fn refusal(report: Report) -> anyhow::Error {
    match report.into_result() {
        Err(e) => e,
        Ok(()) => crate::untokened!(InternalInvariant, "a refusal judged nothing"),
    }
}

/// A record published on a remote names a target this machine resolves only
/// through that remote: its branch published there, and a local branch of
/// that name taking in from it or configured to publish to it. Anything else is that
/// target gone here, never an unrelated branch that happens to share its
/// name.
fn judge_unrelated_target(
    vcs: &dyn Vcs,
    project: &ProjectName,
    dir: &Path,
    feature: &BranchName,
    target: &BranchName,
) -> anyhow::Result<Option<Judged>> {
    let store = Store::open(vcs, dir)?;
    let view = store.view()?;
    let holding = store.remotes_holding(&view, feature)?;
    if holding.is_empty() {
        return Ok(None);
    }
    let target_ref = RawRefName::new(target.as_str());
    let published: Vec<_> = vcs
        .remote_copies(dir, &target_ref)?
        .into_iter()
        .filter(|copy| holding.contains(&copy.remote))
        .collect();
    let local = vcs.resolve_local_branch_tip(dir, &target_ref)?;
    let same_branch_there = match &local {
        None => true,
        Some(_) => {
            let takes_in = vcs.upstream_of(dir, &target_ref)?.is_some_and(|up| {
                holding.contains(&up.remote) && up.branch.as_str() == target.as_str()
            });
            takes_in
                || vcs
                    .push_target_of(dir, &target_ref, None)?
                    .is_some_and(|remote| holding.contains(&remote))
        }
    };
    if !published.is_empty() && same_branch_there {
        return Ok(None);
    }
    let remotes = holding
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let why = if published.is_empty() {
        format!(
            "'{feature}''s record was published on {remotes}, where '{target}' is not published"
        )
    } else {
        format!(
            "'{feature}''s record was published on {remotes}; the local '{target}' neither takes in \
             from there nor publishes there, so it is another branch of that name"
        )
    };
    Ok(Some(
        crate::branch::target_gone(vcs, project, dir, feature, target)?
            .subject(why)
            .by_verb("land")
            .judge(vcs),
    ))
}

/// The workspace that has `target` checked out: never the source, and from
/// the primary never a workweave, which a landing from the primary would
/// leave on the branch the primary switches to.
fn target_checkout(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    dir: &Path,
    target: &BranchName,
    label: &str,
) -> anyhow::Result<Result<PathBuf, Judged>> {
    let vcs = project_vcs();
    let vcs = vcs.as_ref();
    let own = crate::path_spelling::CanonicalPath::of(dir);
    let checkouts: Vec<PathBuf> = vcs
        .checkouts_of_branch(dir, &RawRefName::new(target.as_str()))?
        .into_iter()
        .filter(|c| crate::path_spelling::CanonicalPath::of(c) != own)
        .collect();
    let Some(checkout) = checkouts.first() else {
        return Ok(Err(State::TargetNotCheckedOut
            .of(label)
            .project_repo()
            .subject(target)
            .bind("<W>", target)
            .checkout(dir)
            .by_verb("land")
            .judge(vcs)));
    };
    let at = crate::path_spelling::CanonicalPath::of(checkout);
    let root = checkout
        .parent()
        .and_then(Path::parent)
        .filter(|root| crate::path_spelling::CanonicalPath::of(&project_dir(root, project)) == at)
        .map(Path::to_path_buf);
    let recorded = crate::sync::recorded_workweaves(ctx.primary_path())?;
    let raw = || {
        crate::sync::judge_checkout(
            vcs,
            label,
            checkout,
            dir,
            &recorded,
            Some(("land", target.as_str())),
        )
    };
    let Some(root) = root else {
        return Ok(Err(raw()));
    };
    let in_workweave = recorded
        .iter()
        .any(|(id, at_root)| &id.project == project && at_root.contains(&at));
    if in_workweave && matches!(ctx.checkout, Checkout::Primary { .. }) {
        return Ok(Err(raw()));
    }
    if WorkspaceContext::resolve_for_project(&root, project).is_err() {
        return Ok(Err(raw()));
    }
    Ok(Ok(root))
}

/// The inputs a landing judges its members against, read at `T`'s tip.
struct Sides {
    ours: Manifest,
    theirs: Manifest,
    base: Option<Manifest>,
    our_lock: Option<LockFile>,
    their_lock: Option<LockFile>,
    base_lock: Option<LockFile>,
    history: PinHistory,
}

fn pin_of(lock: Option<&LockFile>, path: &RepoPath) -> Option<CommitId> {
    lock.and_then(|lock| lock.get_entry(path))
        .and_then(|e| e.version.as_commit_id())
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

/// Judge every refusal the landing owes beyond what [`resolve`] judged, and
/// plan each `owned` or `fork` member's receiving branch. Nothing moves.
pub fn judge(
    landing: &Landing,
    source_root: &Path,
    primary: &Path,
    composing: Composing,
    report: &mut Report,
) -> anyhow::Result<Plan> {
    let vcs = project_vcs();
    let vcs = vcs.as_ref();
    let dir = project_dir(source_root, &landing.project);
    let label = project_rel_path(&landing.project);
    let head = vcs.head_revision(&dir)?.id().clone();
    let Some(theirs) = manifest_at(vcs, &dir, &landing.target_tip) else {
        report.add(
            State::UnparseableProjectAt
                .of(&label)
                .project_repo()
                .subject(format!(
                    "'{}' at {}",
                    landing.target,
                    landing.target_tip.short()
                ))
                .bind("<b>", &landing.target)
                .checkout(&dir)
                .judge(vcs),
        );
        return Ok(Plan::default());
    };
    let base_commit = vcs
        .merge_bases(&dir, &head, &landing.target_tip)?
        .into_iter()
        .next();
    let sides = Sides {
        ours: Manifest::from_path(&dir.join(Manifest::FILE_NAME))?,
        theirs,
        base: base_commit.as_ref().and_then(|b| manifest_at(vcs, &dir, b)),
        our_lock: lock_at(vcs, &dir, &head),
        their_lock: lock_at(vcs, &dir, &landing.target_tip),
        base_lock: base_commit.as_ref().and_then(|b| lock_at(vcs, &dir, b)),
        history: PinHistory::of(vcs, &dir, &landing.target_tip)?,
    };
    judge_entry_changes(landing, &sides, source_root, &dir, &label, vcs, report)?;
    judge_merge_in_range(
        composing,
        vcs,
        &dir,
        &label,
        &head,
        &landing.target_tip,
        &landing.target,
        report,
    )?;
    let off_alignment = judge_source_members(
        source_root,
        &landing.project,
        &landing.feature,
        Some(&landing.target),
        report,
    )?;
    let accounting = crate::alignment::Accounting::scan(primary)?;
    let target_aligner = crate::alignment::Aligner::open(
        &landing.target_workspace,
        &landing.project,
        crate::alignment::Accounting::default(),
    )?;
    let as_mode = landing.target_mode;
    let mut plan = Plan::default();
    for (path, entry) in sides.ours.iter_entries() {
        let mdir = source_root.join(path.as_path());
        let mvcs = vcs_for(entry.vcs_type);
        if !mvcs.is_repo(&mdir) || off_alignment.contains(path) {
            continue;
        }
        let member = Member {
            path,
            entry,
            dir: &mdir,
            vcs: mvcs.as_ref(),
        };
        if sides.theirs.get_entry(path).is_none() {
            plan_added_member(landing, &sides, &member, as_mode, &mut plan)?;
            continue;
        }
        if !matches!(entry.role, Role::Owned | Role::Fork) {
            judge_carried_pin(landing, &sides, &member, as_mode, report)?;
            continue;
        }
        let judging = Judging {
            landing,
            sides: &sides,
            accounting: &accounting,
            target_aligner: &target_aligner,
            as_mode,
            composing,
        };
        judge_member(&judging, &member, &mut plan, report)?;
    }
    Ok(plan)
}

struct Member<'a> {
    path: &'a RepoPath,
    entry: &'a RepoEntry,
    dir: &'a Path,
    vcs: &'a dyn Vcs,
}

/// One field of a repo's entry the landing would change, as `T` holds it and
/// as `W` does.
#[derive(Debug, Clone)]
struct FieldChange {
    name: &'static str,
    theirs: String,
    ours: String,
}

impl FieldChange {
    fn line(&self) -> String {
        format!("{}: {} → {}", self.name, self.theirs, self.ours)
    }
}

/// What the landing would change in `T` for one repo: fields of its entry,
/// or, where `W` removed it, the whole entry `T` holds.
struct Changed {
    repo: RepoOperand,
    fields: Vec<FieldChange>,
    removed: Option<RepoEntry>,
}

/// Each field of `ours` that differs from `theirs`.
fn changed_fields(theirs: &RepoEntry, ours: &RepoEntry) -> Vec<FieldChange> {
    let mut out = Vec::new();
    let mut field = |name: &'static str, theirs: String, ours: String| {
        if theirs != ours {
            out.push(FieldChange { name, theirs, ours });
        }
    };
    field(
        "type",
        format!("{:?}", theirs.vcs_type).to_lowercase(),
        format!("{:?}", ours.vcs_type).to_lowercase(),
    );
    field("url", theirs.url.to_string(), ours.url.to_string());
    field("role", theirs.role.to_string(), ours.role.to_string());
    field(
        "branch",
        theirs.branch.local_counterpart().as_str().to_owned(),
        ours.branch.local_counterpart().as_str().to_owned(),
    );
    field(
        "history",
        crate::set_history::spelled(theirs.history),
        crate::set_history::spelled(ours.history),
    );
    field(
        "recurse-submodules",
        bool::from(theirs.recurse_submodules).to_string(),
        bool::from(ours.recurse_submodules).to_string(),
    );
    out
}

/// A `dependency` or `reference` member keeps the pin `W` gave it since the
/// merge base; where `T`'s pin is not an ancestor of that one, the landing
/// moves `T`'s pin backward.
fn backward_pin(
    sides: &Sides,
    path: &RepoPath,
    ours: &RepoEntry,
    source_root: &Path,
) -> Option<FieldChange> {
    if !matches!(ours.role, Role::Dependency | Role::Reference) {
        return None;
    }
    let mine = pin_of(sides.our_lock.as_ref(), path)?;
    let theirs = pin_of(sides.their_lock.as_ref(), path)?;
    if Some(&mine) == pin_of(sides.base_lock.as_ref(), path).as_ref() || mine == theirs {
        return None;
    }
    let reaches = vcs_for(ours.vcs_type)
        .is_ancestor(
            &source_root.join(path.as_path()),
            &rev(&theirs),
            &rev(&mine),
        )
        .unwrap_or(false);
    (!reaches).then(|| FieldChange {
        name: "pin",
        theirs: theirs.short().to_owned(),
        ours: mine.short().to_owned(),
    })
}

/// The changes the landing would make to `T`'s member entries and top-level
/// `history`: each one `W` made since the merge base that `T` does not
/// already hold, and each backward pin of a member `W` re-pinned. A change
/// `--allow-member-change` names lands; every other one refuses, and a name
/// that matches no change is refused as naming nothing.
fn judge_entry_changes(
    landing: &Landing,
    sides: &Sides,
    source_root: &Path,
    dir: &Path,
    label: &str,
    vcs: &dyn Vcs,
    report: &mut Report,
) -> anyhow::Result<()> {
    let mut changes: Vec<Changed> = Vec::new();
    for (path, theirs) in sides.theirs.iter_entries() {
        let was = sides.base.as_ref().and_then(|b| b.get_entry(path));
        match sides.ours.get_entry(path) {
            Some(ours) => {
                let changed_here = was.is_none_or(|was| !changed_fields(was, ours).is_empty());
                let mut fields = if changed_here {
                    changed_fields(theirs, ours)
                } else {
                    Vec::new()
                };
                fields.extend(backward_pin(sides, path, ours, source_root));
                if !fields.is_empty() {
                    changes.push(Changed {
                        repo: RepoOperand::Member(path.clone()),
                        fields,
                        removed: None,
                    });
                }
            }
            None if was.is_some() => changes.push(Changed {
                repo: RepoOperand::Member(path.clone()),
                fields: Vec::new(),
                removed: Some(theirs.clone()),
            }),
            None => {}
        }
    }
    let was = sides.base.as_ref().and_then(|b| b.history);
    if sides.ours.history != was && sides.ours.history != sides.theirs.history {
        changes.push(Changed {
            repo: RepoOperand::Project(landing.project.clone()),
            fields: vec![FieldChange {
                name: "history",
                theirs: crate::set_history::spelled(sides.theirs.history),
                ours: crate::set_history::spelled(sides.ours.history),
            }],
            removed: None,
        });
    }
    for named in &landing.allow_member_change {
        if !changes.iter().any(|c| &c.repo == named) {
            crate::refuse!(
                RefusalKind::InapplicableFlag,
                "`--allow-member-change {named}` names no change this landing makes to \
                 '{}''s entries",
                landing.target
            );
        }
    }
    let unnamed: Vec<&Changed> = changes
        .iter()
        .filter(|c| !landing.allow_member_change.contains(&c.repo))
        .collect();
    if unnamed.is_empty() {
        return Ok(());
    }
    let candidates = rebind_candidates(landing, sides, &unnamed, dir, vcs)?;
    for change in unnamed {
        report.add(judge_change(landing, change, &candidates, label, dir, vcs));
    }
    Ok(())
}

/// The live weave branches whose entries match `W`'s for every unnamed
/// change, nearest first: each a target `W` could land into unchanged.
fn rebind_candidates(
    landing: &Landing,
    sides: &Sides,
    unnamed: &[&Changed],
    dir: &Path,
    vcs: &dyn Vcs,
) -> anyhow::Result<Vec<String>> {
    let store = Store::open(vcs, dir)?;
    let view = store.view()?;
    let mut matching = Vec::new();
    for name in crate::branch::target_candidates(vcs, dir, &view, &landing.feature)? {
        if name == landing.target.as_str() {
            continue;
        }
        let Ok(at) = vcs.resolve_revision(dir, &name) else {
            continue;
        };
        let Some(theirs) = manifest_at(vcs, dir, at.id()) else {
            continue;
        };
        let matches = unnamed.iter().all(|change| match &change.repo {
            RepoOperand::Project(_) => theirs.history == sides.ours.history,
            RepoOperand::Member(path) => {
                match (theirs.get_entry(path), sides.ours.get_entry(path)) {
                    (Some(there), Some(ours)) => changed_fields(there, ours).is_empty(),
                    (None, None) => true,
                    _ => false,
                }
            }
        });
        if matches {
            matching.push(name);
        }
    }
    Ok(matching)
}

/// The `target` member changes line for one repo: each changed field with
/// `T`'s value and `W`'s, then the rebind onto a branch whose entries match
/// `W`'s, the setter that keeps `T`'s value here, and the consent that lands
/// it.
fn judge_change(
    landing: &Landing,
    change: &Changed,
    candidates: &[String],
    label: &str,
    dir: &Path,
    vcs: &dyn Vcs,
) -> Judged {
    let (repo, subject) = match &change.repo {
        RepoOperand::Project(_) => (
            label.to_owned(),
            format!("'{}''s top-level history", landing.target),
        ),
        RepoOperand::Member(path) => (path.to_string(), format!("'{}''s entry", landing.target)),
    };
    let mut lines: Vec<String> = match &change.removed {
        Some(_) => vec!["removed".to_owned()],
        None => change.fields.iter().map(FieldChange::line).collect(),
    };
    let mut forms: Vec<Form> = Vec::new();
    let mut judging = State::TargetMemberChanges
        .of(&repo)
        .subject(subject)
        .bind("<W>", &landing.feature)
        .bind("<m>", &change.repo)
        .checkout(dir)
        .by_verb("land");
    if let Some(first) = candidates.first() {
        lines.push(format!(
            "--target candidates whose entries match: {}",
            candidates.join(", ")
        ));
        judging = judging.bind("<T>", first);
        forms.push(Form::C12Feature);
    }
    if let Some(entry) = &change.removed {
        judging = judging.bind("<url>", &entry.url);
        forms.push(Form::C11MemberAdd);
    }
    for field in &change.fields {
        let top = matches!(change.repo, RepoOperand::Project(_));
        let (form, key) = match (field.name, top, field.theirs.as_str()) {
            ("history", true, "unset") => (Form::C11UnsetTopHistory, "<h>"),
            ("history", true, _) => (Form::C11SetTopHistory, "<h>"),
            ("history", false, "unset") => (Form::C11UnsetHistory, "<h>"),
            ("history", false, _) => (Form::C11SetHistory, "<h>"),
            ("branch", _, _) => (Form::C11SetBranch, "<b>"),
            ("url", _, _) => (Form::C11SetUrl, "<url>"),
            ("role", _, _) => (Form::C11SetRole, "<role>"),
            ("recurse-submodules", _, _) => (Form::C11SetRecurseSubmodules, "<bool>"),
            ("pin", _, _) => (Form::C25, "<rev>"),
            _ => continue,
        };
        judging = judging.bind(key, &field.theirs);
        forms.push(form);
    }
    forms.push(Form::C31);
    judging.listing(lines).only_forms(&forms).judge(vcs)
}

/// A member only `W` lists enters at `W`'s pin; where its receiving branch
/// exists nowhere, the landing creates it there.
fn plan_added_member(
    landing: &Landing,
    sides: &Sides,
    member: &Member<'_>,
    as_mode: Mode,
    plan: &mut Plan,
) -> anyhow::Result<()> {
    let Member {
        path,
        entry,
        dir,
        vcs,
        ..
    } = *member;
    let Some(receiving) = crate::feature_sync::receiving_branch(
        path,
        landing.target.as_str(),
        Some(as_mode),
        &sides.ours,
        Some(&sides.theirs),
        sides.base.as_ref(),
    ) else {
        return Ok(());
    };
    let Some(pin) = pin_of(sides.our_lock.as_ref(), path) else {
        return Ok(());
    };
    let home = home_remote(entry, vcs, dir);
    if matches!(
        crate::push_branch::newer_of_branch_and_upstream(vcs, dir, &receiving, home.as_ref())?,
        crate::push_branch::NewerOfBranchAndUpstream::Nowhere
    ) {
        plan.receiving.insert(
            path.clone(),
            Receiving {
                branch: receiving,
                tip: pin,
                moves: Moves::Enters,
            },
        );
    }
    Ok(())
}

fn home_remote(entry: &RepoEntry, vcs: &dyn Vcs, dir: &Path) -> Option<crate::vcs::RemoteHandle> {
    crate::registry::resolve_to_clone_info(&entry.url)
        .ok()
        .and_then(|info| vcs.url_remote(dir, &info.url).ok())
        .and_then(|found| match found {
            crate::vcs::UrlRemote::Present(remote) => Some(remote),
            _ => None,
        })
}

/// A `dependency` or `reference` member is never moved: it takes `W`'s pin
/// where `W` changed it since the merge base, which its receiving branch must
/// reach; where `T` changed it too, one of the two pins must contain the
/// other.
fn judge_carried_pin(
    landing: &Landing,
    sides: &Sides,
    member: &Member<'_>,
    as_mode: Mode,
    report: &mut Report,
) -> anyhow::Result<()> {
    let Member {
        path,
        entry,
        dir,
        vcs,
        ..
    } = *member;
    let Some(mine) = pin_of(sides.our_lock.as_ref(), path) else {
        return Ok(());
    };
    let base = pin_of(sides.base_lock.as_ref(), path);
    if base.as_ref() == Some(&mine) {
        return Ok(());
    }
    let theirs = pin_of(sides.their_lock.as_ref(), path);
    if let Some(theirs) = theirs.filter(|t| Some(t) != base.as_ref() && *t != mine) {
        let contains = |a: &CommitId, b: &CommitId| vcs.is_ancestor(dir, &rev(a), &rev(b));
        if !contains(&theirs, &mine)? && !contains(&mine, &theirs)? {
            report.add(
                State::PinUnmerged
                    .of(path)
                    .member()
                    .checkout(dir)
                    .bind("<m>", path)
                    .bind("<pin>", &theirs)
                    .bind("<rev>", &theirs)
                    .subject(format!(
                        "this feature pins {} and '{}' pins {}, neither containing the other",
                        mine.short(),
                        landing.target,
                        theirs.short()
                    ))
                    .by_verb("land")
                    .judge(vcs),
            );
        }
        return Ok(());
    }
    let declared = |m: &Manifest| {
        m.get_entry(path)
            .map(|e| e.branch.local_counterpart().as_str().to_owned())
    };
    let Some(receiving) = (match as_mode {
        Mode::Feature => declared(&sides.ours),
        Mode::Declared => crate::feature_sync::receiving_branch(
            path,
            landing.target.as_str(),
            Some(as_mode),
            &sides.ours,
            Some(&sides.theirs),
            sides.base.as_ref(),
        )
        .map(|b| b.as_str().to_owned()),
    }) else {
        return Ok(());
    };
    let receiving = RawRefName::new(receiving);
    let crate::push_branch::NewerOfBranchAndUpstream::Tip(tip) =
        crate::push_branch::newer_of_branch_and_upstream(
            vcs,
            dir,
            &receiving,
            home_remote(entry, vcs, dir).as_ref(),
        )?
    else {
        return Ok(());
    };
    if !vcs.is_ancestor(dir, &rev(&mine), &tip)? {
        report.add(
            State::PinNotOnDeclaredBranch
                .of(path)
                .member()
                .checkout(dir)
                .bind("<m>", path)
                .bind("<b>", &receiving)
                .bind("<T>", &landing.target)
                .subject(format!(
                    "this feature pins {}, which '{receiving}' ({}) does not reach",
                    mine.short(),
                    tip.id().short()
                ))
                .by_verb("land")
                .judge(vcs),
        );
    }
    Ok(())
}

/// What every member of one landing is judged against.
struct Judging<'a> {
    landing: &'a Landing,
    sides: &'a Sides,
    accounting: &'a crate::alignment::Accounting,
    target_aligner: &'a crate::alignment::Aligner,
    as_mode: Mode,
    composing: Composing,
}

/// Judge one `owned` or `fork` member and plan it: its receiving branch
/// (created where it exists nowhere), whether it follows that branch or is
/// composed onto it, the lineage rule when it moves, whether `U` reaches
/// `T`'s pin, and where `U` moves.
fn judge_member(
    judging: &Judging<'_>,
    member: &Member<'_>,
    plan: &mut Plan,
    report: &mut Report,
) -> anyhow::Result<()> {
    let Judging {
        landing,
        sides,
        accounting,
        target_aligner,
        as_mode,
        composing,
    } = *judging;
    let Member {
        path,
        entry,
        dir,
        vcs,
    } = *member;
    let HeadAttachment::Attached(attached) = vcs.head_attachment(dir)? else {
        return Ok(());
    };
    let Some(their_entry) = sides.theirs.get_entry(path) else {
        return Ok(());
    };
    let Some(receiving) = crate::feature_sync::receiving_branch(
        path,
        landing.target.as_str(),
        Some(as_mode),
        &sides.ours,
        Some(&sides.theirs),
        sides.base.as_ref(),
    ) else {
        return Ok(());
    };
    let declared_changed = as_mode == Mode::Declared
        && their_entry.branch.local_counterpart().as_str() != receiving.as_str();
    let head = vcs.head_revision(dir)?.id().clone();
    let pin = pin_of(sides.their_lock.as_ref(), path);
    let lands_on = |tip: &CommitId| vcs.is_ancestor(dir, &rev(&head), &rev(tip));
    use crate::push_branch::NewerOfBranchAndUpstream as Newer;
    let (tip, created) = match crate::push_branch::newer_of_branch_and_upstream(
        vcs,
        dir,
        &receiving,
        home_remote(entry, vcs, dir).as_ref(),
    )? {
        Newer::Tip(tip) => (tip.id().clone(), false),
        Newer::Nowhere => match pin
            .clone()
            .or_else(|| pin_of(sides.our_lock.as_ref(), path))
        {
            Some(at) if vcs.resolve_revision(dir, at.as_str()).is_ok() => (at, true),
            _ => return Ok(()),
        },
        Newer::Diverged {
            local,
            upstream_tip,
            ..
        } => {
            if !declared_changed && lands_on(local.id())? && lands_on(upstream_tip.id())? {
                return Ok(());
            }
            let (ahead, behind) = vcs.ahead_behind(dir, upstream_tip.id(), local.id());
            report.add(
                State::PushBranchDiverged
                    .of(path)
                    .member()
                    .checkout(dir)
                    .bind("<m>", path)
                    .bind("<repo>", path)
                    .subject(format!(
                        "'{receiving}', where it lands, and its upstream have diverged ({ahead} \
                         ahead, {behind} behind)"
                    ))
                    .by_verb("land")
                    .judge(vcs),
            );
            return Ok(());
        }
    };
    if declared_changed {
        if let Some(pin) = &pin {
            let commits =
                vcs.commits_excluding(dir, &rev(pin), std::slice::from_ref(&tip), false)?;
            if !commits.is_empty() {
                plan.left_behind.push(LeftBehind {
                    repo: RepoKey::Member(path.clone()),
                    vcs_type: entry.vcs_type,
                    was: their_entry.branch.local_counterpart().as_str().to_owned(),
                    now: receiving.as_str().to_owned(),
                    commits,
                    checkout: landing.target_workspace.join(path.as_path()),
                });
            }
        }
    }
    let new_work = if declared_changed {
        crate::lineage::new_work(vcs, dir, &head, Some(&tip), &sides.history.of_member(path))?
    } else {
        Vec::new()
    };
    let follows = declared_changed && new_work.is_empty();
    let lands = lands_on(&tip)?;
    if lands && !declared_changed && !created {
        return Ok(());
    }
    let moved = !follows && !lands;
    let reachable_pin = pin
        .as_ref()
        .filter(|pin| vcs.resolve_revision(dir, pin.as_str()).is_ok());
    if let (Some(pin), false, true) = (reachable_pin, declared_changed, moved) {
        if vcs.is_ancestor(dir, &rev(pin), &rev(&tip))? {
            plan.beyond_pin.insert(
                path.clone(),
                vcs.commits_beyond(dir, &rev(&tip), &rev(pin))?,
            );
        } else {
            report.add(
                State::PinNotOnDeclaredBranch
                    .of(path)
                    .member()
                    .checkout(dir)
                    .bind("<m>", path)
                    .bind("<b>", &receiving)
                    .bind("<T>", &landing.target)
                    .subject(format!(
                        "'{}' pins {} for it, which '{receiving}' ({}) does not reach",
                        landing.target,
                        pin.short(),
                        tip.short()
                    ))
                    .by_verb("land")
                    .judge(vcs),
            );
        }
    }
    if moved {
        judge_moved(
            landing, sides, member, &attached, &receiving, &head, &tip, accounting, report,
        )?;
        judge_merge_in_range(
            composing,
            vcs,
            dir,
            path.as_str(),
            &head,
            &tip,
            &landing.target,
            report,
        )?;
    }
    let moves = if follows {
        Moves::Follows
    } else if created {
        Moves::AsRef { create: true }
    } else if declared_changed {
        if !judge_unheld_receiving(landing, member, &receiving, report)? {
            return Ok(());
        }
        Moves::AsRef { create: false }
    } else {
        judge_receiving_checkout(landing, member, &receiving, target_aligner, report)?;
        Moves::InCheckout
    };
    plan.receiving.insert(
        path.clone(),
        Receiving {
            branch: receiving,
            tip,
            moves,
        },
    );
    Ok(())
}

/// The lineage rule for a member the landing moves, and the protection of
/// the branch its work is rebased off.
#[allow(clippy::too_many_arguments)]
fn judge_moved(
    landing: &Landing,
    sides: &Sides,
    member: &Member<'_>,
    attached: &crate::vcs::AttachedRef,
    receiving: &RawRefName,
    head: &CommitId,
    tip: &CommitId,
    accounting: &crate::alignment::Accounting,
    report: &mut Report,
) -> anyhow::Result<()> {
    let Member { path, dir, vcs, .. } = *member;
    let pins = sides.history.of_member(path);
    match crate::lineage::judge(vcs, dir, head, &pins, tip)? {
        Lineage::Passes { .. } => {}
        Lineage::Off { base } => {
            let mut judging = State::PinOffLineage
                .of(path)
                .member()
                .checkout(dir)
                .bind("<m>", path)
                .bind("<b>", receiving)
                .bind("<T>", &landing.target)
                .subject(format!(
                    "its base {} is not on '{receiving}' ({})",
                    base.short(),
                    tip.short()
                ))
                .by_verb("land");
            if let Some(fact) = crate::feature_sync::patch_fact(vcs, dir, head, tip) {
                judging = judging.fact(fact);
            }
            report.add(judging.judge(vcs));
        }
        Lineage::Unreachable { absent } => report.add(
            State::PinUnreachable
                .of(path)
                .member()
                .checkout(dir)
                .bind("<m>", path)
                .subject(format!(
                    "'{}' pinned {} for it, which this clone lacks, so its base cannot be \
                     read here",
                    landing.target,
                    absent
                        .iter()
                        .map(|c| c.short().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .by_verb("land")
                .judge(vcs),
        ),
    }
    if !vcs.is_ancestor(dir, &rev(tip), &rev(head))? {
        let on = attached.to_string();
        let declarers = accounting.declarers(path, &on);
        if !declarers.is_empty() {
            report.add(
                State::ProtectedDeclared
                    .of(path)
                    .member()
                    .subject(&on)
                    .listing(declarers)
                    .bind("<m>", path)
                    .by_verb("land")
                    .judge(vcs),
            );
        }
    }
    Ok(())
}

/// A receiving branch the landing's declared-branch change names moves as a
/// ref, so no checkout may hold it: one that does refuses. Whether it moves.
fn judge_unheld_receiving(
    landing: &Landing,
    member: &Member<'_>,
    receiving: &RawRefName,
    report: &mut Report,
) -> anyhow::Result<bool> {
    let Member { path, dir, vcs, .. } = *member;
    let holding = vcs.checkouts_of_branch(dir, receiving)?;
    if holding.is_empty() {
        return Ok(true);
    }
    report.add(
        State::CheckoutInAnotherWorkspace
            .of(path)
            .member()
            .checkout(dir)
            .subject(format!(
                "'{receiving}', which this lands into, is checked out outside '{}''s workspace",
                landing.target
            ))
            .listing(holding.iter().map(|p| p.display().to_string()))
            .by_verb("land")
            .judge(vcs),
    );
    Ok(false)
}

/// `U` fast-forwards where `T`'s workspace has it checked out: that member
/// must sit on `U` there. Checked out in a third workspace, it is moved by
/// nothing yet.
fn judge_receiving_checkout(
    landing: &Landing,
    member: &Member<'_>,
    receiving: &RawRefName,
    target_aligner: &crate::alignment::Aligner,
    report: &mut Report,
) -> anyhow::Result<()> {
    let Member {
        path, entry, vcs, ..
    } = *member;
    let there = landing.target_workspace.join(path.as_path());
    if !vcs.is_repo(&there) {
        return Ok(());
    }
    if let HeadAttachment::Attached(on) = vcs.head_attachment(&there)? {
        if on.to_string() == receiving.as_str() {
            return Ok(());
        }
    }
    let own = crate::path_spelling::CanonicalPath::of(&there);
    let elsewhere: Vec<PathBuf> = vcs
        .checkouts_of_branch(&there, receiving)?
        .into_iter()
        .filter(|c| crate::path_spelling::CanonicalPath::of(c) != own)
        .collect();
    if !elsewhere.is_empty() {
        report.add(
            State::CheckoutInAnotherWorkspace
                .of(path)
                .member()
                .checkout(&there)
                .subject(format!(
                    "'{receiving}', which this lands into, is checked out outside '{}''s \
                     workspace",
                    landing.target
                ))
                .listing(elsewhere.iter().map(|p| p.display().to_string()))
                .elsewhere(&landing.target_workspace)
                .by_verb("land")
                .judge(vcs),
        );
        return Ok(());
    }
    let checked = crate::alignment::Member {
        path,
        entry,
        dir: &there,
        vcs,
    };
    let lock = LockFile::path_in(&project_dir(&landing.target_workspace, &landing.project))
        .read_to_string()
        .ok()
        .and_then(|text| LockFile::from_json_str(&text).ok());
    let planned = target_aligner.plan(&checked, lock.as_ref())?;
    match target_aligner.refusal(&checked, &planned, path)? {
        Some(off) => report.add(off.elsewhere(&landing.target_workspace)),
        None => report.add(
            State::AlignmentUnaligned
                .of(path)
                .member()
                .checkout(&there)
                .subject(format!(
                    "not on '{receiving}', which this lands into, in '{}''s workspace",
                    landing.target
                ))
                .elsewhere(&landing.target_workspace)
                .by_verb("land")
                .judge(vcs),
        ),
    }
    Ok(())
}

/// Delete each branch an op created that still sits where it was created and
/// that another branch reaches, newest first. Returns those left, each with
/// why.
pub(crate) fn undo_created(
    workspace: &Path,
    created: &[CreatedRef],
) -> Vec<(RepoKey, String, String)> {
    let vcs = crate::vcs::probe_vcs();
    let mut left = Vec::new();
    for made in created.iter().rev() {
        let RepoKey::Member(path) = &made.repo else {
            continue;
        };
        let dir = workspace.join(path.as_path());
        let branch = RawRefName::new(made.branch.clone());
        if let Err(why) = undo_one(vcs.as_ref(), &dir, &branch, &made.at) {
            left.push((made.repo.clone(), made.branch.clone(), why));
        }
    }
    left
}

fn undo_one(vcs: &dyn Vcs, dir: &Path, branch: &RawRefName, at: &CommitId) -> Result<(), String> {
    let tip = vcs
        .resolve_local_branch_tip(dir, branch)
        .map_err(|e| format!("{e:#}"))?;
    match tip {
        None => return Ok(()),
        Some(tip) if tip.id() != at => {
            return Err(format!(
                "it moved to {} since the op created it at {}",
                tip.id().short(),
                at.short()
            ))
        }
        Some(_) => {}
    }
    let others: Vec<ResolvedRevisionId> = vcs
        .branch_tips(dir)
        .map_err(|e| format!("{e:#}"))?
        .into_iter()
        .filter(|other| other.label != branch.as_str())
        .map(|other| other.tip)
        .collect();
    let proof = DestroyProof::landed(vcs, dir, Site::Local, branch, &others)
        .map_err(|e: VcsError| format!("{e:#}"))?
        .ok_or_else(|| "no other branch reaches it".to_owned())?;
    vcs.delete_branch(proof).map_err(|e| format!("{e:#}"))
}
