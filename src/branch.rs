//! `rwv branch`: how each weave branch is used, and the gate that refuses the
//! verbs which align, lock, land or publish on a branch that has no record.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::add_remove::find_project;
use crate::diagnosis::{Form, Judged, Judging, Report, State};
use crate::manifest::{Manifest, ProjectName, RepoPath, Role};
use crate::names::{judge_feature_names, require_feature_names, NameScan};
use crate::parallel::OutputMode;
use crate::path_spelling::CanonicalPath;
use crate::records::{
    chain_text, BranchName, Entry, Lookup, Mode, NameError, Record, Store, View, Write, WriteError,
};
use crate::refusal::{refusal, RefusalKind};
use crate::vcs::{
    project_vcs, short_revision, vcs_for, HeadObservation, LocalRefName, RawRefName, Relation,
    RemoteHandle, ResolvedRevisionId, Upstream, Vcs,
};
use crate::workspace::{project_rel_path, WorkspaceContext};

pub const BRANCH_SCHEMA_URL: &str = crate::schema_url::schema_url!("branch");
pub const BRANCH_DELETE_SCHEMA_URL: &str = crate::schema_url::schema_url!("branch-delete");

/// What `rwv branch set-mode` is asked to record.
#[derive(Debug, Clone)]
pub struct SetMode {
    /// The branch to record; the checked-out one when `None`.
    pub branch: Option<String>,
    pub mode: Mode,
    /// The branch a feature lands into.
    pub target: Option<String>,
}

pub(crate) fn label(project: &ProjectName) -> String {
    project_rel_path(project)
}

pub(crate) fn named(
    vcs: &dyn Vcs,
    project: &ProjectName,
    dir: &Path,
    text: &str,
) -> anyhow::Result<BranchName> {
    match BranchName::parse(vcs, text) {
        Ok(name) => Ok(name),
        Err(NameError::Reserved) => Err(State::NameReserved
            .of(label(project))
            .subject(text)
            .checkout(dir)
            .judge(vcs)
            .refused()),
        Err(NameError::Invalid(e)) => Err(refusal(
            RefusalKind::InvalidRefName,
            format!("`{text}` is not a usable branch name: {e:#}"),
        )),
    }
}

pub(crate) fn checked_out(vcs: &dyn Vcs, dir: &Path) -> anyhow::Result<Option<String>> {
    Ok(match vcs.observe_head(dir)? {
        HeadObservation::Attached { name } | HeadObservation::Unborn { name } => {
            Some(name.as_str().to_owned())
        }
        HeadObservation::Detached { .. } => None,
    })
}

fn writer_text(store: &Store<'_>, branch: &BranchName) -> anyhow::Result<String> {
    Ok(match store.writer_of(branch)? {
        Some(release) => format!("written by rwv {release}"),
        None => "its commit names no rwv release".to_owned(),
    })
}

/// The branches `branch` could land into: each other weave branch with a
/// record that reads as one value, the ones sharing the most history with
/// `branch` first.
pub(crate) fn target_candidates(
    vcs: &dyn Vcs,
    dir: &Path,
    view: &View,
    branch: &BranchName,
) -> anyhow::Result<Vec<String>> {
    let tips = vcs.named_branches(dir)?;
    let own = tips
        .iter()
        .find(|b| b.name == branch.as_str())
        .map(|own| ResolvedRevisionId::from(own.tip.clone()));
    let mut ranked: Vec<(usize, String)> = Vec::new();
    for other in &tips {
        let Ok(name) = BranchName::parse(vcs, &other.name) else {
            continue;
        };
        if &name == branch || !matches!(view.lookup(&name), Lookup::Value(_)) {
            continue;
        }
        let tip = ResolvedRevisionId::from(other.tip.clone());
        if vcs
            .read_file_at_revision(dir, &tip, Path::new(Manifest::FILE_NAME))
            .is_err()
        {
            continue;
        }
        let distance = match own.as_ref() {
            Some(own) => match vcs.merge_bases(dir, own.id(), &other.tip)?.first() {
                Some(base) => {
                    vcs.count_commits_in_range(dir, &ResolvedRevisionId::from(base.clone()), own)?
                }
                None => usize::MAX,
            },
            None => usize::MAX,
        };
        ranked.push((distance, other.name.clone()));
    }
    ranked.sort();
    Ok(ranked.into_iter().map(|(_, name)| name).collect())
}

pub(crate) fn judge_lookup(
    vcs: &dyn Vcs,
    store: &Store<'_>,
    project: &ProjectName,
    dir: &Path,
    branch: &BranchName,
    lookup: Lookup,
) -> anyhow::Result<Judged> {
    let judging = |state: State| {
        state
            .of(label(project))
            .project_repo()
            .subject(branch)
            .checkout(dir)
            .bind("<W>", branch)
    };
    Ok(match lookup {
        Lookup::Value(_) => unreachable!("a record that reads is not judged"),
        Lookup::Unrecorded => {
            let view = store.view()?;
            let candidates = target_candidates(vcs, dir, &view, branch)?;
            let mut judging = judging(State::TargetUnrecorded);
            if !candidates.is_empty() {
                judging = judging.listing([format!(
                    "--target candidates, nearest first: {}",
                    candidates.join(", ")
                )]);
            }
            judging.judge(vcs)
        }
        Lookup::Conflict(first, second) => judging(State::TargetConflict)
            .listing([first.to_string(), second.to_string()])
            .judge(vcs),
        Lookup::Unreadable(why) => judging(State::TargetUnreadable)
            .listing([why, writer_text(store, branch)?])
            .judge(vcs),
        Lookup::Cycle(chain) => judging(State::TargetCycle)
            .listing([chain_text(&chain)])
            .judge(vcs),
    })
}

/// `branch` is recorded as `recorded` and something asks it to be `other`:
/// refused, the record standing, never redirected.
pub(crate) fn judge_differs(
    vcs: &dyn Vcs,
    project: &ProjectName,
    dir: &Path,
    branch: &BranchName,
    recorded: &Record,
    other: &Record,
) -> Judged {
    State::TargetDiffers
        .of(label(project))
        .project_repo()
        .subject(branch)
        .checkout(dir)
        .bind("<W>", branch)
        .listing([format!("recorded: {recorded}"), format!("asked: {other}")])
        .judge(vcs)
}

