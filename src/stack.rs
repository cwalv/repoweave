//! Dependents carried along a rewrite of their target's branches: each one
//! replayed from its target's old tip onto its new one, parents first, all
//! composed before any of their branches moves.

use crate::diagnosis::{Report, State};
use crate::manifest::{Manifest, ProjectName, RepoKey};
use crate::op_state::{Carrying, OpVerb, Rebasing, RefMove};
use crate::records::BranchName;
use crate::refusal::RefusalKind;
use crate::vcs::{
    vcs_for, BackupMark, BackupSource, CommitId, ConflictOp, DerivedContentPolicy, LocalRefName,
    RawRefName, ResolvedRevisionId, Vcs, VcsError,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

fn rev(commit: &CommitId) -> ResolvedRevisionId {
    ResolvedRevisionId::from(commit.clone())
}

/// The repos of one workspace a stack is carried in.
pub(crate) struct Repos<'a> {
    pub(crate) root: &'a Path,
    pub(crate) project: &'a ProjectName,
    pub(crate) manifest: &'a Manifest,
}

impl Repos<'_> {
    fn dir(&self, repo: &RepoKey) -> PathBuf {
        match repo {
            RepoKey::Project => crate::records::project_repo(self.root, self.project),
            RepoKey::Member(path) => self.root.join(path.as_path()),
        }
    }

    fn vcs(&self, repo: &RepoKey) -> Box<dyn Vcs> {
        match repo {
            RepoKey::Project => crate::vcs::project_vcs(),
            RepoKey::Member(path) => self
                .manifest
                .get_entry(path)
                .map(|entry| vcs_for(entry.vcs_type))
                .unwrap_or_else(crate::vcs::probe_vcs),
        }
    }

    fn label(&self, repo: &RepoKey) -> String {
        crate::manifest::RepoOperand::of(repo, self.project).to_string()
    }
}

/// A dependent carried along, with its branch's tip in each repo its target
/// is rewritten in.
#[derive(Debug, Clone)]
pub(crate) struct Carry {
    pub(crate) dependent: BranchName,
    pub(crate) parent: BranchName,
    pub(crate) tips: Vec<(RepoKey, CommitId)>,
}

/// Why a dependent cannot be carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Why {
    /// Its branch in these repos lacks its target's tip there.
    LacksTip(Vec<String>),
    /// Its branch is checked out in these checkouts.
    CheckedOut(Vec<PathBuf>),
    /// Its branch here is only on these remotes.
    OnAnotherMachine(Vec<String>),
}

#[derive(Debug, Clone)]
pub(crate) struct Uncarried {
    pub(crate) dependent: BranchName,
    pub(crate) parent: BranchName,
    pub(crate) why: Why,
}

#[derive(Debug, Default)]
pub(crate) struct Stack {
    pub(crate) carried: Vec<Carry>,
    pub(crate) uncarried: Vec<Uncarried>,
}

