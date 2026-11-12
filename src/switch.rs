//! `rwv switch` and `rwv branch create`: open a weave branch in place. Both
//! judge every repo they would move before anything moves, write the branch's
//! record, put the project repo on the branch, and align the members.

use std::path::{Path, PathBuf};

use crate::add_remove::find_project;
use crate::alignment::{Accounting, Aligned, Aligner, Departure, Member};
use crate::branch::{
    checked_out, feature_record, judge_lookup, judge_name_taken, label, target_recorded_lines,
    write_refusal, Moving,
};
use crate::diagnosis::{ChoiceId, Judged, Report, State};
use crate::manifest::{LockFile, Manifest, ProjectName};
use crate::names::{judge_feature_names, NameScan};
use crate::records::{BranchName, Entry, Lookup, NameError, Record, Store, View, Write};
use crate::refusal::{refusal, RefusalKind};
use crate::vcs::{
    project_vcs, short_revision, vcs_for, HeadAttachment, LocalRefName, RawRefName, RemoteTip,
    ResolvedRevisionId, Upstream, Vcs, VcsError,
};
use crate::workspace::WorkspaceContext;

/// How an opening spells the branch's use, where the record does not say.
#[derive(Debug, Clone)]
pub enum Spelling {
    Target(String),
    Declared,
}

/// What `rwv branch create` is asked to do.
#[derive(Debug, Clone)]
pub struct Create {
    pub branch: String,
    pub spelling: Option<Spelling>,
    pub at: Option<String>,
    pub entries: Vec<crate::member_branch::BranchEntry>,
}

/// What `rwv switch` is asked to do.
#[derive(Debug, Clone)]
pub enum Switch {
    Branch(String),
    Detach(String),
}

const CREATE: &str = "branch create";
const SWITCH: &str = "switch";

pub(crate) fn named(
    vcs: &dyn Vcs,
    project: &ProjectName,
    dir: &Path,
    text: &str,
    verb: &'static str,
) -> anyhow::Result<BranchName> {
    match BranchName::parse(vcs, text) {
        Ok(name) => Ok(name),
        Err(NameError::Reserved) => Err(State::NameReserved
            .of(label(project))
            .subject(text)
            .checkout(dir)
            .by_verb(verb)
            .only(&[ChoiceId::AnotherName])
            .judge(vcs)
            .refused()),
        Err(NameError::Invalid(e)) => Err(refusal(
            RefusalKind::InvalidRefName,
            format!("`{text}` is not a usable branch name: {e:#}"),
        )),
    }
}

/// The project repo's files as the workspace will read them at `at`: the
/// working tree's where `at` is the commit checked out, else the commit's.
/// The manifest is its own result, since a pre-schema one moves the project
/// repo alone; a lock that is there and does not read is an error.
pub(crate) fn project_files(
    vcs: &dyn Vcs,
    dir: &Path,
    at: &ResolvedRevisionId,
) -> anyhow::Result<(anyhow::Result<Manifest>, Option<LockFile>)> {
    if &vcs.head_revision(dir)? == at {
        let lock_path = LockFile::path_in(dir);
        let lock = match lock_path.exists() {
            true => Some(LockFile::from_state(&lock_path)?),
            false => None,
        };
        return Ok((Manifest::from_path(&dir.join(Manifest::FILE_NAME)), lock));
    }
    committed_files(vcs, dir, at)
}

/// The manifest and lock the project repo holds at the commit `at`, whatever
/// its working tree says.
pub(crate) fn committed_files(
    vcs: &dyn Vcs,
    dir: &Path,
    at: &ResolvedRevisionId,
) -> anyhow::Result<(anyhow::Result<Manifest>, Option<LockFile>)> {
    let manifest = vcs
        .read_file_at_revision(dir, at, Path::new(Manifest::FILE_NAME))
        .map_err(anyhow::Error::from)
        .and_then(|text| Manifest::from_toml_str(&text));
    let lock = match vcs.read_file_at_revision(dir, at, Path::new(LockFile::FILE_NAME)) {
        Ok(text) => Some(LockFile::from_json_str(&text)?),
        Err(VcsError::RevisionNotFound { .. }) => None,
        Err(e) => return Err(e.into()),
    };
    Ok((manifest, lock))
}

