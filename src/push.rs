//! `rwv push` — publish a weave branch: its members' branches, then its
//! record, then the project branch.
//!
//! The project branch carries the lock that pins each member's commit, so it
//! is published last: collaborators who clone or update from it must never
//! find a lock naming a commit no remote holds. The record travels with the
//! branch it describes, ahead of it, so a branch rwv publishes is published
//! with its record.

use crate::alignment::{Accounting, Aligner, Member as AlignedMember, Pin, Target};
use crate::branch_ref::BranchRef;
use crate::cli::consent::OverwriteConsent;
use crate::diagnosis::{ChoiceId, Fact, FactKind, Report, State};
use crate::manifest::{
    LockFile, Manifest, Project, ProjectName, RepoEntry, RepoOperand, RepoPath, Role, VcsType,
};
use crate::parallel::{run_in_parallel, OutputMode, Reporter};
use crate::push_branch::{NewerOfBranchAndUpstream, Standing};
use crate::records::{BranchName, Published, Record, Store};
use crate::refusal::RefusalKind;
use crate::selector::RepoFilter;
use crate::vcs::{
    project_vcs, vcs_for, BackupSource, HeadAttachment, LocalRefName, PublishRef, RawRefName,
    RemoteHandle, Replace, ResolvedRevisionId, Vcs,
};
use crate::workspace::{project_dir, project_rel_path, WorkspaceContext};
use anyhow::Context;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Schema URL for `rwv push --json` output. Pins to the committed artifact
/// under `docs/reference/schemas/push.json`. Emitted as the top-level
/// `$schema` field of the [`PushJsonOutput`] envelope.
pub const PUSH_SCHEMA_URL: &str = crate::schema_url::schema_url!("push");

/// One per-repo outcome record in `rwv push --json` output.
///
/// Member-repo records use `kind` values `pushed`, `skipped`, and `failed`.
/// The project-repo record uses `kind` values `project-repo-pushed` and
/// `project-repo-failed`, and the record of the branch `records-published` and
/// `records-rejected`, making each distinguishable from member-repo records in
/// the same flat `outcomes` array.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PushOutcomeOutput {
    /// Member repo was pushed successfully.
    Pushed {
        path: String,
        absolute_path: crate::path_spelling::WirePath,
    },
    /// Member repo was skipped (e.g. by a selector filter).
    Skipped {
        path: String,
        absolute_path: crate::path_spelling::WirePath,
    },
    /// Member repo push failed.
    Failed {
        path: String,
        absolute_path: crate::path_spelling::WirePath,
        /// Free-form error message from the git push attempt.
        message: String,
    },
    /// The branch's record, and the drops this machine had made, reached the
    /// remote the project branch publishes to.
    RecordsPublished {
        path: String,
        absolute_path: crate::path_spelling::WirePath,
        project: ProjectName,
        /// The entries the publish removed from the remote's records.
        dropped: Vec<String>,
    },
    /// The remote refused its records branch. For the remote's default branch
    /// recorded as `declared` the project branch was published all the same.
    RecordsRejected {
        path: String,
        absolute_path: crate::path_spelling::WirePath,
        project: ProjectName,
        /// What the remote answered.
        message: String,
        /// Whether the project branch was withheld because of it.
        withheld: bool,
    },
    /// Project repo was pushed successfully (always the last record).
    ProjectRepoPushed {
        path: String,
        absolute_path: crate::path_spelling::WirePath,
        /// The project name (e.g. `"my-app"`). Distinguishes the project repo's
        /// path convention (`projects/<name>/`) from member-repo paths.
        project: ProjectName,
    },
    /// Project repo push failed.
    ProjectRepoFailed {
        path: String,
        absolute_path: crate::path_spelling::WirePath,
        project: ProjectName,
        /// Free-form error message from the git push attempt.
        message: String,
    },
}

impl PushOutcomeOutput {
    /// True when this record represents a failure (either manifest or project-repo).
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. } | Self::ProjectRepoFailed { .. })
    }
}

/// Top-level envelope for `rwv push --json`.
///
/// Shape: `{ "$schema": "<url>", "outcomes": [<PushOutcomeOutput>, ...] }`.
/// Member-repo records appear first, sorted by repo path; the branch's record
/// and the project-repo record follow, in that order. Consumers can
/// distinguish the project-repo record by checking `kind` for
/// `"project-repo-pushed"` or `"project-repo-failed"`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PushJsonOutput {
    #[serde(rename = "$schema")]
    pub schema_url: String,
    pub outcomes: Vec<PushOutcomeOutput>,
    /// Each repo push judged before refusing, with that state's choices.
    /// Empty, not absent, when nothing was refused on a repo's state; a
    /// project repo withheld because members' pushes failed lists each of
    /// those members as `publish` `withheld`.
    pub refused: Vec<crate::diagnosis::JudgedOutput>,
    /// Each repo push judged and went past without refusing it: a branch
    /// behind its push branch, one whose upstream is gone, a merge whose
    /// unmerged pin a later lock waived, with that state's choices.
    pub reported: Vec<crate::diagnosis::JudgedOutput>,
    /// Resolved workspace coordinates (weave root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<crate::workspace::Resolution>,
}

/// What `rwv push` does once its preconditions pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushMode {
    /// Print the plan and push nothing.
    DryRun,
    Publish,
}

/// What an invocation of `rwv push` asked for.
pub struct PushRequest<'a> {
    pub mode: PushMode,
    /// The `--role` and `--repo` selectors; none pushes `owned` and `fork`
    /// members, the record and the project branch.
    pub filter: &'a RepoFilter,
    /// The repos `--overwrite` names.
    pub overwrite: &'a OverwriteConsent,
    /// `--branch`: push this weave branch instead of the checked-out one.
    pub branch: Option<&'a str>,
    /// Workers for the member fetches and pushes; the project repo always
    /// goes last, alone.
    pub jobs: usize,
    pub output: OutputMode,
}

/// The weave branch being pushed, and what the push reads of it.
struct Pushed {
    project: ProjectName,
    project_dir: PathBuf,
    workweave_dir: Option<PathBuf>,
    branch: BranchName,
    raw: RawRefName,
    /// The commit the push judges and publishes.
    tip: ResolvedRevisionId,
    /// The project repo has it checked out here.
    checked_out: bool,
    record: Record,
    manifest: Manifest,
    lock: Option<LockFile>,
}

impl Pushed {
    fn is_feature(&self) -> bool {
        matches!(self.record, Record::Feature { .. })
    }

    /// The branch a member of `role` is expected on for this weave branch.
    fn expected(&self, role: Role, entry: &RepoEntry) -> RawRefName {
        if self.is_feature() && matches!(role, Role::Owned | Role::Fork) {
            self.raw.clone()
        } else {
            RawRefName::new(entry.branch.local_counterpart().as_str())
        }
    }
}

struct Member {
    path: RepoPath,
    entry: RepoEntry,
    dir: PathBuf,
    vcs: Box<dyn Vcs>,
    /// The push acts on this member: it selects it by default, by role or by
    /// name.
    acting: bool,
    url: Option<crate::registry::CloneUrl>,
}

/// A member branch to publish.
struct PushPlanItem {
    repo_path: RepoPath,
    branch: String,
    vcs_type: VcsType,
    publish_ref: PublishRef,
    /// Where the branch publishes: its push target.
    target: RemoteHandle,
    replace: Replace,
}

/// Outcome of pushing a single manifest repo.
enum PushOutcome {
    Pushed,
    Failed(String),
}

fn project_label(project: &ProjectName) -> String {
    project_rel_path(project).to_string()
}

fn print_envelope(
    outcomes: Vec<PushOutcomeOutput>,
    refusal: &anyhow::Error,
    reported: &Report,
    ctx: &WorkspaceContext,
) {
    let envelope = PushJsonOutput {
        schema_url: PUSH_SCHEMA_URL.to_string(),
        outcomes,
        refused: crate::diagnosis::refused_output(refusal),
        reported: reported.output(),
        resolution: Some(ctx.resolution()),
    };
    if let Ok(out) = serde_json::to_string_pretty(&envelope) {
        outln!("{out}");
    }
}

/// Refuse in the shape the output mode asks for, then hand the refusal back.
fn refuse_json(
    request: &PushRequest<'_>,
    ctx: &WorkspaceContext,
    outcomes: Vec<PushOutcomeOutput>,
    reported: &Report,
    refusal: anyhow::Error,
) -> anyhow::Error {
    if request.output.is_json() {
        print_envelope(outcomes, &refusal, reported, ctx);
    }
    refusal
}