pub(crate) fn write_refusal(
    vcs: &dyn Vcs,
    store: &Store<'_>,
    project: &ProjectName,
    dir: &Path,
    branch: &BranchName,
    error: WriteError,
) -> anyhow::Error {
    let judged = |target: &BranchName, lookup: Lookup| {
        judge_lookup(vcs, store, project, dir, target, lookup).map_or_else(|e| e, Judged::refused)
    };
    match error {
        WriteError::Cycle(chain) => judged(branch, Lookup::Cycle(chain)),
        WriteError::TargetUnrecorded(target) | WriteError::Unrecorded(target) => {
            judged(&target, Lookup::Unrecorded)
        }
        WriteError::TargetUnreadable(target, lookup) => judged(&target, lookup),
        WriteError::Busy => crate::untokened!(EnvironmentPassthrough, "{}", WriteError::Busy),
        WriteError::Vcs(e) => anyhow::Error::new(e).context("could not write the branch record"),
        WriteError::Judging(e) => e.context("could not write the branch record"),
    }
}

/// `target` gone as `feature`'s record names it. The rebind names a target
/// the operator chooses among the candidates listed: never the gone target,
/// nor a branch landing into `feature`, which the rebind would refuse as a
/// cycle. A sole candidate is bound; with none, declaring `feature` is what
/// clears the state.
pub(crate) fn target_gone(
    vcs: &dyn Vcs,
    project: &ProjectName,
    dir: &Path,
    feature: &BranchName,
    target: &BranchName,
) -> anyhow::Result<Judging> {
    let view = view_of(&Store::open(vcs, dir)?)?;
    let candidates: Vec<String> = target_candidates(vcs, dir, &view, feature)?
        .into_iter()
        .filter(|name| name != target.as_str())
        .filter(|name| {
            BranchName::parse(vcs, name)
                .is_ok_and(|name| view.chain_back_to(feature, vec![name]).is_none())
        })
        .collect();
    let judging = State::TargetGone
        .of(label(project))
        .project_repo()
        .subject(target)
        .checkout(dir)
        .bind("<W>", feature);
    Ok(match candidates.as_slice() {
        [] => judging.only_forms(&[Form::C12Declared]),
        [only] => judging
            .bind("<T>", only)
            .listing([format!("--target candidate: {only}")])
            .only_forms(&[Form::C12Feature]),
        _ => judging
            .listing([format!(
                "--target candidates, nearest first: {}",
                candidates.join(", ")
            )])
            .only_forms(&[Form::C12Feature]),
    })
}

pub(crate) fn view_of(store: &Store<'_>) -> anyhow::Result<View> {
    store
        .view()
        .map_err(|e| anyhow::Error::new(e).context("could not read the branch records"))
}

/// The gate of the verbs that align, lock, land or publish: the project
/// repo's checked-out branch has a record that reads as one value, else the
/// verb refuses before changing anything. The records remotes' tips are
/// merged into the local records branch first.
///
/// A project repo that is not on a branch has no record to read; what that
/// state refuses is the alignment axis's.
pub fn require_recorded(ctx: &WorkspaceContext) -> anyhow::Result<()> {
    let (project, dir) = find_project(ctx)?;
    let vcs = project_vcs();
    if !vcs.is_repo(&dir) {
        return Ok(());
    }
    let Some(text) = checked_out(vcs.as_ref(), &dir)? else {
        return Ok(());
    };
    let branch = named(vcs.as_ref(), &project, &dir, &text)?;
    let record = recorded_value(vcs.as_ref(), &project, &dir, &branch)?;
    let Ok(manifest) = Manifest::from_path(&dir.join(Manifest::FILE_NAME)) else {
        return Ok(());
    };
    require_feature_names(
        ctx.active_path(),
        &NameScan::of(ctx.primary_path(), &project)?,
        &branch,
        &record,
        &manifest,
        None,
    )
}

/// [`require_recorded`] without the Names rule, for a verb that records pins
/// and neither aligns nor publishes.
pub fn require_record(ctx: &WorkspaceContext) -> anyhow::Result<()> {
    let (project, dir) = find_project(ctx)?;
    let vcs = project_vcs();
    if !vcs.is_repo(&dir) {
        return Ok(());
    }
    let Some(text) = checked_out(vcs.as_ref(), &dir)? else {
        return Ok(());
    };
    let branch = named(vcs.as_ref(), &project, &dir, &text)?;
    recorded_value(vcs.as_ref(), &project, &dir, &branch).map(|_| ())
}

/// `branch`'s record, the records remotes' tips merged into the local records
/// branch first; a record that does not read as one value refuses, judged.
pub(crate) fn recorded_value(
    vcs: &dyn Vcs,
    project: &ProjectName,
    dir: &Path,
    branch: &BranchName,
) -> anyhow::Result<Record> {
    let store = Store::open(vcs, dir)?;
    let written = store
        .write(|_| Ok(Vec::new()))
        .map_err(|e| write_refusal(vcs, &store, project, dir, branch, e))?;
    match written.view.lookup(branch) {
        Lookup::Value(record) => Ok(record),
        lookup => Err(judge_lookup(vcs, &store, project, dir, branch, lookup)?.refused()),
    }
}

/// Record the project repo's checked-out branch as `declared` when it has no
/// record, after merging in the records its remotes hold: what `init` and
/// `clone` mean, "start from this project's trunk".
pub fn record_trunk(vcs: &dyn Vcs, project: &ProjectName, dir: &Path) -> anyhow::Result<()> {
    let Some(text) = checked_out(vcs, dir)? else {
        return Ok(());
    };
    let trunk = named(vcs, project, dir, &text)?;
    let store = Store::open(vcs, dir)?;
    store
        .write(|view| {
            Ok(match view.lookup(&trunk) {
                Lookup::Unrecorded => vec![Write {
                    branch: trunk.clone(),
                    record: Record::Declared,
                }],
                _ => Vec::new(),
            })
        })
        .map_err(|e| write_refusal(vcs, &store, project, dir, &trunk, e))?;
    Ok(())
}