/// Every uncommitted name in the project repo that matters to a move of its
/// checkout: tracked files with changes, and `rwv.toml` or `rwv.lock` where
/// untracked.
pub(crate) fn uncommitted_names(vcs: &dyn Vcs, dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut names = vcs.tracked_dirty_file_names(dir)?;
    for name in [Manifest::FILE_NAME, LockFile::FILE_NAME] {
        if dir.join(name).exists() && !vcs.is_tracked(dir, Path::new(name))? {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

/// What the move of the project repo's checkout is held back by.
pub(crate) struct ProjectMove<'a> {
    pub(crate) project: &'a ProjectName,
    pub(crate) dir: &'a Path,
    pub(crate) detached: bool,
    pub(crate) verb: &'static str,
    /// The files that refuse the move where uncommitted.
    pub(crate) refused_files: &'static [&'static str],
    /// The commit the checkout ends at, where it is not the one it is at.
    pub(crate) destination: Option<&'a ResolvedRevisionId>,
}

impl ProjectMove<'_> {
    /// Refuse the uncommitted files the move refuses, and every edited file
    /// the move to the destination would overwrite.
    pub(crate) fn judge(&self, vcs: &dyn Vcs, report: &mut Report) -> anyhow::Result<()> {
        let uncommitted = uncommitted_names(vcs, self.dir)?;
        let files: Vec<String> = uncommitted
            .iter()
            .filter(|name| self.refused_files.contains(&name.as_str()))
            .cloned()
            .collect();
        let mut dirty: Vec<String> = uncommitted
            .into_iter()
            .filter(|name| !files.contains(name))
            .collect();
        if let Some(to) = self.destination {
            let from = vcs.head_revision(self.dir)?;
            let changed = vcs.changed_paths_between(self.dir, &from, to)?;
            dirty.retain(|name| changed.contains(name));
            let tracked = vcs.tracked_dirty_file_names(self.dir)?;
            let untracked: Vec<String> = vcs
                .dirty_file_names(self.dir)?
                .into_iter()
                .filter(|name| !tracked.contains(name))
                .collect();
            for path in &changed {
                let overwritten = untracked.iter().any(|name| {
                    name == path || (name.ends_with('/') && path.starts_with(name.as_str()))
                });
                if overwritten && !dirty.contains(path) && !files.contains(path) {
                    dirty.push(path.clone());
                }
            }
        } else {
            dirty.clear();
        }
        report.extend(crate::diagnosis::judge_edits(
            vcs,
            label(self.project),
            self.dir,
            dirty,
            None,
        ));
        if !files.is_empty() {
            let choice = if self.detached {
                ChoiceId::C30
            } else {
                ChoiceId::C5
            };
            report.add(
                State::EditsLockFilesUncommitted
                    .of(label(self.project))
                    .listing(files)
                    .checkout(self.dir)
                    .bind("<W>", "<W>")
                    .by_verb(self.verb)
                    .only(&[choice])
                    .judge(vcs),
            );
        }
        Ok(())
    }
}

/// Each member the workspace has on disk that moving the project repo to the
/// branch would move, judged as alignment would judge it.
fn judge_members(
    ctx: &WorkspaceContext,
    aligner: &Aligner,
    manifest: &Manifest,
    lock: Option<&LockFile>,
    report: &mut Report,
) -> anyhow::Result<()> {
    for (path, entry) in manifest.iter_entries() {
        let dir = ctx.active_path().join(path.as_path());
        if !dir.exists()
            || crate::workweave::is_symlinked_member(&dir)
            || crate::workweave::is_dangling_link(&dir)
        {
            continue;
        }
        let vcs = vcs_for(entry.vcs_type);
        let member = Member {
            path,
            entry,
            dir: &dir,
            vcs: vcs.as_ref(),
        };
        if !vcs.is_repo(&dir) {
            continue;
        }
        let plan = aligner.plan(&member, lock)?;
        for judged in aligner.blocked(&member, &plan, path)? {
            report.add(judged);
        }
    }
    Ok(())
}

