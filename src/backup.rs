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
    project_vcs, vcs_for, Backup, BackupMark, CommitId, ListedBackup, RemoteTracking, Vcs, VcsError,
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

/// The manifest naming the members on disk: while a merge or replay has
/// left `rwv.toml` unmerged, the one the checked-out commit holds.
fn members_manifest(project_dir: &std::path::Path) -> anyhow::Result<Manifest> {
    let vcs = project_vcs();
    let unmerged = vcs.is_repo(project_dir)
        && vcs
            .unmerged_paths(project_dir)?
            .iter()
            .any(|path| path == Manifest::FILE_NAME);
    if !unmerged {
        return Manifest::from_path(&project_dir.join(Manifest::FILE_NAME));
    }
    let head = vcs.head_revision(project_dir)?;
    Manifest::from_toml_str(&vcs.read_file_at_revision(
        project_dir,
        &head,
        std::path::Path::new(Manifest::FILE_NAME),
    )?)
}

fn repos_of_project(ctx: &WorkspaceContext) -> anyhow::Result<(ProjectName, Vec<BackupRepo>)> {
    let (project, active_dir) = find_project(ctx)?;
    let manifest = members_manifest(&active_dir)
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
            if named != project {
                return Err(crate::untokened!(
                    Owed,
                    "`{operand}` is the repo of project `{named}`, not of `{project}`; \
                 name this project's repo as `{}`",
                    RepoOperand::Project(project.clone())
                ));
            }
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
            if !vcs.is_repo(&dir) {
                return Err(crate::untokened!(
                    Owed,
                    "`{operand}` is no repo in this weave; `rwv backup list` names the repos \
                 that hold backups"
                ));
            }
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
    /// `rebased` for a branch a rebase rewrote; absent for any other.
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
    let stranded = crate::pins::stranded_by_dropping(repo.vcs.as_ref(), &repo.dir, live, dropping)
        .with_context(|| format!("{}: could not judge the pin rule", repo.operand))?;
    if stranded.is_empty() {
        return Ok(None);
    }
    let listing: Vec<String> = stranded
        .iter()
        .map(|s| format!("{} is pinned by {}", s.commit.short(), s.held_by.join(", ")))
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

/// Which backups of a repo `rwv backup drop` drops.
pub enum Dropping {
    Named(Vec<String>),
    All,
}

/// `rwv backup drop <repo> <name>...` and `rwv backup drop <repo> --all`.
pub fn run_drop(
    ctx: &WorkspaceContext,
    repo: &RepoOperand,
    dropping: &Dropping,
) -> anyhow::Result<()> {
    let (project, _) = repos_of_project(ctx)?;
    let target = repo_named(ctx, &project, repo)?;
    let listed = target
        .vcs
        .list_backups(&target.dir, RemoteTracking::Counted)
        .map_err(|e| listing_error(&target, e))?;
    let chosen: Vec<&Backup> = match dropping {
        Dropping::All => listed.iter().map(|l| &l.backup).collect(),
        Dropping::Named(names) => {
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
        }
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

/// Whether `branch` is published: its push target holds a remote-tracking
/// copy of its name, as last fetched, whether or not it takes in from it.
pub(crate) fn published(
    vcs: &dyn Vcs,
    dir: &std::path::Path,
    branch: &crate::vcs::RawRefName,
) -> bool {
    let upstream = vcs.upstream_of(dir, branch).ok().flatten();
    let Ok(Some(target)) = vcs.push_target_of(dir, branch, upstream.as_ref().map(|u| &u.remote))
    else {
        return false;
    };
    vcs.remote_copies(dir, branch)
        .is_ok_and(|copies| copies.iter().any(|copy| copy.remote == target))
}

/// Keep `head`, the tip of `branch` a rebase is about to rewrite, as a
/// `rebased` backup, journaled on the op at `workspace`. One of an
/// unpublished branch goes when the op completes; one of a published branch
/// stays until the republish replaces its push branch. Either goes with an
/// abort, which puts the branch back at the tip it holds.
pub(crate) fn keep_rebased(
    vcs: &dyn Vcs,
    dir: &std::path::Path,
    branch: &crate::vcs::RawRefName,
    head: &crate::vcs::ResolvedRevisionId,
    workspace: &std::path::Path,
    repo: crate::manifest::RepoKey,
) -> anyhow::Result<String> {
    let published = published(vcs, dir, branch);
    let kept = vcs.create_backup(
        dir,
        &crate::vcs::BackupSource::Branch(branch.clone()),
        head,
        Some(BackupMark::Rebased),
    )?;
    let name = kept.backup().name.as_str().to_owned();
    crate::op_state::journal_rebased_backup(
        workspace,
        crate::op_state::KeptBackup {
            repo,
            name: name.clone(),
            published,
        },
    )?;
    Ok(name)
}

/// Drop `rebased` backups an op journaled, once nothing needs them: its
/// rebased commits hold their work, or an abort put the branch back at their
/// tip. One whose drop would leave a live pin reached by nothing stays,
/// reported, and so does every one while the live locks cannot be read.
pub(crate) fn drop_rebased<'a>(
    primary: &std::path::Path,
    root: &std::path::Path,
    project: &ProjectName,
    kept: impl Iterator<Item = &'a crate::op_state::KeptBackup>,
) {
    let kept: Vec<_> = kept.collect();
    if kept.is_empty() {
        return;
    }
    let live = match live_pins(primary) {
        Ok(live) => live,
        Err(e) => {
            for one in &kept {
                eprintln!(
                    "rwv: kept the rebased backup {}: could not read the live locks: {e:#}",
                    one.name
                );
            }
            return;
        }
    };
    for one in kept {
        let (dir, vcs, member) = match &one.repo {
            crate::manifest::RepoKey::Project => (project_dir(root, project), project_vcs(), None),
            crate::manifest::RepoKey::Member(path) => (
                root.join(path.as_path()),
                crate::vcs::probe_vcs(),
                Some(path),
            ),
        };
        let Ok(listed) = vcs.list_backups(&dir, RemoteTracking::Counted) else {
            eprintln!(
                "rwv: kept the rebased backup {}: {} could not be read",
                one.name,
                dir.display()
            );
            continue;
        };
        let Some(found) = listed
            .iter()
            .find(|l| l.backup.name.as_str() == one.name)
            .map(|l| l.backup.clone())
        else {
            continue;
        };
        let held = member
            .and_then(|m| live.get(m))
            .map(Vec::as_slice)
            .unwrap_or_default();
        match crate::pins::stranded_by_dropping(vcs.as_ref(), &dir, held, &[&found]) {
            Ok(stranded) if stranded.is_empty() => {
                if let Err(e) = vcs.drop_backup(&dir, &found.name) {
                    eprintln!(
                        "rwv: could not drop the rebased backup {}: {e:#}",
                        found.name
                    );
                }
            }
            Ok(_) => eprintln!(
                "rwv: kept the rebased backup {}: a live lock pins a commit only it reaches",
                found.name
            ),
            Err(e) => eprintln!(
                "rwv: kept the rebased backup {}: could not judge the pin rule: {e:#}",
                found.name
            ),
        }
    }
}

fn rebased_of(backup: &Backup, branch: &crate::vcs::RawRefName, tip: &CommitId) -> bool {
    backup.mark == Some(BackupMark::Rebased)
        && backup.source == crate::vcs::BackupSource::Branch(branch.clone())
        && backup.tip.id() == tip
}

/// Drop `branch`'s `rebased` backup of `dir` holding `replaced`, the push
/// branch tip a republish just replaced: the rebased commits now on the
/// remote hold its work.
pub(crate) fn drop_republished(
    vcs: &dyn Vcs,
    dir: &std::path::Path,
    branch: &crate::vcs::RawRefName,
    replaced: &CommitId,
) {
    let Ok(listed) = vcs.list_backups(dir, RemoteTracking::Counted) else {
        return;
    };
    for entry in listed {
        let backup = entry.backup;
        if rebased_of(&backup, branch, replaced) {
            match vcs.drop_backup(dir, &backup.name) {
                Ok(()) => eprintln!(
                    "dropped the rebased backup {} (held {}): the republish replaced it",
                    backup.name,
                    replaced.short()
                ),
                Err(e) => eprintln!(
                    "rwv: could not drop the rebased backup {}: {e:#}",
                    backup.name
                ),
            }
        }
    }
}

/// Drop every `rebased` backup of `branch` in `dir`, for a branch that is
/// gone or whose push branch an overwrite replaced: no republish recovery can
/// use them. One whose drop would leave a live pin in `held` reached by
/// nothing stays, reported.
pub(crate) fn drop_rebased_of_branch(
    vcs: &dyn Vcs,
    dir: &std::path::Path,
    branch: &crate::vcs::RawRefName,
    held: &[crate::pins::LivePin],
) {
    let Ok(listed) = vcs.list_backups(dir, RemoteTracking::Counted) else {
        return;
    };
    for entry in listed {
        let backup = entry.backup;
        let rebased_of_branch = backup.mark == Some(BackupMark::Rebased)
            && backup.source == crate::vcs::BackupSource::Branch(branch.clone());
        if !rebased_of_branch {
            continue;
        }
        match crate::pins::stranded_by_dropping(vcs, dir, held, &[&backup]) {
            Ok(stranded) if stranded.is_empty() => match vcs.drop_backup(dir, &backup.name) {
                Ok(()) => eprintln!(
                    "dropped the rebased backup {} (held {}): no republish can use it",
                    backup.name,
                    backup.tip.id().short()
                ),
                Err(e) => eprintln!(
                    "rwv: could not drop the rebased backup {}: {e:#}",
                    backup.name
                ),
            },
            Ok(_) => eprintln!(
                "rwv: kept the rebased backup {}: a live lock pins a commit only it reaches",
                backup.name
            ),
            Err(e) => eprintln!(
                "rwv: kept the rebased backup {}: could not judge the pin rule: {e:#}",
                backup.name
            ),
        }
    }
}

/// Whether a `rebased` backup of `branch` in `dir` holds `push_tip`, the push
/// branch's tip: where one does, rwv's own rebase rewrote what was published,
/// and the republish recovery applies.
pub(crate) fn rebased_holding(
    vcs: &dyn Vcs,
    dir: &std::path::Path,
    branch: &crate::vcs::RawRefName,
    push_tip: &CommitId,
) -> bool {
    vcs.list_backups(dir, RemoteTracking::Counted)
        .is_ok_and(|listed| {
            listed
                .iter()
                .any(|l| rebased_of(&l.backup, branch, push_tip))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn a_branch_pushed_raw_with_no_upstream_is_published() {
        let tmp = crate::tempdir().unwrap();
        let (remote, repo) = (tmp.path().join("remote.git"), tmp.path().join("repo"));
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "--bare", "-b", "main"]);
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        for (key, value) in [
            ("user.email", "t@t"),
            ("user.name", "T"),
            ("remote.pushDefault", "origin"),
        ] {
            git(&repo, &["config", key, value]);
        }
        git(
            &repo,
            &["remote", "add", "origin", &remote.display().to_string()],
        );
        git(&repo, &["commit", "--allow-empty", "-m", "one"]);
        git(&repo, &["checkout", "-b", "feat"]);
        git(&repo, &["commit", "--allow-empty", "-m", "two"]);
        let vcs = crate::git::git_vcs();
        let feat = crate::vcs::RawRefName::new("feat");
        assert!(!published(vcs.as_ref(), &repo, &feat));

        git(&repo, &["push", "origin", "feat"]);

        assert!(vcs.upstream_of(&repo, &feat).unwrap().is_none());
        assert!(published(vcs.as_ref(), &repo, &feat));
    }

    fn init(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "T"]);
    }

    fn lock_pinning(proj: &std::path::Path, commit: &CommitId) {
        std::fs::write(
            proj.join("rwv.lock"),
            format!(
                r#"{{"repositories":{{"app":{{"type":"git","url":"file:///app","version":"{commit}"}}}}}}"#
            ),
        )
        .unwrap();
        git(proj, &["add", "rwv.lock"]);
        git(proj, &["commit", "-q", "-m", "lock"]);
    }

    fn rebased_backups(vcs: &dyn Vcs, app: &std::path::Path) -> Vec<String> {
        vcs.list_backups(app, RemoteTracking::Counted)
            .unwrap()
            .into_iter()
            .filter(|l| l.backup.mark == Some(BackupMark::Rebased))
            .map(|l| l.backup.name.as_str().to_owned())
            .collect()
    }

    #[test]
    fn a_rebased_backup_holding_the_only_way_to_a_live_pin_is_kept_by_both_drops() {
        let tmp = crate::tempdir().unwrap();
        let root = tmp.path();
        let (app, proj) = (root.join("app"), root.join("projects").join("proj"));
        init(&app);
        git(&app, &["commit", "-q", "--allow-empty", "-m", "base"]);
        git(&app, &["checkout", "-q", "-b", "feat"]);
        git(&app, &["commit", "-q", "--allow-empty", "-m", "old"]);
        let vcs = crate::git::git_vcs();
        let feat = crate::vcs::RawRefName::new("feat");
        let old = vcs.head_revision(&app).unwrap();
        let backup_old = || {
            vcs.create_backup(
                &app,
                &crate::vcs::BackupSource::Branch(feat.clone()),
                &old,
                Some(BackupMark::Rebased),
            )
            .unwrap()
            .backup()
            .name
            .as_str()
            .to_owned()
        };
        let name = backup_old();
        git(
            &app,
            &["commit", "-q", "--amend", "--allow-empty", "-m", "new"],
        );
        let new = vcs.head_revision(&app).unwrap();
        init(&proj);
        std::fs::write(proj.join("rwv.toml"), "").unwrap();
        git(&proj, &["add", "rwv.toml"]);
        lock_pinning(&proj, old.id());
        let project = ProjectName::new("proj").unwrap();
        let kept = crate::op_state::KeptBackup {
            repo: crate::manifest::RepoKey::Member(RepoPath::new("app").unwrap()),
            name: name.clone(),
            published: false,
        };
        let app_pins = |root: &std::path::Path| {
            live_pins(root)
                .unwrap()
                .remove(&RepoPath::new("app").unwrap())
                .unwrap_or_default()
        };

        drop_rebased(root, root, &project, std::iter::once(&kept));
        assert_eq!(rebased_backups(vcs.as_ref(), &app), vec![name.clone()]);
        drop_rebased_of_branch(vcs.as_ref(), &app, &feat, &app_pins(root));
        assert_eq!(rebased_backups(vcs.as_ref(), &app), vec![name.clone()]);

        lock_pinning(&proj, new.id());
        drop_rebased(root, root, &project, std::iter::once(&kept));
        assert!(rebased_backups(vcs.as_ref(), &app).is_empty());
        backup_old();
        drop_rebased_of_branch(vcs.as_ref(), &app, &feat, &app_pins(root));
        assert!(rebased_backups(vcs.as_ref(), &app).is_empty());
    }
}
