//! The deletion rule: of the branches that belong to a weave branch, which are
//! deleted and which survive, and why.
//!
//! What belongs to a weave branch `W` is the project branch `W` and, in each
//! member `W`'s `rwv.toml` lists, the branch named `W` that no live weave
//! branch declares. A branch that belongs is deleted when every commit on it
//! is reached by the branch it lands into, no live lock pins a commit only it
//! reaches, and, for the project branch, no published copy of it is an open
//! head and none of its member branches survive.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Context;
use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::consent::DiscardUnlandedConsent;
use crate::diagnosis::{Report, State};
use crate::manifest::{Manifest, ProjectName, RepoOperand, RepoPath};
use crate::records::{BranchName, Lookup, Mode, Store};
use crate::vcs::{
    project_vcs, vcs_for, DestroyProof, RawRefName, RemoteHandle, RemoteTip, ResolvedRevisionId,
    Site, Vcs,
};
use crate::workspace::{project_dir, project_rel_path};

/// Why a branch that belongs survived the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// It holds commits the branch it lands into does not reach.
    Unlanded,
    /// A live lock pins a commit that only it reaches.
    Pinned,
    /// Its published copy holds commits the branch it lands into does not
    /// reach: a pull request may be open against it.
    OpenHead,
    /// A member branch that belongs to it survives.
    Members,
    /// A remote it has a copy on could not be fetched.
    Unconfirmed,
}

impl Reason {
    fn token(self) -> &'static str {
        match self {
            Self::Unlanded => "unlanded",
            Self::Pinned => "pinned",
            Self::OpenHead => "open-head",
            Self::Members => "members",
            Self::Unconfirmed => "unconfirmed",
        }
    }
}

/// A branch the rule deleted.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Deleted {
    /// The repo, as `rwv backup drop` names it.
    pub path: String,
    pub branch: String,
    /// `local`, or `remote <name>` for a push branch.
    pub site: String,
    /// What licensed it: the branch it landed on, or the backup kept first.
    pub because: String,
    /// The backup kept first, when the branch had not landed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
}

/// A branch the rule kept.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Survivor {
    pub path: String,
    pub branch: String,
    pub reason: Reason,
    /// What the reason rests on: the commits, the locks, the remote's cause.
    pub detail: Vec<String>,
}

/// What the rule did.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct Outcome {
    pub deleted: Vec<Deleted>,
    pub survivors: Vec<Survivor>,
    /// Branches whose records the drop rule removed.
    pub records_dropped: Vec<String>,
}

impl Outcome {
    pub fn left_something(&self) -> bool {
        !self.survivors.is_empty()
    }

    pub fn print(&self) {
        for deleted in &self.deleted {
            if let Some(backup) = &deleted.backup {
                eprintln!(
                    "backup: {}: {} kept as {backup} (`rwv backup list`)",
                    deleted.path, deleted.branch
                );
            }
            outln!(
                "deleted {} {} ({}): {}",
                deleted.path,
                deleted.branch,
                deleted.site,
                deleted.because
            );
        }
        for survivor in &self.survivors {
            eprintln!(
                "left: {} {}: {}",
                survivor.path,
                survivor.branch,
                survivor.reason.token()
            );
            for line in &survivor.detail {
                eprintln!("  {line}");
            }
        }
        for name in &self.records_dropped {
            outln!("dropped the record of {name}");
        }
    }
}

/// Which push branches the rule may delete besides the local branches.
#[derive(Debug, Clone, Default)]
pub struct RemoteDeletes {
    /// `--delete-remote`: each push branch that has landed.
    pub landed: bool,
    /// `--delete-remote-unlanded <repo>`: these repos' push branches, landed
    /// or not, once `--discard-unlanded` has confirmed them.
    pub unlanded: Vec<RepoOperand>,
}

