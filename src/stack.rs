//! Dependents carried along a rewrite of their target's branches: each one
//! replayed from its target's old tip onto its new one, parents first, all
//! composed before any of their branches moves.

use crate::diagnosis::{ChoiceId, Report, State};
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
    pub(crate) primary: &'a Path,
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

/// A dependent known here only on remotes whose copies its target's tip
/// already holds, in every repo the rewrite moves: nothing to carry.
#[derive(Debug, Clone)]
pub(crate) struct Landed {
    pub(crate) dependent: BranchName,
    pub(crate) parent: BranchName,
    pub(crate) remotes: Vec<String>,
}

impl Landed {
    pub(crate) fn line(&self) -> String {
        format!(
            "'{}' has landed into '{}': nothing to carry; its branch still exists on {}",
            self.dependent,
            self.parent,
            self.remotes.join(", ")
        )
    }
}

#[derive(Debug, Default)]
pub(crate) struct Stack {
    pub(crate) carried: Vec<Carry>,
    pub(crate) uncarried: Vec<Uncarried>,
    pub(crate) landed: Vec<Landed>,
}

/// The dependents a rewrite of `w`'s branches would carry, parents first, and
/// those it could not; `moved` names each repo `w`'s branch is rewritten in,
/// with its tip there before. A dependent's own dependents are walked only
/// where it is carried. A record whose branch exists nowhere is no
/// dependent, and neither is one known only on a remote `w` is not
/// published on; one known only on remotes whose copies `w`'s old tips hold
/// is landed. A dependent's branch at the tip a journaled carry in
/// `carried` composed and moved it to is read at the tip it was carried
/// from, so a rerun judges the stack as the carry first did.
pub(crate) fn walk(
    repos: &Repos<'_>,
    w: &BranchName,
    moved: &[(RepoKey, CommitId)],
    carried: &[Carrying],
) -> anyhow::Result<Stack> {
    let before_carry =
        |repo: &RepoKey, dependent: &BranchName, now: &CommitId| -> anyhow::Result<CommitId> {
            let dir = repos.dir(repo);
            let vcs = repos.vcs(repo);
            for c in carried
                .iter()
                .filter(|c| &c.repo == repo && c.branch == dependent.as_str())
            {
                let Some(composed) = &c.composed else {
                    continue;
                };
                if composed == now || vcs.is_ancestor(&dir, &rev(composed), &rev(now))? {
                    return Ok(c.from.clone());
                }
            }
            Ok(now.clone())
        };
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
                if shared.is_empty() {
                    continue;
                }
                if holds_remote_copies(repos, &parent, &parent_moved, &name, &shared)? {
                    stack.landed.push(Landed {
                        dependent,
                        parent: parent.clone(),
                        remotes: shared,
                    });
                } else {
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
                let Some(now) = vcs.resolve_local_branch_tip(&dir, &name)? else {
                    continue;
                };
                let tip = before_carry(repo, &dependent, now.id())?;
                checked_out.extend(vcs.checkouts_of_branch(&dir, &name)?);
                if !vcs.is_ancestor(&dir, &rev(old), &rev(&tip))? {
                    lacking.push(repos.label(repo));
                }
                tips.push((repo.clone(), tip));
            }
            let project_moved = parent_moved.iter().any(|(r, _)| r == &RepoKey::Project);
            if !tips.is_empty() && !project_moved {
                if let Some(tip) = held.as_ref() {
                    checked_out.extend(project_vcs.checkouts_of_branch(&project_dir, &name)?);
                    tips.push((
                        RepoKey::Project,
                        before_carry(&RepoKey::Project, &dependent, tip.id())?,
                    ));
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

/// Whether `parent`'s tips before the rewrite hold every copy of `dependent`
/// on `remotes`, in each repo `parent_moved` names and in the project repo.
fn holds_remote_copies(
    repos: &Repos<'_>,
    parent: &BranchName,
    parent_moved: &[(RepoKey, CommitId)],
    dependent: &RawRefName,
    remotes: &[String],
) -> anyhow::Result<bool> {
    let mut tips = parent_moved.to_vec();
    if !tips.iter().any(|(repo, _)| repo == &RepoKey::Project) {
        let dir = repos.dir(&RepoKey::Project);
        let vcs = repos.vcs(&RepoKey::Project);
        match vcs.resolve_local_branch_tip(&dir, &RawRefName::new(parent.as_str()))? {
            Some(tip) => tips.push((RepoKey::Project, tip.id().clone())),
            None => return Ok(false),
        }
    }
    for (repo, old) in &tips {
        let dir = repos.dir(repo);
        let vcs = repos.vcs(repo);
        for copy in vcs.remote_copies(&dir, dependent)? {
            if remotes.iter().any(|r| r == copy.remote.name())
                && !vcs.is_ancestor(&dir, &copy.tip, &rev(old))?
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
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
/// with its own choices; `t` is the target the rebind names. `held` is
/// `None` where no operation of this workspace stands, and else the branches
/// it holds the landing claim on: a landing into one of those cannot run, and
/// nothing that moves or deletes one can either, so a dependent held leaves
/// only the abort that puts the operation back; the abort is offered only
/// where an operation stands.
pub(crate) fn judge(
    stack: &Stack,
    repos: &Repos<'_>,
    t: &str,
    verb: &'static str,
    held: Option<&[&str]>,
    report: &mut Report,
) -> anyhow::Result<()> {
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
        use ChoiceId as Choice;
        match held {
            None => judging = judging.without(&[Choice::LostRace]),
            Some(claimed) => {
                if claimed.contains(&dependent) {
                    judging = judging.without(&[Choice::C35, Choice::C16, Choice::C12]);
                } else if claimed.contains(&uncarried.parent.as_str()) {
                    judging = judging.without(&[Choice::C35]);
                }
            }
        }
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
    judge_protection(stack, repos, t, verb, report)
}

/// One `declared` protection state per member branch a carry would rewrite
/// that a live weave branch declares.
fn judge_protection(
    stack: &Stack,
    repos: &Repos<'_>,
    t: &str,
    verb: &'static str,
    report: &mut Report,
) -> anyhow::Result<()> {
    let members: Vec<_> = stack
        .carried
        .iter()
        .flat_map(|carry| {
            carry.tips.iter().filter_map(move |(repo, _)| match repo {
                RepoKey::Member(path) => Some((path, &carry.dependent)),
                RepoKey::Project => None,
            })
        })
        .collect();
    if members.is_empty() {
        return Ok(());
    }
    let accounting = crate::alignment::Accounting::scan(repos.primary)?;
    for (path, dependent) in members {
        let declarers = accounting.declarers(path, dependent.as_str());
        if declarers.is_empty() {
            continue;
        }
        report.add(
            State::ProtectedDeclared
                .of(path)
                .member()
                .subject(dependent.as_str())
                .listing(declarers)
                .bind("<m>", path)
                .bind("<W>", dependent.as_str())
                .bind("<T>", t)
                .only(&[ChoiceId::C12])
                .by_verb(verb)
                .judge(repos.vcs(&RepoKey::Project).as_ref()),
        );
    }
    Ok(())
}

/// Refuse where a carry would write over something a checkout holds that no
/// commit records: a path a carried dependent's tip adds, or a directory
/// above one, standing untracked, ignored or not, where HEAD tracks nothing.
fn refuse_in_the_way(stack: &Stack, repos: &Repos<'_>, verb: OpVerb) -> anyhow::Result<()> {
    let mut in_the_way = Vec::new();
    for carry in &stack.carried {
        for (repo, tip) in &carry.tips {
            let dir = repos.dir(repo);
            let vcs = repos.vcs(repo);
            let head = vcs.head_revision(&dir)?;
            for changed in vcs.changed_paths_between(&dir, &head, &rev(tip))? {
                let changed = Path::new(&changed);
                let standing = changed
                    .ancestors()
                    .filter(|p| !p.as_os_str().is_empty())
                    .find(|p| match std::fs::symlink_metadata(dir.join(p)) {
                        Ok(meta) => *p == changed || !meta.is_dir(),
                        Err(_) => false,
                    });
                if let Some(path) = standing {
                    if !vcs.is_tracked(&dir, path)? {
                        in_the_way.push(format!(
                            "{}: {} ('{}' tracks it)",
                            repos.label(repo),
                            path.display(),
                            carry.dependent
                        ));
                    }
                }
            }
        }
    }
    if in_the_way.is_empty() {
        return Ok(());
    }
    crate::refuse!(
        RefusalKind::UntrackedCollision,
        "carrying a dependent along would write over what these checkouts hold untracked, \
         ignored files included:\n  {}\nmove them aside, then run `{}`",
        in_the_way.join("\n  "),
        crate::op_state::resume_command(verb)
    );
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

/// What a carry moved, and the dependents it found landed already.
#[derive(Debug, Default)]
pub(crate) struct Carries {
    pub(crate) carried: Vec<Carried>,
    pub(crate) landed: Vec<Landed>,
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

    fn failed(
        &self,
        repo: &RepoKey,
        dependent: &str,
        dir: &Path,
        error: &VcsError,
    ) -> anyhow::Error {
        crate::refusal::refusal(
            RefusalKind::OpParked,
            format!(
                "rwv {verb}: carrying '{dependent}' along in {repo} failed at {}: {error:#}\n\
                 Nothing moved; fix the cause, then run `{}`, or `rwv abort` to put everything \
                 back as it was",
                dir.display(),
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
        for carrying in record.carries.iter().filter(|c| c.composed.is_some()) {
            let dir = self.repos.dir(&carrying.repo);
            let vcs = self.repos.vcs(&carrying.repo);
            if !matches!(
                vcs.head_attachment(&dir)?,
                crate::vcs::HeadAttachment::Attached(_)
            ) {
                self.back_on(carrying)?;
            }
        }
        for carrying in record.carries.iter().filter(|c| c.composed.is_none()) {
            let dir = self.repos.dir(&carrying.repo);
            let vcs = self.repos.vcs(&carrying.repo);
            if matches!(vcs.mid_op(&dir), Some(ConflictOp::Rebase)) {
                vcs.rebase_continue(&dir, policy(&carrying.repo))
                    .map_err(|e| match e {
                        VcsError::RebaseConflict { .. } => {
                            self.stopped(&carrying.repo, &carrying.branch, &dir, vcs.as_ref())
                        }
                        e => self.failed(&carrying.repo, &carrying.branch, &dir, &e),
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

    /// Record `head` as what carrying `carrying` composed, once the replay's
    /// record describes it, a project carry first given the pins the member
    /// carries made.
    fn composed(
        &self,
        carrying: &Carrying,
        head: CommitId,
        rebases: &[Rebasing],
    ) -> anyhow::Result<CommitId> {
        let dir = self.repos.dir(&carrying.repo);
        let vcs = self.repos.vcs(&carrying.repo);
        let mut tip = head;
        let replayed = crate::remap::recorded(
            vcs.as_ref(),
            &dir,
            &self.repos.label(&carrying.repo),
            &carrying.base,
            &carrying.from,
            &carrying.onto,
            &tip,
            crate::remap::HeldBy {
                workspace: self.workspace,
                repo: &carrying.repo,
            },
        )?;
        if carrying.repo == RepoKey::Project {
            let carries = take_in(self.workspace)?.carries;
            let replays = crate::remap::member_replays(
                self.repos.root,
                self.repos.manifest,
                rebases,
                &carries,
                self.workspace,
            )?;
            let remap = crate::remap::remap_project(
                vcs.as_ref(),
                &dir,
                &replayed,
                &carrying.onto,
                &replays,
            )?;
            if let Some((rewritten, became)) = remap.rewritten {
                vcs.record_replayed(&dir, &carrying.from, &carrying.onto, &became)?;
                crate::test_pause::point("carry-recorded");
                vcs.set_detached_head(&dir, &rev(&rewritten))?;
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
        crate::test_pause::point("carry-journaled");
        Ok(tip)
    }

    /// Carry every dependent of `w` along `w`'s rewrite, as the journaled
    /// rebases record it: claim each as a landing into it would, compose
    /// each in its repo's checkout, parents first, then move every
    /// dependent's branch. A dependent that can no longer be carried refuses
    /// before any of their branches moves. A dependent this op already moved
    /// stays where it moved it. The claims are held until the op ends or is
    /// aborted.
    pub(crate) fn carry(&self) -> anyhow::Result<Carries> {
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
        let stack = walk(&self.repos, self.w, &moved, &record.carries)?;
        let mut report = Report::default();
        let verb = match self.verb {
            OpVerb::Land => "land",
            _ => "sync",
        };
        let claimed: Vec<&str> = record
            .carries
            .iter()
            .map(|carrying| carrying.branch.as_str())
            .collect();
        judge(
            &stack,
            &self.repos,
            self.t,
            verb,
            Some(&claimed),
            &mut report,
        )?;
        report.into_result()?;
        refuse_in_the_way(&stack, &self.repos, self.verb)?;
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
                let moved = record.carries.iter().find_map(|c| {
                    let to = c.composed.as_ref()?;
                    (&c.repo == repo
                        && c.branch == carry.dependent.as_str()
                        && record
                            .ref_moves
                            .iter()
                            .any(|m| &m.repo == repo && m.branch == c.branch && &m.to == to))
                    .then(|| to.clone())
                });
                if let Some(to) = moved {
                    new_tips.insert(
                        (carry.dependent.as_str().to_owned(), repo.clone()),
                        (from.clone(), to),
                    );
                    continue;
                }
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
                let carries = take_in(self.workspace)?.carries;
                let journaled = carries
                    .iter()
                    .find(|c| &c.repo == repo && c.branch == carry.dependent.as_str());
                let sibling = carries.iter().find(|c| {
                    &c.repo == repo
                        && c.branch != carry.dependent.as_str()
                        && c.composed.is_some()
                        && c.from == *from
                        && c.base == base
                        && c.onto == onto
                });
                let reused = journaled.and_then(|c| c.composed.clone().map(|t| (c, t)));
                let tip = match (reused, sibling) {
                    (Some((c, tip)), _)
                        if (c.from == *from || c.composed.as_ref() == Some(from))
                            && c.base == base
                            && c.onto == onto =>
                    {
                        tip
                    }
                    (_, Some(sibling)) => {
                        crate::op_state::journal_carrying(
                            self.workspace,
                            Carrying {
                                branch: carry.dependent.as_str().to_owned(),
                                ..sibling.clone()
                            },
                        )?;
                        sibling.composed.clone().unwrap_or_else(|| from.clone())
                    }
                    _ => {
                        let tip = self.compose(repo, carry, from, &base, &onto, &record.rebases)?;
                        crate::test_pause::point("carry-composed");
                        tip
                    }
                };
                new_tips.insert(
                    (carry.dependent.as_str().to_owned(), repo.clone()),
                    (from.clone(), tip),
                );
            }
        }
        Ok(Carries {
            carried: self.move_carried(verb, &stack.carried)?,
            landed: stack.landed,
        })
    }

    /// Where a dependent's project branch is replayed when its target's is
    /// not rewritten, so its pins can follow its carried members: its own
    /// commits, those beyond the target's tip where it holds it, else beyond
    /// their merge base, onto the target's tip, whose lock pins what the
    /// members' rewrites made.
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
        Ok(Some((base, tip.id().clone())))
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
            Err(e) => {
                vcs.cancel_in_flight_op(&dir);
                self.back_on(&carrying)?;
                return Err(self.failed(repo, carry.dependent.as_str(), &dir, &e));
            }
        }
        let head = vcs.head_revision(&dir)?.id().clone();
        let tip = self.composed(&carrying, head, rebases)?;
        self.back_on(&carrying)?;
        Ok(tip)
    }

    fn moved_before(
        &self,
        record: &crate::op_state::TakeInRecord,
        carrying: &Carrying,
        to: &CommitId,
    ) -> Option<Carried> {
        record.ref_moves.iter().find(|m| {
            m.repo == carrying.repo
                && m.branch == carrying.branch
                && m.from == carrying.from
                && &m.to == to
        })?;
        let kept = format!("{}/rebased-{}", carrying.branch, carrying.from.short());
        let backup = record
            .rebased_backups
            .iter()
            .find(|b| b.repo == carrying.repo && b.name.starts_with(&kept))?
            .name
            .clone();
        Some(Carried {
            repo: carrying.repo.clone(),
            label: self.repos.label(&carrying.repo),
            branch: carrying.branch.clone(),
            was: carrying.from.clone(),
            now: to.clone(),
            backup,
        })
    }

    /// Move each dependent's branch to what its carry composed, a `rebased`
    /// backup kept and the move journaled first; one already there, or built
    /// on since, is left. Where any other moved since the carry read it,
    /// nothing moves.
    fn move_carried(&self, verb: &'static str, carries: &[Carry]) -> anyhow::Result<Vec<Carried>> {
        crate::test_pause::point("carry-move");
        let record = take_in(self.workspace)?;
        let mut moving = Vec::new();
        let mut carried = Vec::new();
        let mut raced = Vec::new();
        let mut opened = Stack::default();
        for carrying in &record.carries {
            let Some(to) = &carrying.composed else {
                continue;
            };
            let dir = self.repos.dir(&carrying.repo);
            let vcs = self.repos.vcs(&carrying.repo);
            let branch = RawRefName::new(carrying.branch.clone());
            let now = vcs
                .resolve_local_branch_tip(&dir, &branch)?
                .map(|n| n.id().clone());
            match &now {
                Some(now) if now == to || vcs.is_ancestor(&dir, &rev(to), &rev(now))? => {
                    if let Some(done) = self.moved_before(&record, carrying, to) {
                        carried.push(done);
                    }
                }
                Some(now) if now == &carrying.from => {
                    let checkouts = vcs.checkouts_of_branch(&dir, &branch)?;
                    let carry = carries
                        .iter()
                        .find(|c| c.dependent.as_str() == carrying.branch);
                    match carry {
                        Some(carry) if !checkouts.is_empty() => {
                            if let Some(listed) = opened
                                .uncarried
                                .iter_mut()
                                .find(|u| u.dependent.as_str() == carrying.branch)
                            {
                                if let Why::CheckedOut(at) = &mut listed.why {
                                    at.extend(checkouts);
                                }
                            } else {
                                opened.uncarried.push(Uncarried {
                                    dependent: carry.dependent.clone(),
                                    parent: carry.parent.clone(),
                                    why: Why::CheckedOut(checkouts),
                                });
                            }
                        }
                        _ => moving.push((carrying, to)),
                    }
                }
                _ => raced.push(format!(
                    "'{}' in {} is {}, not at {}, where the carry read it",
                    carrying.branch,
                    self.repos.label(&carrying.repo),
                    now.as_ref()
                        .map_or_else(|| "gone".to_owned(), |now| format!("at {}", now.short())),
                    carrying.from.short()
                )),
            }
        }
        if !opened.uncarried.is_empty() {
            let claimed: Vec<&str> = record.carries.iter().map(|c| c.branch.as_str()).collect();
            let mut report = Report::default();
            judge(
                &opened,
                &self.repos,
                self.t,
                verb,
                Some(&claimed),
                &mut report,
            )?;
            report.into_result()?;
        }
        if !raced.is_empty() {
            let mut report = Report::default();
            report.add(
                State::TargetDependentMovedMidOperation
                    .of(self.repos.label(&RepoKey::Project))
                    .project_repo()
                    .subject("a dependent's branch the carry moves moved after the carry read it")
                    .listing(raced)
                    .by_verb(verb)
                    .judge(self.repos.vcs(&RepoKey::Project).as_ref()),
            );
            report.into_result()?;
        }
        for (carrying, to) in moving {
            let dir = self.repos.dir(&carrying.repo);
            let vcs = self.repos.vcs(&carrying.repo);
            let branch = RawRefName::new(carrying.branch.clone());
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
        crate::test_pause::point("carry-moved");
        Ok(carried)
    }
}
