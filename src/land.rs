//! `rwv land`: where a feature lands, and what refuses its landing.
//!
//! [`resolve`] reads the record of the checked-out feature `W` and finds its
//! target `T`, `T`'s tip and the workspace `T` is checked out in. [`judge`]
//! plans each member's receiving branch `U` and judges every refusal the
//! landing owes before anything moves; the phase machine in `crate::sync`
//! composes in the source and fast-forwards the receiving branches.

use crate::diagnosis::{Form, Judged, Report, State};
use crate::lineage::{Lineage, PinHistory};
use crate::manifest::{LockFile, Manifest, ProjectName, RepoEntry, RepoPath, Role};
use crate::records::{BranchName, Mode, Record, Store};
use crate::vcs::{
    project_vcs, vcs_for, CommitId, HeadAttachment, RawRefName, ResolvedRevisionId, Vcs,
};
use crate::workspace::{project_dir, project_rel_path, Checkout, WorkspaceContext};
use std::collections::BTreeMap;
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
    /// Refusals judged while resolving, reported with the rest.
    pub judged: Report,
}

/// What [`judge`] planned: the receiving branch's tip each `owned` or `fork`
/// member is composed onto.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub sources: BTreeMap<RepoPath, CommitId>,
}

fn rev(commit: &CommitId) -> ResolvedRevisionId {
    ResolvedRevisionId::from(commit.clone())
}

/// Resolve the landing the checked-out feature makes: `--target` for one run,
/// else the record's. A refusal that leaves nothing to land into refuses here,
/// with every refusal judged before it.
pub fn resolve(ctx: &WorkspaceContext, target_flag: Option<&str>) -> anyhow::Result<Landing> {
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
            return Err(refusal(judged));
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
            return Err(refusal(judged));
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
                State::TargetGone
                    .of(&label)
                    .project_repo()
                    .subject(&target)
                    .bind("<W>", &feature)
                    .bind("<T>", &target)
                    .checkout(&dir)
                    .by_verb("land")
                    .judge(vcs),
            );
            return Err(refusal(judged));
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
            return Err(refusal(judged));
        }
    };
    Ok(Landing {
        project,
        feature,
        target,
        target_workspace,
        target_tip,
        judged,
    })
}

fn refusal(report: Report) -> anyhow::Error {
    match report.into_result() {
        Err(e) => e,
        Ok(()) => crate::untokened!(InternalInvariant, "a refusal judged nothing"),
    }
}

/// A record another machine published names a target this machine resolves
/// only through that remote: its branch published there, and a local branch
/// of that name taking in from it. Anything else is that target gone here,
/// never an unrelated branch that happens to share its name.
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
    let takes_in_from_there = match &local {
        None => true,
        Some(_) => vcs.upstream_of(dir, &target_ref)?.is_some_and(|up| {
            holding.contains(&up.remote) && up.branch.as_str() == target.as_str()
        }),
    };
    if !published.is_empty() && takes_in_from_there {
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
            "'{feature}''s record was published on {remotes}; the local '{target}' does not take in \
             from there, so it is another branch of that name"
        )
    };
    Ok(Some(
        State::TargetGone
            .of(project_rel_path(project))
            .project_repo()
            .subject(why)
            .bind("<W>", feature)
            .bind("<T>", target)
            .checkout(dir)
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
    their_lock: Option<LockFile>,
    history: PinHistory,
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
        their_lock: lock_at(vcs, &dir, &landing.target_tip),
        history: PinHistory::of(vcs, &dir, &landing.target_tip)?,
    };
    judge_entry_changes(landing, &sides, &dir, &label, vcs, report);
    for dependent in crate::merge_upstream::dependents_of(vcs, &dir, landing.feature.as_str())? {
        report.add(
            State::TargetDependents
                .of(&label)
                .project_repo()
                .subject(format!("{dependent} lands into {}", landing.feature))
                .bind("<dep>", &dependent)
                .bind("<W>", &dependent)
                .bind("<T>", &landing.target)
                .checkout(&dir)
                .by_verb("land")
                .judge(vcs),
        );
    }
    let accounting = crate::alignment::Accounting::scan(primary)?;
    let target_aligner = crate::alignment::Aligner::open(
        &landing.target_workspace,
        &landing.project,
        crate::alignment::Accounting::default(),
    )?;
    let source_aligner = crate::alignment::Aligner::open(
        source_root,
        &landing.project,
        crate::alignment::Accounting::default(),
    )?;
    let our_lock = lock_at(vcs, &dir, &head);
    let mut plan = Plan::default();
    for (path, entry) in sides.ours.iter_entries() {
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
        let aligned = source_aligner.plan(&checked, our_lock.as_ref())?;
        if let Some(off) = source_aligner.refusal(&checked, &aligned, path)? {
            report.add(off);
            continue;
        }
        if !matches!(entry.role, Role::Owned | Role::Fork) {
            continue;
        }
        let member = Member {
            path,
            entry,
            dir: &mdir,
            vcs: mvcs.as_ref(),
        };
        if let Some(source) = judge_member(
            landing,
            &sides,
            &member,
            &accounting,
            &target_aligner,
            report,
        )? {
            plan.sources.insert(path.clone(), source);
        }
    }
    Ok(plan)
}

struct Member<'a> {
    path: &'a RepoPath,
    entry: &'a RepoEntry,
    dir: &'a Path,
    vcs: &'a dyn Vcs,
}

