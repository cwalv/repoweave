//! `rwv fetch` — the pruning fetch of every remote of every repo.
//!
//! It brings remote branches and tags into each repo's object store, drops
//! the remote-tracking branches a remote no longer has, and reports what
//! moved. No local branch, no checkout and no lock moves: `rwv update` and
//! `rwv pull` are the verbs that act on what it brought.

use crate::diagnosis::Judged;
use crate::fetch_claim::FetchClaim;
use crate::manifest::{Manifest, ProjectName};
use crate::member_selection::MemberSelection;
use crate::parallel::{run_in_parallel, OutputMode};
use crate::refusal::RefusalKind;
use crate::vcs::{
    project_vcs, vcs_for, AbsentRemote, BranchChange, PruningFetch, RemoteBranchMove, Vcs,
};
use crate::workspace::{project_dir, Checkout, Resolution, WorkspaceContext};
use anyhow::Context;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Schema URL embedded in `rwv fetch --json` output. Pins to the committed
/// artifact under `docs/reference/schemas/fetch.json`.
pub const FETCH_SCHEMA_URL: &str = crate::schema_url::schema_url!("fetch");

/// How a remote-tracking branch moved, as `rwv fetch --json` spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum BranchMoveKind {
    /// The remote has the branch and the repo had no record of it.
    Created,
    /// The new tip descends from the old one.
    Advanced,
    /// The new tip does not descend from the old one. The old tip is kept as
    /// the branch's superseded tip.
    Rewritten,
    /// The remote no longer has the branch.
    Pruned,
}

/// One remote-tracking branch a fetch moved.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BranchMoveOutput {
    /// The branch, qualified by its remote (`origin/main`).
    pub branch: String,
    #[serde(rename = "move")]
    pub kind: BranchMoveKind,
    /// The tip before the fetch. Absent for a `created` branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The tip after the fetch. Absent for a `pruned` branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

impl BranchMoveOutput {
    fn of(moved: &RemoteBranchMove) -> Self {
        let (kind, from, to) = match &moved.change {
            BranchChange::Created { to } => (BranchMoveKind::Created, None, Some(to.clone())),
            BranchChange::Advanced { from, to } => (
                BranchMoveKind::Advanced,
                Some(from.clone()),
                Some(to.clone()),
            ),
            BranchChange::Rewritten { from, to } => (
                BranchMoveKind::Rewritten,
                Some(from.clone()),
                Some(to.clone()),
            ),
            BranchChange::Pruned { from } => (BranchMoveKind::Pruned, Some(from.clone()), None),
        };
        Self {
            branch: moved.branch.clone(),
            kind,
            from,
            to,
        }
    }

    fn line(&self) -> String {
        let short = |tip: &Option<String>| {
            tip.as_deref()
                .map(|t| crate::vcs::short_revision(t).to_owned())
                .unwrap_or_default()
        };
        match self.kind {
            BranchMoveKind::Created => format!("{} created at {}", self.branch, short(&self.to)),
            BranchMoveKind::Advanced => {
                format!("{} {}..{}", self.branch, short(&self.from), short(&self.to))
            }
            BranchMoveKind::Rewritten => format!(
                "{} rewritten {}..{} (the previous tip is kept as superseded)",
                self.branch,
                short(&self.from),
                short(&self.to)
            ),
            BranchMoveKind::Pruned => {
                format!("{} pruned (was {})", self.branch, short(&self.from))
            }
        }
    }
}

/// How a repo's fetch ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FetchStatus {
    /// Every remote answered.
    Fetched,
    /// A remote could not be completed, the cause named in `message`. Later
    /// verbs read the repo's remote-tracking branches as last fetched.
    Unconfirmed,
    /// The repo was not fetched, the reason named in `message`.
    Skipped,
}

/// One repo's record in `rwv fetch --json`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FetchedRepoOutput {
    /// The repo's path: a member's manifest path, or `projects/<name>` for the
    /// project repo.
    pub path: String,
    pub absolute_path: crate::path_spelling::WirePath,
    pub status: FetchStatus,
    /// Each remote-tracking branch that moved, by branch.
    pub moved: Vec<BranchMoveOutput>,
    /// The cause of an `unconfirmed` fetch, or the reason for a `skipped` one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Each commit some live lock pins that no remote-tracking branch of this
    /// repo reaches after the fetch (`pin` off remote), with the locks that
    /// hold it and the choices that apply in this workspace.
    pub pins_off_remote: Vec<crate::diagnosis::JudgedOutput>,
    /// The refusal token of an `unconfirmed` repo whose cause is a refusal,
    /// resolvable with `rwv explain <token>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