/// The dependents a rewrite of `w`'s branches would carry, parents first, and
/// those it could not; `moved` names each repo `w`'s branch is rewritten in,
/// with its tip there before. A dependent's own dependents are walked only
/// where it is carried. A record whose branch exists nowhere is no
/// dependent, and neither is one known only on a remote `w` is not
/// published on.
pub(crate) fn walk(
    repos: &Repos<'_>,
    w: &BranchName,
    moved: &[(RepoKey, CommitId)],
) -> anyhow::Result<Stack> {
    let mut stack = Stack::default();
    if moved.is_empty() {
        return Ok(stack);
    }
    let project_dir = repos.dir(&RepoKey::Project);
    let project_vcs = repos.vcs(&RepoKey::Project);
    let view = crate::records::Store::open(project_vcs.as_ref(), &project_dir)?.view()?;
    let mut seen: BTreeSet<String> = BTreeSet::from([w.as_str().to_owned()]);
    let mut queue: VecDeque<(BranchName, Vec<(RepoKey, CommitId)>)> =
        VecDeque::from([(w.clone(), moved.to_vec())]);
    while let Some((parent, parent_moved)) = queue.pop_front() {
        for dependent in view.dependents(&parent) {
            if !seen.insert(dependent.as_str().to_owned()) {
                continue;
            }
            let name = RawRefName::new(dependent.as_str());
            let held = project_vcs.resolve_local_branch_tip(&project_dir, &name)?;
            if held.is_none() {
                let copies = project_vcs.remote_copies(&project_dir, &name)?;
                let parents =
                    project_vcs.remote_copies(&project_dir, &RawRefName::new(parent.as_str()))?;
                let shared: Vec<String> = copies
                    .iter()
                    .filter(|copy| parents.iter().any(|p| p.remote == copy.remote))
                    .map(|copy| copy.remote.name().to_owned())
                    .collect();
                if !shared.is_empty() {
                    stack.uncarried.push(Uncarried {
                        dependent,
                        parent: parent.clone(),
                        why: Why::OnAnotherMachine(shared),
                    });
                }
                continue;
            }
            let mut lacking = Vec::new();
            let mut checked_out = Vec::new();
            let mut tips = Vec::new();
            for (repo, old) in &parent_moved {
                let dir = repos.dir(repo);
                let vcs = repos.vcs(repo);
                let Some(tip) = vcs.resolve_local_branch_tip(&dir, &name)? else {
                    continue;
                };
                checked_out.extend(vcs.checkouts_of_branch(&dir, &name)?);
                if !vcs.is_ancestor(&dir, &rev(old), &tip)? {
                    lacking.push(repos.label(repo));
                }
                tips.push((repo.clone(), tip.id().clone()));
            }
            let project_moved = parent_moved.iter().any(|(r, _)| r == &RepoKey::Project);
            if !tips.is_empty() && !project_moved {
                if let Some(tip) = held.as_ref() {
                    checked_out.extend(project_vcs.checkouts_of_branch(&project_dir, &name)?);
                    tips.push((RepoKey::Project, tip.id().clone()));
                }
            }
            let why = if !lacking.is_empty() {
                Some(Why::LacksTip(lacking))
            } else if !checked_out.is_empty() {
                Some(Why::CheckedOut(checked_out))
            } else {
                None
            };
            match why {
                Some(why) => stack.uncarried.push(Uncarried {
                    dependent,
                    parent: parent.clone(),
                    why,
                }),
                None => {
                    stack.carried.push(Carry {
                        dependent: dependent.clone(),
                        parent: parent.clone(),
                        tips: tips.clone(),
                    });
                    queue.push_back((dependent, tips));
                }
            }
        }
    }
    Ok(stack)
}

/// The workweave a checkout belongs to, by its root's name.
fn workweave_of(checkout: &Path, project: &ProjectName) -> Option<String> {
    let prefix = format!("{project}--");
    checkout.ancestors().find_map(|dir| {
        dir.file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(&prefix))
            .map(str::to_owned)
    })
}

/// One `target` dependents state per dependent `stack` cannot carry, each
/// with its own choices; `t` is the target the rebind names.
pub(crate) fn judge(
    stack: &Stack,
    repos: &Repos<'_>,
    t: &str,
    verb: &'static str,
    report: &mut Report,
) {
    let project_dir = repos.dir(&RepoKey::Project);
    let vcs = repos.vcs(&RepoKey::Project);
    let label = repos.label(&RepoKey::Project);
    for uncarried in &stack.uncarried {
        let dependent = uncarried.dependent.as_str();
        let state = match &uncarried.why {
            Why::LacksTip(_) => State::TargetDependentLacksTip,
            Why::CheckedOut(_) => State::TargetDependentCheckedOut,
            Why::OnAnotherMachine(_) => State::TargetDependentOnAnotherMachine,
        };
        let mut judging = state
            .of(&label)
            .project_repo()
            .subject(format!("{dependent} lands into {}", uncarried.parent))
            .bind("<dep>", dependent)
            .bind("<W>", dependent)
            .bind("<T>", t)
            .bind("<project>", repos.project)
            .checkout(&project_dir)
            .by_verb(verb);
        judging = match &uncarried.why {
            Why::LacksTip(lacking) => judging.listing(lacking.iter().map(|repo| {
                format!(
                    "'{dependent}' lacks the tip of '{}' in {repo}",
                    uncarried.parent
                )
            })),
            Why::CheckedOut(at) => {
                if let Some(ww) = at.iter().find_map(|p| workweave_of(p, repos.project)) {
                    judging = judging.bind("<ww>", ww);
                }
                judging.listing(at.iter().map(|p| format!("checked out at {}", p.display())))
            }
            Why::OnAnotherMachine(remotes) => judging.listing(
                remotes
                    .iter()
                    .map(|remote| format!("'{dependent}' is only on {remote}")),
            ),
        };
        report.add(judging.judge(vcs.as_ref()));
    }
}