/// Each field of `ours` that differs from `theirs`, as `field: theirs → ours`.
fn changed_fields(theirs: &RepoEntry, ours: &RepoEntry) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = |name: &str, t: String, o: String| {
        if t != o {
            out.push(format!("{name}: {t} → {o}"));
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
    let history = |h: Option<crate::manifest::History>| {
        h.map_or_else(|| "unset".to_owned(), |h| h.to_string())
    };
    field("history", history(theirs.history), history(ours.history));
    field(
        "recurse-submodules",
        format!("{:?}", theirs.recurse_submodules).to_lowercase(),
        format!("{:?}", ours.recurse_submodules).to_lowercase(),
    );
    out
}

/// The changes the landing would make to `T`'s member entries and top-level
/// `history`: each one `W` made since the merge base that `T` does not
/// already hold. None lands yet, so each refuses.
fn judge_entry_changes(
    landing: &Landing,
    sides: &Sides,
    dir: &Path,
    label: &str,
    vcs: &dyn Vcs,
    report: &mut Report,
) {
    let mut changes: Vec<(String, Vec<String>)> = Vec::new();
    for (path, theirs) in sides.theirs.iter_entries() {
        let was = sides.base.as_ref().and_then(|b| b.get_entry(path));
        match sides.ours.get_entry(path) {
            Some(ours) => {
                let changed_here = was.is_none_or(|was| !changed_fields(was, ours).is_empty());
                let fields = changed_fields(theirs, ours);
                if changed_here && !fields.is_empty() {
                    changes.push((path.to_string(), fields));
                }
            }
            None if was.is_some() => {
                changes.push((path.to_string(), vec!["removed".to_owned()]));
            }
            None => {}
        }
    }
    let was = sides.base.as_ref().and_then(|b| b.history);
    if sides.ours.history != was && sides.ours.history != sides.theirs.history {
        let show = |h: Option<crate::manifest::History>| {
            h.map_or_else(|| "unset".to_owned(), |h| h.to_string())
        };
        changes.push((
            label.to_owned(),
            vec![format!(
                "history: {} → {}",
                show(sides.theirs.history),
                show(sides.ours.history)
            )],
        ));
    }
    for (repo, fields) in changes {
        report.add(
            State::TargetMemberChanges
                .of(&repo)
                .subject(format!("'{}''s entry", landing.target))
                .listing(fields)
                .bind("<W>", &landing.feature)
                .bind("<T>", &landing.target)
                .bind("<m>", &repo)
                .checkout(dir)
                .by_verb("land")
                .judge(vcs),
        );
    }
}

/// Judge one `owned` or `fork` member: its receiving branch, the lineage
/// rule when it moves, whether `U` reaches `T`'s pin, and the checkout `U`
/// fast-forwards in. The receiving branch's tip, where there is one.
fn judge_member(
    landing: &Landing,
    sides: &Sides,
    member: &Member<'_>,
    accounting: &crate::alignment::Accounting,
    target_aligner: &crate::alignment::Aligner,
    report: &mut Report,
) -> anyhow::Result<Option<CommitId>> {
    let Member {
        path,
        entry,
        dir,
        vcs,
    } = *member;
    let HeadAttachment::Attached(attached) = vcs.head_attachment(dir)? else {
        return Ok(None);
    };
    if sides.theirs.get_entry(path).is_none() {
        return Ok(None);
    }
    let as_mode = crate::branch::recorded_value(
        project_vcs().as_ref(),
        &landing.project,
        &project_dir(&landing.target_workspace, &landing.project),
        &landing.target,
    )
    .map(|r| r.mode())
    .unwrap_or(Mode::Declared);
    let Some(receiving) = crate::feature_sync::receiving_branch(
        path,
        landing.target.as_str(),
        Some(as_mode),
        &sides.ours,
        Some(&sides.theirs),
        sides.base.as_ref(),
    ) else {
        return Ok(None);
    };
    let head = vcs.head_revision(dir)?.id().clone();
    let home = crate::registry::resolve_to_clone_info(&entry.url)
        .ok()
        .and_then(|info| vcs.url_remote(dir, &info.url).ok())
        .and_then(|found| match found {
            crate::vcs::UrlRemote::Present(remote) => Some(remote),
            _ => None,
        });
    use crate::push_branch::NewerOfBranchAndUpstream as Newer;
    let tip = match crate::push_branch::newer_of_branch_and_upstream(
        vcs,
        dir,
        &receiving,
        home.as_ref(),
    )? {
        Newer::Tip(tip) => tip.id().clone(),
        Newer::Nowhere => return Ok(None),
        Newer::Diverged {
            local,
            upstream_tip,
            ..
        } => {
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
            return Ok(None);
        }
    };
    let pin = sides
        .their_lock
        .as_ref()
        .and_then(|lock| lock.get_entry(path))
        .and_then(|e| e.version.as_commit_id());
    if let Some(pin) = &pin {
        if vcs.resolve_revision(dir, pin.as_str()).is_ok()
            && !vcs.is_ancestor(dir, &rev(pin), &rev(&tip))?
        {
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
    let moved = !vcs.is_ancestor(dir, &rev(&head), &rev(&tip))?;
    if moved {
        let pins = sides.history.of_member(path);
        match crate::lineage::judge(vcs, dir, &head, &pins, &tip)? {
            Lineage::Passes { .. } => {}
            Lineage::Off { base } => {
                let mut judging = State::PinOffLineage
                    .of(path)
                    .member()
                    .checkout(dir)
                    .bind("<m>", path)
                    .bind("<b>", &receiving)
                    .bind("<T>", &landing.target)
                    .subject(format!(
                        "its base {} is not on '{receiving}' ({})",
                        base.short(),
                        tip.short()
                    ))
                    .by_verb("land");
                if let Some(fact) = crate::feature_sync::patch_fact(vcs, dir, &head, &tip) {
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
        if !vcs.is_ancestor(dir, &rev(&tip), &rev(&head))? {
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
    }
    judge_receiving_checkout(landing, member, &receiving, target_aligner, report)?;
    Ok(Some(tip))
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
