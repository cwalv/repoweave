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

use crate::branch_ref::BranchRef;
use crate::cli::consent::DiscardUnlandedConsent;
use crate::diagnosis::{Report, State};
use crate::manifest::{Manifest, ProjectName, RepoOperand, RepoPath};
use crate::pins::{stranded_by, LivePin, RefChange};
use crate::push_branch::{newer_of_branch_and_upstream, upstream_copy, NewerOfBranchAndUpstream};
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
    /// A remote it has a copy on could not be fetched, or did not take the
    /// delete.
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

    /// Each branch deleted after a backup, once: a local branch and its push
    /// branch deleted together share one backup.
    pub fn backed_up(&self) -> Vec<&Deleted> {
        let mut seen: Vec<(&str, &str, &str)> = Vec::new();
        self.deleted
            .iter()
            .filter(|d| {
                let Some(backup) = &d.backup else {
                    return false;
                };
                let key = (d.path.as_str(), d.branch.as_str(), backup.as_str());
                let first = !seen.contains(&key);
                seen.push(key);
                first
            })
            .collect()
    }

    pub fn print(&self) {
        for deleted in self.backed_up() {
            if let Some(backup) = &deleted.backup {
                eprintln!(
                    "backup: {}: {} kept as {backup} (`rwv backup list`)",
                    deleted.path, deleted.branch
                );
            }
        }
        for deleted in &self.deleted {
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
    target_mode: Mode,
    unconfirmed: BTreeMap<usize, BTreeMap<String, String>>,
}

type NamedTip = (BranchRef, ResolvedRevisionId);

/// Each of `names` here and its upstream copy: the branches a branch lands on.
fn landing_refs(
    vcs: &dyn Vcs,
    dir: &Path,
    names: &[RawRefName],
    home: Option<&RemoteHandle>,
) -> anyhow::Result<Vec<NamedTip>> {
    let mut refs: Vec<NamedTip> = Vec::new();
    for name in names {
        if let Some(tip) = vcs.resolve_local_branch_tip(dir, name)? {
            refs.push((BranchRef::Local(name.clone()), tip));
        }
        refs.extend(upstream_copy(vcs, dir, name, home)?);
    }
    Ok(refs)
}

fn tips(refs: &[NamedTip]) -> Vec<ResolvedRevisionId> {
    let mut tips: Vec<ResolvedRevisionId> = Vec::new();
    for (_, tip) in refs {
        if !tips.contains(tip) {
            tips.push(tip.clone());
        }
    }
    tips
}

fn branch_tip(
    vcs: &dyn Vcs,
    dir: &Path,
    name: &RawRefName,
    home: Option<&RemoteHandle>,
) -> anyhow::Result<Option<ResolvedRevisionId>> {
    Ok(match newer_of_branch_and_upstream(vcs, dir, name, home)? {
        NewerOfBranchAndUpstream::Tip(tip) => Some(tip),
        NewerOfBranchAndUpstream::Diverged { local, .. } => Some(local),
        NewerOfBranchAndUpstream::Nowhere => vcs
            .remote_copies(dir, name)?
            .into_iter()
            .next()
            .map(|copy| copy.tip),
    })
}

impl<'a> Rule<'a> {
    /// Judge every refusal: the branch exists, its target has a record that
    /// reads, nothing lands into it, no workspace holds it checked out, the
    /// flags make sense together, and, after the pruning fetch, no remote
    /// delete reaches a declared branch. Only remote-tracking branches move.
    pub fn open(request: Request<'a>) -> anyhow::Result<Self> {
        let project_vcs = project_vcs();
        let project_path = project_dir(request.primary, request.project);
        let project_label = project_rel_path(request.project);
        let branch_name = RawRefName::new(request.branch.as_str());
        crate::landing_claim::refuse_moving_claimed(
            project_vcs.as_ref(),
            &project_path,
            request.project,
            &[(
                crate::manifest::RepoKey::Project,
                request.branch.to_string(),
            )],
            None,
        )?;
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

        let home = repos[0].url_remote.clone();
        let w_tip = branch_tip(
            repos[0].vcs.as_ref(),
            &project_path,
            &branch_name,
            home.as_ref(),
        )?
        .expect("the branch was listed");
        let manifest = w_manifest(
            repos[0].vcs.as_ref(),
            &project_path,
            &w_tip,
            request.project,
            &request.branch,
            request.verb,
        )?;
        if let Some(manifest) = manifest {
            for (path, entry) in manifest.iter_entries() {
                let dir = request.primary.join(path.as_path());
                let vcs = vcs_for(entry.vcs_type);
                if !vcs.is_repo(&dir) {
                    continue;
                }
                let url_remote = crate::url_remote::present(vcs.as_ref(), &dir, &entry.url)?;
                repos.push(Repo {
                    operand: RepoOperand::Member(path.clone()),
                    dir,
                    vcs,
                    url_remote,
                    member: Some(path.clone()),
                    names_in_target: Vec::new(),
                });
            }
        }

        let rule = Self {
            request,
            branch_name,
            repos,
            w_record_mode,
            target_mode,
            unconfirmed: BTreeMap::new(),
        };
        let mut rule = rule;
        rule.judge_flags()?;
        rule.judge_checkouts()?;
        rule.unconfirmed = rule.fetch()?;
        rule.judge_declared()?;
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
                return Err(State::ProtectedDeclaredMode
                    .of(repo.label())
                    .project_repo()
                    .subject(&self.request.branch)
                    .by_verb(self.request.verb)
                    .judge(repo.vcs.as_ref())
                    .refused());
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
                    None,
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

    /// After the fetch: refuse a remote delete of a member branch a live weave
    /// branch declares, leave every such member branch out of the rule, and
    /// read the branches each other member lands on.
    fn judge_declared(&mut self) -> anyhow::Result<()> {
        let declarations = crate::declarations::LiveDeclarations::scan(self.request.primary)?;
        let home = self.repos[0].url_remote.clone();
        let project_path = self.repos[0].dir.clone();
        let t_raw = RawRefName::new(self.request.target.as_str());
        let project = self.repos[0].vcs.as_ref();
        let t_tip = branch_tip(project, &project_path, &t_raw, home.as_ref())?;
        let target_manifest = match &t_tip {
            Some(tip) => manifest_at(project, &project_path, tip),
            None => None,
        };
        let history: Vec<Manifest> = match &t_tip {
            Some(tip) => project
                .file_versions(&project_path, tip, Path::new(Manifest::FILE_NAME))?
                .iter()
                .filter_map(|text| Manifest::from_toml_str(text).ok())
                .collect(),
            None => Vec::new(),
        };
        let mut protected = Report::default();
        let mut declared: Vec<usize> = Vec::new();
        for (index, repo) in self.repos.iter_mut().enumerate().skip(1) {
            let path = repo
                .member
                .clone()
                .expect("every repo after the first is a member");
            let declarers = declarations.declarers(&path, self.request.branch.as_str());
            if !declarers.is_empty() {
                let named = self
                    .request
                    .remote_deletes
                    .unlanded
                    .contains(&RepoOperand::Member(path.clone()));
                if self.request.remote_deletes.landed || named {
                    protected.add(
                        State::ProtectedDeclared
                            .of(&path)
                            .member()
                            .subject(&self.request.branch)
                            .listing(declarers.iter().map(ToString::to_string))
                            .bind("<m>", &path)
                            .without(&[crate::diagnosis::ChoiceId::C12])
                            .by_verb(self.request.verb)
                            .judge(repo.vcs.as_ref()),
                    );
                }
                declared.push(index);
                continue;
            }
            let mut names: Vec<RawRefName> = Vec::new();
            let mut note = |name: RawRefName| {
                if !names.contains(&name) {
                    names.push(name);
                }
            };
            match self.target_mode {
                Mode::Feature => note(t_raw.clone()),
                Mode::Declared => {
                    if let Some(entry) = target_manifest.as_ref().and_then(|m| m.get_entry(&path)) {
                        note(RawRefName::new(entry.branch.local_counterpart().as_str()));
                    }
                }
            }
            for earlier in &history {
                if let Some(entry) = earlier.get_entry(&path) {
                    note(RawRefName::new(entry.branch.local_counterpart().as_str()));
                }
            }
            repo.names_in_target = names;
        }
        protected.into_result()?;
        for index in declared.into_iter().rev() {
            self.repos.remove(index);
        }
        Ok(())
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

    /// Run the rule: judge each branch that belongs, delete what it deletes,
    /// and drop the records it leaves without a branch.
    pub fn apply(self) -> anyhow::Result<Outcome> {
        let unconfirmed = self.unconfirmed.clone();
        let pins = crate::pins::live_pins(self.request.primary)?;
        let mut outcome = Outcome::default();
        let mut member_survives = false;

        for index in 1..self.repos.len() {
            member_survives |= self.judge_member(index, &unconfirmed, &pins, &mut outcome)?;
        }
        self.judge_project(&unconfirmed, &pins, member_survives, &mut outcome)?;

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
        pins: &BTreeMap<RepoPath, Vec<LivePin>>,
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
        let remote_kept = self.delete_remote(repo, pins, outcome)?;
        let Some(local) = local else {
            return Ok(remote_kept);
        };
        let member = repo.member.as_ref().expect("a member repo names its path");
        let held = self.pinned(repo, member, &local, pins)?;
        if !held.is_empty() {
            self.survives(repo, Reason::Pinned, held, outcome);
            return Ok(true);
        }
        Ok(self.delete_local(repo, &local, outcome)? || remote_kept)
    }

    fn judge_project(
        &self,
        unconfirmed: &BTreeMap<usize, BTreeMap<String, String>>,
        pins: &BTreeMap<RepoPath, Vec<LivePin>>,
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
        self.delete_remote(repo, pins, outcome)?;
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
        let refs = landing_refs(
            repo.vcs.as_ref(),
            &repo.dir,
            &repo.names_in_target,
            repo.url_remote.as_ref(),
        )?;
        let onto = tips(&refs);
        let proof = match DestroyProof::landed_on(
            repo.vcs.as_ref(),
            &repo.dir,
            Site::Local,
            &self.branch_name,
            &refs,
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
        let deleted = self.destroy(repo, proof, Site::Local, outcome)?;
        if deleted {
            let pins = crate::pins::live_pins(self.request.primary)?;
            let held = repo
                .member
                .as_ref()
                .and_then(|m| pins.get(m))
                .map(Vec::as_slice)
                .unwrap_or_default();
            crate::backup::drop_rebased_of_branch(
                repo.vcs.as_ref(),
                &repo.dir,
                &self.branch_name,
                held,
            );
        }
        Ok(!deleted)
    }

    fn survives(&self, repo: &Repo, reason: Reason, detail: Vec<String>, outcome: &mut Outcome) {
        outcome.survivors.push(Survivor {
            path: repo.label(),
            branch: self.branch_name.to_string(),
            reason,
            detail,
        });
    }

    /// Delete what `proof` names. A remote that does not take the delete
    /// keeps its branch, reported unconfirmed with its answer. Whether it was
    /// deleted.
    fn destroy(
        &self,
        repo: &Repo,
        proof: DestroyProof,
        site: Site,
        outcome: &mut Outcome,
    ) -> anyhow::Result<bool> {
        let because = proof.describe();
        let backup = proof.backup().map(|b| b.backup().name.to_string());
        match (repo.vcs.delete_branch(proof), &site) {
            (Ok(()), _) => {}
            (Err(e), Site::Remote(remote)) => {
                let mut detail = vec![format!("{remote} did not take the delete: {e:#}")];
                detail.extend(backup.map(|b| format!("kept as backup {b}")));
                self.survives(repo, Reason::Unconfirmed, detail, outcome);
                return Ok(false);
            }
            (Err(e), Site::Local) => {
                return Err(anyhow::Error::new(e).context(format!(
                    "{}: could not delete {}",
                    repo.label(),
                    self.branch_name
                )))
            }
        }
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
        Ok(true)
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

    fn target_upstream_refs(&self, repo: &Repo) -> anyhow::Result<Vec<NamedTip>> {
        let mut refs = Vec::new();
        for name in &repo.names_in_target {
            refs.extend(upstream_copy(
                repo.vcs.as_ref(),
                &repo.dir,
                name,
                repo.url_remote.as_ref(),
            )?);
        }
        Ok(refs)
    }

    /// The push branch, when `--delete-remote` or `--delete-remote-unlanded`
    /// reaches it and the proof it needs can be made.
    fn delete_remote(
        &self,
        repo: &Repo,
        pins: &BTreeMap<RepoPath, Vec<LivePin>>,
        outcome: &mut Outcome,
    ) -> anyhow::Result<bool> {
        let flags = self.request.remote_deletes;
        let named = flags.unlanded.contains(&repo.operand);
        if !flags.landed && !named {
            return Ok(false);
        }
        let Some(pushed) = self.push_branch_of(repo)? else {
            return Ok(false);
        };
        let site = Site::Remote(pushed.remote.clone());
        let onto = self.target_upstream_refs(repo)?;
        let landed = DestroyProof::landed_on(
            repo.vcs.as_ref(),
            &repo.dir,
            site.clone(),
            &self.branch_name,
            &onto,
        )?;
        if landed.is_none() && !(named && self.request.discard_unlanded.is_some()) {
            return Ok(false);
        }
        if let Some(member) = &repo.member {
            let removed = BranchRef::Remote {
                remote: pushed.remote.clone(),
                name: self.branch_name.clone(),
            };
            let change = RefChange {
                branch: &removed,
                was: &pushed.tip,
                now: None,
                was_backed_up: landed.is_none(),
            };
            let held = pins.get(member).map(Vec::as_slice).unwrap_or_default();
            let stranded = stranded_by(repo.vcs.as_ref(), &repo.dir, held, &change, |pin| {
                self.own_lock_goes(pin)
            })?;
            if !stranded.is_empty() {
                let detail = stranded.iter().map(|s| s.line(&change)).collect();
                self.survives(repo, Reason::Pinned, detail, outcome);
                return Ok(true);
            }
        }
        let proof = match (landed, self.request.discard_unlanded) {
            (Some(proof), _) => proof,
            (None, Some(consent)) => DestroyProof::backed_up(
                repo.vcs.as_ref(),
                &repo.dir,
                site.clone(),
                &self.branch_name,
                consent,
            )?,
            (None, None) => return Ok(false),
        };
        Ok(!self.destroy(repo, proof, site, outcome)?)
    }

    /// A lock of `W` itself whose branch this run deletes: the project branch
    /// here, and its push branch when the flags reach it.
    fn own_lock_goes(&self, pin: &LivePin) -> bool {
        if pin.project != *self.request.project || pin.branch.name() != &self.branch_name {
            return false;
        }
        match &pin.branch {
            BranchRef::Local(_) => true,
            BranchRef::Remote { .. } => {
                let project = &self.repos[0];
                let flags = self.request.remote_deletes;
                (flags.unlanded.contains(&project.operand)
                    && self.request.discard_unlanded.is_some())
                    || (flags.landed && matches!(self.project_push_branch_landed(), Ok(true)))
            }
        }
    }

    fn project_push_branch_landed(&self) -> anyhow::Result<bool> {
        let project = &self.repos[0];
        let Some(pushed) = self.push_branch_of(project)? else {
            return Ok(false);
        };
        Ok(DestroyProof::landed_on(
            project.vcs.as_ref(),
            &project.dir,
            Site::Remote(pushed.remote),
            &self.branch_name,
            &self.target_upstream_refs(project)?,
        )?
        .is_some())
    }

    /// The commits on the published copy that the branch it lands into does
    /// not reach, when the project branch has a published copy at all.
    fn open_head(&self, repo: &Repo) -> anyhow::Result<Option<Vec<String>>> {
        let Some(pushed) = self.push_branch_of(repo)? else {
            return Ok(None);
        };
        let onto = tips(&landing_refs(
            repo.vcs.as_ref(),
            &repo.dir,
            &repo.names_in_target,
            repo.url_remote.as_ref(),
        )?);
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

    /// The live pins that only the local branch reaches: pins in locks
    /// other than the weave branch's own.
    fn pinned(
        &self,
        repo: &Repo,
        member: &RepoPath,
        tip: &ResolvedRevisionId,
        pins: &BTreeMap<RepoPath, Vec<LivePin>>,
    ) -> anyhow::Result<Vec<String>> {
        let Some(live) = pins.get(member) else {
            return Ok(Vec::new());
        };
        let local = BranchRef::Local(self.branch_name.clone());
        let change = RefChange {
            branch: &local,
            was: tip,
            now: None,
            was_backed_up: false,
        };
        let own = |pin: &LivePin| {
            pin.project == *self.request.project && pin.branch.name() == &self.branch_name
        };
        Ok(
            stranded_by(repo.vcs.as_ref(), &repo.dir, live, &change, own)?
                .iter()
                .map(|s| s.line(&change))
                .collect(),
        )
    }
}

/// `W`'s `rwv.toml` at its tip: `None` where the tip has none; a pre-schema
/// one refuses the rule, which acts on that branch.
fn w_manifest(
    vcs: &dyn Vcs,
    dir: &Path,
    tip: &ResolvedRevisionId,
    project: &crate::manifest::ProjectName,
    branch: &crate::records::BranchName,
    verb: &'static str,
) -> anyhow::Result<Option<Manifest>> {
    let text = match vcs.read_file_at_revision(dir, tip, Path::new(Manifest::FILE_NAME)) {
        Ok(text) => text,
        Err(crate::vcs::VcsError::RevisionNotFound { .. }) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    match Manifest::from_toml_str(&text) {
        Ok(manifest) => Ok(Some(manifest)),
        Err(e) => Err(State::UnparseableProjectAt
            .of(project_rel_path(project))
            .project_repo()
            .subject(branch)
            .listing([format!("{e:#}")])
            .bind("<b>", branch)
            .by_verb(verb)
            .judge(vcs)
            .refused()),
    }
}

fn manifest_at(vcs: &dyn Vcs, dir: &Path, tip: &ResolvedRevisionId) -> Option<Manifest> {
    let text = vcs
        .read_file_at_revision(dir, tip, Path::new(Manifest::FILE_NAME))
        .ok()?;
    Manifest::from_toml_str(&text).ok()
}