/// Run `rwv push` for the current workspace context.
///
/// Pushes the weave branch checked out here (or `--branch`'s) from any
/// workspace, on a branch that has a record and a name rwv publishes. With no
/// selector it pushes the `owned` and `fork` members' branches, then the
/// branch's record and the drops this machine has made, then the project
/// branch; `--role` and `--repo` push only the repos they select, and the
/// project branch and the record only where `projects/<name>` is named.
/// A `dependency` or `reference` member is pushed only when named. Nothing
/// here writes `rwv.lock`.
///
/// Every refusal is reported before anything is published, and the project
/// branch is withheld while any pin its lock holds is not reached as the pin
/// gate requires, any member push failed, or the remote refused the record.
pub fn run_push(ctx: &WorkspaceContext, request: PushRequest<'_>) -> anyhow::Result<()> {
    let pvcs = project_vcs();
    let pushed = resolve(ctx, &request, pvcs.as_ref())?;
    let json_refusal = |reported: &Report, e: anyhow::Error| -> anyhow::Error {
        refuse_json(&request, ctx, Vec::new(), reported, e)
    };
    let nothing_reported = Report::default();

    if pushed.checked_out {
        refuse_uncommitted_project_state(pvcs.as_ref(), &pushed.project_dir, &pushed.project)
            .map_err(|e| json_refusal(&nothing_reported, e))?;
    }

    if pushed.checked_out && request.mode == PushMode::Publish {
        let project_aligner =
            Aligner::open(ctx.active_path(), &pushed.project, Accounting::default())?;
        crate::entry_report::report(
            "push",
            ctx,
            &project_aligner,
            &pushed.project_dir,
            &pushed.manifest,
            false,
        );
    }

    let members = members_of(ctx, &request, &pushed);
    let acting_roles = request.filter.is_empty();
    let push_project = acting_roles || request.filter.names_text(&project_label(&pushed.project));
    validate_overwrites(ctx, &request, &pushed, &members, push_project)
        .map_err(|e| json_refusal(&nothing_reported, e))?;

    let project_url_remote = pvcs
        .project_url_remote(&pushed.project_dir)
        .with_context(|| format!("failed to read the remotes of {}", pushed.project))?;

    let mut fetched = Fetched::default();
    if request.mode == PushMode::Publish {
        fetched = fetch_all(
            &request,
            &pushed,
            &members,
            pvcs.as_ref(),
            project_url_remote.as_ref(),
        )
        .map_err(|e| json_refusal(&nothing_reported, e))?;
    } else {
        fetched.fill_present(&members);
    }

    let plan = judge(
        ctx,
        &request,
        &pushed,
        &members,
        pvcs.as_ref(),
        project_url_remote.as_ref(),
        &fetched,
        push_project,
    )
    .map_err(|(reported, e)| json_refusal(&reported, e))?;

    if request.mode == PushMode::DryRun {
        plan.print_dry_run(
            &pushed,
            push_project,
            &fetch_age_text(
                pvcs.as_ref(),
                &pushed.project_dir,
                plan.project
                    .as_ref()
                    .map(|project| &project.target)
                    .or(project_url_remote.as_ref()),
            ),
        );
        return Ok(());
    }

    publish(
        ctx,
        &request,
        &pushed,
        &members,
        pvcs.as_ref(),
        &fetched,
        plan,
        push_project,
    )
}

/// The weave branch to push, with the record and the manifest and lock it
/// is judged against; refuses a detached workspace, a name rwv does not
/// publish, a branch with no record or one that reads as no one value.
fn resolve(
    ctx: &WorkspaceContext,
    request: &PushRequest<'_>,
    pvcs: &dyn Vcs,
) -> anyhow::Result<Pushed> {
    let (project, workweave_dir) = crate::lock::scope(ctx)?;
    let dir = project_dir(ctx.active_path(), &project);
    let attachment = pvcs
        .head_attachment(&dir)
        .with_context(|| format!("failed to read current branch for {project}"))?;
    let current = match &attachment {
        HeadAttachment::Attached(attached) => attached.to_string(),
        HeadAttachment::Unborn(unborn) => crate::refuse!(
            RefusalKind::HeadNotOnBranch,
            "rwv push: project repo at projects/{project}/ is on branch '{unborn}' with no \
             commits yet; make an initial commit before pushing"
        ),
        HeadAttachment::Detached(_) => {
            let aligner = Aligner::open(ctx.active_path(), &project, Accounting::default())?;
            crate::lock::refuse_detached(&aligner, &project)?;
            crate::refuse!(
                RefusalKind::HeadNotOnBranch,
                "rwv push: project repo at projects/{project}/ is on a detached HEAD; \
                 check out the branch to publish first"
            );
        }
    };
    let (text, checked_out) = match request.branch {
        Some(named) if named != current => (named.to_owned(), false),
        _ => (current, true),
    };
    let branch = crate::branch::named(pvcs, &project, &dir, &text)?;
    if crate::naming::workweave_name_in(&project, branch.as_str()).is_some() {
        return Err(State::NameMinted
            .of(project_label(&project))
            .project_repo()
            .subject(&branch)
            .checkout(&dir)
            .judge(pvcs)
            .refused());
    }
    let raw = RawRefName::new(branch.as_str());
    let tip = pvcs.resolve_local_branch_tip(&dir, &raw)?;
    if tip.is_none() && !checked_out {
        return Err(State::NameNoSuchBranch
            .of(project_label(&project))
            .project_repo()
            .subject(&branch)
            .judge(pvcs)
            .refused());
    }
    let record = crate::branch::recorded_value(pvcs, &project, &dir, &branch)?;
    let tip = tip.expect("an attached branch with commits has a tip");
    let (manifest, lock) = if checked_out {
        let loaded = Project::from_dir(&dir, project.clone())
            .with_context(|| format!("failed to load project '{project}'"))?;
        (loaded.manifest, loaded.lock)
    } else {
        let manifest = Manifest::from_toml_str(
            &pvcs
                .read_file_at_revision(&dir, &tip, Path::new(Manifest::FILE_NAME))
                .with_context(|| format!("failed to read rwv.toml at {branch}"))?,
        )
        .with_context(|| format!("failed to load rwv.toml at {branch}"))?;
        let lock = pvcs
            .read_file_at_revision(&dir, &tip, Path::new(LockFile::FILE_NAME))
            .ok()
            .and_then(|text| LockFile::from_json_str(&text).ok());
        (manifest, lock)
    };
    Ok(Pushed {
        project,
        project_dir: dir,
        workweave_dir,
        branch,
        raw,
        tip,
        checked_out,
        record,
        manifest,
        lock,
    })
}

fn members_of(ctx: &WorkspaceContext, request: &PushRequest<'_>, pushed: &Pushed) -> Vec<Member> {
    pushed
        .manifest
        .iter_entries()
        .map(|(path, entry)| {
            let dir = crate::workspace::member_checkout_dir(
                path,
                ctx.primary_path(),
                pushed.workweave_dir.as_deref(),
            )
            .path()
            .to_path_buf();
            let acting = if request.filter.is_empty() {
                matches!(entry.role, Role::Owned | Role::Fork)
            } else {
                request.filter.matches(path, entry.role)
            };
            Member {
                path: path.clone(),
                entry: entry.clone(),
                dir,
                vcs: vcs_for(entry.vcs_type),
                acting,
                url: crate::registry::resolve_to_clone_info(&entry.url)
                    .ok()
                    .map(|info| info.url),
            }
        })
        .collect()
}