/// The branch a new workweave made from the workspace whose project repo is
/// `source_dir` lands into: the branch that repo has checked out, whose record
/// must read as one value. `None` where `source_dir` is not a repo, which has
/// no records.
///
/// Refuses before anything is created, so a create reports it with the
/// others.
pub fn workweave_parent(
    source_dir: &Path,
    project: &ProjectName,
) -> anyhow::Result<Option<BranchName>> {
    let vcs = project_vcs();
    if !vcs.is_repo(source_dir) {
        return Ok(None);
    }
    let Some(text) = checked_out(vcs.as_ref(), source_dir)? else {
        return Err(refuse_off_branch(
            vcs.as_ref(),
            source_dir,
            project,
            "workweave create",
            "the new workweave's branch has no branch to land into; switch it to a branch first",
        ));
    };
    let parent = named(vcs.as_ref(), project, source_dir, &text)?;
    let store = Store::open(vcs.as_ref(), source_dir)?;
    let view = view_of(&store)?;
    match view.lookup(&parent) {
        Lookup::Value(_) => Ok(Some(parent)),
        lookup => {
            Err(judge_lookup(vcs.as_ref(), &store, project, source_dir, &parent, lookup)?.refused())
        }
    }
}

/// The record of `branch` as a feature onto `target`, which must have a
/// record that reads and must not lead back to `branch`; `target`'s mode as it
/// stands is stored as `as`.
pub(crate) fn feature_record(
    view: &View,
    branch: &BranchName,
    target: &BranchName,
) -> Result<Record, WriteError> {
    if let Some(chain) = view.chain_back_to(branch, vec![target.clone()]) {
        return Err(WriteError::Cycle(chain));
    }
    let as_mode = match view.lookup(target) {
        Lookup::Value(record) => record.mode(),
        Lookup::Unrecorded => return Err(WriteError::TargetUnrecorded(target.clone())),
        other => return Err(WriteError::TargetUnreadable(target.clone(), other)),
    };
    Ok(Record::Feature {
        target: target.clone(),
        as_mode,
    })
}

/// Record `minted`, the branch a new workweave of `project` is on, as a
/// feature onto `parent` with `parent`'s mode as it stands, and say where it
/// lands. A branch that already has a record keeps it.
pub fn record_workweave(
    repo: &Path,
    project: &ProjectName,
    parent: &BranchName,
    minted: &str,
) -> anyhow::Result<Vec<String>> {
    let vcs = project_vcs();
    let branch = named(vcs.as_ref(), project, repo, minted)?;
    let store = Store::open(vcs.as_ref(), repo)?;
    let written = store
        .write(|view| {
            if !matches!(view.lookup(&branch), Lookup::Unrecorded) {
                return Ok(Vec::new());
            }
            Ok(vec![Write {
                branch: branch.clone(),
                record: feature_record(view, &branch, parent)?,
            }])
        })
        .map_err(|e| write_refusal(vcs.as_ref(), &store, project, repo, &branch, e))?;
    match written.view.lookup(&branch) {
        Lookup::Value(Record::Feature { target, as_mode }) => {
            target_recorded_lines(vcs.as_ref(), repo, &branch, &target, as_mode)
        }
        _ => Ok(Vec::new()),
    }
}

/// `rwv branch set-mode [<W>] <feature|declared> [--target <T>]`.
pub fn run_set_mode(ctx: &WorkspaceContext, request: SetMode) -> anyhow::Result<()> {
    let (project, dir) = find_project(ctx)?;
    let vcs = project_vcs();
    let text = match request.branch {
        Some(text) => text,
        None => checked_out(vcs.as_ref(), &dir)?.ok_or_else(|| {
            refusal(
                RefusalKind::HeadNotOnBranch,
                "rwv branch set-mode: the project repo is on a detached HEAD, which has no \
                 branch to record; name the branch: `rwv branch set-mode <W> <mode>`",
            )
        })?,
    };
    let branch = named(vcs.as_ref(), &project, &dir, &text)?;
    let target = request
        .target
        .as_deref()
        .map(|t| named(vcs.as_ref(), &project, &dir, t))
        .transpose()?;
    let known = vcs.named_branches(&dir)?;
    let Some(known) = known.iter().find(|b| b.name == branch.as_str()) else {
        return Err(State::NameNoSuchBranch
            .of(label(&project))
            .project_repo()
            .subject(&branch)
            .judge(vcs.as_ref())
            .refused());
    };
    if request.mode == Mode::Feature {
        let tip = ResolvedRevisionId::from(known.tip.clone());
        let manifest = match vcs.read_file_at_revision(&dir, &tip, Path::new(Manifest::FILE_NAME)) {
            Ok(text) => Manifest::from_toml_str(&text).ok(),
            Err(crate::vcs::VcsError::RevisionNotFound { .. }) => None,
            Err(e) => return Err(e.into()),
        };
        if let Some(manifest) = manifest {
            let mut report = Report::default();
            for judged in judge_feature_names(
                ctx.active_path(),
                &NameScan::of(ctx.primary_path(), &project)?,
                &branch,
                &manifest,
                Some("branch set-mode"),
            )? {
                report.add(judged);
            }
            report.into_result()?;
        }
    }
    let store = Store::open(vcs.as_ref(), &dir)?;
    let before = view_of(&store)?;
    let written = store
        .write(|view| {
            let record = match (&request.mode, &target) {
                (Mode::Declared, _) => Record::Declared,
                (Mode::Feature, Some(target)) => feature_record(view, &branch, target)?,
                (Mode::Feature, None) => unreachable!("dispatch requires --target with feature"),
            };
            Ok(vec![Write {
                branch: branch.clone(),
                record,
            }])
        })
        .map_err(|e| write_refusal(vcs.as_ref(), &store, &project, &dir, &branch, e))?;
    let Lookup::Value(record) = written.view.lookup(&branch) else {
        unreachable!("the record just written reads as one value");
    };
    crate::alignment::store_record(vcs.as_ref(), &dir, &branch, &record)?;
    outln!("rwv branch set-mode: {branch} is {record}");
    if let Record::Feature { target, as_mode } = &record {
        for line in target_recorded_lines(vcs.as_ref(), &dir, &branch, target, *as_mode)? {
            outln!("{line}");
        }
    }
    report_dependents(&before, &written.view, &branch);
    if checked_out(vcs.as_ref(), &dir)?.as_deref() == Some(branch.as_str()) {
        crate::update::align_after_switch(ctx, "branch set-mode")?;
    }
    Ok(())
}