pub struct Request<'a> {
    pub primary: &'a Path,
    pub project: &'a ProjectName,
    pub branch: BranchName,
    pub target: BranchName,
    pub discard_unlanded: Option<DiscardUnlandedConsent>,
    pub remote_deletes: &'a RemoteDeletes,
    /// A workspace the verb removes before the rule runs: a checkout of the
    /// branch in it is not a reason to keep the branch.
    pub leaving: Option<&'a Path>,
    pub verb: &'static str,
}

struct Repo {
    operand: RepoOperand,
    dir: PathBuf,
    vcs: Box<dyn Vcs>,
    url_remote: Option<RemoteHandle>,
    member: Option<RepoPath>,
    names_in_target: Vec<RawRefName>,
}

impl Repo {
    fn label(&self) -> String {
        self.operand.to_string()
    }
}

/// The rule, with every refusal already judged and nothing yet changed.
pub struct Rule<'a> {
    request: Request<'a>,
    branch_name: RawRefName,
    repos: Vec<Repo>,
    w_record_mode: Option<Mode>,
}

fn tips_of(
    vcs: &dyn Vcs,
    dir: &Path,
    names: &[RawRefName],
) -> anyhow::Result<Vec<ResolvedRevisionId>> {
    let mut tips: Vec<ResolvedRevisionId> = Vec::new();
    for name in names {
        let found = vcs
            .resolve_local_branch_tip(dir, name)?
            .into_iter()
            .chain(vcs.remote_copies(dir, name)?.into_iter().map(|c| c.tip));
        for tip in found {
            if !tips.contains(&tip) {
                tips.push(tip);
            }
        }
    }
    Ok(tips)
}

fn branch_tip(
    vcs: &dyn Vcs,
    dir: &Path,
    name: &RawRefName,
) -> anyhow::Result<Option<ResolvedRevisionId>> {
    if let Some(tip) = vcs.resolve_local_branch_tip(dir, name)? {
        return Ok(Some(tip));
    }
    Ok(vcs
        .remote_copies(dir, name)?
        .into_iter()
        .next()
        .map(|c| c.tip))
}