/// Top-level envelope for `rwv fetch --json`.
///
/// Shape: `{ "$schema": "<url>", "repos": [<FetchedRepoOutput>, ...] }`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FetchJsonOutput {
    #[serde(rename = "$schema")]
    pub schema: String,
    /// The repos sorted by path: the project repo first, then the members.
    pub repos: Vec<FetchedRepoOutput>,
    /// Resolved workspace coordinates (weave root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
}

impl FetchJsonOutput {
    fn print(&self) -> anyhow::Result<()> {
        let out = serde_json::to_string_pretty(self).context("failed to serialize fetch output")?;
        outln!("{out}");
        Ok(())
    }
}

/// Fetch every remote of the repo at `dir` under the claim on its object
/// store, recording each superseded tip, and report what moved. `add` is the
/// url remote the repo lacks, added first under the same claim.
pub(crate) fn pruning_fetch(
    vcs: &dyn Vcs,
    dir: &Path,
    add: Option<&AbsentRemote>,
) -> anyhow::Result<PruningFetch> {
    FetchClaim::hold(vcs, dir, |claim| {
        if let Some(absent) = add {
            vcs.add_url_remote(dir, absent)?;
        }
        claim.fetch_pruning(vcs, dir, None, crate::vcs::Superseded::Kept)
    })
}

/// A pruning fetch of every remote of the repo at `dir` that writes nothing
/// but its remote-tracking branches.
pub(crate) fn refreshing_fetch(vcs: &dyn Vcs, dir: &Path) -> anyhow::Result<PruningFetch> {
    FetchClaim::hold(vcs, dir, |claim| {
        claim.fetch_pruning(vcs, dir, None, crate::vcs::Superseded::Forgotten)
    })
}

/// A pruning fetch of only the remotes `remotes` names.
pub(crate) fn pruning_fetch_of(
    vcs: &dyn Vcs,
    dir: &Path,
    remotes: &[crate::vcs::RemoteHandle],
) -> anyhow::Result<PruningFetch> {
    FetchClaim::hold(vcs, dir, |claim| {
        claim.fetch_pruning(vcs, dir, Some(remotes), crate::vcs::Superseded::Kept)
    })
}

struct Target {
    path: String,
    member: Option<crate::manifest::RepoPath>,
    dir: PathBuf,
    vcs: Box<dyn Vcs>,
    /// The url the member declares: its url remote is added before the fetch
    /// when the clone lacks it. The project repo has none.
    url: Option<crate::registry::CloneUrl>,
}

/// The repos `rwv fetch` covers in `ctx`'s project: the project repo, unless a
/// selector narrows the fetch to members, then each selected member.
fn targets(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    selection: &MemberSelection,
) -> anyhow::Result<Vec<Target>> {
    let manifest_path = project_dir(ctx.active_path(), project).join(Manifest::FILE_NAME);
    let manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to read manifest from {}", manifest_path.display()))?;
    let mut targets = Vec::new();
    if selection.names_every_member() {
        targets.push(Target {
            path: crate::workspace::project_rel_path(project).to_string(),
            member: None,
            dir: project_dir(ctx.primary_path(), project),
            vcs: project_vcs(),
            url: None,
        });
    }
    targets.extend(
        manifest
            .iter_entries()
            .filter(|(path, entry)| selection.covers(path, entry.role))
            .map(|(path, entry)| Target {
                path: path.to_string(),
                member: Some(path.clone()),
                dir: ctx.primary_path().join(path.as_path()),
                vcs: vcs_for(entry.vcs_type),
                url: crate::registry::resolve_to_clone_info(&entry.url)
                    .ok()
                    .map(|info| info.url),
            }),
    );
    Ok(targets)
}

/// The url remote each member target lacks, `None` for the targets that have
/// theirs; a name held by another url refuses, for every target that has one,
/// before any remote is added or fetched.
fn url_remotes_to_add(targets: &[Target]) -> anyhow::Result<Vec<Option<AbsentRemote>>> {
    let mut report = crate::diagnosis::Report::default();
    let mut adds = Vec::with_capacity(targets.len());
    for target in targets {
        let planned = match (&target.url, target.dir.exists()) {
            (Some(url), true) => crate::url_remote::find(
                target.vcs.as_ref(),
                &target.path,
                "fetch",
                &target.dir,
                url,
            )?
            .plan(),
            _ => {
                adds.push(None);
                continue;
            }
        };
        match planned {
            Ok(planned) => adds.push(planned.add),
            Err(judged) => {
                report.add(*judged);
                adds.push(None);
            }
        }
    }
    report.into_result()?;
    Ok(adds)
}