/// What `rwv branch rename` is asked to do.
pub struct Rename {
    pub new: String,
    pub adopt: bool,
}

pub(crate) struct Moving {
    pub(crate) label: String,
    pub(crate) dir: std::path::PathBuf,
    pub(crate) vcs: Box<dyn Vcs>,
    url_remote: Option<RemoteHandle>,
    pub(crate) member: Option<RepoPath>,
    adopted: Option<Upstream>,
    /// The weave branches declaring this member's branch of the old name,
    /// which the rename leaves where it stands.
    declared_by: Vec<String>,
}

impl Moving {
    pub(crate) fn present(
        label: String,
        dir: PathBuf,
        vcs: Box<dyn Vcs>,
        member: Option<RepoPath>,
    ) -> Self {
        Self {
            label,
            dir,
            vcs,
            url_remote: None,
            member,
            adopted: None,
            declared_by: Vec::new(),
        }
    }

    pub(crate) fn judging(&self, state: State) -> Judging {
        let judging = state.of(&self.label).checkout(&self.dir);
        match &self.member {
            Some(path) => judging.member().bind("<m>", path),
            None => judging.project_repo(),
        }
    }
}

fn moving_repos(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    dir: &Path,
    feature: bool,
    old: &RawRefName,
) -> anyhow::Result<Vec<Moving>> {
    let project_vcs = project_vcs();
    let url_remote = project_vcs.project_url_remote(dir)?;
    let mut repos = vec![Moving {
        label: label(project),
        dir: dir.to_path_buf(),
        vcs: project_vcs,
        url_remote,
        member: None,
        adopted: None,
        declared_by: Vec::new(),
    }];
    if !feature {
        return Ok(repos);
    }
    let manifest = Manifest::from_path(&dir.join(Manifest::FILE_NAME))?;
    for (path, entry) in manifest.iter_entries() {
        if !matches!(entry.role, Role::Owned | Role::Fork) {
            continue;
        }
        let member_dir = ctx.active_path().join(path.as_path());
        let vcs = vcs_for(entry.vcs_type);
        if !vcs.is_repo(&member_dir) || vcs.resolve_local_branch_tip(&member_dir, old)?.is_none() {
            continue;
        }
        let url_remote = crate::url_remote::present(vcs.as_ref(), &member_dir, &entry.url)?;
        repos.push(Moving {
            label: path.to_string(),
            dir: member_dir,
            vcs,
            url_remote,
            member: Some(path.clone()),
            adopted: None,
            declared_by: Vec::new(),
        });
    }
    Ok(repos)
}

fn other_checkouts(repo: &Moving, old: &RawRefName) -> anyhow::Result<Vec<PathBuf>> {
    let here = CanonicalPath::of(&repo.dir);
    Ok(repo
        .vcs
        .checkouts_of_branch(&repo.dir, old)?
        .into_iter()
        .filter(|checkout| CanonicalPath::of(checkout) != here)
        .collect())
}

/// `name` taken in the repo: a local branch, or one a remote has as last
/// fetched unless `remote_is_free`. Reported as `name` exists; whether it was
/// is returned.
pub(crate) fn judge_name_taken(
    repo: &Moving,
    name: &BranchName,
    remote_is_free: bool,
    verb: &'static str,
    report: &mut Report,
) -> anyhow::Result<bool> {
    let raw = RawRefName::new(name.as_str());
    let taken_locally = repo
        .vcs
        .resolve_local_branch_tip(&repo.dir, &raw)?
        .is_some();
    let copies = repo.vcs.remote_copies(&repo.dir, &raw)?;
    if !taken_locally && (copies.is_empty() || remote_is_free) {
        return Ok(false);
    }
    let mut listing = Vec::new();
    if taken_locally {
        listing.push(format!("{name} is a local branch"));
    }
    listing.extend(copies.iter().map(|c| format!("{name} is on {}", c.remote)));
    report.add(
        repo.judging(State::NameExists)
            .subject(name)
            .listing(listing)
            .bind("<W>", name)
            .by_verb(verb)
            .judge(repo.vcs.as_ref()),
    );
    Ok(true)
}

fn judge_rename_of(
    repo: &mut Moving,
    request: &Rename,
    old: &BranchName,
    new: &BranchName,
    scan: &NameScan<'_>,
    report: &mut Report,
) -> anyhow::Result<()> {
    let old_raw = RawRefName::new(old.as_str());
    let new_raw = RawRefName::new(new.as_str());
    if let Some(path) = &repo.member {
        repo.declared_by = scan.accounting.declarers(path, old.as_str());
    }
    if repo.declared_by.is_empty() {
        let recorded = crate::sync::recorded_workweaves(scan.primary)?;
        for checkout in other_checkouts(repo, &old_raw)? {
            report.add(crate::sync::judge_checkout(
                repo.vcs.as_ref(),
                &repo.label,
                &checkout,
                &repo.dir,
                &recorded,
                None,
            ));
        }
    }
    if !repo.declared_by.is_empty() {
        let tip_of = |name: &RawRefName| repo.vcs.resolve_local_branch_tip(&repo.dir, name);
        if tip_of(&new_raw)?.is_some() && tip_of(&new_raw)? == tip_of(&old_raw)? {
            return Ok(());
        }
    }
    if judge_name_taken(repo, new, request.adopt, "branch rename", report)? {
        return Ok(());
    }
    if !request.adopt {
        return Ok(());
    }
    let copies = repo.vcs.remote_copies(&repo.dir, &new_raw)?;
    let target = repo
        .vcs
        .push_target_of(&repo.dir, &old_raw, repo.url_remote.as_ref())?;
    let Some(copy) = copies
        .into_iter()
        .find(|copy| Some(&copy.remote) == target.as_ref())
    else {
        if repo.member.is_none() {
            report.add(
                repo.judging(State::NameNoSuchBranch)
                    .subject(format!("{new} on the remote {} publishes to", repo.label))
                    .judge(repo.vcs.as_ref()),
            );
        }
        return Ok(());
    };
    let tip = repo
        .vcs
        .resolve_local_branch_tip(&repo.dir, &old_raw)?
        .expect("a branch to rename exists");
    if repo.vcs.relation(&repo.dir, &tip, &copy.tip)? == Relation::Diverged {
        let unpushed = repo.vcs.unpushed_commits(&repo.dir, &old_raw)?;
        report.add(
            repo.judging(State::PushBranchDiverged)
                .subject(format!("{old} and {}/{new}", copy.remote))
                .listing(
                    unpushed
                        .iter()
                        .map(|c| format!("{} {}", short_revision(c.id.as_str()), c.subject)),
                )
                .judge(repo.vcs.as_ref()),
        );
    }
    repo.adopted = Some(Upstream {
        remote: copy.remote,
        branch: new_raw,
    });
    Ok(())
}