/// What carrying one dependent's branch in one repo did.
#[derive(Debug, Clone)]
pub(crate) struct Carried {
    pub(crate) repo: RepoKey,
    pub(crate) label: String,
    pub(crate) branch: String,
    pub(crate) was: CommitId,
    pub(crate) now: CommitId,
    pub(crate) backup: String,
}

/// The op a carry runs under.
pub(crate) struct Carrier<'a> {
    pub(crate) repos: Repos<'a>,
    pub(crate) op: &'a crate::op_state::OpId,
    pub(crate) workspace: &'a Path,
    pub(crate) verb: OpVerb,
    pub(crate) w: &'a BranchName,
    pub(crate) t: &'a str,
}

fn policy(repo: &RepoKey) -> DerivedContentPolicy {
    match repo {
        RepoKey::Project => DerivedContentPolicy::rewritten_after(),
        RepoKey::Member(_) => DerivedContentPolicy::keep_target_side(),
    }
}

fn take_in(workspace: &Path) -> anyhow::Result<crate::op_state::TakeInRecord> {
    Ok(crate::op_state::read_owner(workspace)?
        .and_then(|owner| owner.take_in)
        .unwrap_or_default())
}

impl Carrier<'_> {
    fn stopped(&self, repo: &RepoKey, dependent: &str, dir: &Path, vcs: &dyn Vcs) -> anyhow::Error {
        crate::refusal::refusal(
            RefusalKind::OpParked,
            format!(
                "rwv {verb}: carrying '{dependent}' along in {repo} stopped on a conflict at {}.\n\
                 {}\nthen run `{}`, or `rwv abort` to put everything back as it was; no \
                 dependent's branch moves until every one is carried",
                dir.display(),
                vcs.conflict_resolution_hint(ConflictOp::Rebase),
                crate::op_state::resume_command(self.verb),
                verb = self.verb,
                repo = self.repos.label(repo),
            ),
        )
    }

    /// Finish a carry a stop left in a checkout: drive its replay on, or
    /// take the tip the operator's own continue left, and put the checkout
    /// back on `w`'s branch. Run before anything reads the checkouts.
    pub(crate) fn finish_stopped(&self) -> anyhow::Result<()> {
        let record = take_in(self.workspace)?;
        for carrying in record.carries.iter().filter(|c| c.composed.is_none()) {
            let dir = self.repos.dir(&carrying.repo);
            let vcs = self.repos.vcs(&carrying.repo);
            if matches!(vcs.mid_op(&dir), Some(ConflictOp::Rebase)) {
                vcs.rebase_continue(&dir, policy(&carrying.repo))
                    .map_err(|_| {
                        self.stopped(&carrying.repo, &carrying.branch, &dir, vcs.as_ref())
                    })?;
            }
            if matches!(
                vcs.head_attachment(&dir)?,
                crate::vcs::HeadAttachment::Attached(_)
            ) {
                continue;
            }
            let head = vcs.head_revision(&dir)?.id().clone();
            if vcs.is_ancestor(&dir, &rev(&carrying.onto), &rev(&head))? {
                self.composed(carrying, head, &record.rebases)?;
            }
            self.back_on(carrying)?;
        }
        Ok(())
    }

    fn back_on(&self, carrying: &Carrying) -> anyhow::Result<()> {
        let dir = self.repos.dir(&carrying.repo);
        let vcs = self.repos.vcs(&carrying.repo);
        let branch = carrying.checkout_on.clone();
        if let crate::vcs::HeadAttachment::Attached(on) = vcs.head_attachment(&dir)? {
            if on.to_string() == branch {
                return Ok(());
            }
        }
        vcs.attach_head_to(&dir, &LocalRefName::of_branch(&RawRefName::new(branch)))?;
        Ok(())
    }

    /// Record `head` as what carrying `carrying` composed, a project carry
    /// first given the pins the member carries made.
    fn composed(
        &self,
        carrying: &Carrying,
        head: CommitId,
        rebases: &[Rebasing],
    ) -> anyhow::Result<CommitId> {
        let dir = self.repos.dir(&carrying.repo);
        let vcs = self.repos.vcs(&carrying.repo);
        let mut tip = head;
        if carrying.repo == RepoKey::Project {
            let replayed = crate::remap::recorded(
                vcs.as_ref(),
                &dir,
                &self.repos.label(&carrying.repo),
                &carrying.base,
                &carrying.from,
                &carrying.onto,
                &tip,
            )?;
            let carries = take_in(self.workspace)?.carries;
            let replays = crate::remap::member_replays(
                self.repos.root,
                self.repos.manifest,
                rebases,
                &carries,
            )?;
            let remap = crate::remap::remap_project(
                vcs.as_ref(),
                &dir,
                &replayed,
                &carrying.onto,
                &replays,
            )?;
            if let Some((rewritten, became)) = remap.rewritten {
                vcs.set_detached_head(&dir, &rev(&rewritten))?;
                vcs.record_replayed(&dir, &carrying.from, &carrying.onto, &became)?;
                tip = rewritten;
            }
        }
        crate::op_state::journal_carrying(
            self.workspace,
            Carrying {
                composed: Some(tip.clone()),
                ..carrying.clone()
            },
        )?;
        Ok(tip)
    }

    /// Carry every dependent of `w` along `w`'s rewrite, as the journaled
    /// rebases record it: claim each as a landing into it would, compose
    /// each in its repo's checkout, parents first, then move every
    /// dependent's branch. A dependent that can no longer be carried refuses
    /// before any of their branches moves. The claims are held until the op
    /// ends or is aborted.
    pub(crate) fn carry(&self) -> anyhow::Result<Vec<Carried>> {
        self.finish_stopped()?;
        let record = take_in(self.workspace)?;
        let mut new_tips: BTreeMap<(String, RepoKey), (CommitId, CommitId)> = BTreeMap::new();
        let mut moved = Vec::new();
        for rebase in record.rebases.iter() {
            let dir = self.repos.dir(&rebase.repo);
            let vcs = self.repos.vcs(&rebase.repo);
            let Some(now) =
                vcs.resolve_local_branch_tip(&dir, &RawRefName::new(rebase.branch.clone()))?
            else {
                continue;
            };
            moved.push((rebase.repo.clone(), rebase.from.clone()));
            new_tips.insert(
                (self.w.as_str().to_owned(), rebase.repo.clone()),
                (rebase.from.clone(), now.id().clone()),
            );
        }
        let stack = walk(&self.repos, self.w, &moved)?;
        let mut report = Report::default();
        let verb = match self.verb {
            OpVerb::Land => "land",
            _ => "sync",
        };
        judge(&stack, &self.repos, self.t, verb, &mut report);
        report.into_result()?;
        let project_dir = self.repos.dir(&RepoKey::Project);
        let project_vcs = self.repos.vcs(&RepoKey::Project);
        for carry in &stack.carried {
            crate::landing_claim::LandingClaim::take(
                project_vcs.as_ref(),
                &project_dir,
                self.repos.project,
                carry.dependent.as_str(),
                self.op,
                self.workspace,
            )?;
        }
        for carry in &stack.carried {
            let mut tips = carry.tips.clone();
            tips.sort_by_key(|(repo, _)| *repo == RepoKey::Project);
            for (repo, from) in &tips {
                let Some((base, onto)) = new_tips
                    .get(&(carry.parent.as_str().to_owned(), repo.clone()))
                    .map(|(_, new)| new.clone())
                    .map(|new| {
                        let (old, _) = &new_tips[&(carry.parent.as_str().to_owned(), repo.clone())];
                        (old.clone(), new)
                    })
                    .or(self.in_place(repo, &carry.parent, from)?)
                else {
                    continue;
                };
                let journaled = take_in(self.workspace)?
                    .carries
                    .into_iter()
                    .find(|c| &c.repo == repo && c.branch == carry.dependent.as_str());
                let tip = match journaled.and_then(|c| c.composed.clone().map(|t| (c, t))) {
                    Some((c, tip)) if c.from == *from || c.composed.as_ref() == Some(from) => tip,
                    _ => self.compose(repo, carry, from, &base, &onto, &record.rebases)?,
                };
                new_tips.insert(
                    (carry.dependent.as_str().to_owned(), repo.clone()),
                    (from.clone(), tip),
                );
            }
        }
        self.move_carried()
    }

    /// Where a dependent's project branch is replayed when its target's is
    /// not rewritten, so its pins can follow its carried members: onto the
    /// target's tip, where it holds it, else onto their merge base, its own
    /// commits replayed in place.
    fn in_place(
        &self,
        repo: &RepoKey,
        parent: &BranchName,
        from: &CommitId,
    ) -> anyhow::Result<Option<(CommitId, CommitId)>> {
        if repo != &RepoKey::Project {
            return Ok(None);
        }
        let dir = self.repos.dir(repo);
        let vcs = self.repos.vcs(repo);
        let Some(tip) = vcs.resolve_local_branch_tip(&dir, &RawRefName::new(parent.as_str()))?
        else {
            return Ok(None);
        };
        let base = if vcs.is_ancestor(&dir, &tip, &rev(from))? {
            tip.id().clone()
        } else {
            match vcs.merge_bases(&dir, tip.id(), from)?.into_iter().next() {
                Some(base) => base,
                None => return Ok(None),
            }
        };
        Ok(Some((base.clone(), base)))
    }

    fn compose(
        &self,
        repo: &RepoKey,
        carry: &Carry,
        from: &CommitId,
        base: &CommitId,
        onto: &CommitId,
        rebases: &[Rebasing],
    ) -> anyhow::Result<CommitId> {
        let dir = self.repos.dir(repo);
        let vcs = self.repos.vcs(repo);
        let crate::vcs::HeadAttachment::Attached(on) = vcs.head_attachment(&dir)? else {
            return Err(crate::untokened!(
                EnvironmentPassthrough,
                "{}: is on no branch, so no dependent is carried in it",
                dir.display()
            ));
        };
        let carrying = Carrying {
            repo: repo.clone(),
            branch: carry.dependent.as_str().to_owned(),
            checkout_on: on.to_string(),
            base: base.clone(),
            from: from.clone(),
            onto: onto.clone(),
            composed: None,
        };
        crate::op_state::journal_carrying(self.workspace, carrying.clone())?;
        vcs.set_detached_head(&dir, &rev(from))?;
        match vcs.rebase(&dir, &rev(onto), &rev(base), policy(repo)) {
            Ok(_) => {}
            Err(VcsError::RebaseConflict { .. }) => {
                return Err(self.stopped(repo, carry.dependent.as_str(), &dir, vcs.as_ref()))
            }
            Err(e) => return Err(e.into()),
        }
        let head = vcs.head_revision(&dir)?.id().clone();
        let tip = self.composed(&carrying, head, rebases)?;
        self.back_on(&carrying)?;
        Ok(tip)
    }

    /// Move each dependent's branch to what its carry composed, a `rebased`
    /// backup kept and the move journaled first; one already there is left.
    fn move_carried(&self) -> anyhow::Result<Vec<Carried>> {
        let record = take_in(self.workspace)?;
        let mut carried = Vec::new();
        for carrying in &record.carries {
            let Some(to) = &carrying.composed else {
                continue;
            };
            let dir = self.repos.dir(&carrying.repo);
            let vcs = self.repos.vcs(&carrying.repo);
            let branch = RawRefName::new(carrying.branch.clone());
            let now = vcs.resolve_local_branch_tip(&dir, &branch)?;
            if now.as_ref().map(|n| n.id()) == Some(to) {
                continue;
            }
            if now.as_ref().map(|n| n.id()) != Some(&carrying.from) {
                return Err(crate::untokened!(
                    EnvironmentPassthrough,
                    "{}: '{}' moved since the carry read it at {}; `rwv abort` puts back what \
                     this operation moved",
                    self.repos.label(&carrying.repo),
                    carrying.branch,
                    carrying.from.short()
                ));
            }
            let kept = vcs.create_backup(
                &dir,
                &BackupSource::Branch(branch.clone()),
                &rev(&carrying.from),
                Some(BackupMark::Rebased),
            )?;
            let backup = kept.backup().name.as_str().to_owned();
            crate::op_state::journal_rebased_backup(
                self.workspace,
                crate::op_state::KeptBackup {
                    repo: carrying.repo.clone(),
                    name: backup.clone(),
                    published: crate::backup::published(vcs.as_ref(), &dir, &branch),
                },
            )?;
            crate::op_state::journal_ref_move(
                self.workspace,
                RefMove {
                    repo: carrying.repo.clone(),
                    branch: carrying.branch.clone(),
                    from: carrying.from.clone(),
                    to: to.clone(),
                },
            )?;
            vcs.reset_branch_ref(&dir, &branch, &carrying.from, to, &kept)?;
            carried.push(Carried {
                repo: carrying.repo.clone(),
                label: self.repos.label(&carrying.repo),
                branch: carrying.branch.clone(),
                was: carrying.from.clone(),
                now: to.clone(),
                backup,
            });
        }
        Ok(carried)
    }
}