fn judge_members_at(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    vcs: &dyn Vcs,
    dir: &Path,
    at: &ResolvedRevisionId,
    branch: Option<(&BranchName, &Record)>,
    report: &mut Report,
) -> anyhow::Result<()> {
    let (manifest, lock) = project_files(vcs, dir, at)?;
    let Ok(manifest) = manifest else {
        return Ok(());
    };
    let aligner = Aligner::prospective(
        ctx.active_path(),
        project,
        Accounting::scan(ctx.primary_path())?,
        branch,
    )?;
    judge_members(ctx, &aligner, &manifest, lock.as_ref(), report)
}

/// The copy of `branch` a remote holds that this clone takes it from: the
/// only one, else the project's home remote's.
fn remote_copy(
    vcs: &dyn Vcs,
    dir: &Path,
    branch: &RawRefName,
) -> anyhow::Result<Option<RemoteTip>> {
    let copies = vcs.remote_copies(dir, branch)?;
    Ok(match copies.as_slice() {
        [] => None,
        [only] => Some(only.clone()),
        several => match vcs.project_url_remote(dir)? {
            Some(url_remote) => {
                let home = vcs.home_remote(dir, &url_remote)?;
                several.iter().find(|copy| copy.remote == home).cloned()
            }
            None => None,
        },
    })
}

pub(crate) fn no_such_branch(
    vcs: &dyn Vcs,
    project: &ProjectName,
    dir: &Path,
    what: impl std::fmt::Display,
    verb: &'static str,
) -> Judged {
    State::NameNoSuchBranch
        .of(label(project))
        .project_repo()
        .subject(what)
        .checkout(dir)
        .by_verb(verb)
        .only(&[ChoiceId::C33])
        .judge(vcs)
}

/// A refusal for a move the VCS declined: the edits it would have
/// overwritten, where the repo holds any.
fn refused_move(
    vcs: &dyn Vcs,
    project: &ProjectName,
    dir: &Path,
    error: VcsError,
) -> anyhow::Error {
    let dirty = vcs.dirty_file_names(dir).unwrap_or_default();
    match crate::diagnosis::judge_edits(vcs, label(project), dir, dirty, None) {
        Some(judged) => judged.refused(),
        None => anyhow::Error::new(error),
    }
}

fn after_project_move(
    ctx: &WorkspaceContext,
    dir: &Path,
    verb: &'static str,
) -> anyhow::Result<()> {
    Manifest::from_path(&dir.join(Manifest::FILE_NAME))?;
    crate::update::align_after_switch(ctx, verb)
}

/// A failed record computation as the judged line of the state it is: the
/// target that lacks a record, the cycle, the record that does not read.
fn judged_write_error(
    vcs: &dyn Vcs,
    store: &Store<'_>,
    project: &ProjectName,
    dir: &Path,
    branch: &BranchName,
    error: crate::records::WriteError,
) -> anyhow::Result<Judged> {
    use crate::records::WriteError;
    match error {
        WriteError::Cycle(chain) => {
            judge_lookup(vcs, store, project, dir, branch, Lookup::Cycle(chain))
        }
        WriteError::TargetUnrecorded(target) | WriteError::Unrecorded(target) => {
            judge_lookup(vcs, store, project, dir, &target, Lookup::Unrecorded)
        }
        WriteError::TargetUnreadable(target, lookup) => {
            judge_lookup(vcs, store, project, dir, &target, lookup)
        }
        other => Err(write_refusal(vcs, store, project, dir, branch, other)),
    }
}

fn spelled_record(
    view: &View,
    branch: &BranchName,
    spelling: &Spelling,
    parse: &dyn Fn(&str) -> anyhow::Result<BranchName>,
) -> anyhow::Result<Result<Record, crate::records::WriteError>> {
    Ok(match spelling {
        Spelling::Declared => Ok(Record::Declared),
        Spelling::Target(target) => feature_record(view, branch, &parse(target)?),
    })
}

