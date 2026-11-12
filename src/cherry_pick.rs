//! `rwv cherry-pick <rev>`: port one project commit onto the checked-out
//! feature.
//!
//! The commit's lock is read against its parent's. Each `owned` or `fork`
//! member whose pin it moves forward gains copies of the commits of that
//! range its branch lacks; each `dependency` or `reference` member's new pin
//! is carried by name; then one project commit mirrors the commit, its
//! project files and the relock. Member entries and the top-level `history`
//! are never ported, only reported. Every refusal is judged before anything
//! moves, and the copies run under the op record.

use crate::alignment::{Accounting, Aligner, Member};
use crate::diagnosis::{ChoiceId, Form, Judged, Report, State};
use crate::entry_report::fields_between;
use crate::manifest::{
    LockFile, Manifest, ProjectName, RepoEntry, RepoKey, RepoOperand, RepoPath, Role,
};
use crate::merge_upstream::Op;
use crate::op_state::{MemberPick, OpVerb, PickRecord, SavepointKey, TakeInRecord};
use crate::parallel::OutputMode;
use crate::records::Record;
use crate::refusal::RefusalKind;
use crate::vcs::{
    project_vcs, vcs_for, Annotation, CommitId, Copied, HeadAttachment, ResolvedRevisionId, Vcs,
};
use crate::workspace::{project_dir, project_rel_path, WorkspaceContext};
use anyhow::Context;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const VERB: &str = "cherry-pick";

/// The annotation the project commit records the commit it mirrors under.
pub const PICKED_FROM: &str = "Rwv-Picked-From";

pub const CHERRY_PICK_JSON_SCHEMA_URL: &str = crate::schema_url::schema_url!("cherry-pick");

/// What `rwv cherry-pick` was asked to port.
#[derive(Debug, Clone)]
pub struct Args {
    pub rev: String,
    pub mainline: Option<usize>,
    pub except: Vec<RepoOperand>,
}

pub enum Run {
    Fresh(Args),
    Continue,
}