/// `--overwrite` names repos this push acts on, and none of them is a branch
/// something declares.
fn validate_overwrites(
    ctx: &WorkspaceContext,
    request: &PushRequest<'_>,
    pushed: &Pushed,
    members: &[Member],
    push_project: bool,
) -> anyhow::Result<()> {
    let mut accounting = None;
    for operand in request.overwrite.named() {
        match operand {
            RepoOperand::Project(name) => {
                if *name != pushed.project || !push_project {
                    crate::refuse!(
                        RefusalKind::InapplicableFlag,
                        "rwv push: `--overwrite {operand}` names a repo this push does not \
                         publish; `projects/{}` is published when no selector narrows the push \
                         or `--repo projects/{}` names it",
                        pushed.project,
                        pushed.project
                    );
                }
                if pushed.record == Record::Declared {
                    return Err(State::ProtectedDeclaredMode
                        .of(project_label(&pushed.project))
                        .project_repo()
                        .subject(&pushed.branch)
                        .by_verb("push")
                        .judge(project_vcs().as_ref())
                        .refused());
                }
            }
            RepoOperand::Member(path) => {
                let Some(member) = members.iter().find(|m| &m.path == path && m.acting) else {
                    crate::refuse!(
                        RefusalKind::InapplicableFlag,
                        "rwv push: `--overwrite {operand}` names no member this push publishes; \
                         a `dependency` or `reference` member is published only when `--repo` \
                         or `--role` names it"
                    );
                };
                let expected = pushed.expected(member.entry.role, &member.entry);
                let accounting =
                    accounting.get_or_insert_with(|| Accounting::scan(ctx.primary_path()));
                let declarers = accounting.declarers(&member.path, expected.as_str());
                if !declarers.is_empty() {
                    return Err(State::ProtectedDeclared
                        .of(&member.path)
                        .member()
                        .subject(&expected)
                        .listing(declarers)
                        .bind("<m>", &member.path)
                        .by_verb("push")
                        .judge(project_vcs().as_ref())
                        .refused());
                }
            }
        }
    }
    Ok(())
}

/// Each repo's url remote after the fetch, and the remotes whose fetch
/// could not complete.
#[derive(Default)]
struct Fetched {
    url_remotes: BTreeMap<RepoPath, Option<RemoteHandle>>,
    unconfirmed: BTreeMap<String, Vec<crate::vcs::UnconfirmedRemote>>,
}

impl Fetched {
    fn fill_present(&mut self, members: &[Member]) {
        for member in members {
            let present =
                crate::url_remote::present(member.vcs.as_ref(), &member.dir, &member.entry.url)
                    .ok()
                    .flatten();
            self.url_remotes.insert(member.path.clone(), present);
        }
    }

    fn url_remote(&self, path: &RepoPath) -> Option<&RemoteHandle> {
        self.url_remotes.get(path).and_then(Option::as_ref)
    }
}