pub(crate) fn spelling_agrees(
    view: &View,
    branch: &BranchName,
    spelling: &Spelling,
    existing: &Record,
    vcs: &dyn Vcs,
    (project, dir, verb): (&ProjectName, &Path, &'static str),
) -> anyhow::Result<bool> {
    let parse = |text: &str| named(vcs, project, dir, text, verb);
    Ok(match spelled_record(view, branch, spelling, &parse)? {
        Ok(spelled) => agrees(existing, &spelled),
        Err(_) => false,
    })
}

fn agrees(existing: &Record, spelled: &Record) -> bool {
    match (existing, spelled) {
        (Record::Declared, Record::Declared) => true,
        (Record::Feature { target: a, .. }, Record::Feature { target: b, .. }) => a == b,
        _ => false,
    }
}

/// Where a created branch starts.
#[derive(Clone, Copy)]
enum Start<'a> {
    At(&'a str),
    Head,
    TargetTip(&'a RawRefName),
}

/// What a create judged: the branch, its record (`None` where the record did
/// not compute, which the report says), where it starts, and the parent
/// project repo's own commit.
pub(crate) struct Planned {
    pub new: BranchName,
    pub record: Option<Record>,
    pub start: ResolvedRevisionId,
    pub head: ResolvedRevisionId,
    pub detached: bool,
}

/// The rules `branch create` and `workweave create` share for a new branch:
/// its name, the spelling that records it, where it starts, and whether any
/// repo of the workspace already has it. Everything it finds goes into
/// `report`; nothing is written.
#[allow(clippy::too_many_arguments)]
pub(crate) fn judge_creation(
    ctx: &WorkspaceContext,
    vcs: &dyn Vcs,
    project: &ProjectName,
    dir: &Path,
    branch: &str,
    spelling: Option<&Spelling>,
    at: Option<&str>,
    verb: &'static str,
    from_commits: bool,
    report: &mut Report,
) -> anyhow::Result<Option<Planned>> {
    let parse = |text: &str| named(vcs, project, dir, text, verb);
    let new = parse(branch)?;
    let current = checked_out(vcs, dir)?;
    let detached = current.is_none();
    let head = vcs.head_revision(dir)?;

    let target = match (spelling, &current) {
        (Some(Spelling::Declared), _) => None,
        (Some(Spelling::Target(text)), _) => Some(parse(text)?),
        (None, Some(text)) => Some(parse(text)?),
        (None, None) => {
            return Err(crate::branch::refuse_off_branch(
                vcs,
                dir,
                project,
                verb,
                "a bare create has no branch to land into; spell it with `--target <T>` or \
                 `--declared`",
            ));
        }
    };

    let store = Store::open(vcs, dir)?;
    let view = store
        .settle()
        .map_err(|e| write_refusal(vcs, &store, project, dir, &new, e))?;

    let record = match &target {
        None => Some(Record::Declared),
        Some(target) => match feature_record(&view, &new, target) {
            Ok(record) => Some(record),
            Err(error) => {
                report.add(judged_write_error(vcs, &store, project, dir, &new, error)?);
                None
            }
        },
    };

    let target_raw = target.as_ref().map(|t| RawRefName::new(t.as_str()));
    let start_from = match (at, &target_raw) {
        (Some(rev), _) => Start::At(rev),
        (None, _) if detached => Start::Head,
        (None, None) => Start::Head,
        (None, Some(target_raw)) => Start::TargetTip(target_raw),
    };
    let start = match start_from {
        Start::At(rev) => match vcs.resolve_revision(dir, rev) {
            Ok(at) => Some(at),
            Err(VcsError::RevisionNotFound { .. }) => {
                report.add(no_such_branch(vcs, project, dir, rev, verb));
                None
            }
            Err(e) => return Err(e.into()),
        },
        Start::Head => Some(head.clone()),
        Start::TargetTip(target_raw) => {
            let home = vcs.project_url_remote(dir)?;
            match crate::push_branch::newer_of_branch_and_upstream(
                vcs,
                dir,
                target_raw,
                home.as_ref(),
            )? {
                crate::push_branch::NewerOfBranchAndUpstream::Tip(tip) => Some(tip),
                crate::push_branch::NewerOfBranchAndUpstream::Diverged { local, .. } => {
                    diverged_start(vcs, project, dir, target_raw, verb, report)?;
                    Some(local)
                }
                crate::push_branch::NewerOfBranchAndUpstream::Nowhere => {
                    report.add(no_such_branch(vcs, project, dir, target_raw.as_str(), verb));
                    None
                }
            }
        }
    };

    let Some(start) = start else {
        return Ok(None);
    };
    report_names(
        ctx,
        vcs,
        project,
        dir,
        &new,
        record.as_ref(),
        &start,
        verb,
        from_commits,
        report,
    )?;
    Ok(Some(Planned {
        new,
        record,
        start,
        head,
        detached,
    }))
}

/// Write `record` as the record of `new`, a branch that now exists,
/// reporting a record it replaced because its branch existed nowhere.
pub(crate) fn write_created_record(
    vcs: &dyn Vcs,
    project: &ProjectName,
    dir: &Path,
    new: &BranchName,
    record: &Record,
) -> anyhow::Result<Option<String>> {
    let store = Store::open(vcs, dir)?;
    let replaced = store.settle().ok().and_then(|view| view.entry(new));
    store
        .write_created(|view| {
            Ok(vec![Write {
                branch: new.clone(),
                record: match record {
                    Record::Declared => Record::Declared,
                    Record::Feature { target, .. } => feature_record(view, new, target)?,
                },
            }])
        })
        .map_err(|e| write_refusal(vcs, &store, project, dir, new, e))?;
    Ok(replaced.map(|old| {
        format!(
            "replaced the record of {new}: its branch exists nowhere ({})",
            match old {
                Entry::Value(old) => old.to_string(),
                Entry::Conflict(..) => "conflict".to_owned(),
                Entry::Unreadable(why) => format!("unreadable: {why}"),
            }
        )
    }))
}

/// `rwv branch create <W> [--target <T> | --declared] [--at <rev>]`.
pub fn run_create(ctx: &WorkspaceContext, request: Create) -> anyhow::Result<()> {
    let (project, dir) = find_project(ctx)?;
    let vcs = project_vcs();
    let vcs = vcs.as_ref();
    let mut report = Report::default();
    let Some(Planned {
        new,
        record,
        start,
        head,
        detached,
    }) = judge_creation(
        ctx,
        vcs,
        &project,
        &dir,
        &request.branch,
        request.spelling.as_ref(),
        request.at.as_deref(),
        CREATE,
        false,
        &mut report,
    )?
    else {
        return report.into_result();
    };
    let new_raw = RawRefName::new(new.as_str());

    let at_head = start == head;
    ProjectMove {
        project: &project,
        dir: &dir,
        detached,
        verb: CREATE,
        refused_files: if at_head {
            &[]
        } else {
            &[crate::manifest::LockFile::FILE_NAME]
        },
        destination: (!at_head).then_some(&start),
    }
    .judge(vcs, &mut report)?;
    if let Some(record) = &record {
        judge_members_at(
            ctx,
            &project,
            vcs,
            &dir,
            &start,
            Some((&new, record)),
            &mut report,
        )?;
    }
    let mut steps = Vec::new();
    if let Some(record) = &record {
        let (manifest, lock) = project_files(vcs, &dir, &start)?;
        if let Ok(manifest) = manifest {
            steps = crate::member_branch::judge_entries(
                ctx,
                CREATE,
                (manifest, lock),
                (&new, record),
                &request.entries,
                true,
                &mut report,
            )?;
        }
    }
    let Some(record) = record else {
        return report.into_result();
    };
    report.into_result()?;

    let departure = Departure::of_move(vcs, &dir, &start)?;
    vcs.create_branch_and_attach(&dir, &new_raw, &start, &departure)
        .map_err(|e| refused_move(vcs, &project, &dir, e))?;
    crate::alignment::forget_branch(vcs, &dir, new.as_str())?;
    let replaced = write_created_record(vcs, &project, &dir, &new, &record)?;
    if let Lookup::Value(written) = Store::open(vcs, &dir)?.view()?.lookup(&new) {
        crate::alignment::store_record(vcs, &dir, &new, &written)?;
    }
    if let Some(line) = replaced {
        outln!("{line}");
    }
    outln!(
        "created {new} at {}: {record}",
        short_revision(start.id().as_str())
    );
    if let Record::Feature { target, as_mode } = &record {
        for line in target_recorded_lines(vcs, &dir, &new, target, *as_mode)? {
            outln!("{line}");
        }
    }
    after_project_move(ctx, &dir, CREATE)?;
    crate::member_branch::apply_entries(
        ctx,
        CREATE,
        &steps,
        crate::activate::ActivateOptions::default(),
    )
}

fn diverged_start(
    vcs: &dyn Vcs,
    project: &ProjectName,
    dir: &Path,
    target: &RawRefName,
    verb: &'static str,
    report: &mut Report,
) -> anyhow::Result<()> {
    let home = vcs.project_url_remote(dir)?;
    let crate::push_branch::NewerOfBranchAndUpstream::Diverged { upstream, .. } =
        crate::push_branch::newer_of_branch_and_upstream(vcs, dir, target, home.as_ref())?
    else {
        return Ok(());
    };
    let unpushed = vcs.unpushed_commits(dir, target)?;
    report.add(
        State::PushBranchDiverged
            .of(label(project))
            .project_repo()
            .subject(format!("{target} and {upstream}"))
            .listing(
                unpushed
                    .iter()
                    .map(|c| format!("{} {}", short_revision(c.id.as_str()), c.subject)),
            )
            .checkout(dir)
            .bind("<T>", target)
            .by_verb(verb)
            .only(&[ChoiceId::C34])
            .judge(vcs),
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn report_names(
    ctx: &WorkspaceContext,
    vcs: &dyn Vcs,
    project: &ProjectName,
    dir: &Path,
    new: &BranchName,
    record: Option<&Record>,
    start: &ResolvedRevisionId,
    verb: &'static str,
    from_commits: bool,
    report: &mut Report,
) -> anyhow::Result<()> {
    let (manifest, _) = if from_commits {
        committed_files(vcs, dir, start)?
    } else {
        project_files(vcs, dir, start)?
    };
    let manifest = manifest?;
    let mut repos = vec![Moving::present(
        label(project),
        dir.to_path_buf(),
        project_vcs(),
        None,
    )];
    for (path, entry) in manifest.iter_entries() {
        let member_dir = ctx.active_path().join(path.as_path());
        let member_vcs = vcs_for(entry.vcs_type);
        if !member_vcs.is_repo(&member_dir) {
            continue;
        }
        repos.push(Moving::present(
            path.to_string(),
            member_dir,
            member_vcs,
            Some(path.clone()),
        ));
    }
    if matches!(record, Some(Record::Feature { .. })) {
        let scan = NameScan::of(ctx.primary_path(), project)?;
        for judged in judge_feature_names(ctx.active_path(), &scan, new, &manifest, Some(verb))? {
            report.add(judged);
        }
    }
    for repo in &repos {
        judge_name_taken(repo, new, false, verb, report)?;
    }
    Ok(())
}

/// Which checkouts of the branch an opening refuses.
pub(crate) enum Checkouts<'a> {
    /// The branch is where the workspace already is.
    Unchecked,
    /// Any checkout but this one.
    Except(&'a Path),
    /// Any checkout: the opening makes a new workspace.
    All,
}

/// What opening a branch found: where it is and the record it is opened by.
pub(crate) struct Opening {
    pub branch: BranchName,
    pub raw: RawRefName,
    pub record: Option<Record>,
    pub local: Option<ResolvedRevisionId>,
    pub copy: Option<RemoteTip>,
    pub destination: ResolvedRevisionId,
}

/// The rules `switch` and `workweave create --branch` share for opening a
/// branch: it must exist here or on a remote, it must have a record, which
/// opens it, and a branch checked out elsewhere is refused. Findings go into
/// `report`; nothing is written.
#[allow(clippy::too_many_arguments)]
pub(crate) fn judge_opening(
    ctx: &WorkspaceContext,
    vcs: &dyn Vcs,
    project: &ProjectName,
    dir: &Path,
    text: &str,
    verb: &'static str,
    checkouts: Checkouts<'_>,
    report: &mut Report,
) -> anyhow::Result<Opening> {
    let parse = |text: &str| named(vcs, project, dir, text, verb);
    let branch = parse(text)?;
    let raw = RawRefName::new(branch.as_str());

    let local = vcs.resolve_local_branch_tip(dir, &raw)?;
    let copy = if local.is_none() {
        remote_copy(vcs, dir, &raw)?
    } else {
        None
    };
    let destination = local
        .clone()
        .or_else(|| copy.as_ref().map(|c| c.tip.clone()));
    let Some(destination) = destination else {
        return Err(no_branch_here(ctx, project, vcs, dir, &branch, verb));
    };

    let store = Store::open(vcs, dir)?;
    let view = store
        .settle()
        .map_err(|e| write_refusal(vcs, &store, project, dir, &branch, e))?;
    let record = match view.lookup(&branch) {
        Lookup::Value(record) => Some(record),
        lookup => {
            report.add(judge_lookup(vcs, &store, project, dir, &branch, lookup)?);
            None
        }
    };

    if let Some(Record::Feature { .. }) = &record {
        if let Ok(manifest) = committed_files(vcs, dir, &destination)?.0 {
            let scan = NameScan::of(ctx.primary_path(), project)?;
            for judged in
                judge_feature_names(ctx.active_path(), &scan, &branch, &manifest, Some(verb))?
            {
                report.add(judged);
            }
        }
    }

    if !matches!(checkouts, Checkouts::Unchecked) {
        let recorded = crate::sync::recorded_workweaves(ctx.primary_path())?;
        let here = match checkouts {
            Checkouts::Except(here) => Some(crate::path_spelling::CanonicalPath::of(here)),
            _ => None,
        };
        for checkout in vcs.checkouts_of_branch(dir, &raw)? {
            if Some(crate::path_spelling::CanonicalPath::of(&checkout)) != here {
                report.add(crate::sync::judge_checkout(
                    vcs,
                    label(project),
                    &checkout,
                    dir,
                    &recorded,
                    Some((verb, branch.as_str())),
                ));
            }
        }
    }
    Ok(Opening {
        branch,
        raw,
        record,
        local,
        copy,
        destination,
    })
}

/// Give the project repo a local branch from the remote copy a branch only
/// a remote had.
pub(crate) fn localize_opened(vcs: &dyn Vcs, dir: &Path, opening: &Opening) -> anyhow::Result<()> {
    if opening.local.is_some() {
        return Ok(());
    }
    let copy = opening
        .copy
        .clone()
        .expect("a branch found nowhere refused before");
    vcs.create_branch(
        dir,
        &opening.raw,
        &copy.tip,
        Some(&Upstream {
            remote: copy.remote,
            branch: opening.raw.clone(),
        }),
    )?;
    Ok(())
}

/// `rwv switch <W> [--target <T> | --declared]` and `rwv switch --detach
/// <rev>`.
pub fn run_switch(ctx: &WorkspaceContext, request: Switch) -> anyhow::Result<()> {
    let (project, dir) = find_project(ctx)?;
    let vcs = project_vcs();
    let vcs = vcs.as_ref();
    let text = match &request {
        Switch::Detach(rev) => return switch_detached(ctx, vcs, &project, &dir, rev),
        Switch::Branch(branch) => branch,
    };
    let current = checked_out(vcs, &dir)?;
    let detached = current.is_none();
    let mut report = Report::default();
    let already = current.as_deref() == Some(text.as_str());
    let opening = judge_opening(
        ctx,
        vcs,
        &project,
        &dir,
        text,
        SWITCH,
        if already {
            Checkouts::Unchecked
        } else {
            Checkouts::Except(&dir)
        },
        &mut report,
    )?;
    let Opening {
        branch,
        raw,
        record,
        destination,
        ..
    } = &opening;
    let (branch, raw, destination) = (branch.clone(), raw.clone(), destination.clone());
    let record = record.clone();

    ProjectMove {
        project: &project,
        dir: &dir,
        detached,
        verb: SWITCH,
        refused_files: &[Manifest::FILE_NAME, LockFile::FILE_NAME],
        destination: Some(&destination),
    }
    .judge(vcs, &mut report)?;
    if let Some(record) = &record {
        judge_members_at(
            ctx,
            &project,
            vcs,
            &dir,
            &destination,
            Some((&branch, record)),
            &mut report,
        )?;
    }
    report.into_result()?;

    if current.as_deref() != Some(branch.as_str()) {
        let departure = Departure::of_move(vcs, &dir, &destination)?;
        localize_opened(vcs, &dir, &opening)?;
        vcs.reattach_head(
            vcs.head_attachment(&dir)?,
            &LocalRefName::of_branch(&raw),
            &departure,
        )
        .map_err(|e| refused_move(vcs, &project, &dir, e))?;
    }
    outln!("switched to {branch}");
    after_project_move(ctx, &dir, SWITCH)
}

fn no_branch_here(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    vcs: &dyn Vcs,
    dir: &Path,
    branch: &BranchName,
    verb: &'static str,
) -> anyhow::Error {
    let raw = RawRefName::new(branch.as_str());
    let mut report = Report::default();
    let manifest = Manifest::from_path(&dir.join(Manifest::FILE_NAME)).ok();
    type Holder = (String, PathBuf, Box<dyn Vcs>, Option<RemoteTip>);
    let mut members: Vec<Holder> = Vec::new();
    for (path, entry) in manifest.iter().flat_map(|m| m.iter_entries()) {
        let member_dir = ctx.active_path().join(path.as_path());
        let member_vcs = vcs_for(entry.vcs_type);
        if !member_vcs.is_repo(&member_dir) {
            continue;
        }
        let local = member_vcs
            .resolve_local_branch_tip(&member_dir, &raw)
            .ok()
            .flatten()
            .is_some();
        let copy = member_vcs
            .remote_copies(&member_dir, &raw)
            .ok()
            .and_then(|copies| copies.into_iter().next());
        if local || copy.is_some() {
            members.push((path.to_string(), member_dir, member_vcs, copy));
        }
    }
    if members.is_empty() {
        report.add(no_such_branch(vcs, project, dir, branch, verb));
    }
    for (path, member_dir, member_vcs, copy) in members {
        let unpushed = match member_vcs.observe_head(&member_dir) {
            Ok(crate::vcs::HeadObservation::Detached { at }) => member_vcs
                .stray_commits(&member_dir, &at, None, None)
                .map(|commits| !commits.is_empty())
                .unwrap_or(true),
            _ => false,
        };
        let choice = if unpushed {
            ChoiceId::C14
        } else {
            ChoiceId::C24
        };
        let mut judging = State::NameNoSuchBranch
            .of(&path)
            .member()
            .subject(format!("{branch} is a branch of {path} only"))
            .checkout(&member_dir)
            .bind("<b>", branch)
            .by_verb(verb)
            .only(&[choice]);
        if let Some(copy) = copy {
            judging = judging.bind("<r>", copy.remote);
        }
        report.add(judging.judge(member_vcs.as_ref()));
    }
    report
        .into_result()
        .expect_err("a report holding a line refuses")
}

fn switch_detached(
    ctx: &WorkspaceContext,
    vcs: &dyn Vcs,
    project: &ProjectName,
    dir: &Path,
    rev: &str,
) -> anyhow::Result<()> {
    let mut report = Report::default();
    let detached = checked_out(vcs, dir)?.is_none();
    let destination = match vcs.resolve_revision(dir, rev) {
        Ok(destination) => destination,
        Err(VcsError::RevisionNotFound { .. }) => {
            return Err(no_such_branch(vcs, project, dir, rev, SWITCH).refused())
        }
        Err(e) => return Err(e.into()),
    };
    ProjectMove {
        project,
        dir,
        detached,
        verb: SWITCH,
        refused_files: &[Manifest::FILE_NAME, LockFile::FILE_NAME],
        destination: Some(&destination),
    }
    .judge(vcs, &mut report)?;
    judge_members_at(ctx, project, vcs, dir, &destination, None, &mut report)?;
    report.into_result()?;

    let departure = Departure::of_move(vcs, dir, &destination)?;
    match vcs.head_attachment(dir)? {
        HeadAttachment::Attached(on) => vcs.detach_head(&on, &destination, &departure),
        HeadAttachment::Detached(was) => vcs.advance_detached_head(&was, &destination),
        HeadAttachment::Unborn(_) => Err(VcsError::CommandFailed {
            args: vec!["switch".to_owned(), "--detach".to_owned()],
            repo: dir.to_path_buf(),
            stderr: "the project repo has no commit to leave".to_owned(),
        }),
    }
    .map_err(|e| refused_move(vcs, project, dir, e))?;
    let manifest = Manifest::from_path(&dir.join(Manifest::FILE_NAME));
    let aligner = Aligner::open(ctx.active_path(), project, Accounting::default())?;
    if let Some(why) = &aligner.corrupt {
        eprintln!(
            "rwv {SWITCH}: the workspace state of {} reads as absent: {why}",
            crate::workspace::project_rel_path(project)
        );
    }
    let mut state = aligner.state.clone();
    state.aligned = Some(Aligned::Detached);
    crate::alignment::store_state(vcs, dir, &state)?;
    outln!(
        "detached at {}: this workspace is detached; every repo is at its pin",
        short_revision(destination.id().as_str())
    );
    manifest?;
    crate::update::align_after_switch(ctx, SWITCH)
}