fn rename_in(repo: &Moving, old: &RawRefName, new: &RawRefName) -> anyhow::Result<()> {
    let Some(tip) = repo.vcs.resolve_local_branch_tip(&repo.dir, old)? else {
        return Ok(());
    };
    if repo.declared_by.is_empty() {
        repo.vcs.rename_branch(&repo.dir, old, new)?;
        outln!("renamed {}: {old} -> {new}", repo.label);
    } else {
        branch_off_declared(repo, old, new, &tip)?;
    }
    if let Some(upstream) = &repo.adopted {
        repo.vcs.set_upstream(&repo.dir, new, upstream)?;
        outln!(
            "adopted {}: {new} takes in from {}/{}",
            repo.label,
            upstream.remote,
            upstream.branch
        );
    }
    Ok(())
}

/// A member branch of the old name that a weave branch declares is not the
/// feature's own, so it stays: the feature's branch of the new name starts at
/// its tip, and a checkout on it here moves to the new one.
fn branch_off_declared(
    repo: &Moving,
    old: &RawRefName,
    new: &RawRefName,
    tip: &ResolvedRevisionId,
) -> anyhow::Result<()> {
    let on_old = checked_out(repo.vcs.as_ref(), &repo.dir)?.as_deref() == Some(old.as_str());
    let made = repo.vcs.resolve_local_branch_tip(&repo.dir, new)?.is_some();
    let departure = || crate::alignment::Departure::of_move(repo.vcs.as_ref(), &repo.dir, tip);
    match (made, on_old) {
        (false, true) => repo
            .vcs
            .create_branch_and_attach(&repo.dir, new, tip, &departure()?)?,
        (false, false) => repo.vcs.create_branch(&repo.dir, new, tip, None)?,
        (true, true) => repo.vcs.reattach_head(
            repo.vcs.head_attachment(&repo.dir)?,
            &LocalRefName::of_branch(new),
            &departure()?,
        )?,
        (true, false) => {}
    }
    outln!(
        "branched {}: {new} starts at {old}, which stays as {} declare it",
        repo.label,
        repo.declared_by.join(", ")
    );
    Ok(())
}

/// `rwv branch rename <new> [--adopt]`.
/// The refusal of a verb that needs the project repo on a branch: in a
/// workspace made detached, that it is one; else the table's detached project
/// repo, with the commits its HEAD holds.
pub(crate) fn refuse_off_branch(
    vcs: &dyn Vcs,
    dir: &Path,
    project: &ProjectName,
    verb: &'static str,
    why: &str,
) -> anyhow::Error {
    let detached_workspace = matches!(
        crate::alignment::read_state(vcs, dir).map(|reading| reading.state.aligned),
        Ok(Some(crate::alignment::Aligned::Detached))
    );
    if detached_workspace {
        return refusal(
            RefusalKind::HeadNotOnBranch,
            format!("rwv {verb}: this workspace is detached, so {why}"),
        );
    }
    match crate::alignment::project_detached(dir, project) {
        Ok(judging) => judging.by_verb(verb).judge(vcs).refused(),
        Err(e) => e,
    }
}

