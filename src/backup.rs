//! `rwv backup list`, `drop` and `prune`: the backups a weave's repos hold, and
//! the pin rule that governs dropping them.
//!
//! A backup is kept by the VCS under its own spelling; this module holds only
//! the names the seam handed it. What a drop may not do is leave a live pin
//! reached by no branch, remote-tracking branch or backup, and a pin is live
//! in every project of the weave, not only the one the verb runs in.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::add_remove::find_project;
use crate::diagnosis::State;
use crate::manifest::{Manifest, ProjectName, RepoOperand, RepoPath};
use crate::parallel::OutputMode;
use crate::pins::{live_pins, LivePin};
use crate::vcs::{
    project_vcs, vcs_for, Backup, BackupMark, BackupName, CommitId, ListedBackup, RemoteTracking,
    Vcs, VcsError,
};
use crate::workspace::{project_dir, WorkspaceContext};

pub const BACKUP_SCHEMA_URL: &str = crate::schema_url::schema_url!("backup");

const LISTED_COMMITS_CAP: usize = 20;

/// One repo whose backups a verb acts on: its operand spelling, where its
/// clone sits, and the backend that keeps its refs.
struct BackupRepo {
    operand: RepoOperand,
    member: Option<RepoPath>,
    dir: PathBuf,
    vcs: Box<dyn Vcs>,
    /// The url a member declares, whose remote a pruning fetch adds when the
    /// clone lacks it.
    url: Option<crate::registry::CloneUrl>,
}

fn repos_of_project(ctx: &WorkspaceContext) -> anyhow::Result<(ProjectName, Vec<BackupRepo>)> {
    let (project, active_dir) = find_project(ctx)?;
    let manifest = Manifest::from_path(&active_dir.join(Manifest::FILE_NAME))
        .with_context(|| format!("failed to load the manifest of project `{project}`"))?;
    let primary = ctx.primary_path();
    let mut repos = vec![BackupRepo {
        operand: RepoOperand::Project(project.clone()),
        member: None,
        dir: project_dir(primary, &project),
        vcs: project_vcs(),
        url: None,
    }];
    for (path, entry) in manifest.iter_entries() {
        repos.push(BackupRepo {
            operand: RepoOperand::Member(path.clone()),
            member: Some(path.clone()),
            dir: primary.join(path.as_path()),
            vcs: vcs_for(entry.vcs_type),
            url: crate::registry::resolve_to_clone_info(&entry.url)
                .ok()
                .map(|info| info.url),
        });
    }
    repos.retain(|repo| repo.vcs.is_repo(&repo.dir));
    Ok((project, repos))
}