impl<'a> Rule<'a> {
    /// Judge every refusal: the branch exists, its target has a record that
    /// reads, nothing lands into it, no workspace holds it checked out, and
    /// the flags make sense together. Nothing is changed.
    pub fn open(request: Request<'a>) -> anyhow::Result<Self> {
        let project_vcs = project_vcs();
        let project_path = project_dir(request.primary, request.project);
        let project_label = project_rel_path(request.project);
        let branch_name = RawRefName::new(request.branch.as_str());
        let store = Store::open(project_vcs.as_ref(), &project_path)?;
        let view = store.view()?;

        let dependents = view.dependents(&request.branch);
        if !dependents.is_empty() {
            let mut report = Report::default();
            for dependent in dependents {
                report.add(
                    State::TargetDependents
                        .of(&project_label)
                        .project_repo()
                        .subject(&dependent)
                        .listing([format!("{dependent} lands into {}", request.branch)])
                        .bind("<W>", &dependent)
                        .bind("<T>", &request.target)
                        .judge(project_vcs.as_ref()),
                );
            }
            report.into_result()?;
        }

        let target_mode = match view.lookup(&request.target) {
            Lookup::Value(record) => record.mode(),
            lookup => {
                return Err(crate::branch::judge_lookup(
                    project_vcs.as_ref(),
                    &store,
                    request.project,
                    &project_path,
                    &request.target,
                    lookup,
                )?
                .refused())
            }
        };
        let w_record_mode = match view.lookup(&request.branch) {
            Lookup::Value(record) => Some(record.mode()),
            _ => None,
        };

        let known = project_vcs.named_branches(&project_path)?;
        if !known.iter().any(|b| b.name == request.branch.as_str()) {
            return Err(State::NameNoSuchBranch
                .of(&project_label)
                .project_repo()
                .subject(&request.branch)
                .judge(project_vcs.as_ref())
                .refused());
        }

        let mut repos = vec![Repo {
            operand: RepoOperand::Project(request.project.clone()),
            dir: project_path.clone(),
            vcs: project_vcs,
            url_remote: None,
            member: None,
            names_in_target: vec![RawRefName::new(request.target.as_str())],
        }];
        let project_repo = &repos[0];
        let url_remote = project_repo.vcs.project_url_remote(&project_repo.dir)?;
        repos[0].url_remote = url_remote;

        let w_tip = branch_tip(repos[0].vcs.as_ref(), &project_path, &branch_name)?
            .expect("the branch was listed");
        let t_raw = RawRefName::new(request.target.as_str());
        let t_tip = branch_tip(repos[0].vcs.as_ref(), &project_path, &t_raw)?;
        let target_manifest = match &t_tip {
            Some(tip) => manifest_at(repos[0].vcs.as_ref(), &project_path, tip),
            None => None,
        };
        let history: Vec<Manifest> = match &t_tip {
            Some(tip) => repos[0]
                .vcs
                .file_versions(&project_path, tip, Path::new(Manifest::FILE_NAME))?
                .iter()
                .filter_map(|text| Manifest::from_toml_str(text).ok())
                .collect(),
            None => Vec::new(),
        };

        if let Some(manifest) = manifest_at(repos[0].vcs.as_ref(), &project_path, &w_tip) {
            let declared = declared_branches(request.primary);
            for (path, entry) in manifest.iter_entries() {
                let dir = request.primary.join(path.as_path());
                let vcs = vcs_for(entry.vcs_type);
                if !vcs.is_repo(&dir) {
                    continue;
                }
                if declared
                    .get(path.as_str())
                    .is_some_and(|set| set.contains(request.branch.as_str()))
                {
                    continue;
                }
                let mut names: Vec<RawRefName> = Vec::new();
                let mut note = |name: RawRefName| {
                    if !names.contains(&name) {
                        names.push(name);
                    }
                };
                match target_mode {
                    Mode::Feature => note(t_raw.clone()),
                    Mode::Declared => {
                        if let Some(entry) =
                            target_manifest.as_ref().and_then(|m| m.get_entry(path))
                        {
                            note(RawRefName::new(entry.branch.local_counterpart().as_str()));
                        }
                    }
                }
                for earlier in &history {
                    if let Some(entry) = earlier.get_entry(path) {
                        note(RawRefName::new(entry.branch.local_counterpart().as_str()));
                    }
                }
                let url_remote = crate::url_remote::present(vcs.as_ref(), &dir, &entry.url)?;
                repos.push(Repo {
                    operand: RepoOperand::Member(path.clone()),
                    dir,
                    vcs,
                    url_remote,
                    member: Some(path.clone()),
                    names_in_target: names,
                });
            }
        }

        let rule = Self {
            request,
            branch_name,
            repos,
            w_record_mode,
        };
        rule.judge_flags()?;
        rule.judge_checkouts()?;
        Ok(rule)
    }