pub fn run_rename(ctx: &WorkspaceContext, request: Rename) -> anyhow::Result<()> {
    let (project, dir) = find_project(ctx)?;
    let vcs = project_vcs();
    let Some(old_text) = checked_out(vcs.as_ref(), &dir)? else {
        return Err(refuse_off_branch(
            vcs.as_ref(),
            &dir,
            &project,
            "branch rename",
            "there is no branch to rename",
        ));
    };
    let old = named(vcs.as_ref(), &project, &dir, &old_text)?;
    let new = named(vcs.as_ref(), &project, &dir, &request.new)?;
    let old_raw = RawRefName::new(old.as_str());
    let new_raw = RawRefName::new(new.as_str());
    if old == new {
        return Err(State::NameExists
            .of(label(&project))
            .project_repo()
            .subject(&new)
            .checkout(&dir)
            .by_verb("branch rename")
            .judge(vcs.as_ref())
            .refused());
    }
    let store = Store::open(vcs.as_ref(), &dir)?;
    let view = store
        .settle()
        .map_err(|e| write_refusal(vcs.as_ref(), &store, &project, &dir, &old, e))?;
    let resuming = crate::alignment::is_renaming(vcs.as_ref(), &dir, old.as_str(), new.as_str())?;
    let record = match (view.lookup(&old), view.lookup(&new)) {
        (Lookup::Value(record), _) => record,
        (Lookup::Unrecorded, Lookup::Value(record)) if resuming => record,
        (lookup, _) => {
            return Err(judge_lookup(vcs.as_ref(), &store, &project, &dir, &old, lookup)?.refused())
        }
    };
    let mut report = Report::default();
    if let (Lookup::Value(moved), held) = (view.lookup(&old), view.lookup(&new)) {
        match held {
            Lookup::Unrecorded => {}
            Lookup::Value(ref existing) if *existing == moved => {}
            Lookup::Value(existing) if request.adopt => report.add(judge_differs(
                vcs.as_ref(),
                &project,
                &dir,
                &new,
                &existing,
                &moved,
            )),
            held if request.adopt => report.add(judge_lookup(
                vcs.as_ref(),
                &store,
                &project,
                &dir,
                &new,
                held,
            )?),
            _ => report.add(
                State::NameExists
                    .of(label(&project))
                    .project_repo()
                    .subject(&new)
                    .checkout(&dir)
                    .listing([format!("{new} has a record of another value")])
                    .by_verb("branch rename")
                    .judge(vcs.as_ref()),
            ),
        }
    }
    let dependents: Vec<BranchName> = view
        .dependents(&old)
        .into_iter()
        .filter(|dependent| *dependent != new)
        .collect();
    for dependent in &dependents {
        match view.lookup(dependent) {
            Lookup::Value(_) => {}
            lookup => report.add(judge_lookup(
                vcs.as_ref(),
                &store,
                &project,
                &dir,
                dependent,
                lookup,
            )?),
        }
    }
    if let Some(chain) = view.rename_cycle(&old, &new, &record) {
        report.add(judge_lookup(
            vcs.as_ref(),
            &store,
            &project,
            &dir,
            &new,
            Lookup::Cycle(chain),
        )?);
    }
    let mut repos = moving_repos(
        ctx,
        &project,
        &dir,
        record.mode() == Mode::Feature,
        &old_raw,
    )?;
    let scan = NameScan::of(ctx.primary_path(), &project)?;
    if record.mode() == Mode::Feature {
        let manifest = Manifest::from_path(&dir.join(Manifest::FILE_NAME))?;
        for judged in judge_feature_names(
            ctx.active_path(),
            &scan,
            &new,
            &manifest,
            Some("branch rename"),
        )? {
            report.add(judged);
        }
    }
    for repo in &mut repos {
        judge_rename_of(repo, &request, &old, &new, &scan, &mut report)?;
    }
    report.into_result()?;

    crate::alignment::mark_renaming(vcs.as_ref(), &dir, Some((old.as_str(), new.as_str())))?;
    for repo in repos.iter().filter(|repo| repo.member.is_some()) {
        rename_in(repo, &old_raw, &new_raw)?;
    }
    let written = store
        .rename(&old, &new)
        .map_err(|e| write_refusal(vcs.as_ref(), &store, &project, &dir, &old, e))?;
    outln!("moved the record of {old} to {new}: {}", {
        match written.view.lookup(&new) {
            Lookup::Value(record) => record.to_string(),
            _ => unreachable!("the record just moved reads as one value"),
        }
    });
    for dependent in &dependents {
        outln!(
            "retargeted {dependent}: it now lands into {new}; `rwv push --branch {dependent}` \
             publishes it"
        );
    }
    if let Some(why) =
        crate::alignment::rename_branch(vcs.as_ref(), &dir, old.as_str(), new.as_str())?
    {
        eprintln!(
            "rwv branch rename: the workspace state of {} reads as absent: {why}",
            label(&project)
        );
    }
    if let Some(project_repo) = repos.iter().find(|repo| repo.member.is_none()) {
        rename_in(project_repo, &old_raw, &new_raw)?;
    }
    crate::alignment::mark_renaming(vcs.as_ref(), &dir, None)?;
    for repo in repos.iter().filter(|repo| repo.declared_by.is_empty()) {
        let copies = repo.vcs.remote_copies(&repo.dir, &old_raw)?;
        if copies.is_empty() {
            continue;
        }
        for copy in &copies {
            eprintln!("left: {} {old}: {} still has it", repo.label, copy.remote);
        }
        eprintln!(
            "  delete what is landed: rwv branch delete {old} --target {new} --delete-remote"
        );
    }
    Ok(())
}

fn member_entries(
    vcs: &dyn Vcs,
    dir: &Path,
    tip: &ResolvedRevisionId,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let text = vcs
        .read_file_at_revision(dir, tip, Path::new(Manifest::FILE_NAME))
        .map_err(|e| format!("{e:#}"))?;
    let manifest = Manifest::from_toml_str(&text).map_err(|e| format!("{e:#}"))?;
    Ok(manifest
        .iter_entries()
        .map(|(path, entry)| {
            (
                path.to_string(),
                toml::to_string(entry).unwrap_or_else(|e| format!("{e:#}")),
            )
        })
        .collect())
}