fn repo_named(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    operand: &RepoOperand,
) -> anyhow::Result<BackupRepo> {
    match operand {
        RepoOperand::Project(named) => {
            anyhow::ensure!(
                named == project,
                "`{operand}` is the repo of project `{named}`, not of `{project}`; \
                 name this project's repo as `{}`",
                RepoOperand::Project(project.clone())
            );
            Ok(BackupRepo {
                operand: operand.clone(),
                member: None,
                dir: project_dir(ctx.primary_path(), project),
                vcs: project_vcs(),
                url: None,
            })
        }
        RepoOperand::Member(path) => {
            let dir = ctx.primary_path().join(path.as_path());
            let vcs = crate::vcs::probe_vcs();
            anyhow::ensure!(
                vcs.is_repo(&dir),
                "`{operand}` is no repo in this weave; `rwv backup list` names the repos \
                 that hold backups"
            );
            Ok(BackupRepo {
                operand: operand.clone(),
                member: Some(path.clone()),
                dir,
                vcs,
                url: None,
            })
        }
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct HeldCommitOutput {
    pub sha: CommitId,
    pub short: String,
    pub subject: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct BackupOutput {
    /// The name `rwv backup drop` takes.
    pub name: String,
    /// The commit the backup holds.
    pub tip: CommitId,
    /// `rebased` for a branch a rebase rewrote, `abandoned` for a tip
    /// `rwv abort --abandon-foreign-tip` gave up; absent for any other.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mark: Option<BackupMark>,
    /// A branch reaches everything the backup holds, so dropping it loses
    /// nothing.
    pub redundant: bool,
    /// The commits the backup holds that no branch reaches, newest first.
    pub held: Vec<HeldCommitOutput>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RepoBackupsOutput {
    /// The repo as `rwv backup drop` names it: its workspace-relative path, or
    /// `projects/<name>` for the project repo.
    pub path: String,
    pub backups: Vec<BackupOutput>,
}

/// The envelope `rwv backup list --json` prints.
#[derive(Debug, Serialize, JsonSchema)]
pub struct BackupJsonOutput {
    #[serde(rename = "$schema")]
    pub schema_url: String,
    /// Only repos that hold a backup.
    pub repos: Vec<RepoBackupsOutput>,
}

fn output_of(listed: &ListedBackup) -> BackupOutput {
    BackupOutput {
        name: listed.backup.name.to_string(),
        tip: listed.backup.tip.id().clone(),
        mark: listed.backup.mark,
        redundant: listed.redundant(),
        held: listed
            .held
            .iter()
            .map(|c| HeldCommitOutput {
                sha: c.id.clone(),
                short: c.id.short().to_owned(),
                subject: c.subject.clone(),
            })
            .collect(),
    }
}

fn listing_error(repo: &BackupRepo, e: VcsError) -> anyhow::Error {
    anyhow::Error::new(e).context(format!("{}: could not list its backups", repo.operand))
}

/// Every backup the project's repos hold, as `rwv status` reports them: the
/// repos that hold one, with what each backup holds.
pub(crate) fn held_by_project(ctx: &WorkspaceContext) -> anyhow::Result<Vec<RepoBackupsOutput>> {
    let (_, repos) = repos_of_project(ctx)?;
    let mut held = Vec::new();
    for repo in &repos {
        let listed = repo
            .vcs
            .list_backups(&repo.dir, RemoteTracking::Counted)
            .map_err(|e| listing_error(repo, e))?;
        if !listed.is_empty() {
            held.push(RepoBackupsOutput {
                path: repo.operand.to_string(),
                backups: listed.iter().map(output_of).collect(),
            });
        }
    }
    Ok(held)
}

/// `rwv backup list`: every backup the project's repos hold.
pub fn run_list(ctx: &WorkspaceContext, output: OutputMode) -> anyhow::Result<()> {
    let (_, repos) = repos_of_project(ctx)?;
    let mut held: Vec<(String, Vec<ListedBackup>)> = Vec::new();
    for repo in &repos {
        let listed = repo
            .vcs
            .list_backups(&repo.dir, RemoteTracking::Counted)
            .map_err(|e| listing_error(repo, e))?;
        if !listed.is_empty() {
            held.push((repo.operand.to_string(), listed));
        }
    }
    if output.is_json() {
        let envelope = BackupJsonOutput {
            schema_url: BACKUP_SCHEMA_URL.to_owned(),
            repos: held
                .iter()
                .map(|(path, listed)| RepoBackupsOutput {
                    path: path.clone(),
                    backups: listed.iter().map(output_of).collect(),
                })
                .collect(),
        };
        outln!("{}", serde_json::to_string_pretty(&envelope)?);
        return Ok(());
    }
    if held.is_empty() {
        outln!("no backups");
        return Ok(());
    }
    for (path, listed) in &held {
        outln!("{path}");
        for entry in listed {
            outln!("  {}", backup_line(entry));
            for commit in entry.held.iter().take(LISTED_COMMITS_CAP) {
                outln!("    {} {}", commit.id.short(), commit.subject);
            }
            if entry.held.len() > LISTED_COMMITS_CAP {
                outln!(
                    "    ... and {} more (`rwv backup list --json` lists them all)",
                    entry.held.len() - LISTED_COMMITS_CAP
                );
            }
        }
    }
    Ok(())
}

fn backup_line(entry: &ListedBackup) -> String {
    let mut line = format!("{} -> {}", entry.backup.name, entry.backup.tip.id().short());
    if let Some(mark) = entry.backup.mark {
        line.push_str(&format!(" [{}]", crate::wire_token::wire_token(&mark)));
    }
    if entry.redundant() {
        line.push_str(" [redundant]");
    }
    line
}

/// Refuse to drop `dropping` from `repo` when that would leave a live pin
/// reached by no branch, remote-tracking branch or backup, naming the locks.
fn apply_pin_rule(
    repo: &BackupRepo,
    dropping: &[&Backup],
    pins: &BTreeMap<RepoPath, Vec<LivePin>>,
) -> anyhow::Result<Option<crate::diagnosis::Judged>> {
    let Some(member) = &repo.member else {
        return Ok(None);
    };
    let Some(live) = pins.get(member) else {
        return Ok(None);
    };
    let mut commits: Vec<CommitId> = live.iter().map(|p| p.commit.clone()).collect();
    commits.sort();
    commits.dedup();
    let names: Vec<BackupName> = dropping.iter().map(|b| b.name.clone()).collect();
    let stranded = repo
        .vcs
        .left_unreached_by_dropping(&repo.dir, &commits, &names)
        .with_context(|| format!("{}: could not judge the pin rule", repo.operand))?;
    if stranded.is_empty() {
        return Ok(None);
    }
    let listing: Vec<String> = stranded
        .iter()
        .map(|commit| {
            let locks: Vec<&str> = live
                .iter()
                .filter(|p| p.commit == *commit)
                .map(|p| p.held_by.as_str())
                .collect();
            format!("{} is pinned by {}", commit.short(), locks.join(", "))
        })
        .collect();
    Ok(Some(
        State::PinWouldBeUnreachable
            .of(&repo.operand)
            .listing(listing)
            .checkout(&repo.dir)
            .judge(repo.vcs.as_ref()),
    ))
}

fn drop_backups(repo: &BackupRepo, dropping: &[&Backup]) -> anyhow::Result<()> {
    for backup in dropping {
        repo.vcs
            .drop_backup(&repo.dir, &backup.name)
            .with_context(|| format!("{}: could not drop {}", repo.operand, backup.name))?;
        outln!(
            "dropped {}: {} (held {})",
            repo.operand,
            backup.name,
            backup.tip.id().short()
        );
    }
    Ok(())
}

fn refuse_if_any(refusals: Vec<crate::diagnosis::Judged>) -> anyhow::Result<()> {
    let mut report = crate::diagnosis::Report::default();
    for judged in refusals {
        report.add(judged);
    }
    report.into_result()
}

/// `rwv backup drop <repo> [<name>...]`: the named backups of one repo, or all
/// of them.
pub fn run_drop(
    ctx: &WorkspaceContext,
    repo: &RepoOperand,
    names: &[String],
) -> anyhow::Result<()> {
    crate::op_state::check_no_op_in_progress(&[ctx.active_path()])?;
    let (project, _) = repos_of_project(ctx)?;
    let target = repo_named(ctx, &project, repo)?;
    let listed = target
        .vcs
        .list_backups(&target.dir, RemoteTracking::Counted)
        .map_err(|e| listing_error(&target, e))?;
    let chosen: Vec<&Backup> = if names.is_empty() {
        listed.iter().map(|l| &l.backup).collect()
    } else {
        let mut chosen = Vec::new();
        for name in names {
            match listed.iter().find(|l| l.backup.name.as_str() == name) {
                Some(l) => chosen.push(&l.backup),
                None => eprintln!(
                    "{}: no backup named {name}; already dropped, or not a name \
                     `rwv backup list` shows",
                    target.operand
                ),
            }
        }
        chosen
    };
    if chosen.is_empty() {
        return Ok(());
    }
    let pins = live_pins(ctx.primary_path())?;
    let refusal = apply_pin_rule(&target, &chosen, &pins)?;
    refuse_if_any(refusal.into_iter().collect())?;
    drop_backups(&target, &chosen)
}

/// `rwv backup prune`: after a pruning fetch, every redundant backup of every
/// repo.
pub fn run_prune(ctx: &WorkspaceContext) -> anyhow::Result<()> {
    crate::op_state::check_no_op_in_progress(&[ctx.active_path()])?;
    let (_, repos) = repos_of_project(ctx)?;
    let mut judged: Vec<(&BackupRepo, Vec<ListedBackup>)> = Vec::new();
    let mut taken = crate::diagnosis::Report::default();
    let mut planned = Vec::with_capacity(repos.len());
    for repo in &repos {
        let Some(url) = &repo.url else {
            planned.push(None);
            continue;
        };
        match crate::url_remote::find(
            repo.vcs.as_ref(),
            &repo.operand,
            "backup prune",
            &repo.dir,
            url,
        )?
        .plan()
        {
            Ok(plan) => planned.push(plan.add),
            Err(judged) => {
                taken.add(*judged);
                planned.push(None);
            }
        }
    }
    taken.into_result()?;
    for (repo, add) in repos.iter().zip(&planned) {
        let unconfirmed = crate::fetch::pruning_fetch(repo.vcs.as_ref(), &repo.dir, add.as_ref())
            .with_context(|| format!("{}: the pruning fetch failed", repo.operand))?
            .unconfirmed;
        for remote in &unconfirmed {
            eprintln!(
                "{}: remote {} unconfirmed ({}); its remote-tracking branches do not count \
                 toward a backup being redundant",
                repo.operand, remote.remote, remote.cause
            );
        }
        let counted = if unconfirmed.is_empty() {
            RemoteTracking::Counted
        } else {
            RemoteTracking::Ignored
        };
        let redundant: Vec<ListedBackup> = repo
            .vcs
            .list_backups(&repo.dir, counted)
            .map_err(|e| listing_error(repo, e))?
            .into_iter()
            .filter(ListedBackup::redundant)
            .collect();
        if !redundant.is_empty() {
            judged.push((repo, redundant));
        }
    }
    if judged.is_empty() {
        outln!("nothing to prune");
        return Ok(());
    }
    let pins = live_pins(ctx.primary_path())?;
    let mut refusals = Vec::new();
    for (repo, redundant) in &judged {
        let dropping: Vec<&Backup> = redundant.iter().map(|l| &l.backup).collect();
        refusals.extend(apply_pin_rule(repo, &dropping, &pins)?);
    }
    refuse_if_any(refusals)?;
    for (repo, redundant) in &judged {
        let dropping: Vec<&Backup> = redundant.iter().map(|l| &l.backup).collect();
        drop_backups(repo, &dropping)?;
    }
    Ok(())
}