/// Top-level envelope for `rwv cherry-pick --json`.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct CherryPickJsonOutput {
    #[serde(rename = "$schema")]
    pub schema: String,
    /// The project commit ported, once it resolved.
    pub rev: Option<String>,
    /// Each `owned` or `fork` member the port copies onto.
    pub members: Vec<MemberPickOutput>,
    /// Each `dependency` or `reference` member whose pin the port carries.
    pub carried: Vec<CarriedOutput>,
    /// Each member `--except` left out, with the tip recorded for it.
    pub excepted: Vec<ExceptedOutput>,
    /// Each change to a member entry or the top-level `history` the commit
    /// made that the port leaves to its setter.
    pub not_ported: Vec<NotPortedOutput>,
    /// The project commit the port made; absent where it made none.
    pub commit: Option<String>,
    /// True iff the port ran to its end and cleared its op-state.
    pub complete: bool,
    /// Each repo the run judged when it refused, with that state's choices.
    pub refused: Vec<crate::diagnosis::JudgedOutput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MemberPickOutput {
    pub path: String,
    /// Each commit copied, with the commit that copies it.
    pub copied: Vec<CopiedOutput>,
    /// The commits whose copy came out empty, their change already here.
    pub dropped: Vec<String>,
    /// The commits of the range this branch already holds, not copied.
    pub left_out: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CopiedOutput {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CarriedOutput {
    pub path: String,
    /// The pin this branch held; absent where it held none.
    pub from: Option<String>,
    pub to: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExceptedOutput {
    pub path: String,
    pub tip: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct NotPortedOutput {
    /// The member's path, or `projects/<p>` for the top-level `history`.
    pub entry: String,
    /// The field, or `added` / `removed` for the member itself.
    pub field: String,
    pub parent: Option<String>,
    pub rev: Option<String>,
    pub here: Option<String>,
}

pub fn run_cherry_pick(ctx: &WorkspaceContext, run: Run, output: OutputMode) -> anyhow::Result<()> {
    let mut reported = CherryPickJsonOutput {
        schema: CHERRY_PICK_JSON_SCHEMA_URL.to_owned(),
        ..CherryPickJsonOutput::default()
    };
    let result = pick(ctx, run, output, &mut reported);
    if output.is_json() {
        reported.complete = result.is_ok();
        reported.refused = result
            .as_ref()
            .err()
            .map(crate::diagnosis::refused_output)
            .unwrap_or_default();
        outln!(
            "{}",
            serde_json::to_string_pretty(&reported)
                .context("failed to serialize cherry-pick output")?
        );
    }
    result
}

fn say(output: OutputMode, line: &str) {
    if output == OutputMode::Text {
        eprintln!("rwv {VERB}: {line}");
    }
}

fn rev(commit: &CommitId) -> ResolvedRevisionId {
    ResolvedRevisionId::from(commit.clone())
}

fn manifest_at(vcs: &dyn Vcs, dir: &Path, commit: &CommitId) -> Option<Manifest> {
    let text = vcs
        .read_file_at_revision(dir, &rev(commit), Path::new(Manifest::FILE_NAME))
        .ok()?;
    Manifest::from_other_commit(&text).ok()
}

fn lock_at(vcs: &dyn Vcs, dir: &Path, commit: &CommitId) -> Option<LockFile> {
    let text = vcs
        .read_file_at_revision(dir, &rev(commit), Path::new(LockFile::FILE_NAME))
        .ok()?;
    LockFile::from_json_str(&text).ok()
}

fn pins_of(lock: Option<&LockFile>) -> BTreeMap<RepoPath, CommitId> {
    lock.into_iter()
        .flat_map(LockFile::iter_entries)
        .filter_map(|(path, entry)| Some((path.clone(), entry.version.as_commit_id()?)))
        .collect()
}

fn listed(vcs: &dyn Vcs, dir: &Path, commits: &[CommitId]) -> Vec<String> {
    let subjects: BTreeMap<CommitId, String> = commits
        .iter()
        .filter_map(|c| {
            let parents = vcs.parents(dir, c).ok()?;
            let summary = vcs
                .commits_excluding(dir, &rev(c), &parents, false)
                .ok()?
                .into_iter()
                .find(|s| &s.id == c)
                .map(|s| s.subject);
            Some((c.clone(), summary?))
        })
        .collect();
    commits
        .iter()
        .map(|c| match subjects.get(c) {
            Some(subject) => format!("{} {subject}", c.short()),
            None => c.short().to_owned(),
        })
        .collect()
}

/// The workspace's project and the branch it has checked out.
struct Here<'a> {
    ctx: &'a WorkspaceContext,
    project: ProjectName,
    dir: PathBuf,
    vcs: Box<dyn Vcs>,
}

impl Here<'_> {
    fn label(&self) -> String {
        project_rel_path(&self.project)
    }

    fn member_dir(&self, path: &RepoPath) -> PathBuf {
        self.ctx.active_path().join(path.as_path())
    }
}

fn pick(
    ctx: &WorkspaceContext,
    run: Run,
    output: OutputMode,
    reported: &mut CherryPickJsonOutput,
) -> anyhow::Result<()> {
    let project = ctx.require_bound_project()?.clone();
    let dir = project_dir(ctx.active_path(), &project);
    let vcs = project_vcs();
    crate::workweave::refuse_symlinked_members(ctx.active_path(), &project)?;
    let here = Here {
        ctx,
        project,
        dir,
        vcs,
    };
    match run {
        Run::Fresh(args) => {
            crate::op_state::check_no_op_in_progress(&[ctx.active_path()])?;
            fresh(&here, args, output, reported)
        }
        Run::Continue => {
            let (op, recorded, take_in) = Op::resume(ctx, OpVerb::CherryPick)?;
            if recorded != here.project {
                return Err(crate::untokened!(
                    Owed,
                    "the op was started for project `{recorded}`, not `{}`",
                    here.project
                ));
            }
            let record = take_in
                .pick
                .clone()
                .context("the op record holds no port to resume")?;
            reported.rev = Some(record.rev.to_string());
            act(&here, &op, record, output, reported)
        }
    }
}

/// One change `<rev>` made to an entry or the top-level `history`, left to
/// its setter.
struct NotPorted {
    repo: String,
    field: String,
    parent: Option<String>,
    rev: Option<String>,
    here: Option<String>,
    form: Option<(Form, &'static str)>,
}

impl NotPorted {
    fn output(&self) -> NotPortedOutput {
        NotPortedOutput {
            entry: self.repo.clone(),
            field: self.field.clone(),
            parent: self.parent.clone(),
            rev: self.rev.clone(),
            here: self.here.clone(),
        }
    }

    fn render(&self, vcs: &dyn Vcs) -> String {
        let shown = |v: &Option<String>| v.clone().unwrap_or_else(|| "none".to_owned());
        let mut text = format!(
            "rwv {VERB}: {}: not-ported {}: {} -> {} in the commit; here {}\n",
            self.repo,
            self.field,
            shown(&self.parent),
            shown(&self.rev),
            shown(&self.here),
        );
        if let Some((form, key)) = self.form {
            let mut bindings = BTreeMap::new();
            if self.repo.starts_with("projects/") {
                bindings.insert("<repo>", self.repo.clone());
            } else {
                bindings.insert("<m>", self.repo.clone());
            }
            if let Some(value) = &self.rev {
                bindings.insert(key, value.clone());
            }
            text.push_str(&crate::diagnosis::choices_text(vcs, &[form], &bindings));
        }
        text
    }
}

fn setter_of(field: &str, unset: bool) -> (Form, &'static str) {
    match field {
        "branch" => (Form::C11SetBranch, "<b>"),
        "url" => (Form::C11SetUrl, "<url>"),
        "role" => (Form::C11SetRole, "<role>"),
        "history" if unset => (Form::C11UnsetHistory, "<h>"),
        "history" => (Form::C11SetHistory, "<h>"),
        _ => (Form::C11SetRecurseSubmodules, "<bool>"),
    }
}

/// Each change `<rev>` made to a member entry or the top-level `history`,
/// with the value this branch holds.
fn not_ported(
    label: &str,
    parent: &Manifest,
    theirs: &Manifest,
    ours: &Manifest,
) -> Vec<NotPorted> {
    let mut out = Vec::new();
    let paths: BTreeSet<&RepoPath> = parent
        .iter_entries()
        .chain(theirs.iter_entries())
        .map(|(path, _)| path)
        .collect();
    for path in paths {
        let here = ours.get_entry(path);
        match (parent.get_entry(path), theirs.get_entry(path)) {
            (Some(was), Some(now)) => {
                for field in fields_between(was, now) {
                    let here_value = here.map(|h| {
                        fields_between(now, h)
                            .into_iter()
                            .find(|f| f.name == field.name)
                            .map_or_else(|| field.now.clone(), |f| f.now)
                    });
                    out.push(NotPorted {
                        repo: path.to_string(),
                        field: field.name.to_owned(),
                        parent: Some(field.was.clone()),
                        rev: Some(field.now.clone()),
                        here: here_value,
                        form: Some(setter_of(field.name, now.history.is_none())),
                    });
                }
            }
            (None, Some(now)) => out.push(NotPorted {
                repo: path.to_string(),
                field: "added".to_owned(),
                parent: None,
                rev: Some(now.url.to_string()),
                here: here.map(|h| h.url.to_string()),
                form: Some((Form::C11MemberAdd, "<url>")),
            }),
            (Some(was), None) => out.push(NotPorted {
                repo: path.to_string(),
                field: "removed".to_owned(),
                parent: Some(was.url.to_string()),
                rev: None,
                here: here.map(|h| h.url.to_string()),
                form: here.map(|_| (Form::C11MemberRemove, "<m>")),
            }),
            (None, None) => {}
        }
    }
    if parent.history != theirs.history {
        let spelled = crate::set_history::spelled;
        out.push(NotPorted {
            repo: label.to_owned(),
            field: "history".to_owned(),
            parent: Some(spelled(parent.history)),
            rev: Some(spelled(theirs.history)),
            here: Some(spelled(ours.history)),
            form: Some(if theirs.history.is_none() {
                (Form::C11UnsetTopHistory, "<h>")
            } else {
                (Form::C11SetTopHistory, "<h>")
            }),
        });
    }
    out
}

/// What the run is to do, judged whole before anything moves.
struct Plan {
    record: PickRecord,
    not_ported: Vec<NotPorted>,
    notes: Vec<String>,
    carried_from: BTreeMap<RepoKey, Option<CommitId>>,
    project_moves: bool,
}

fn fresh(
    here: &Here<'_>,
    args: Args,
    output: OutputMode,
    reported: &mut CherryPickJsonOutput,
) -> anyhow::Result<()> {
    let ctx = here.ctx;
    let vcs = here.vcs.as_ref();
    let dir = &here.dir;
    let label = here.label();
    let Some(branch) = crate::branch::checked_out(vcs, dir)? else {
        return Err(State::AlignmentProjectDetached
            .of(&label)
            .project_repo()
            .checkout(dir)
            .judge(vcs)
            .refused());
    };
    crate::branch::require_recorded(ctx)?;
    let name = crate::branch::named(vcs, &here.project, dir, &branch)?;
    let record = crate::branch::recorded_value(vcs, &here.project, dir, &name)?;
    if let Some(line) =
        crate::alignment::unseen_record_change(vcs, &here.project, dir, &name, &record, VERB)?
    {
        return Err(line.refused());
    }
    if let Record::Declared = record {
        return Err(State::TargetNoTarget
            .of(&label)
            .project_repo()
            .subject(&branch)
            .checkout(dir)
            .by_verb(VERB)
            .only_forms(&[Form::C37BranchCreate, Form::C37WorkweaveCreate])
            .judge(vcs)
            .refused());
    }
    let plan = plan(here, &args)?;
    reported.rev = Some(plan.record.rev.to_string());
    for line in &plan.notes {
        say(output, line);
    }
    let report_not_ported = |reported: &mut CherryPickJsonOutput| {
        for change in &plan.not_ported {
            if output == OutputMode::Text {
                eprint!("{}", change.render(vcs));
            }
            reported.not_ported.push(change.output());
        }
    };
    let nothing = plan.record.members.iter().all(|m| m.commits.is_empty())
        && plan.record.carried.is_empty()
        && plan.record.excepted.is_empty()
        && !plan.project_moves;
    if nothing {
        report_not_ported(reported);
        for member in &plan.record.members {
            reported.members.push(member_output(member));
        }
        say(
            output,
            &format!(
                "{label}: already holds what {} introduced; nothing to port",
                plan.record.rev.short()
            ),
        );
        reported.complete = true;
        return Ok(());
    }
    report_not_ported(reported);
    for (key, from) in &plan.carried_from {
        let (RepoKey::Member(path), Some(to)) = (key, plan.record.carried.get(key)) else {
            continue;
        };
        reported.carried.push(CarriedOutput {
            path: path.to_string(),
            from: from.as_ref().map(ToString::to_string),
            to: to.to_string(),
        });
    }
    let take_in = TakeInRecord {
        pick: Some(plan.record.clone()),
        ..TakeInRecord::default()
    };
    let repos = savepoint_repos(here);
    let op = Op::begin(
        ctx.active_path(),
        &here.project,
        OpVerb::CherryPick,
        take_in,
        &repos,
    )?;
    act(here, &op, plan.record, output, reported)
}

fn savepoint_repos(here: &Here<'_>) -> Vec<(PathBuf, Box<dyn Vcs>)> {
    let mut repos: Vec<(PathBuf, Box<dyn Vcs>)> = vec![(here.dir.clone(), project_vcs())];
    if let Ok(manifest) = Manifest::from_path(&here.dir.join(Manifest::FILE_NAME)) {
        for (path, entry) in manifest.iter_entries() {
            let member = here.member_dir(path);
            if member.exists() {
                repos.push((member, vcs_for(entry.vcs_type)));
            }
        }
    }
    repos
}

/// The parent `<rev>` is read against, judged as the VCS's own cherry-pick
/// judges `-m`.
fn judge_mainline(
    here: &Here<'_>,
    args: &Args,
    commit: &CommitId,
    parents: &[CommitId],
) -> Result<Option<CommitId>, Judged> {
    let vcs = here.vcs.as_ref();
    let listing: Vec<String> = parents
        .iter()
        .enumerate()
        .map(|(i, parent)| {
            format!(
                "{} {}",
                i + 1,
                listed(vcs, &here.dir, std::slice::from_ref(parent)).join("")
            )
        })
        .collect();
    let refuse = |subject: String, form: Form| {
        State::PortMainline
            .of(here.label())
            .project_repo()
            .subject(subject)
            .listing(listing.clone())
            .bind("<rev>", &args.rev)
            .checkout(&here.dir)
            .only_forms(&[form])
            .judge(vcs)
    };
    match (args.mainline, parents.len()) {
        (None, 0 | 1) => Ok(parents.first().cloned()),
        (None, _) => Err(refuse(
            format!(
                "{} is a merge; name the parent to pick it against",
                commit.short()
            ),
            Form::C38,
        )),
        (Some(_), 0 | 1) => Err(refuse(
            format!("{} is no merge, so `-m` names nothing", commit.short()),
            Form::C38NoMainline,
        )),
        (Some(n), count) if n == 0 || n > count => Err(refuse(
            format!("{} has no parent {n}", commit.short()),
            Form::C38,
        )),
        (Some(n), _) => Ok(Some(parents[n - 1].clone())),
    }
}

fn plan(here: &Here<'_>, args: &Args) -> anyhow::Result<Plan> {
    let ctx = here.ctx;
    let vcs = here.vcs.as_ref();
    let dir = &here.dir;
    let label = here.label();
    let mut report = Report::default();

    let Ok(resolved) = vcs.resolve_revision(dir, &args.rev) else {
        return Err(State::NameNoSuchBranch
            .of(&label)
            .project_repo()
            .subject(&args.rev)
            .checkout(dir)
            .by_verb(VERB)
            .only(&[ChoiceId::C33])
            .judge(vcs)
            .refused());
    };
    let commit = resolved.id().clone();
    let parents = vcs.parents(dir, &commit)?;
    let parent = match judge_mainline(here, args, &commit, &parents) {
        Ok(parent) => parent,
        Err(line) => return Err(line.refused()),
    };

    let empty = Manifest::from_toml_str("[repositories]\n")?;
    let theirs = manifest_at(vcs, dir, &commit).unwrap_or_else(|| empty.clone());
    let base = parent
        .as_ref()
        .and_then(|p| manifest_at(vcs, dir, p))
        .unwrap_or_else(|| empty.clone());
    let p0s = pins_of(parent.as_ref().and_then(|p| lock_at(vcs, dir, p)).as_ref());
    let p1s = pins_of(lock_at(vcs, dir, &commit).as_ref());

    let head = vcs.head_revision(dir)?.id().clone();
    let ours = Manifest::from_path(&dir.join(Manifest::FILE_NAME))?;
    let our_lock = lock_at(vcs, dir, &head);
    let our_pins = pins_of(our_lock.as_ref());

    let authored = crate::activate::owned_paths(ctx.active_path(), &here.project, &ours);
    let dirty: Vec<String> = vcs
        .tracked_dirty_file_names(dir)?
        .into_iter()
        .filter(|path| !authored.contains(path))
        .collect();
    let lock_files = [Manifest::FILE_NAME, LockFile::FILE_NAME];
    if vcs.mid_operation(dir).is_none()
        && !dirty.is_empty()
        && dirty.iter().all(|p| lock_files.contains(&p.as_str()))
    {
        report.add(
            State::EditsLockFilesUncommitted
                .of(&label)
                .project_repo()
                .listing(dirty)
                .checkout(dir)
                .judge(vcs),
        );
    } else {
        report.extend(crate::diagnosis::judge_edits(vcs, &label, dir, dirty, None));
    }

    let moved: BTreeSet<RepoPath> = p0s
        .keys()
        .chain(p1s.keys())
        .filter(|path| p0s.get(*path) != p1s.get(*path))
        .cloned()
        .collect();
    let mut except: BTreeSet<RepoPath> = BTreeSet::new();
    for named in &args.except {
        match named {
            RepoOperand::Member(path) if moved.contains(path) => {
                except.insert(path.clone());
            }
            _ => crate::refuse!(
                RefusalKind::InapplicableFlag,
                "`--except {named}` names no member {} moves; it moves {}",
                commit.short(),
                if moved.is_empty() {
                    "none".to_owned()
                } else {
                    moved
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
        }
    }

    let aligner = Aligner::open(ctx.active_path(), &here.project, Accounting::default())?;
    let live = crate::pins::live_pins(ctx.primary_path())?;
    let mut record = PickRecord {
        rev: commit.clone(),
        mainline: args.mainline,
        except: args.except.iter().map(ToString::to_string).collect(),
        members: Vec::new(),
        carried: BTreeMap::new(),
        excepted: BTreeMap::new(),
        project_started: false,
    };
    let mut notes = Vec::new();
    let mut carried_from = BTreeMap::new();

    for path in &moved {
        let (p0, p1) = (p0s.get(path), p1s.get(path));
        let entry = ours.get_entry(path);
        if except.contains(path) {
            if let Some(entry) = entry {
                judge_excepted(
                    here,
                    path,
                    entry,
                    our_pins.get(path),
                    &mut record,
                    &mut report,
                )?;
            } else {
                notes.push(format!("{path}: left out; this branch does not list it"));
            }
            continue;
        }
        let Some(entry) = entry else {
            let url = theirs
                .get_entry(path)
                .or_else(|| base.get_entry(path))
                .map(|e| e.url.to_string());
            let mut judging = State::PortNotListed
                .of(path)
                .member()
                .subject(format!("{} moves its pin", commit.short()))
                .bind("<m>", path);
            if let Some(url) = url {
                judging = judging.bind("<url>", url);
            }
            report.add(judging.judge(vcs));
            continue;
        };
        let (Some(p0), Some(p1)) = (p0, p1) else {
            notes.push(format!(
                "{path}: {} has no range for it ({}); its pin here stays",
                commit.short(),
                if p0.is_none() {
                    "the parent does not pin it"
                } else {
                    "the commit does not pin it"
                }
            ));
            continue;
        };
        let mdir = here.member_dir(path);
        let mvcs = vcs_for(entry.vcs_type);
        if !mvcs.is_repo(&mdir) {
            return Err(crate::untokened!(
                Owed,
                "{path}: no checkout in this workspace; run `rwv update` to create it, then run \
                 `rwv cherry-pick` again"
            ));
        }
        let mvcs = mvcs.as_ref();
        let unreachable: Vec<&CommitId> = [p0, p1]
            .into_iter()
            .filter(|c| mvcs.resolve_revision(&mdir, c.as_str()).is_err())
            .collect();
        if !unreachable.is_empty() {
            report.add(
                State::PinUnreachable
                    .of(path)
                    .member()
                    .checkout(&mdir)
                    .bind("<m>", path)
                    .subject(format!(
                        "{} pins {} for it, which this clone lacks",
                        commit.short(),
                        unreachable
                            .iter()
                            .map(|c| c.short().to_owned())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                    .judge(mvcs),
            );
            continue;
        }
        if !mvcs.is_ancestor(&mdir, &rev(p0), &rev(p1))? {
            let dropped = mvcs.commits_beyond(&mdir, &rev(p0), &rev(p1))?;
            let ids: Vec<CommitId> = dropped.iter().rev().map(|c| c.id.clone()).collect();
            let owned = matches!(entry.role, Role::Owned | Role::Fork);
            let mut judging = State::PortBackward
                .of(path)
                .member()
                .subject(format!(
                    "{} moves it from {} to {}",
                    commit.short(),
                    p0.short(),
                    p1.short()
                ))
                .listing(listed(mvcs, &mdir, &ids))
                .checkout(&mdir)
                .bind("<m>", path)
                .bind("<rev>", p1.short())
                .bind(
                    "<ids>",
                    ids.iter()
                        .map(|c| c.short().to_owned())
                        .collect::<Vec<_>>()
                        .join(" "),
                )
                .only_forms(&[if owned {
                    Form::C39
                } else {
                    Form::C25AtCherryPick
                }]);
            if owned {
                judging = judging.bind_place("<b-checkout>", &mdir);
            }
            report.add(judging.judge(mvcs));
            continue;
        }
        let member = Member {
            path,
            entry,
            dir: &mdir,
            vcs: mvcs,
        };
        match entry.role {
            Role::Owned | Role::Fork => {
                if let Some(pick) = plan_copies(
                    &aligner,
                    &member,
                    (p0, p1),
                    args.mainline,
                    our_pins.get(path),
                    &live,
                    &mut report,
                )? {
                    if !pick.commits.is_empty() || !pick.left_out_is_empty() {
                        record.members.push(pick.into_record());
                    }
                }
            }
            Role::Dependency | Role::Reference => {
                let here_pin = our_pins.get(path);
                if let Some(pin) = here_pin {
                    if mvcs.is_ancestor(&mdir, &rev(p1), &rev(pin))? {
                        continue;
                    }
                }
                let candidate =
                    crate::lock::with_pins(&ours, our_lock.as_ref(), [(path, &rev(p1))]);
                for line in crate::member_pin::pin_refusals(&aligner, &member, &candidate, true)? {
                    if line.state() == State::PinNotOnDeclaredBranch {
                        report.add(
                            State::PinNotOnDeclaredBranch
                                .of(path)
                                .member()
                                .subject(format!(
                                    "{} is on neither '{}' nor its upstream",
                                    p1.short(),
                                    entry.branch.local_counterpart().as_str()
                                ))
                                .checkout(&mdir)
                                .bind("<m>", path)
                                .by_verb(VERB)
                                .only_forms(&[Form::C11SetBranch, Form::C25AtCherryPick])
                                .judge(mvcs),
                        );
                    } else {
                        report.add(line);
                    }
                }
                let key = RepoKey::Member(path.clone());
                record.carried.insert(key.clone(), p1.clone());
                carried_from.insert(key, here_pin.cloned());
            }
        }
    }

    let project_moves = project_files_move(
        vcs,
        dir,
        &head,
        parent.as_ref(),
        &commit,
        &ours,
        &base,
        &theirs,
    )?;
    report.into_result()?;
    let not_ported = not_ported(&label, &base, &theirs, &ours);
    Ok(Plan {
        record,
        not_ported,
        notes,
        carried_from,
        project_moves,
    })
}

/// A member's copies, as planned.
struct PlannedCopies {
    repo: RepoKey,
    commits: Vec<CommitId>,
    mainline: Option<usize>,
    left_out: Vec<CommitId>,
}

impl PlannedCopies {
    fn left_out_is_empty(&self) -> bool {
        self.left_out.is_empty()
    }

    fn into_record(self) -> MemberPick {
        MemberPick {
            repo: self.repo,
            commits: self.commits,
            mainline: self.mainline,
            copied: Vec::new(),
            dropped: Vec::new(),
            left_out: self.left_out,
        }
    }
}

/// The copies an `owned` or `fork` member takes, `p0` → `p1` being the change
/// `<rev>` made to its pin; judged as a member the port moves.
#[allow(clippy::too_many_arguments)]
fn plan_copies(
    aligner: &Aligner,
    member: &Member<'_>,
    (p0, p1): (&CommitId, &CommitId),
    mainline: Option<usize>,
    pin_here: Option<&CommitId>,
    live: &BTreeMap<RepoPath, Vec<crate::pins::LivePin>>,
    report: &mut Report,
) -> anyhow::Result<Option<PlannedCopies>> {
    let (path, mdir, mvcs) = (member.path, member.dir, member.vcs);
    let head = mvcs.head_revision(mdir)?;
    let range = mvcs.pushed_range(mdir, &rev(p1), Some(&rev(p0)))?;
    let mut copy = Vec::new();
    let mut left_out = Vec::new();
    for one in &range {
        if mvcs.is_ancestor(mdir, &rev(&one.id), &head)? {
            left_out.push(one.id.clone());
        } else {
            copy.push(one.clone());
        }
    }
    if copy.is_empty() {
        return Ok(Some(PlannedCopies {
            repo: RepoKey::Member(path.clone()),
            commits: Vec::new(),
            mainline: None,
            left_out,
        }));
    }
    let landing_merge = mainline.and_then(|n| {
        let tip = range.iter().find(|c| &c.id == p1)?;
        (tip.parents.get(n.checked_sub(1)?) == Some(p0)).then_some(n)
    });
    let (commits, member_mainline) = match landing_merge {
        Some(n) => (vec![p1.clone()], Some(n)),
        None => {
            if copy.iter().any(|c| c.parents.len() > 1) {
                let ids: Vec<CommitId> = copy.iter().map(|c| c.id.clone()).collect();
                let merges: BTreeSet<&CommitId> = copy
                    .iter()
                    .filter(|c| c.parents.len() > 1)
                    .map(|c| &c.id)
                    .collect();
                let lines = listed(mvcs, mdir, &ids)
                    .into_iter()
                    .zip(&ids)
                    .map(|(line, id)| {
                        if merges.contains(id) {
                            format!("{line} (merge)")
                        } else {
                            line
                        }
                    });
                report.add(
                    State::PortMergeInRange
                        .of(path)
                        .member()
                        .subject(format!("{} to {}", p0.short(), p1.short()))
                        .listing(lines)
                        .checkout(mdir)
                        .bind_place("<b-checkout>", mdir)
                        .bind("<m>", path)
                        .bind("<pin>", p1.short())
                        .bind(
                            "<ids>",
                            ids.iter()
                                .map(|c| c.short().to_owned())
                                .collect::<Vec<_>>()
                                .join(" "),
                        )
                        .judge(mvcs),
                );
                return Ok(None);
            }
            (copy.iter().map(|c| c.id.clone()).collect(), None)
        }
    };

    let plan = aligner.plan(member, None)?;
    if let Some(off) = aligner.refusal(member, &plan, path)? {
        report.add(off);
        return Ok(None);
    }
    report.extend(crate::diagnosis::judge_edits(
        mvcs,
        path,
        mdir,
        mvcs.tracked_dirty_file_names(mdir)?,
        None,
    ));
    if !matches!(mvcs.head_attachment(mdir)?, HeadAttachment::Attached(_)) {
        return Err(crate::untokened!(
            Owed,
            "{path}: its checkout is on no branch here, so there is no branch to copy onto"
        ));
    }
    match pin_here {
        Some(pin) => {
            let held = live.get(path).map(Vec::as_slice).unwrap_or_default();
            let standing = crate::pins::standing(mvcs, mdir, &head, &rev(pin), held)?;
            if let Some(line) =
                crate::pins::judge(path, mdir, mvcs, &standing, (&head, &rev(pin)), None)
            {
                report.add(line);
            }
        }
        None => report.add(
            State::PinNone
                .of(path)
                .member()
                .checkout(mdir)
                .bind("<m>", path)
                .judge(mvcs),
        ),
    }
    Ok(Some(PlannedCopies {
        repo: RepoKey::Member(path.clone()),
        commits,
        mainline: member_mainline,
        left_out,
    }))
}

/// A member `--except` names: its checked-out tip is recorded as `lock`
/// records it, forward only.
fn judge_excepted(
    here: &Here<'_>,
    path: &RepoPath,
    entry: &RepoEntry,
    pin: Option<&CommitId>,
    record: &mut PickRecord,
    report: &mut Report,
) -> anyhow::Result<()> {
    let mdir = here.member_dir(path);
    let mvcs = vcs_for(entry.vcs_type);
    if !mvcs.is_repo(&mdir) {
        return Ok(());
    }
    let mvcs = mvcs.as_ref();
    report.extend(crate::diagnosis::judge_edits(
        mvcs,
        path,
        &mdir,
        mvcs.tracked_dirty_file_names(&mdir)?,
        None,
    ));
    let tip = mvcs.head_revision(&mdir)?;
    if let Some(pin) = pin {
        let pinned = rev(pin);
        let forward = mvcs.resolve_revision(&mdir, pin.as_str()).is_ok()
            && mvcs.is_ancestor(&mdir, &pinned, &tip)?;
        if !forward {
            let relation = mvcs.relation(&mdir, &tip, &pinned)?;
            let subject = crate::lock::backwards_subject(
                mvcs,
                &mdir,
                "its checkout",
                &tip,
                &pinned,
                relation,
            );
            report.add(crate::lock::backwards_line(
                path,
                &mdir,
                mvcs,
                subject,
                Some(tip.id().short()),
            ));
            return Ok(());
        }
        if pin == tip.id() {
            return Ok(());
        }
    }
    record
        .excepted
        .insert(RepoKey::Member(path.clone()), tip.id().clone());
    Ok(())
}

/// Whether porting `<rev>`'s project files changes anything here: a file
/// other than the lock files whose content here differs from `<rev>`'s, or a
/// table of `rwv.toml` other than the entries and `history` that `<rev>`
/// changed and this branch does not hold as `<rev>` has it.
#[allow(clippy::too_many_arguments)]
fn project_files_move(
    vcs: &dyn Vcs,
    dir: &Path,
    head: &CommitId,
    parent: Option<&CommitId>,
    commit: &CommitId,
    ours: &Manifest,
    base: &Manifest,
    theirs: &Manifest,
) -> anyhow::Result<bool> {
    let changed = match parent {
        Some(parent) => vcs.changed_paths_between(dir, &rev(parent), &rev(commit))?,
        None => vcs
            .files_at(dir, commit)?
            .into_iter()
            .map(|f| f.path)
            .collect(),
    };
    for path in changed {
        if path == LockFile::FILE_NAME || path == Manifest::FILE_NAME {
            continue;
        }
        let at = |c: &CommitId| {
            vcs.read_file_at_revision(dir, &rev(c), Path::new(&path))
                .ok()
        };
        if at(head) != at(commit) {
            return Ok(true);
        }
    }
    Ok(!ported_tables(base, theirs, ours).taken.is_empty())
}

/// The tables of `rwv.toml` that port as content, each as JSON.
fn tables(manifest: &Manifest) -> BTreeMap<&'static str, serde_json::Value> {
    BTreeMap::from([
        (
            "integrations",
            serde_json::to_value(&manifest.integrations).unwrap_or_default(),
        ),
        (
            "workweave",
            serde_json::to_value(&manifest.workweave).unwrap_or_default(),
        ),
    ])
}

/// The three-way merge of the tables that port as content.
struct Tables {
    taken: Vec<&'static str>,
    conflicted: Vec<&'static str>,
}

fn ported_tables(base: &Manifest, theirs: &Manifest, ours: &Manifest) -> Tables {
    let (base, theirs, ours) = (tables(base), tables(theirs), tables(ours));
    let mut out = Tables {
        taken: Vec::new(),
        conflicted: Vec::new(),
    };
    for (name, was) in &base {
        let (now, here) = (&theirs[name], &ours[name]);
        if now == was || now == here {
            continue;
        }
        if here == was {
            out.taken.push(name);
        } else {
            out.conflicted.push(name);
        }
    }
    out
}

/// `ours` with the tables `taken` names set to `theirs`'.
fn with_tables(ours: &Manifest, theirs: &Manifest, taken: &[&str]) -> Manifest {
    let mut merged = ours.clone();
    for name in taken {
        match *name {
            "integrations" => merged.integrations = theirs.integrations.clone(),
            "workweave" => merged.workweave = theirs.workweave.clone(),
            _ => {}
        }
    }
    merged
}

fn member_output(pick: &MemberPick) -> MemberPickOutput {
    MemberPickOutput {
        path: match &pick.repo {
            RepoKey::Member(path) => path.to_string(),
            RepoKey::Project => String::new(),
        },
        copied: pick
            .copied
            .iter()
            .map(|(from, to)| CopiedOutput {
                from: from.to_string(),
                to: to.to_string(),
            })
            .collect(),
        dropped: pick.dropped.iter().map(ToString::to_string).collect(),
        left_out: pick.left_out.iter().map(ToString::to_string).collect(),
    }
}

fn save(op: &Op, record: &PickRecord) -> anyhow::Result<()> {
    let mut owner = crate::op_state::read_owner(&op.workspace)?
        .context("the op record vanished while the port ran")?;
    if let Some(take_in) = owner.take_in.as_mut() {
        take_in.pick = Some(record.clone());
    }
    crate::op_state::write_owner(&op.workspace, &owner)
}

fn conflict_stop(
    repo: &str,
    checkout: &Path,
    paths: &[String],
    what: &str,
    vcs: &dyn Vcs,
) -> anyhow::Error {
    crate::refusal::refusal(
        RefusalKind::OpParked,
        format!(
            "rwv {VERB}: the copy of {what} in {repo} stopped on a conflict ({}) at {}.\n{}\nthen \
             run `{}`, or `rwv abort` to put everything back as it was",
            paths.join(", "),
            checkout.display(),
            vcs.conflict_resolution_hint(crate::vcs::ConflictOp::Rebase),
            crate::op_state::resume_command(OpVerb::CherryPick),
        ),
    )
}

/// Carry out the port, or resume it: each member's remaining copies, then the
/// project commit. Every step finds what an earlier run did and goes on.
fn act(
    here: &Here<'_>,
    op: &Op,
    mut record: PickRecord,
    output: OutputMode,
    reported: &mut CherryPickJsonOutput,
) -> anyhow::Result<()> {
    let key = SavepointKey::Op(op.id.clone());
    let pre_head = here
        .vcs
        .resolve_savepoint(&here.dir, &key)
        .context("the op's savepoint of the project repo is gone")?
        .id()
        .clone();
    let ours = manifest_at(here.vcs.as_ref(), &here.dir, &pre_head)
        .context("the project's rwv.toml did not parse when the port began")?;
    let result = copy_members(here, op, &ours, &mut record, output);
    reported.members = record.members.iter().map(member_output).collect();
    reported.excepted = record
        .excepted
        .iter()
        .filter_map(|(key, tip)| match key {
            RepoKey::Member(path) => Some(ExceptedOutput {
                path: path.to_string(),
                tip: tip.to_string(),
            }),
            RepoKey::Project => None,
        })
        .collect();
    result?;
    let made = commit_project(here, op, &ours, &pre_head, &mut record, output)?;
    reported.commit = made.as_ref().map(ToString::to_string);
    if reported.carried.is_empty() {
        let before = pins_of(lock_at(here.vcs.as_ref(), &here.dir, &pre_head).as_ref());
        for (key, to) in &record.carried {
            if let RepoKey::Member(path) = key {
                reported.carried.push(CarriedOutput {
                    path: path.to_string(),
                    from: before.get(path).map(ToString::to_string),
                    to: to.to_string(),
                });
            }
        }
    }
    let run = crate::update::align_after(here.ctx, 1, output, VERB)?;
    run.tail?;
    op.finish(&savepoint_repos(here));
    match made {
        Some(commit) => say(
            output,
            &format!(
                "{}: ported {} as {}",
                here.label(),
                record.rev.short(),
                commit.short()
            ),
        ),
        None => say(
            output,
            &format!(
                "{}: already holds what {} introduced; no commit made",
                here.label(),
                record.rev.short()
            ),
        ),
    }
    Ok(())
}

fn copy_members(
    here: &Here<'_>,
    op: &Op,
    ours: &Manifest,
    record: &mut PickRecord,
    output: OutputMode,
) -> anyhow::Result<()> {
    let key = SavepointKey::Op(op.id.clone());
    for i in 0..record.members.len() {
        let RepoKey::Member(path) = record.members[i].repo.clone() else {
            continue;
        };
        let entry = ours
            .get_entry(&path)
            .with_context(|| format!("{path} is no member of the project"))?;
        let dir = here.member_dir(&path);
        let vcs = vcs_for(entry.vcs_type);
        let vcs = vcs.as_ref();
        loop {
            let Some(next) = record.members[i].remaining().first().cloned() else {
                break;
            };
            let expected = record.members[i]
                .copied
                .last()
                .map(|(_, to)| to.clone())
                .or_else(|| vcs.resolve_savepoint(&dir, &key).map(|r| r.id().clone()));
            let head = vcs.head_revision(&dir)?.id().clone();
            let copied = if matches!(vcs.mid_op(&dir), Some(crate::vcs::ConflictOp::CherryPick)) {
                vcs.continue_copy(&dir)?
            } else if expected.as_ref().is_some_and(|e| e != &head)
                && vcs.is_ancestor(&dir, &rev(expected.as_ref().unwrap_or(&head)), &rev(&head))?
            {
                Copied::Commit(head)
            } else {
                vcs.copy_commit(&dir, &next, record.members[i].mainline)?
            };
            match copied {
                Copied::Commit(to) => {
                    crate::op_state::journal_tip(
                        &op.workspace,
                        RepoKey::Member(path.clone()),
                        &to,
                    )?;
                    say(
                        output,
                        &format!("{path}: copied {} as {}", next.short(), to.short()),
                    );
                    record.members[i].copied.push((next, to));
                    save(op, record)?;
                }
                Copied::Empty => {
                    say(
                        output,
                        &format!(
                            "{path}: dropped {}; its change is already here",
                            next.short()
                        ),
                    );
                    record.members[i].dropped.push(next);
                    save(op, record)?;
                }
                Copied::Conflicted(paths) => {
                    return Err(conflict_stop(
                        path.as_str(),
                        &dir,
                        &paths,
                        next.short(),
                        vcs,
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The project commit: `<rev>`'s project files, the ported tables of
/// `rwv.toml` and the relock, recording the commit it mirrors. `None` where
/// nothing changed, and no commit was made.
fn commit_project(
    here: &Here<'_>,
    op: &Op,
    ours: &Manifest,
    pre_head: &CommitId,
    record: &mut PickRecord,
    output: OutputMode,
) -> anyhow::Result<Option<CommitId>> {
    let vcs = here.vcs.as_ref();
    let dir = &here.dir;
    let label = here.label();
    let commit = record.rev.clone();
    let parents = vcs.parents(dir, &commit)?;
    let parent = match record.mainline {
        Some(n) => parents.get(n - 1).cloned(),
        None => parents.first().cloned(),
    };
    let empty = Manifest::from_toml_str("[repositories]\n")?;
    let theirs = manifest_at(vcs, dir, &commit).unwrap_or_else(|| empty.clone());
    let base = parent
        .as_ref()
        .and_then(|p| manifest_at(vcs, dir, p))
        .unwrap_or_else(|| empty.clone());
    let tables = ported_tables(&base, &theirs, ours);

    if !record.project_started {
        record.project_started = true;
        save(op, record)?;
        let mut conflicted = vcs.copy_uncommitted(dir, &commit, record.mainline)?;
        if tables.conflicted.is_empty() {
            conflicted.retain(|p| p != Manifest::FILE_NAME);
        }
        conflicted.retain(|p| p != LockFile::FILE_NAME);
        if !conflicted.is_empty() {
            return Err(conflict_stop(&label, dir, &conflicted, commit.short(), vcs));
        }
    } else {
        let mut conflicted = vcs.unmerged_paths(dir)?;
        conflicted.retain(|p| p != LockFile::FILE_NAME);
        if !conflicted.is_empty() {
            return Err(conflict_stop(&label, dir, &conflicted, commit.short(), vcs));
        }
    }

    let manifest_path = dir.join(Manifest::FILE_NAME);
    let settled = if tables.conflicted.is_empty() {
        with_tables(ours, &theirs, &tables.taken)
    } else {
        let mut resolved = Manifest::from_path(&manifest_path)
            .with_context(|| format!("{}: the resolved rwv.toml does not parse", label))?;
        resolved.history = ours.history;
        resolved.repositories = ours.repositories.clone();
        resolved
    };
    let unchanged = toml::to_string(&settled).ok() == toml::to_string(ours).ok();
    if unchanged {
        let text =
            vcs.read_file_at_revision(dir, &rev(pre_head), Path::new(Manifest::FILE_NAME))?;
        crate::durable_file::replace(&manifest_path, text.as_bytes())?;
    } else {
        settled.write(&manifest_path)?;
    }

    let held = lock_at(vcs, dir, pre_head);
    let mut pins: Vec<(RepoPath, ResolvedRevisionId)> = Vec::new();
    for member in &record.members {
        if let (RepoKey::Member(path), Some((_, to))) = (&member.repo, member.copied.last()) {
            pins.push((path.clone(), rev(to)));
        }
    }
    for (key, to) in record.carried.iter().chain(record.excepted.iter()) {
        if let RepoKey::Member(path) = key {
            pins.push((path.clone(), rev(to)));
        }
    }
    let lock = crate::lock::with_pins(ours, held.as_ref(), pins.iter().map(|(p, c)| (p, c)));
    crate::lock::write_lock_in(&lock, dir)?;
    vcs.stage_paths(dir, &[Manifest::FILE_NAME, LockFile::FILE_NAME])?;

    let mut paths: Vec<String> = vcs.staged_paths(dir)?;
    for file in [Manifest::FILE_NAME, LockFile::FILE_NAME] {
        if !paths.iter().any(|p| p == file) {
            paths.push(file.to_owned());
        }
    }
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
    let message = vcs.message_without(dir, &commit, &[crate::repin::KEY, PICKED_FROM])?;
    let naming = crate::repin::Naming::by_name(
        VERB,
        record.carried.keys().filter_map(|key| match key {
            RepoKey::Member(path) => Some(path.clone()),
            RepoKey::Project => None,
        }),
    );
    let made = crate::repin::commit_annotated(
        vcs,
        dir,
        &paths,
        &message,
        Some(&lock),
        &naming,
        vec![Annotation {
            key: PICKED_FROM,
            value: commit.to_string(),
        }],
    )?;
    if !made {
        vcs.cancel_in_flight_op(dir);
        return Ok(None);
    }
    let tip = vcs.head_revision(dir)?.id().clone();
    crate::op_state::journal_tip(&op.workspace, RepoKey::Project, &tip)?;
    let before = pins_of(held.as_ref());
    for (key, to) in &record.carried {
        let RepoKey::Member(path) = key else {
            continue;
        };
        let Some(entry) = ours.get_entry(path) else {
            continue;
        };
        let mvcs = vcs_for(entry.vcs_type);
        let mdir = here.member_dir(path);
        match before.get(path) {
            Some(was) => crate::lock::report_backward_repin(
                mvcs.as_ref(),
                &mdir,
                VERB,
                path,
                &rev(was),
                &rev(to),
            )?,
            None => say(
                output,
                &format!("{path}: pinned at {}; it had no pin", to.short()),
            ),
        }
    }
    Ok(Some(tip))
}