/// What recording `branch` as a feature onto `target` says about where it
/// lands: into `target`'s member branches when `target` is a feature, else into
/// each branch `target` declares, listing the members whose entry in `branch`
/// differs from `target`'s.
pub(crate) fn target_recorded_lines(
    vcs: &dyn Vcs,
    dir: &Path,
    branch: &BranchName,
    target: &BranchName,
    as_mode: Mode,
) -> anyhow::Result<Vec<String>> {
    let lands = match as_mode {
        Mode::Feature => format!("into {target}'s member branches"),
        Mode::Declared => format!("into each branch {target} declares"),
    };
    let mut lines = vec![format!("target-recorded: {branch} lands {lands}")];
    if as_mode == Mode::Feature {
        return Ok(lines);
    }
    let known = vcs.named_branches(dir)?;
    let tip = |name: &BranchName| {
        known
            .iter()
            .find(|b| b.name == name.as_str())
            .map(|b| ResolvedRevisionId::from(b.tip.clone()))
    };
    let (Some(own), Some(theirs)) = (tip(branch), tip(target)) else {
        return Ok(lines);
    };
    match (
        member_entries(vcs, dir, &own),
        member_entries(vcs, dir, &theirs),
    ) {
        (Ok(own), Ok(theirs)) => {
            let differing: Vec<&String> = own
                .keys()
                .chain(theirs.keys())
                .filter(|key| own.get(*key) != theirs.get(*key))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            if !differing.is_empty() {
                lines.push(format!(
                    "target-recorded: members whose entry in {branch} differs from {target}'s: {}",
                    differing
                        .iter()
                        .map(|m| m.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        (Err(why), _) | (_, Err(why)) => {
            lines.push(format!(
                "target-recorded: member entries were not compared: {why}"
            ));
        }
    }
    Ok(lines)
}

fn report_dependents(before: &View, after: &View, branch: &BranchName) {
    let mode_before = match before.lookup(branch) {
        Lookup::Value(record) => Some(record.mode()),
        _ => None,
    };
    let Lookup::Value(record) = after.lookup(branch) else {
        return;
    };
    if mode_before == Some(record.mode()) {
        return;
    }
    for dependent in after.dependents(branch) {
        if let Some(Entry::Value(Record::Feature { as_mode, .. })) = after.entry(&dependent) {
            if as_mode != record.mode() {
                outln!(
                    "dependent: {dependent} records {branch} as {}, and {branch} is now {}; \
                     it refuses to land until rebound with `rwv branch set-mode {dependent} \
                     feature --target {branch}`",
                    as_mode.token(),
                    record.mode().token(),
                );
            }
        }
    }
}

/// What `rwv branch delete` is asked to do.
pub struct Delete {
    pub branch: String,
    pub target: Option<String>,
    pub discard_unlanded: Option<crate::cli::consent::DiscardUnlandedConsent>,
    pub remote_deletes: crate::deletion::RemoteDeletes,
}

/// The envelope `rwv branch delete --json` prints.
#[derive(Debug, Serialize, JsonSchema)]
pub struct BranchDeleteJsonOutput {
    #[serde(rename = "$schema")]
    pub schema_url: String,
    pub branch: String,
    /// The branch the deleted branches were judged landed against.
    pub landed_on: String,
    #[serde(flatten)]
    pub outcome: crate::deletion::Outcome,
}

/// `rwv branch delete <W> [--target <T>] [--discard-unlanded]
/// [--delete-remote] [--delete-remote-unlanded <repo>]...`.
pub fn run_delete(
    ctx: &WorkspaceContext,
    request: Delete,
    output: OutputMode,
) -> anyhow::Result<()> {
    let (project, dir) = find_project(ctx)?;
    let vcs = project_vcs();
    let branch = named(vcs.as_ref(), &project, &dir, &request.branch)?;
    let store = Store::open(vcs.as_ref(), &dir)?;
    let view = view_of(&store)?;
    let target = match (&request.target, view.lookup(&branch)) {
        (Some(text), _) => named(vcs.as_ref(), &project, &dir, text)?,
        (None, Lookup::Value(Record::Feature { target, .. })) => target,
        (None, Lookup::Value(Record::Declared)) => {
            return Err(State::TargetNoTarget
                .of(label(&project))
                .project_repo()
                .subject(&branch)
                .bind("<W>", &branch)
                .judge(vcs.as_ref())
                .refused())
        }
        (None, lookup) => {
            return Err(
                judge_lookup(vcs.as_ref(), &store, &project, &dir, &branch, lookup)?.refused(),
            )
        }
    };
    let rule = crate::deletion::Rule::open(crate::deletion::Request {
        primary: ctx.primary_path(),
        project: &project,
        branch: branch.clone(),
        target: target.clone(),
        discard_unlanded: request.discard_unlanded,
        remote_deletes: &request.remote_deletes,
        leaving: None,
        verb: "branch delete",
    })?;
    let outcome = rule.apply()?;
    if output.is_json() {
        let envelope = BranchDeleteJsonOutput {
            schema_url: BRANCH_DELETE_SCHEMA_URL.to_owned(),
            branch: branch.to_string(),
            landed_on: target.to_string(),
            outcome: outcome.clone(),
        };
        outln!("{}", serde_json::to_string_pretty(&envelope)?);
    } else {
        if outcome.deleted.is_empty() && outcome.survivors.is_empty() {
            outln!("nothing of {branch} to delete");
        }
        outcome.print();
    }
    if outcome.left_something() {
        return Err(crate::exit_status::CompletedWithRemainder.into());
    }
    Ok(())
}

/// Drop the records whose branches exist nowhere: no local branch, no
/// remote-tracking branch, and no workspace of this machine aligned for it.
/// Nothing drops while a fetch of the project repo went unconfirmed. What
/// exists is read once per attempt, as the write that drops reads the
/// records; a writer that creates a branch creates it before it writes the
/// record, and that write always commits, so a drop judged before the branch
/// existed loses its compare-and-swap and judges again. What cannot be read
/// fails the drop.
pub fn drop_records(
    project: &ProjectName,
    primary: &Path,
    unconfirmed: bool,
) -> anyhow::Result<Vec<String>> {
    if unconfirmed {
        return Ok(Vec::new());
    }
    let vcs = project_vcs();
    let dir = crate::workspace::project_dir(primary, project);
    let gone = |names: &[String]| -> anyhow::Result<std::collections::BTreeSet<String>> {
        let mut kept: std::collections::BTreeSet<String> = vcs
            .named_branches(&dir)?
            .into_iter()
            .map(|b| b.name)
            .collect();
        let mut workspaces: Vec<std::path::PathBuf> = vec![dir.clone()];
        for (id, root) in crate::sync::recorded_workweaves(primary)? {
            if &id.project == project {
                workspaces.push(crate::workspace::project_dir(root.as_path(), project));
            }
        }
        for workspace in workspaces {
            if let crate::alignment::StateReading {
                state:
                    crate::alignment::WorkspaceState {
                        aligned: Some(crate::alignment::Aligned::Branch(aligned)),
                        ..
                    },
                ..
            } = crate::alignment::read_state(vcs.as_ref(), &workspace)?
            {
                kept.insert(aligned);
            }
        }
        Ok(names
            .iter()
            .filter(|name| !kept.contains(*name))
            .cloned()
            .collect())
    };
    let store = Store::open(vcs.as_ref(), &dir)?;
    let written = store
        .write_dropping(|_| Ok(Vec::new()), &gone)
        .map_err(|e| anyhow::Error::new(e).context("could not drop the branch records"))?;
    Ok(written.dropped)
}

/// One weave branch's record, as `rwv branch list --json` carries it.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum RecordOutput {
    Declared,
    Feature {
        /// The branch the feature lands into.
        target_branch: String,
        #[serde(rename = "as")]
        as_mode: Mode,
    },
    /// The branch has no record here.
    Unrecorded,
    /// Two writers recorded the branch differently; both values are listed.
    Conflict {
        values: Vec<ValueOutput>,
    },
    /// The entry does not read as a record.
    Unreadable {
        reason: String,
    },
    /// The branch's target chain returns to it.
    Cycle {
        chain: Vec<String>,
    },
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ValueOutput {
    pub mode: Mode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_branch: Option<String>,
    #[serde(rename = "as", skip_serializing_if = "Option::is_none")]
    pub as_mode: Option<Mode>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListedBranch {
    pub name: String,
    /// The project repo is on this branch.
    pub checked_out: bool,
    /// The branch exists locally; false for one only on a remote, as last
    /// fetched.
    pub local: bool,
    pub record: RecordOutput,
    /// A remote's records, as last fetched, hold this entry. False for an
    /// entry only this machine has, which a push of the branch publishes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<bool>,
    /// The record's target exists nowhere, local or remote.
    pub target_gone: bool,
}

/// The envelope `rwv branch list --json` prints.
#[derive(Debug, Serialize, JsonSchema)]
pub struct BranchListJsonOutput {
    #[serde(rename = "$schema")]
    pub schema_url: String,
    pub branches: Vec<ListedBranch>,
}

fn value_output(record: &Record) -> ValueOutput {
    match record {
        Record::Declared => ValueOutput {
            mode: Mode::Declared,
            target_branch: None,
            as_mode: None,
        },
        Record::Feature { target, as_mode } => ValueOutput {
            mode: Mode::Feature,
            target_branch: Some(target.to_string()),
            as_mode: Some(*as_mode),
        },
    }
}

fn record_output(lookup: &Lookup) -> RecordOutput {
    match lookup {
        Lookup::Unrecorded => RecordOutput::Unrecorded,
        Lookup::Value(Record::Declared) => RecordOutput::Declared,
        Lookup::Value(Record::Feature { target, as_mode }) => RecordOutput::Feature {
            target_branch: target.to_string(),
            as_mode: *as_mode,
        },
        Lookup::Conflict(first, second) => RecordOutput::Conflict {
            values: vec![value_output(first), value_output(second)],
        },
        Lookup::Unreadable(why) => RecordOutput::Unreadable {
            reason: why.clone(),
        },
        Lookup::Cycle(chain) => RecordOutput::Cycle {
            chain: chain.iter().map(ToString::to_string).collect(),
        },
    }
}

fn record_line(lookup: &Lookup) -> String {
    match lookup {
        Lookup::Unrecorded => "unrecorded".to_owned(),
        Lookup::Value(record) => record.to_string(),
        Lookup::Conflict(first, second) => format!("conflict: {first} | {second}"),
        Lookup::Unreadable(why) => format!("unreadable: {why}"),
        Lookup::Cycle(chain) => format!("cycle: {}", chain_text(chain)),
    }
}

/// A project repo's checked-out branch and what its record reads as, with the
/// branches that land into it, as `rwv status` shows it.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct BranchRecordStatus {
    pub project: ProjectName,
    pub branch: String,
    pub record: RecordOutput,
    /// The branches whose records name this one as their target.
    pub dependents: Vec<String>,
}

impl BranchRecordStatus {
    pub fn line(&self) -> String {
        let record = match &self.record {
            RecordOutput::Declared => "declared".to_owned(),
            RecordOutput::Feature {
                target_branch,
                as_mode,
            } => {
                format!("feature onto {target_branch} (as {})", as_mode.token())
            }
            RecordOutput::Unrecorded => "unrecorded".to_owned(),
            RecordOutput::Conflict { .. } => "conflict".to_owned(),
            RecordOutput::Unreadable { reason } => format!("unreadable: {reason}"),
            RecordOutput::Cycle { chain } => format!("cycle: {}", chain.join(" -> ")),
        };
        let mut line = format!("record: {} {}: {record}", self.project, self.branch);
        if !self.dependents.is_empty() {
            line.push_str(&format!("; landed into by {}", self.dependents.join(", ")));
        }
        line
    }
}

/// The record of the branch the project repo at `dir` is on, computed in
/// memory from the local records branch and the records remotes as last
/// fetched. `None` where the repo is not on a branch.
pub fn project_record(
    project: &ProjectName,
    dir: &Path,
) -> anyhow::Result<Option<BranchRecordStatus>> {
    let vcs = project_vcs();
    if !vcs.is_repo(dir) {
        return Ok(None);
    }
    let Some(text) = checked_out(vcs.as_ref(), dir)? else {
        return Ok(None);
    };
    let branch = named(vcs.as_ref(), project, dir, &text)?;
    let store = Store::open(vcs.as_ref(), dir)?;
    let view = view_of(&store)?;
    Ok(Some(BranchRecordStatus {
        project: project.clone(),
        record: record_output(&view.lookup(&branch)),
        dependents: view
            .dependents(&branch)
            .iter()
            .map(ToString::to_string)
            .collect(),
        branch: text,
    }))
}

/// `rwv branch list`: every weave branch with its record, computed in memory
/// from the local records branch and the records remotes' as last fetched.
pub fn run_list(ctx: &WorkspaceContext, output: OutputMode) -> anyhow::Result<()> {
    let (_, dir) = find_project(ctx)?;
    let vcs = project_vcs();
    let current = checked_out(vcs.as_ref(), &dir)?;
    let store = Store::open(vcs.as_ref(), &dir)?;
    let view = view_of(&store)?;
    let branches = vcs.named_branches(&dir)?;
    let published_names = store.published_names(&view)?;
    let mut listed: Vec<ListedBranch> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    for branch in &branches {
        let Ok(name) = BranchName::parse(vcs.as_ref(), &branch.name) else {
            continue;
        };
        let tip = ResolvedRevisionId::from(branch.tip.clone());
        if vcs
            .read_file_at_revision(&dir, &tip, Path::new(Manifest::FILE_NAME))
            .is_err()
        {
            continue;
        }
        let lookup = view.lookup(&name);
        let published = match lookup {
            Lookup::Unrecorded => None,
            _ => Some(published_names.contains(name.as_str())),
        };
        let target_gone = match &lookup {
            Lookup::Value(Record::Feature { target, .. }) => {
                !branches.iter().any(|b| b.name == target.as_str())
            }
            _ => false,
        };
        let checked_out = current.as_deref() == Some(branch.name.as_str());
        let mut line = format!(
            "{} {:<24} {}",
            if checked_out { "*" } else { " " },
            branch.name,
            record_line(&lookup)
        );
        if target_gone {
            line.push_str(" [target gone]");
        }
        if published == Some(false) {
            line.push_str(" [unpublished]");
        }
        if !branch.local {
            line.push_str(" [remote only]");
        }
        lines.push(line);
        listed.push(ListedBranch {
            name: branch.name.clone(),
            checked_out,
            local: branch.local,
            record: record_output(&lookup),
            published,
            target_gone,
        });
    }
    if output.is_json() {
        let envelope = BranchListJsonOutput {
            schema_url: BRANCH_SCHEMA_URL.to_owned(),
            branches: listed,
        };
        outln!("{}", serde_json::to_string_pretty(&envelope)?);
        return Ok(());
    }
    if lines.is_empty() {
        outln!("no weave branches");
    }
    for line in lines {
        outln!("{line}");
    }
    Ok(())
}