    fn judge_flags(&self) -> anyhow::Result<()> {
        let named = &self.request.remote_deletes.unlanded;
        if named.is_empty() {
            return Ok(());
        }
        if self.request.discard_unlanded.is_none() {
            crate::refuse!(
                crate::refusal::RefusalKind::InapplicableFlag,
                "rwv {}: `--delete-remote-unlanded` deletes a push branch the deletion rule \
                 judged unlanded, which only `--discard-unlanded` confirms; name both",
                self.request.verb
            );
        }
        for operand in named {
            let Some(repo) = self.repos.iter().find(|r| &r.operand == operand) else {
                crate::refuse!(
                    crate::refusal::RefusalKind::InapplicableFlag,
                    "rwv {}: `--delete-remote-unlanded {operand}` names no repo that \
                     `{}` has a branch in; the repos are: {}",
                    self.request.verb,
                    self.request.branch,
                    self.repos
                        .iter()
                        .map(Repo::label)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            };
            if repo.member.is_none() && self.w_record_mode == Some(Mode::Declared) {
                crate::refuse!(
                    crate::refusal::RefusalKind::InapplicableFlag,
                    "rwv {}: `{}` is a declared-mode project branch, whose push branch \
                     `--delete-remote-unlanded` never deletes; `--delete-remote` deletes it \
                     once it has landed",
                    self.request.verb,
                    self.request.branch
                );
            }
        }
        Ok(())
    }

    fn judge_checkouts(&self) -> anyhow::Result<()> {
        let recorded = crate::sync::recorded_workweaves(self.request.primary)?;
        let leaving = self
            .request
            .leaving
            .map(crate::path_spelling::CanonicalPath::of);
        let mut report = Report::default();
        for repo in &self.repos {
            for checkout in repo.vcs.checkouts_of_branch(&repo.dir, &self.branch_name)? {
                let at = crate::path_spelling::CanonicalPath::of(&checkout);
                if leaving.as_ref().is_some_and(|dir| dir.contains(&at)) {
                    continue;
                }
                report.add(crate::sync::judge_checkout(
                    repo.vcs.as_ref(),
                    repo.label(),
                    &checkout,
                    &repo.dir,
                    &recorded,
                ));
            }
        }
        report.into_result()
    }

    fn remote_of(&self, repo: &Repo) -> anyhow::Result<BTreeSet<String>> {
        let mut remotes: BTreeSet<String> = BTreeSet::new();
        if let Some(upstream) = repo.vcs.upstream_of(&repo.dir, &self.branch_name)? {
            remotes.insert(upstream.remote.name().to_owned());
        }
        for copy in repo.vcs.remote_copies(&repo.dir, &self.branch_name)? {
            remotes.insert(copy.remote.name().to_owned());
        }
        Ok(remotes)
    }

    /// Fetch the remotes the branches have an upstream or a push branch on,
    /// and return, per repo, the remotes that could not be fetched with the
    /// cause.
    fn fetch(&self) -> anyhow::Result<BTreeMap<usize, BTreeMap<String, String>>> {
        let mut unconfirmed: BTreeMap<usize, BTreeMap<String, String>> = BTreeMap::new();
        for (index, repo) in self.repos.iter().enumerate() {
            let remotes: Vec<RemoteHandle> = self
                .remote_of(repo)?
                .into_iter()
                .map(RemoteHandle::new)
                .collect();
            if remotes.is_empty() {
                continue;
            }
            let fetched = crate::fetch::pruning_fetch_of(repo.vcs.as_ref(), &repo.dir, &remotes)
                .with_context(|| format!("{}: the pruning fetch failed", repo.label()))?;
            for remote in fetched.unconfirmed {
                unconfirmed
                    .entry(index)
                    .or_default()
                    .insert(remote.remote, remote.cause);
            }
        }
        Ok(unconfirmed)
    }

    /// Run the rule: fetch, judge each branch that belongs, delete what it
    /// deletes, and drop the records it leaves without a branch.
    pub fn apply(self) -> anyhow::Result<Outcome> {
        let unconfirmed = self.fetch()?;
        let pins = crate::pins::live_pins(self.request.primary)?;
        let mut outcome = Outcome::default();
        let mut member_survives = false;

        for index in 1..self.repos.len() {
            member_survives |= self.judge_member(index, &unconfirmed, &pins, &mut outcome)?;
        }
        self.judge_project(&unconfirmed, member_survives, &mut outcome)?;

        outcome.records_dropped = crate::branch::drop_records(
            self.request.project,
            self.request.primary,
            unconfirmed.contains_key(&0),
        )?;
        Ok(outcome)
    }

    fn cut_off(
        &self,
        index: usize,
        unconfirmed: &BTreeMap<usize, BTreeMap<String, String>>,
    ) -> anyhow::Result<Vec<String>> {
        let Some(blocked) = unconfirmed.get(&index) else {
            return Ok(Vec::new());
        };
        Ok(self
            .remote_of(&self.repos[index])?
            .iter()
            .filter_map(|r| blocked.get(r).map(|cause| format!("{r}: {cause}")))
            .collect())
    }

    /// Whether the member branch survived.
    fn judge_member(
        &self,
        index: usize,
        unconfirmed: &BTreeMap<usize, BTreeMap<String, String>>,
        pins: &BTreeMap<RepoPath, Vec<crate::pins::LivePin>>,
        outcome: &mut Outcome,
    ) -> anyhow::Result<bool> {
        let repo = &self.repos[index];
        let local = repo
            .vcs
            .resolve_local_branch_tip(&repo.dir, &self.branch_name)?;
        let cut_off = self.cut_off(index, unconfirmed)?;
        if !cut_off.is_empty() {
            if local.is_none() {
                return Ok(false);
            }
            self.survives(repo, Reason::Unconfirmed, cut_off, outcome);
            return Ok(true);
        }
        self.delete_remote(repo, outcome)?;
        let Some(local) = local else {
            return Ok(false);
        };
        let member = repo.member.as_ref().expect("a member repo names its path");
        let held = self.pinned(repo, member, &local, pins)?;
        if !held.is_empty() {
            self.survives(repo, Reason::Pinned, held, outcome);
            return Ok(true);
        }
        self.delete_local(repo, &local, outcome)
    }

    fn judge_project(
        &self,
        unconfirmed: &BTreeMap<usize, BTreeMap<String, String>>,
        member_survives: bool,
        outcome: &mut Outcome,
    ) -> anyhow::Result<()> {
        let repo = &self.repos[0];
        let local = repo
            .vcs
            .resolve_local_branch_tip(&repo.dir, &self.branch_name)?;
        let cut_off = self.cut_off(0, unconfirmed)?;
        if !cut_off.is_empty() {
            if local.is_some() {
                self.survives(repo, Reason::Unconfirmed, cut_off, outcome);
            }
            return Ok(());
        }
        self.delete_remote(repo, outcome)?;
        let Some(local) = local else {
            return Ok(());
        };
        if member_survives {
            self.survives(
                repo,
                Reason::Members,
                vec!["a member branch that belongs to it survives".to_owned()],
                outcome,
            );
            return Ok(());
        }
        if let Some(open) = self.open_head(repo)? {
            self.survives(repo, Reason::OpenHead, open, outcome);
            return Ok(());
        }
        self.delete_local(repo, &local, outcome)?;
        Ok(())
    }

    /// Delete the local branch when it has landed, or when
    /// `--discard-unlanded` backs it up first. Whether it survived.
    fn delete_local(
        &self,
        repo: &Repo,
        local: &ResolvedRevisionId,
        outcome: &mut Outcome,
    ) -> anyhow::Result<bool> {
        let onto = tips_of(repo.vcs.as_ref(), &repo.dir, &repo.names_in_target)?;
        let proof = match DestroyProof::landed(
            repo.vcs.as_ref(),
            &repo.dir,
            Site::Local,
            &self.branch_name,
            &onto,
        )? {
            Some(proof) => proof,
            None => match self.request.discard_unlanded {
                Some(consent) => DestroyProof::backed_up(
                    repo.vcs.as_ref(),
                    &repo.dir,
                    Site::Local,
                    &self.branch_name,
                    consent,
                )?,
                None => {
                    let detail = self.unlanded_commits(repo, local, &onto);
                    self.survives(repo, Reason::Unlanded, detail, outcome);
                    return Ok(true);
                }
            },
        };
        self.destroy(repo, proof, Site::Local, outcome)?;
        Ok(false)
    }

    fn survives(&self, repo: &Repo, reason: Reason, detail: Vec<String>, outcome: &mut Outcome) {
        outcome.survivors.push(Survivor {
            path: repo.label(),
            branch: self.branch_name.to_string(),
            reason,
            detail,
        });
    }

    fn destroy(
        &self,
        repo: &Repo,
        proof: DestroyProof,
        site: Site,
        outcome: &mut Outcome,
    ) -> anyhow::Result<()> {
        let because = proof.describe();
        let backup = proof.backup().map(|b| b.backup().name.to_string());
        repo.vcs
            .delete_branch(proof)
            .with_context(|| format!("{}: could not delete {}", repo.label(), self.branch_name))?;
        outcome.deleted.push(Deleted {
            path: repo.label(),
            branch: self.branch_name.to_string(),
            site: match site {
                Site::Local => "local".to_owned(),
                Site::Remote(remote) => format!("remote {remote}"),
            },
            because,
            backup,
        });
        Ok(())
    }

    fn push_branch_of(&self, repo: &Repo) -> anyhow::Result<Option<RemoteTip>> {
        let Some(target) =
            repo.vcs
                .push_target_of(&repo.dir, &self.branch_name, repo.url_remote.as_ref())?
        else {
            return Ok(None);
        };
        Ok(repo
            .vcs
            .remote_copies(&repo.dir, &self.branch_name)?
            .into_iter()
            .find(|copy| copy.remote == target))
    }

    fn target_remote_tips(&self, repo: &Repo) -> anyhow::Result<Vec<ResolvedRevisionId>> {
        let mut tips = Vec::new();
        for name in &repo.names_in_target {
            tips.extend(
                repo.vcs
                    .remote_copies(&repo.dir, name)?
                    .into_iter()
                    .map(|c| c.tip),
            );
        }
        Ok(tips)
    }

    /// The push branch, when `--delete-remote` or `--delete-remote-unlanded`
    /// reaches it and the proof it needs can be made.
    fn delete_remote(&self, repo: &Repo, outcome: &mut Outcome) -> anyhow::Result<()> {
        let flags = self.request.remote_deletes;
        let named = flags.unlanded.contains(&repo.operand);
        if !flags.landed && !named {
            return Ok(());
        }
        let Some(pushed) = self.push_branch_of(repo)? else {
            return Ok(());
        };
        let site = Site::Remote(pushed.remote.clone());
        let onto = self.target_remote_tips(repo)?;
        let proof = match DestroyProof::landed(
            repo.vcs.as_ref(),
            &repo.dir,
            site.clone(),
            &self.branch_name,
            &onto,
        )? {
            Some(proof) => proof,
            None if named => match self.request.discard_unlanded {
                Some(consent) => DestroyProof::backed_up(
                    repo.vcs.as_ref(),
                    &repo.dir,
                    site.clone(),
                    &self.branch_name,
                    consent,
                )?,
                None => return Ok(()),
            },
            None => return Ok(()),
        };
        self.destroy(repo, proof, site, outcome)
    }

    /// The commits on the published copy that the branch it lands into does
    /// not reach, when the project branch has a published copy at all.
    fn open_head(&self, repo: &Repo) -> anyhow::Result<Option<Vec<String>>> {
        let Some(pushed) = self.push_branch_of(repo)? else {
            return Ok(None);
        };
        let mut onto = tips_of(repo.vcs.as_ref(), &repo.dir, &repo.names_in_target)?;
        onto.dedup();
        for target in &onto {
            if repo.vcs.is_ancestor(&repo.dir, &pushed.tip, target)? {
                return Ok(None);
            }
        }
        let lines = match onto.first() {
            Some(base) => repo
                .vcs
                .commits_beyond(&repo.dir, &pushed.tip, base)?
                .iter()
                .map(|c| format!("{} {}", c.id.short(), c.subject))
                .collect(),
            None => Vec::new(),
        };
        let mut detail = vec![format!(
            "{}/{} holds commits {} does not reach",
            pushed.remote, self.branch_name, self.request.target
        )];
        detail.extend(lines);
        Ok(Some(detail))
    }

    fn unlanded_commits(
        &self,
        repo: &Repo,
        tip: &ResolvedRevisionId,
        onto: &[ResolvedRevisionId],
    ) -> Vec<String> {
        let mut best: Option<Vec<String>> = None;
        for base in onto {
            let Ok(commits) = repo.vcs.commits_beyond(&repo.dir, tip, base) else {
                continue;
            };
            let lines: Vec<String> = commits
                .iter()
                .map(|c| format!("{} {}", c.id.short(), c.subject))
                .collect();
            if best.as_ref().is_none_or(|b| lines.len() < b.len()) {
                best = Some(lines);
            }
        }
        let mut detail = vec![format!(
            "holds commits {}'s corresponding branch does not reach; \
             `--discard-unlanded` keeps them as a backup and deletes the branch",
            self.request.target
        )];
        detail.extend(best.unwrap_or_default());
        detail
    }

    /// The live pins that only `tip` reaches: pins in locks other than the
    /// weave branch's own.
    fn pinned(
        &self,
        repo: &Repo,
        member: &RepoPath,
        tip: &ResolvedRevisionId,
        pins: &BTreeMap<RepoPath, Vec<crate::pins::LivePin>>,
    ) -> anyhow::Result<Vec<String>> {
        let Some(live) = pins.get(member) else {
            return Ok(Vec::new());
        };
        let project_repo = &self.repos[0];
        let own: BTreeSet<String> = std::iter::once(self.request.branch.as_str().to_owned())
            .chain(
                project_repo
                    .vcs
                    .recorded_urls(&project_repo.dir)?
                    .into_iter()
                    .map(|r| format!("{}/{}", r.remote, self.request.branch)),
            )
            .collect();
        let others: Vec<ResolvedRevisionId> = repo
            .vcs
            .branch_tips(&repo.dir)?
            .into_iter()
            .filter(|b| {
                b.label != self.branch_name.as_str()
                    && !b.label.ends_with(&format!("/{}", self.branch_name))
            })
            .map(|b| b.tip)
            .chain(
                repo.vcs
                    .list_backups(&repo.dir, crate::vcs::RemoteTracking::Counted)?
                    .into_iter()
                    .map(|l| l.backup.tip),
            )
            .collect();
        let mut held: Vec<String> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for pin in live {
            if pin.project == *self.request.project && own.contains(&pin.branch) {
                continue;
            }
            if !seen.insert(pin.commit.to_string()) {
                continue;
            }
            let pinned = ResolvedRevisionId::from(pin.commit.clone());
            let reached_here = repo
                .vcs
                .is_ancestor(&repo.dir, &pinned, tip)
                .unwrap_or(false);
            if !reached_here {
                continue;
            }
            let elsewhere = others.iter().any(|other| {
                repo.vcs
                    .is_ancestor(&repo.dir, &pinned, other)
                    .unwrap_or(false)
            });
            if !elsewhere {
                let by: Vec<&str> = live
                    .iter()
                    .filter(|p| p.commit == pin.commit)
                    .map(|p| p.held_by.as_str())
                    .collect();
                held.push(format!(
                    "{} is pinned by {}",
                    pin.commit.short(),
                    by.join(", ")
                ));
            }
        }
        Ok(held)
    }
}

fn manifest_at(vcs: &dyn Vcs, dir: &Path, tip: &ResolvedRevisionId) -> Option<Manifest> {
    let text = vcs
        .read_file_at_revision(dir, tip, Path::new(Manifest::FILE_NAME))
        .ok()?;
    Manifest::from_toml_str(&text).ok()
}

/// The branch each member is declared on, across every live weave branch of
/// every project of the weave.
fn declared_branches(primary: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let mut declared: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let vcs = project_vcs();
    for project in crate::workspace::discover_projects(primary) {
        let dir = project_dir(primary, &project);
        if !vcs.is_repo(&dir) {
            continue;
        }
        let Ok(tips) = vcs.branch_tips(&dir) else {
            continue;
        };
        for branch in tips {
            let tip = branch.tip;
            let Some(manifest) = manifest_at(vcs.as_ref(), &dir, &tip) else {
                continue;
            };
            for (path, entry) in manifest.iter_entries() {
                declared
                    .entry(path.to_string())
                    .or_default()
                    .insert(entry.branch.local_counterpart().as_str().to_owned());
            }
        }
    }
    declared
}