/// The pruning fetch of the project repo and every member, the url remotes
/// added first. A remote the fetch could not complete, when it is one this
/// push publishes to or judges pins by, refuses the push.
fn fetch_all(
    request: &PushRequest<'_>,
    pushed: &Pushed,
    members: &[Member],
    pvcs: &dyn Vcs,
    project_url_remote: Option<&RemoteHandle>,
) -> anyhow::Result<Fetched> {
    let mut report = Report::default();
    let mut adds = Vec::with_capacity(members.len());
    for member in members {
        let planned = match (&member.url, member.dir.exists()) {
            (Some(url), true) => {
                match crate::url_remote::find(
                    member.vcs.as_ref(),
                    &member.path,
                    "push",
                    &member.dir,
                    url,
                )?
                .plan()
                {
                    Ok(planned) => Some(planned),
                    Err(judged) => {
                        report.add(*judged);
                        None
                    }
                }
            }
            _ => None,
        };
        adds.push(planned);
    }
    report.into_result()?;

    struct Job<'a> {
        label: String,
        dir: &'a Path,
        vcs: &'a dyn Vcs,
        add: Option<&'a crate::vcs::AbsentRemote>,
    }
    let mut jobs: Vec<Job<'_>> = vec![Job {
        label: project_label(&pushed.project),
        dir: &pushed.project_dir,
        vcs: pvcs,
        add: None,
    }];
    for (member, planned) in members.iter().zip(&adds) {
        if member.dir.exists() {
            jobs.push(Job {
                label: member.path.to_string(),
                dir: &member.dir,
                vcs: member.vcs.as_ref(),
                add: planned.as_ref().and_then(|p| p.add.as_ref()),
            });
        }
    }
    let results = run_in_parallel(&jobs, request.jobs, |_, job| {
        crate::fetch::pruning_fetch(job.vcs, job.dir, job.add)
            .map_err(|e| format!("{}: fetch failed: {e:#}", job.label))
    });
    let mut fetched = Fetched::default();
    for (job, result) in jobs.iter().zip(results) {
        match result {
            Ok(done) if done.unconfirmed.is_empty() => {}
            Ok(done) => {
                fetched
                    .unconfirmed
                    .insert(job.label.clone(), done.unconfirmed);
            }
            Err(message) => return Err(crate::untokened!(EnvironmentPassthrough, "{message}")),
        }
    }
    for (member, planned) in members.iter().zip(&adds) {
        let remote = planned.as_ref().map(|p| p.remote.clone()).or_else(|| {
            crate::url_remote::present(member.vcs.as_ref(), &member.dir, &member.entry.url)
                .ok()
                .flatten()
        });
        fetched.url_remotes.insert(member.path.clone(), remote);
    }

    let mut blocked = Report::default();
    let project_target =
        pvcs.push_target_of(&pushed.project_dir, &pushed.raw, project_url_remote)?;
    let mut judged_unconfirmed = |label: &str,
                                  dir: &Path,
                                  vcs: &dyn Vcs,
                                  relevant: &[Option<&RemoteHandle>],
                                  member: bool| {
        let Some(remotes) = fetched.unconfirmed.get(label) else {
            return;
        };
        let blocking: Vec<&crate::vcs::UnconfirmedRemote> = remotes
            .iter()
            .filter(|u| relevant.iter().flatten().any(|r| r.name() == u.remote))
            .collect();
        if blocking.is_empty() {
            return;
        }
        let mut judging = State::PushBranchUnconfirmed
            .of(label)
            .checkout(dir)
            .subject(
                blocking
                    .iter()
                    .map(|u| u.remote.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
            .listing(
                blocking
                    .iter()
                    .map(|u| format!("{}: {}", u.remote, u.cause)),
            );
        judging = if member {
            judging.member().bind("<m>", label)
        } else {
            judging.project_repo()
        };
        blocked.add(judging.judge(vcs));
    };
    judged_unconfirmed(
        &project_label(&pushed.project),
        &pushed.project_dir,
        pvcs,
        &[project_url_remote, project_target.as_ref()],
        false,
    );
    for member in members.iter().filter(|m| m.dir.exists()) {
        let expected = pushed.expected(member.entry.role, &member.entry);
        let target =
            member
                .vcs
                .push_target_of(&member.dir, &expected, fetched.url_remote(&member.path))?;
        judged_unconfirmed(
            member.path.as_str(),
            &member.dir,
            member.vcs.as_ref(),
            &[fetched.url_remote(&member.path), target.as_ref()],
            true,
        );
    }
    blocked.into_result()?;
    Ok(fetched)
}

/// What the judging found to do.
struct Plan {
    items: Vec<PushPlanItem>,
    /// Members left out of this push because their role is not pushed by
    /// default, for the output.
    default_skipped: Vec<RepoPath>,
    /// Members whose branch the push left alone: even, behind, or nothing to
    /// publish.
    state_skipped: Vec<RepoPath>,
    project: Option<ProjectPlan>,
    reported: Report,
}

struct ProjectPlan {
    target: RemoteHandle,
    /// The branch has something the push branch lacks.
    push_branch: bool,
    replace: Replace,
}

impl Plan {
    fn print_dry_run(&self, pushed: &Pushed, push_project: bool, fetch_age: &str) {
        outln!(
            "rwv push (dry-run): judged on refs as last fetched{}",
            fetch_age
        );
        for item in &self.items {
            outln!(
                "  {}: would push {} -> {}",
                item.repo_path.as_str(),
                item.branch,
                item.target,
            );
        }
        for path in &self.default_skipped {
            outln!(
                "  {}: skipped (not pushed by default; pass --repo or --role to include)",
                path.as_str(),
            );
        }
        if let (true, Some(project)) = (push_project, &self.project) {
            outln!(
                "  projects/{}: would publish the record of {} -> {}",
                pushed.project,
                pushed.branch,
                project.target,
            );
            if project.push_branch {
                outln!(
                    "  projects/{}: would push {} -> {} (last)",
                    pushed.project,
                    pushed.branch,
                    project.target,
                );
            }
        }
    }
}

/// Judge everything the push acts on before anything is published: the
/// project repo's name, record and standing, each acting member's alignment,
/// pin and standing, and the merges in the pushed range.
#[allow(clippy::too_many_arguments)]
fn judge(
    ctx: &WorkspaceContext,
    request: &PushRequest<'_>,
    pushed: &Pushed,
    members: &[Member],
    pvcs: &dyn Vcs,
    project_url_remote: Option<&RemoteHandle>,
    fetched: &Fetched,
    push_project: bool,
) -> Result<Plan, (Report, anyhow::Error)> {
    let mut refusals = Report::default();
    let mut reported = Report::default();
    let mut failures: Vec<String> = Vec::new();
    let result = judge_into(
        ctx,
        request,
        pushed,
        members,
        pvcs,
        project_url_remote,
        fetched,
        push_project,
        &mut refusals,
        &mut reported,
        &mut failures,
    );
    if !failures.is_empty() {
        if refusals.is_empty() {
            return Err((
                reported,
                crate::untokened!(Tally, "{}", failures.join("\n")),
            ));
        }
        for failure in &failures {
            refusals.note(failure);
        }
    }
    match (result, refusals.is_empty()) {
        (Err(e), true) => Err((reported, e)),
        (_, false) => {
            if request.output == OutputMode::Text && !reported.is_empty() {
                eprint!("{}", reported.render("push"));
            }
            Err((
                reported,
                refusals
                    .into_result()
                    .expect_err("a report holding a line refuses"),
            ))
        }
        (Ok(mut plan), true) => {
            if request.output == OutputMode::Text && !reported.is_empty() {
                eprint!("{}", reported.render("push"));
            }
            plan.reported = reported;
            Ok(plan)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn judge_into(
    ctx: &WorkspaceContext,
    request: &PushRequest<'_>,
    pushed: &Pushed,
    members: &[Member],
    pvcs: &dyn Vcs,
    project_url_remote: Option<&RemoteHandle>,
    fetched: &Fetched,
    push_project: bool,
    refusals: &mut Report,
    reported: &mut Report,
    failures: &mut Vec<String>,
) -> anyhow::Result<Plan> {
    let primary = ctx.primary_path();
    let live = crate::pins::live_pins(primary)?;
    let aligner = if pushed.checked_out {
        Some(Aligner::open(
            ctx.active_path(),
            &pushed.project,
            Accounting::scan(primary),
        )?)
    } else {
        None
    };
    let mut plan = Plan {
        items: Vec::new(),
        default_skipped: Vec::new(),
        state_skipped: Vec::new(),
        project: None,
        reported: Report::default(),
    };

    let target_lock = target_lock_of(pushed, pvcs, project_url_remote, refusals)?;
    let declared = declared_by_pushed_range(pushed, pvcs, project_url_remote)?;
    for member in members {
        if !member.acting {
            if request.filter.is_empty() {
                plan.default_skipped.push(member.path.clone());
            }
            continue;
        }
        if !member.dir.exists() {
            failures.push(format!(
                "{}: clone missing on disk; run `rwv update` from the workspace \
                 to re-materialize missing manifest members, then re-run `rwv push`",
                member.path.as_str(),
            ));
            continue;
        }
        judge_member(
            request,
            pushed,
            member,
            aligner.as_ref(),
            fetched.url_remote(&member.path),
            &live,
            target_lock.as_ref(),
            refusals,
            reported,
            failures,
            &mut plan,
        )?;
        if let Some(names) = declared.get(&member.path) {
            plan_declared_branches(
                pushed,
                member,
                fetched.url_remote(&member.path),
                names,
                &mut plan,
            )?;
        }
    }

    judge_project(
        request,
        pushed,
        members,
        pvcs,
        project_url_remote,
        push_project,
        refusals,
        reported,
        &mut plan,
    )?;
    judge_overwrites(pushed, members, &live, push_project, refusals, &mut plan)?;
    Ok(plan)
}

#[allow(clippy::too_many_arguments)]
fn judge_member(
    request: &PushRequest<'_>,
    pushed: &Pushed,
    member: &Member,
    aligner: Option<&Aligner>,
    url_remote: Option<&RemoteHandle>,
    live: &BTreeMap<RepoPath, Vec<crate::pins::LivePin>>,
    target_lock: Option<&LockFile>,
    refusals: &mut Report,
    reported: &mut Report,
    failures: &mut Vec<String>,
    plan: &mut Plan,
) -> anyhow::Result<()> {
    let vcs = member.vcs.as_ref();
    let path = &member.path;
    let aligned = AlignedMember {
        path,
        entry: &member.entry,
        dir: &member.dir,
        vcs,
    };
    let expected = pushed.expected(member.entry.role, &member.entry);
    let mut branch = Some(expected.clone());
    if let Some(aligner) = aligner {
        let aligned_plan = aligner
            .plan(&aligned, pushed.lock.as_ref())
            .map_err(|e| crate::untokened!(EnvironmentPassthrough, "{path}: {e:#}"))?;
        if let Some(off) = aligner.refusal(&aligned, &aligned_plan, path)? {
            refusals.add(off);
        }
        branch = match &aligned_plan.target {
            Target::Branch { name, .. } => Some(name.clone()),
            Target::Detached { .. } | Target::AsFound => None,
        };
    }

    let judging = |state: State| {
        state
            .of(path)
            .member()
            .checkout(&member.dir)
            .bind("<m>", path.as_str())
    };
    let pin = match crate::alignment::pin_of(vcs, &member.dir, pushed.lock.as_ref(), path) {
        Pin::None => {
            refusals.add(judging(State::PinNone).judge(vcs));
            return Ok(());
        }
        Pin::Unreachable => {
            let version = pushed
                .lock
                .as_ref()
                .and_then(|lock| lock.get_entry(path))
                .map(|entry| entry.version.short_display().to_owned())
                .unwrap_or_default();
            refusals.add(judging(State::PinUnreachable).subject(version).judge(vcs));
            return Ok(());
        }
        Pin::At(pin) => pin,
    };
    let Some(branch) = branch else {
        return Ok(());
    };
    let head = match vcs.resolve_local_branch_tip(&member.dir, &branch)? {
        Some(tip) => tip,
        None if pushed.checked_out => {
            failures.push(format!("{path}: branch {branch} does not exist"));
            return Ok(());
        }
        None => {
            plan.state_skipped.push(path.clone());
            return Ok(());
        }
    };
    let held = live.get(path).map(Vec::as_slice).unwrap_or_default();
    let standing = match crate::pins::standing(vcs, &member.dir, &head, &pin, held) {
        Ok(standing) => standing,
        Err(e) => {
            failures.push(format!("{path}: {e:#}"));
            return Ok(());
        }
    };
    if let Some(judged) = crate::pins::judge(path, &member.dir, vcs, &standing, &head, &pin) {
        if judged.state() == State::PinAhead {
            reported.add(judged);
        } else {
            refusals.add(judged);
            return Ok(());
        }
    }

    let (branch_standing, target) =
        crate::push_branch::standing_of_branch(vcs, &member.dir, &branch, url_remote)
            .with_context(|| format!("{path}: failed to find where the branch publishes"))?;
    let subject = crate::push_branch::Subject {
        repo: path,
        checkout: &member.dir,
        branch: branch.as_str(),
        declared: Some(&member.entry.branch),
        member: true,
    };
    let judged = crate::push_branch::judge(vcs, &subject, &branch_standing);
    if pushed.is_feature()
        && matches!(member.entry.role, Role::Owned | Role::Fork)
        && !holds_commits_beyond_target_pin(vcs, member, &head, target_lock)?
    {
        plan.state_skipped.push(path.clone());
        return Ok(());
    }
    let named = !request.filter.is_empty();
    let operand = RepoOperand::Member(path.clone());
    let replace = match &branch_standing {
        Standing::NoPushTarget | Standing::ForeignUpstream { .. } => {
            refusals.extend(judged);
            return Ok(());
        }
        Standing::Diverged { .. } if branch_standing.diverged_with_unpushed() => {
            let remote_tip = target
                .as_ref()
                .and_then(|target| {
                    vcs.remote_copies(&member.dir, &branch)
                        .ok()?
                        .into_iter()
                        .find(|copy| &copy.remote == target)
                })
                .map(|copy| copy.tip);
            let Some(lease) = remote_tip
                .clone()
                .and_then(|tip| request.overwrite.lease(&operand, tip))
            else {
                refusals.extend(judged);
                return Ok(());
            };
            Replace::Leased(lease)
        }
        Standing::Behind | Standing::Diverged { .. } => {
            reported.extend(judged);
            plan.state_skipped.push(path.clone());
            return Ok(());
        }
        Standing::Gone { .. } if !named => {
            reported.extend(judged);
            plan.state_skipped.push(path.clone());
            return Ok(());
        }
        Standing::Even => {
            plan.state_skipped.push(path.clone());
            return Ok(());
        }
        Standing::Ahead | Standing::Unpublished | Standing::Gone { .. } => Replace::FastForwardOnly,
    };
    let target = target.expect("a standing other than no push target has one");
    plan.items.push(PushPlanItem {
        repo_path: path.clone(),
        branch: branch.as_str().to_owned(),
        vcs_type: member.entry.vcs_type,
        publish_ref: PublishRef::judged(&LocalRefName::of_branch(&branch), head),
        target,
        replace,
    });
    Ok(())
}

/// `T`'s lock, read at `T`'s tip: the newer of the record's target and its
/// upstream. `None` where this is no feature or the target has no lock here;
/// a target diverged from its upstream is refused.
fn target_lock_of(
    pushed: &Pushed,
    pvcs: &dyn Vcs,
    project_url_remote: Option<&RemoteHandle>,
    refusals: &mut Report,
) -> anyhow::Result<Option<LockFile>> {
    let Record::Feature { target, .. } = &pushed.record else {
        return Ok(None);
    };
    let raw = RawRefName::new(target.as_str());
    let tip = match crate::push_branch::newer_of_branch_and_upstream(
        pvcs,
        &pushed.project_dir,
        &raw,
        project_url_remote,
    )? {
        NewerOfBranchAndUpstream::Tip(tip) => tip,
        NewerOfBranchAndUpstream::Nowhere => return Ok(None),
        NewerOfBranchAndUpstream::Diverged {
            local,
            upstream,
            upstream_tip,
        } => {
            refusals.add(
                State::PushBranchDiverged
                    .of(project_label(&pushed.project))
                    .project_repo()
                    .subject(format!("{target} and {upstream}"))
                    .listing([format!(
                        "{target} is at {} and {upstream} at {}; {target}'s tip, whose lock says \
                         what {} holds beyond it, is not one commit",
                        local.id().short(),
                        upstream_tip.id().short(),
                        pushed.branch
                    )])
                    .checkout(&pushed.project_dir)
                    .bind("<T>", target.as_str())
                    .judge(pvcs),
            );
            return Ok(None);
        }
    };
    Ok(pvcs
        .read_file_at_revision(&pushed.project_dir, &tip, Path::new(LockFile::FILE_NAME))
        .ok()
        .and_then(|text| LockFile::from_json_str(&text).ok()))
}

/// Whether a feature's member branch holds commits the target's pin for the
/// member lacks. A member the target's lock does not pin, or pins at a commit
/// this clone lacks, has no pin to lack them: the branch is published.
fn holds_commits_beyond_target_pin(
    vcs: &dyn Vcs,
    member: &Member,
    head: &ResolvedRevisionId,
    target_lock: Option<&LockFile>,
) -> anyhow::Result<bool> {
    let Some(entry) = target_lock.and_then(|lock| lock.get_entry(&member.path)) else {
        return Ok(true);
    };
    let Ok(pin) = vcs.resolve_revision(&member.dir, entry.version.as_str()) else {
        return Ok(true);
    };
    Ok(!matches!(
        vcs.relation(&member.dir, head, &pin)?,
        crate::vcs::Relation::Even | crate::vcs::Relation::Behind
    ))
}

/// The commits a push of the project branch publishes, oldest first.
fn pushed_project_range(
    pushed: &Pushed,
    pvcs: &dyn Vcs,
    project_url_remote: Option<&RemoteHandle>,
) -> anyhow::Result<(Vec<crate::vcs::RangeCommit>, Option<ResolvedRevisionId>)> {
    let tip = pushed.tip.clone();
    let (standing, target) = crate::push_branch::standing_of_branch(
        pvcs,
        &pushed.project_dir,
        &pushed.raw,
        project_url_remote,
    )?;
    let on_target = match (&standing, &target) {
        (Standing::Unpublished | Standing::Gone { .. }, _) | (_, None) => None,
        (_, Some(target)) => pvcs
            .remote_copies(&pushed.project_dir, &pushed.raw)?
            .into_iter()
            .find(|copy| &copy.remote == target)
            .map(|copy| copy.tip),
    };
    let range = pvcs.pushed_range(&pushed.project_dir, &tip, on_target.as_ref())?;
    Ok((range, Some(tip)))
}

/// Every branch each member is declared on, by the project branch's tip and,
/// for a branch that is not a feature, by each commit of the range being pushed. A commit whose `rwv.toml` does not
/// parse under the current schema declares nothing.
fn declared_by_pushed_range(
    pushed: &Pushed,
    pvcs: &dyn Vcs,
    project_url_remote: Option<&RemoteHandle>,
) -> anyhow::Result<BTreeMap<RepoPath, std::collections::BTreeSet<String>>> {
    let mut declared: BTreeMap<RepoPath, std::collections::BTreeSet<String>> = BTreeMap::new();
    for (path, entry) in pushed.manifest.iter_entries() {
        declared
            .entry(path.clone())
            .or_default()
            .insert(entry.branch.local_counterpart().as_str().to_owned());
    }
    if pushed.is_feature() {
        return Ok(declared);
    }
    let (range, _) = pushed_project_range(pushed, pvcs, project_url_remote)?;
    for commit in range {
        let at = ResolvedRevisionId::from(commit.id);
        let Some(manifest) = pvcs
            .read_file_at_revision(&pushed.project_dir, &at, Path::new(Manifest::FILE_NAME))
            .ok()
            .and_then(|text| Manifest::from_toml_str(&text).ok())
        else {
            continue;
        };
        for (path, entry) in manifest.iter_entries() {
            declared
                .entry(path.clone())
                .or_default()
                .insert(entry.branch.local_counterpart().as_str().to_owned());
        }
    }
    Ok(declared)
}

/// Each branch the tip or a commit of the pushed range declares for `member`,
/// other than the one already judged, goes to its push target when it exists
/// here and has commits the push target lacks or no push branch.
fn plan_declared_branches(
    pushed: &Pushed,
    member: &Member,
    url_remote: Option<&RemoteHandle>,
    names: &std::collections::BTreeSet<String>,
    plan: &mut Plan,
) -> anyhow::Result<()> {
    let vcs = member.vcs.as_ref();
    let judged = pushed.expected(member.entry.role, &member.entry);
    for name in names {
        let branch = RawRefName::new(name.clone());
        if branch == judged
            || plan
                .items
                .iter()
                .any(|i| i.repo_path == member.path && i.branch == *name)
        {
            continue;
        }
        let Some(tip) = vcs.resolve_local_branch_tip(&member.dir, &branch)? else {
            continue;
        };
        let (standing, target) =
            crate::push_branch::standing_of_branch(vcs, &member.dir, &branch, url_remote)?;
        let Some(target) = target else { continue };
        let publishes = match standing {
            Standing::Unpublished => true,
            Standing::Ahead => !pushed.is_feature(),
            _ => false,
        };
        if publishes {
            plan.items.push(PushPlanItem {
                repo_path: member.path.clone(),
                branch: name.clone(),
                vcs_type: member.entry.vcs_type,
                publish_ref: PublishRef::judged(&LocalRefName::of_branch(&branch), tip),
                target,
                replace: Replace::FastForwardOnly,
            });
        }
    }
    Ok(())
}

/// How long ago rwv last fetched the project repo's `remote`, or that it has
/// no record of doing so.
fn fetch_age_text(vcs: &dyn Vcs, repo: &Path, remote: Option<&RemoteHandle>) -> String {
    let age = match remote {
        Some(remote) => crate::fetch_claim::last_fetch_age(vcs, repo, remote),
        None => Ok(None),
    };
    match age {
        Ok(Some(age)) => {
            let secs = age.as_secs();
            let (n, unit) = match secs {
                0..=59 => (secs, "second"),
                60..=3599 => (secs / 60, "minute"),
                3600..=86399 => (secs / 3600, "hour"),
                _ => (secs / 86400, "day"),
            };
            format!(
                " (last fetch {n} {unit}{} ago)",
                if n == 1 { "" } else { "s" }
            )
        }
        Ok(None) => " (no fetch on record)".to_owned(),
        Err(e) => format!(" (the fetch record does not read: {e:#})"),
    }
}

/// The pin rule for each member overwrite the plan holds: the replaced tip is
/// kept as a backup, so what counts is a live pin a remote-tracking lock holds
/// that no remote-tracking branch of the member would reach. The lock of the
/// project branch this run publishes, fast-forward, is exempt where the lock
/// it publishes pins a commit the new member tip reaches.
fn judge_overwrites(
    pushed: &Pushed,
    members: &[Member],
    live: &BTreeMap<RepoPath, Vec<crate::pins::LivePin>>,
    push_project: bool,
    refusals: &mut Report,
    plan: &mut Plan,
) -> anyhow::Result<()> {
    let replaced_project = plan
        .project
        .as_ref()
        .filter(|p| push_project && p.push_branch && matches!(p.replace, Replace::FastForwardOnly))
        .map(|p| BranchRef::Remote {
            remote: p.target.clone(),
            name: pushed.raw.clone(),
        });
    let mut kept = Vec::with_capacity(plan.items.len());
    for item in std::mem::take(&mut plan.items) {
        let Replace::Leased(lease) = &item.replace else {
            kept.push(item);
            continue;
        };
        let member = members
            .iter()
            .find(|m| m.path == item.repo_path)
            .expect("a planned member is a member");
        let vcs = member.vcs.as_ref();
        let head = item.publish_ref.commit();
        let new_lock_reached = pushed
            .lock
            .as_ref()
            .and_then(|lock| lock.get_entry(&member.path))
            .and_then(|entry| {
                vcs.resolve_revision(&member.dir, entry.version.as_str())
                    .ok()
            })
            .map(|pin| vcs.is_ancestor(&member.dir, &pin, head))
            .transpose()?
            .unwrap_or(false);
        let exempt = |pin: &crate::pins::LivePin| {
            new_lock_reached
                && pin.project == pushed.project
                && replaced_project.as_ref() == Some(&pin.branch)
        };
        let replaced = BranchRef::Remote {
            remote: item.target.clone(),
            name: RawRefName::new(item.branch.clone()),
        };
        let change = crate::pins::RefChange {
            branch: &replaced,
            was: lease.expected(),
            now: Some(head),
            was_backed_up: true,
        };
        let held = live
            .get(&member.path)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let stranded = crate::pins::stranded_by(vcs, &member.dir, held, &change, exempt)?;
        if stranded.is_empty() {
            kept.push(item);
            continue;
        }
        refusals.add(
            State::PinWouldBeUnreachable
                .of(&member.path)
                .member()
                .checkout(&member.dir)
                .bind("<m>", member.path.as_str())
                .listing(stranded.iter().map(|s| s.line(&change)))
                .judge(vcs),
        );
    }
    plan.items = kept;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn judge_project(
    request: &PushRequest<'_>,
    pushed: &Pushed,
    members: &[Member],
    pvcs: &dyn Vcs,
    project_url_remote: Option<&RemoteHandle>,
    push_project: bool,
    refusals: &mut Report,
    reported: &mut Report,
    plan: &mut Plan,
) -> anyhow::Result<()> {
    if !push_project {
        return Ok(());
    }
    let label = project_label(&pushed.project);
    let (standing, target) = crate::push_branch::standing_of_branch(
        pvcs,
        &pushed.project_dir,
        &pushed.raw,
        project_url_remote,
    )
    .with_context(|| format!("failed to find where {} publishes", pushed.project))?;
    let subject = crate::push_branch::Subject {
        repo: &label,
        checkout: &pushed.project_dir,
        branch: pushed.branch.as_str(),
        declared: None,
        member: false,
    };
    let judged = crate::push_branch::judge(pvcs, &subject, &standing);
    let operand = RepoOperand::Project(pushed.project.clone());
    let mut replace = Replace::FastForwardOnly;
    let push_branch = match &standing {
        Standing::NoPushTarget | Standing::ForeignUpstream { .. } => {
            refusals.extend(judged);
            return Ok(());
        }
        Standing::Diverged { .. } if standing.diverged_with_unpushed() => {
            let remote_tip = target
                .as_ref()
                .and_then(|target| {
                    pvcs.remote_copies(&pushed.project_dir, &pushed.raw)
                        .ok()?
                        .into_iter()
                        .find(|copy| &copy.remote == target)
                })
                .map(|copy| copy.tip);
            match remote_tip.and_then(|tip| request.overwrite.lease(&operand, tip)) {
                Some(lease) => {
                    replace = Replace::Leased(lease);
                    true
                }
                None => {
                    refusals.extend(judged);
                    return Ok(());
                }
            }
        }
        Standing::Behind | Standing::Diverged { .. } => {
            reported.extend(judged);
            false
        }
        Standing::Even => false,
        Standing::Gone { .. } => {
            reported.extend(judged);
            request.filter.names_text(&label)
        }
        Standing::Ahead | Standing::Unpublished => true,
    };
    let target = target.expect("a project branch with no push target was refused");

    if push_branch {
        judge_range(
            pushed, members, pvcs, &standing, &target, refusals, reported,
        )?;
    }
    plan.project = Some(ProjectPlan {
        target,
        push_branch,
        replace,
    });
    Ok(())
}

/// The merges in the range the project branch publishes: each member one
/// joined without joining the member is refused unless waived.
fn judge_range(
    pushed: &Pushed,
    members: &[Member],
    pvcs: &dyn Vcs,
    standing: &Standing,
    target: &RemoteHandle,
    refusals: &mut Report,
    reported: &mut Report,
) -> anyhow::Result<()> {
    let tip = pushed.tip.clone();
    let pushed_tip = match standing {
        Standing::Unpublished | Standing::Gone { .. } => None,
        _ => pvcs
            .remote_copies(&pushed.project_dir, &pushed.raw)?
            .into_iter()
            .find(|copy| &copy.remote == target)
            .map(|copy| copy.tip),
    };
    let range = pvcs.pushed_range(&pushed.project_dir, &tip, pushed_tip.as_ref())?;
    if !range.iter().any(|commit| commit.parents.len() > 1) {
        return Ok(());
    }
    let judged: Vec<crate::unmerged::Member<'_>> = members
        .iter()
        .filter(|m| m.dir.exists())
        .map(|m| crate::unmerged::Member {
            path: &m.path,
            dir: m.dir.clone(),
            vcs: m.vcs.as_ref(),
        })
        .collect();
    let findings = crate::unmerged::analyze(pvcs, &pushed.project_dir, &judged, &range, tip.id())?;
    for finding in findings {
        let dir = members
            .iter()
            .find(|m| m.path == finding.member)
            .map(|m| m.dir.clone())
            .unwrap_or_default();
        let member_vcs = members
            .iter()
            .find(|m| m.path == finding.member)
            .map(|m| m.vcs.as_ref())
            .unwrap_or(pvcs);
        let pins = format!(
            "its parents pinned {} and {}",
            finding.pins.0.short(),
            finding.pins.1.short()
        );
        let judging = |state: State| {
            state
                .of(&finding.member)
                .member()
                .checkout(&dir)
                .bind("<m>", finding.member.as_str())
                .subject(format!("merge {}", finding.merge.short()))
        };
        match &finding.verdict {
            crate::unmerged::Verdict::Unmerged => refusals.add(
                judging(State::PinUnmerged)
                    .listing([
                        pins,
                        "the pushed lock holds no commit containing both".to_owned(),
                    ])
                    .bind("<rev>", finding.pins.1.short())
                    .judge(member_vcs),
            ),
            crate::unmerged::Verdict::ListedOnly => reported.add(
                judging(State::PinUnmerged)
                    .listing([
                        pins,
                        "the parents declare the member differently".to_owned(),
                    ])
                    .bind("<rev>", finding.pins.1.short())
                    .only(&[ChoiceId::C26])
                    .judge(member_vcs),
            ),
            crate::unmerged::Verdict::Waived { by, to } => reported.add(
                judging(State::PinWaived)
                    .listing([
                        pins,
                        format!("re-pinned to {} in {}", to.short(), by.short()),
                    ])
                    .fact(Fact {
                        kind: FactKind::PinLanded,
                        about: None,
                    })
                    .judge(member_vcs),
            ),
        }
    }
    Ok(())
}

/// Push the members' branches, then judge the pins the project branch's lock
/// holds against what the remotes now hold, then publish the record and the
/// project branch.
#[allow(clippy::too_many_arguments)]
fn publish(
    ctx: &WorkspaceContext,
    request: &PushRequest<'_>,
    pushed: &Pushed,
    members: &[Member],
    pvcs: &dyn Vcs,
    fetched: &Fetched,
    plan: Plan,
    push_project: bool,
) -> anyhow::Result<()> {
    let output = request.output;
    let reported = &plan.reported;
    let parallel = request.jobs > 1;
    let write_lock: Mutex<()> = Mutex::new(());
    let plan_vcs: Vec<Box<dyn Vcs>> = plan.items.iter().map(|i| vcs_for(i.vcs_type)).collect();
    let dirs: Vec<PathBuf> = plan
        .items
        .iter()
        .map(|item| {
            members
                .iter()
                .find(|m| m.path == item.repo_path)
                .map(|m| m.dir.clone())
                .expect("a planned member is a member")
        })
        .collect();
    crate::test_pause::point("push-members");
    let raw_outcomes: Vec<PushOutcome> = run_in_parallel(&plan.items, request.jobs, |idx, item| {
        let reporter = if output.is_json() {
            Reporter::silent()
        } else if parallel {
            Reporter::parallel(item.repo_path.as_str().to_string(), &write_lock)
        } else {
            Reporter::serial()
        };
        push_one(plan_vcs[idx].as_ref(), item, &dirs[idx], &reporter)
    });

    let mut json_outcomes: Vec<PushOutcomeOutput> = Vec::new();
    let mut push_errors: Vec<String> = Vec::new();
    let mut withheld = Report::default();
    let mut pushed_count = 0usize;
    let mut skipped = 0usize;
    let wire_path = |path: &RepoPath| {
        crate::path_spelling::WirePath::of(
            &members
                .iter()
                .find(|m| &m.path == path)
                .map(|m| m.dir.clone())
                .unwrap_or_else(|| ctx.primary_path().join(path.as_path())),
        )
    };

    for path in &plan.default_skipped {
        if output == OutputMode::Text {
            outln!(
                "rwv push: skipped {} (not pushed by default)",
                path.as_str()
            );
        }
        skipped += 1;
        json_outcomes.push(PushOutcomeOutput::Skipped {
            path: path.as_str().to_string(),
            absolute_path: wire_path(path),
        });
    }
    for path in &plan.state_skipped {
        json_outcomes.push(PushOutcomeOutput::Skipped {
            path: path.as_str().to_string(),
            absolute_path: wire_path(path),
        });
    }
    for ((item, dir), raw) in plan.items.iter().zip(&dirs).zip(raw_outcomes) {
        let path = item.repo_path.to_string();
        let absolute_path = crate::path_spelling::WirePath::of(dir);
        match raw {
            PushOutcome::Pushed => {
                pushed_count += 1;
                json_outcomes.push(PushOutcomeOutput::Pushed {
                    path,
                    absolute_path,
                });
            }
            PushOutcome::Failed(message) => {
                push_errors.push(message.clone());
                withheld.add(
                    State::PublishWithheld
                        .of(&item.repo_path)
                        .listing(message.lines())
                        .checkout(dir)
                        .bind("<b>", &item.branch)
                        .judge(vcs_for(item.vcs_type).as_ref()),
                );
                json_outcomes.push(PushOutcomeOutput::Failed {
                    path,
                    absolute_path,
                    message,
                });
            }
        }
    }

    if !push_errors.is_empty() {
        if output == OutputMode::Text {
            eprintln!(
                "rwv push: {}/{} member repo(s) failed; project repo not pushed:",
                push_errors.len(),
                plan.items.len(),
            );
            for msg in &push_errors {
                eprintln!("  - {msg}");
            }
        }
        withheld.note(format!(
            "projects/{} was not pushed; member pushes that succeeded are published",
            pushed.project
        ));
        let refusal = withheld
            .into_result()
            .expect_err("a failed member push is judged withheld");
        if output.is_json() {
            print_envelope(json_outcomes, &refusal, reported, ctx);
        }
        return Err(refusal);
    }

    if output == OutputMode::Text {
        if skipped > 0 {
            outln!(
                "rwv push: pushed {} member repo(s); {} skipped (not pushed by default; pass --repo or --role to include)",
                pushed_count, skipped
            );
        } else {
            outln!("rwv push: pushed {} member repo(s)", pushed_count);
        }
    }

    let Some(project_plan) = plan.project.as_ref().filter(|_| push_project) else {
        if output.is_json() {
            let envelope = PushJsonOutput {
                schema_url: PUSH_SCHEMA_URL.to_string(),
                outcomes: json_outcomes,
                refused: Vec::new(),
                reported: reported.output(),
                resolution: Some(ctx.resolution()),
            };
            outln!("{}", serde_json::to_string_pretty(&envelope)?);
        } else {
            outln!("rwv push: done");
        }
        return Ok(());
    };

    if project_plan.push_branch {
        let mut gate = Report::default();
        gate_reachability(pushed, members, fetched, &mut gate);
        if let Err(refusal) = gate.into_result() {
            if output.is_json() {
                print_envelope(json_outcomes, &refusal, reported, ctx);
            }
            return Err(refusal);
        }
    }

    let project_path = project_label(&pushed.project);
    let project_abs = crate::path_spelling::WirePath::of(&pushed.project_dir);
    let store = Store::open(pvcs, &pushed.project_dir)?;
    let refetch = || {
        crate::fetch::pruning_fetch_of(
            pvcs,
            &pushed.project_dir,
            std::slice::from_ref(&project_plan.target),
        )
        .map(|_| ())
    };
    if output == OutputMode::Text {
        outln!(
            "rwv push: publishing the record of {} ({})",
            pushed.branch,
            project_plan.target,
        );
    }
    match store.publish(&pushed.branch, &project_plan.target, &refetch)? {
        Published::AlreadyThere => {}
        Published::Sent { dropped } => {
            json_outcomes.push(PushOutcomeOutput::RecordsPublished {
                path: project_path.clone(),
                absolute_path: project_abs.clone(),
                project: pushed.project.clone(),
                dropped,
            });
        }
        Published::Rejected(message) => {
            let default_branch = pvcs
                .remote_default_branch(&pushed.project_dir, &project_plan.target)?
                .map(|default| default.local_counterpart());
            let is_default = default_branch
                .as_ref()
                .is_some_and(|default| default.as_str() == pushed.branch.as_str());
            let is_fact = is_default && pushed.record == Record::Declared;
            let line = State::PublishRecordsRejected
                .of(&project_path)
                .project_repo()
                .checkout(&pushed.project_dir)
                .listing([message.clone()])
                .judge(pvcs);
            json_outcomes.push(PushOutcomeOutput::RecordsRejected {
                path: project_path.clone(),
                absolute_path: project_abs.clone(),
                project: pushed.project.clone(),
                message,
                withheld: !is_fact,
            });
            if is_fact {
                let mut fact = Report::default();
                fact.add(line);
                if output == OutputMode::Text {
                    eprint!("{}", fact.render("push"));
                }
            } else {
                let mut refusal = Report::default();
                refusal.add(line);
                refusal.note(format!(
                    "projects/{} {} was not pushed; the members' branches that were pushed are published",
                    pushed.project, pushed.branch
                ));
                let refusal = refusal
                    .into_result()
                    .expect_err("a rejected record is judged withheld");
                if output.is_json() {
                    print_envelope(json_outcomes, &refusal, reported, ctx);
                }
                return Err(refusal);
            }
        }
    }

    let mut project_error = None;
    if project_plan.push_branch {
        if output == OutputMode::Text {
            outln!(
                "rwv push: pushing project repo projects/{} ({} -> {})",
                pushed.project,
                pushed.branch,
                project_plan.target,
            );
        }
        let judged = PublishRef::judged(&LocalRefName::of_branch(&pushed.raw), pushed.tip.clone());
        let result = publish_ref(
            pvcs,
            &pushed.project_dir,
            &project_plan.target,
            &judged,
            &project_plan.replace,
        )
        .map_err(|e| anyhow::anyhow!("{e:#}"))
        .and_then(|()| {
            give_upstream(pvcs, &pushed.project_dir, &pushed.raw, &project_plan.target)
                .map_err(anyhow::Error::from)
        });
        match &result {
            Ok(()) => json_outcomes.push(PushOutcomeOutput::ProjectRepoPushed {
                path: project_path,
                absolute_path: project_abs,
                project: pushed.project.clone(),
            }),
            Err(e) => {
                json_outcomes.push(PushOutcomeOutput::ProjectRepoFailed {
                    path: project_path,
                    absolute_path: project_abs,
                    project: pushed.project.clone(),
                    message: format!("{e:#}"),
                });
                project_error = Some(format!("{e:#}"));
            }
        }
    }

    if let Some(message) = project_error {
        let mut withheld = Report::default();
        withheld.add(
            State::PublishWithheld
                .of(project_label(&pushed.project))
                .project_repo()
                .checkout(&pushed.project_dir)
                .listing(message.lines())
                .bind("<b>", pushed.branch.as_str())
                .only(&[ChoiceId::C7, ChoiceId::C17])
                .judge(pvcs),
        );
        withheld.note(format!(
            "projects/{} {} was not pushed; the members' branches that were pushed are published",
            pushed.project, pushed.branch
        ));
        let refusal = withheld
            .into_result()
            .expect_err("a rejected project push is judged withheld");
        if output.is_json() {
            print_envelope(json_outcomes, &refusal, reported, ctx);
        }
        return Err(refusal);
    }

    if output.is_json() {
        let envelope = PushJsonOutput {
            schema_url: PUSH_SCHEMA_URL.to_string(),
            outcomes: json_outcomes,
            refused: Vec::new(),
            reported: reported.output(),
            resolution: Some(ctx.resolution()),
        };
        let out = serde_json::to_string_pretty(&envelope)
            .context("failed to serialize push outcomes to JSON")?;
        outln!("{out}");
    } else {
        outln!("rwv push: done");
    }
    Ok(())
}

/// The pins the project branch's lock holds, judged against what the remotes
/// hold after the members were pushed: a feature's `owned` and `fork` pins by
/// a remote-tracking branch of the url remote or the branch's push target, a
/// declared branch's by the push branch of the branch each member declares,
/// and a `dependency` or `reference` member's by a remote-tracking branch of
/// its url remote.
fn gate_reachability(pushed: &Pushed, members: &[Member], fetched: &Fetched, gate: &mut Report) {
    for member in members {
        let vcs = member.vcs.as_ref();
        let path = &member.path;
        let judging = |state: State| {
            state
                .of(path)
                .member()
                .checkout(&member.dir)
                .bind("<m>", path.as_str())
        };
        let Some(entry) = pushed.lock.as_ref().and_then(|lock| lock.get_entry(path)) else {
            gate.add(judging(State::PublishNoPin).judge(vcs));
            continue;
        };
        let pin = match member.dir.exists() {
            true => vcs
                .resolve_revision(&member.dir, entry.version.as_str())
                .ok(),
            false => None,
        };
        let Some(pin) = pin else {
            gate.add(
                judging(State::PinUnreachable)
                    .subject(entry.version.short_display())
                    .judge(vcs),
            );
            continue;
        };
        let url_remote = fetched.url_remote(path);
        let reached_by = |remote: Option<&RemoteHandle>| {
            remote.is_some_and(|remote| {
                vcs.reached_by_remote(&member.dir, &pin, remote)
                    .unwrap_or(false)
            })
        };
        let reached = match member.entry.role {
            Role::Owned | Role::Fork if pushed.is_feature() => {
                let target = vcs
                    .push_target_of(&member.dir, &pushed.raw, url_remote)
                    .ok()
                    .flatten();
                reached_by(url_remote) || reached_by(target.as_ref())
            }
            Role::Owned | Role::Fork => {
                let declared = RawRefName::new(member.entry.branch.local_counterpart().as_str());
                let target = vcs
                    .push_target_of(&member.dir, &declared, url_remote)
                    .ok()
                    .flatten();
                vcs.remote_copies(&member.dir, &declared)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|copy| Some(&copy.remote) == target.as_ref())
                    .any(|copy| {
                        vcs.is_ancestor(&member.dir, &pin, &copy.tip)
                            .unwrap_or(false)
                    })
            }
            Role::Dependency | Role::Reference => reached_by(url_remote),
        };
        if reached {
            continue;
        }
        let reaching: Vec<String> = vcs
            .branch_tips(&member.dir)
            .unwrap_or_default()
            .into_iter()
            .filter(|tip| tip.label.contains('/'))
            .filter(|tip| {
                vcs.is_ancestor(&member.dir, &pin, &tip.tip)
                    .unwrap_or(false)
            })
            .map(|tip| tip.label)
            .collect();
        let listing = if reaching.is_empty() {
            format!(
                "the pin {} is reached by no remote-tracking branch",
                pin.id().short()
            )
        } else {
            format!(
                "the pin {} is reached only by {}",
                pin.id().short(),
                reaching.join(", ")
            )
        };
        gate.add(
            judging(State::PublishWithheld)
                .listing([listing])
                .bind("<b>", member.entry.branch.local_counterpart().as_str())
                .only(&[ChoiceId::C7, ChoiceId::C27, ChoiceId::C17])
                .judge(vcs),
        );
    }
}

/// Per-repo worker: push one member branch to its push target. All
/// user-facing output is routed through `reporter`, which prefixes `[<repo>]`
/// and serializes writes under `-j > 1`.
fn push_one(
    vcs: &dyn Vcs,
    item: &PushPlanItem,
    repo_dir: &Path,
    reporter: &Reporter<'_>,
) -> PushOutcome {
    reporter.out(&format!(
        "rwv push: pushing {} ({} -> {})",
        item.repo_path.as_str(),
        item.branch,
        item.target,
    ));
    match publish_ref(
        vcs,
        repo_dir,
        &item.target,
        &item.publish_ref,
        &item.replace,
    ) {
        Ok(()) => {
            let branch = RawRefName::new(item.branch.clone());
            match give_upstream(vcs, repo_dir, &branch, &item.target) {
                Ok(()) => PushOutcome::Pushed,
                Err(e) => PushOutcome::Failed(format!(
                    "{}: pushed, but could not record the upstream: {e:#}",
                    item.repo_path.as_str()
                )),
            }
        }
        Err(e) => PushOutcome::Failed(format!("{}: {e:#}", item.repo_path.as_str())),
    }
}

/// Why a publish did not happen.
#[derive(Debug)]
enum PublishFailure {
    Backup(crate::vcs::VcsError),
    Push(crate::vcs::VcsError),
}

impl std::fmt::Display for PublishFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backup(e) => write!(f, "could not keep the remote tip as a backup first: {e:#}"),
            Self::Push(e) => write!(f, "the push failed: {e:#}"),
        }
    }
}

/// Publish `r` to `target`. A leased replace keeps the tip it replaces as a
/// backup of the branch first, so the remote's commits stay reachable here.
fn publish_ref(
    vcs: &dyn Vcs,
    repo: &Path,
    target: &RemoteHandle,
    r: &PublishRef,
    replace: &Replace,
) -> Result<(), PublishFailure> {
    if let Replace::Leased(lease) = replace {
        vcs.create_backup(
            repo,
            &BackupSource::Branch(r.name().clone()),
            lease.expected(),
            None,
        )
        .map_err(PublishFailure::Backup)?;
    }
    vcs.push_ref(repo, target, r, replace)
        .map_err(PublishFailure::Push)
}

/// The pushed branch takes in from its push branch when it takes in from
/// nothing yet; one that already has an upstream keeps it.
fn give_upstream(
    vcs: &dyn Vcs,
    repo: &Path,
    branch: &RawRefName,
    target: &RemoteHandle,
) -> Result<(), crate::vcs::VcsError> {
    if vcs.upstream_of(repo, branch)?.is_some() {
        return Ok(());
    }
    vcs.set_upstream(
        repo,
        branch,
        &crate::vcs::Upstream {
            remote: target.clone(),
            branch: branch.clone(),
        },
    )
}

/// Push publishes the project repo's last commit but plans from `rwv.toml`
/// and `rwv.lock` as they stand on disk. The two agree only while nothing
/// tracked is dirty and neither of those files is untracked.
fn refuse_uncommitted_project_state(
    vcs: &dyn Vcs,
    project_dir: &Path,
    project_name: &ProjectName,
) -> anyhow::Result<()> {
    let uncommitted = crate::switch::uncommitted_names(vcs, project_dir)
        .with_context(|| format!("failed to read project repo status for {project_name}"))?;
    if uncommitted.is_empty() {
        return Ok(());
    }
    let only_lock_files = uncommitted
        .iter()
        .all(|name| name == Manifest::FILE_NAME || name == LockFile::FILE_NAME);
    let state = if only_lock_files {
        State::EditsLockFilesUncommitted
    } else {
        State::EditsDirty
    };
    let mut report = Report::default();
    report.add(
        state
            .of(project_rel_path(project_name))
            .listing(uncommitted)
            .checkout(project_dir)
            .judge(vcs),
    );
    report.into_result()
}