fn project_of(ctx: &WorkspaceContext) -> anyhow::Result<ProjectName> {
    match &ctx.checkout {
        Checkout::Primary { .. } => Ok(ctx.require_active_project_on_disk()?.clone()),
        Checkout::Workweave { project, .. } => Ok(project.clone()),
    }
}

/// Run `rwv fetch` for the project `ctx` resolved: the pruning fetch of the
/// project repo and each selected member, `jobs` at a time.
///
/// The canonical clone is fetched whether the verb runs in the primary or in
/// a workweave, whose worktrees draw from it. Nothing is checked out or
/// moved, so every checkout, branch and the lock read the same afterwards.
pub fn run_fetch(
    ctx: &WorkspaceContext,
    selection: &MemberSelection,
    jobs: usize,
    output: OutputMode,
) -> anyhow::Result<()> {
    let fetched = fetch_repos(ctx, selection, jobs, output)?;
    let total = fetched.records.len();
    let unconfirmed = fetched.unconfirmed;
    if output.is_json() {
        FetchJsonOutput {
            schema: FETCH_SCHEMA_URL.to_owned(),
            repos: fetched.records,
            resolution: Some(ctx.resolution()),
        }
        .print()?;
    } else {
        outln!(
            "rwv fetch: done ({} repo(s) fetched)",
            fetched
                .records
                .iter()
                .filter(|r| r.status == FetchStatus::Fetched)
                .count()
        );
    }
    if unconfirmed > 0 {
        crate::refuse!(
            RefusalKind::PartialRunAborted,
            "fetch could not complete {unconfirmed} repo(s) out of {total}; they read as last \
             fetched"
        );
    }
    Ok(())
}

/// What [`fetch_repos`] fetched.
pub(crate) struct FetchedRepos {
    pub(crate) records: Vec<FetchedRepoOutput>,
    pub(crate) unconfirmed: usize,
}

/// The pruning fetch of the repos `selection` covers, each record printed as
/// it is reported under text output.
pub(crate) fn fetch_repos(
    ctx: &WorkspaceContext,
    selection: &MemberSelection,
    jobs: usize,
    output: OutputMode,
) -> anyhow::Result<FetchedRepos> {
    let project = project_of(ctx)?;
    let targets = targets(ctx, &project, selection)?;
    let adds = url_remotes_to_add(&targets)?;

    let results: Vec<anyhow::Result<Option<PruningFetch>>> =
        run_in_parallel(&targets, jobs, |idx, target| {
            if !target.dir.exists() {
                return Ok(None);
            }
            pruning_fetch(target.vcs.as_ref(), &target.dir, adds[idx].as_ref()).map(Some)
        });

    let off_remote = OffRemote::open(ctx, &project);
    let mut records = Vec::new();
    let mut unconfirmed = 0usize;
    for (target, result) in targets.iter().zip(results) {
        let absolute_path = crate::path_spelling::WirePath::of(&target.dir);
        let mut pins_off = Vec::new();
        let record = match result {
            Ok(None) => FetchedRepoOutput {
                path: target.path.clone(),
                absolute_path,
                status: FetchStatus::Skipped,
                moved: Vec::new(),
                message: Some(
                    "not on disk; `rwv update` clones the members that are missing".to_owned(),
                ),
                refusal: None,
                pins_off_remote: Vec::new(),
            },
            Ok(Some(fetched)) => {
                let unreached = unreached_remotes(&fetched.unconfirmed);
                if unreached.is_some() {
                    unconfirmed += 1;
                } else {
                    pins_off = off_remote.pins_off(target);
                }
                FetchedRepoOutput {
                    path: target.path.clone(),
                    absolute_path,
                    status: if unreached.is_some() {
                        FetchStatus::Unconfirmed
                    } else {
                        FetchStatus::Fetched
                    },
                    moved: fetched.moves.iter().map(BranchMoveOutput::of).collect(),
                    message: unreached,
                    refusal: None,
                    pins_off_remote: pins_off.iter().map(Judged::output).collect(),
                }
            }
            Err(error) => {
                unconfirmed += 1;
                FetchedRepoOutput {
                    path: target.path.clone(),
                    absolute_path,
                    status: FetchStatus::Unconfirmed,
                    moved: Vec::new(),
                    message: Some(format!("{error:#}")),
                    refusal: crate::refusal::kind_of(&error).map(RefusalKind::token),
                    pins_off_remote: Vec::new(),
                }
            }
        };
        if output == OutputMode::Text {
            print_record(&record);
            if !pins_off.is_empty() {
                let mut report = crate::diagnosis::Report::default();
                for line in pins_off {
                    report.add(line);
                }
                outln!("{}", report.render("fetch").trim_end_matches('\n'));
            }
        }
        records.push(record);
    }
    Ok(FetchedRepos {
        records,
        unconfirmed,
    })
}

/// What `pin off remote` is judged against: the pins every live lock holds,
/// and how the lock this workspace has checked out holds them.
struct OffRemote<'a> {
    ctx: &'a WorkspaceContext,
    project: ProjectName,
    live: std::collections::BTreeMap<crate::manifest::RepoPath, Vec<crate::pins::LivePin>>,
    checked_out: Option<crate::manifest::LockFile>,
    mode: Option<crate::alignment::Mode>,
}

impl<'a> OffRemote<'a> {
    fn open(ctx: &'a WorkspaceContext, project: &ProjectName) -> Self {
        let project_dir = project_dir(ctx.active_path(), project);
        let lock_path = crate::manifest::LockFile::path_in(&project_dir);
        Self {
            ctx,
            project: project.clone(),
            live: crate::pins::live_pins(ctx.primary_path()).unwrap_or_default(),
            checked_out: lock_path
                .exists()
                .then(|| crate::manifest::LockFile::from_state(&lock_path).ok())
                .flatten(),
            mode: crate::alignment::Aligner::open(
                ctx.active_path(),
                project,
                crate::alignment::Accounting::default(),
            )
            .ok()
            .map(|aligner| aligner.mode),
        }
    }

    /// The pins of the member `target` is that no remote-tracking branch of
    /// its clone reaches, as last fetched.
    fn pins_off(&self, target: &Target) -> Vec<Judged> {
        let Some(member) = &target.member else {
            return Vec::new();
        };
        let Some(live) = self.live.get(member) else {
            return Vec::new();
        };
        let held = self
            .checked_out
            .as_ref()
            .and_then(|lock| lock.get_entry(member))
            .and_then(|entry| entry.version.as_commit_id());
        let holding = |commit: &crate::vcs::CommitId| {
            if held.as_ref() != Some(commit) {
                return crate::pins::Holding::NotHeld;
            }
            match self.mode {
                Some(crate::alignment::Mode::Feature { .. }) => crate::pins::Holding::Feature,
                Some(crate::alignment::Mode::Declared) => crate::pins::Holding::Declared,
                Some(crate::alignment::Mode::Detached) | None => crate::pins::Holding::NotHeld,
            }
        };
        match crate::pins::off_remote(
            target.vcs.as_ref(),
            &target.dir,
            member,
            live,
            holding,
            || crate::feature_sync::reset_offer(self.ctx, &self.project, member),
        ) {
            Ok(lines) => lines,
            Err(e) => {
                eprintln!(
                    "rwv fetch: {}: could not judge its pins: {e:#}",
                    target.path
                );
                Vec::new()
            }
        }
    }
}

fn unreached_remotes(unconfirmed: &[crate::vcs::UnconfirmedRemote]) -> Option<String> {
    if unconfirmed.is_empty() {
        return None;
    }
    Some(
        unconfirmed
            .iter()
            .map(|u| format!("remote {} could not be fetched: {}", u.remote, u.cause))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

fn print_record(record: &FetchedRepoOutput) {
    match record.status {
        FetchStatus::Fetched if record.moved.is_empty() => {
            outln!("rwv fetch: {}: nothing moved", record.path);
        }
        FetchStatus::Fetched => {
            for moved in &record.moved {
                outln!("rwv fetch: {}: {}", record.path, moved.line());
            }
        }
        FetchStatus::Skipped => {
            outln!(
                "rwv fetch: {}: skipped: {}",
                record.path,
                record.message.as_deref().unwrap_or("no reason reported")
            );
        }
        FetchStatus::Unconfirmed => {
            eprintln!(
                "rwv fetch: {}: unconfirmed: {}",
                record.path,
                record.message.as_deref().unwrap_or("no cause reported")
            );
        }
    }
}
