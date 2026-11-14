//! Where a member's checkout stands against the commit a lock pins for it,
//! and the live locks that pin commits of a member.
//!
//! A pin is live when it is in the lock at the tip of any local or
//! remote-tracking branch of any project of the weave. A commit beyond a pin
//! that no remote-tracking branch reaches and no live lock reaches is
//! unpinned: nothing but this checkout holds it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Context;

use crate::branch_ref::BranchRef;
use crate::diagnosis::{ChoiceId, Fact, FactKind, Form, Judged, State};
use crate::manifest::{LockFile, ProjectName, RepoPath};
use crate::vcs::{
    project_vcs, CommitId, CommitSummary, Relation, ResolvedRevisionId, Vcs, VcsError,
};
use crate::workspace::{discover_projects, project_dir};

const LISTED_COMMITS_CAP: usize = 20;

/// A pin some project's live lock holds, and the lock that holds it.
#[derive(Clone)]
pub(crate) struct LivePin {
    pub(crate) commit: CommitId,
    pub(crate) project: ProjectName,
    /// The project branch whose lock holds it.
    pub(crate) branch: BranchRef,
    /// The project and branch whose lock holds it, as an operator reads them.
    pub(crate) held_by: String,
}

/// Every live pin in the weave, by the member it pins: the commits named by
/// the `rwv.lock` at the tip of each local and remote-tracking branch of each
/// project's repo. A branch with no lock is no weave branch and pins nothing,
/// and a lock that does not parse pins nothing.
pub(crate) fn live_pins(primary: &Path) -> anyhow::Result<BTreeMap<RepoPath, Vec<LivePin>>> {
    let mut pins: BTreeMap<RepoPath, Vec<LivePin>> = BTreeMap::new();
    let vcs = project_vcs();
    for project in discover_projects(primary) {
        let dir = project_dir(primary, &project);
        if !vcs.is_repo(&dir) {
            continue;
        }
        let tips = vcs
            .branch_tips(&dir)
            .with_context(|| format!("projects/{project}: could not list its branches"))?;
        for branch in tips {
            let text = match vcs.read_file_at_revision(
                &dir,
                &branch.tip,
                Path::new(LockFile::FILE_NAME),
            ) {
                Ok(text) => text,
                Err(VcsError::RevisionNotFound { .. }) => continue,
                Err(e) => {
                    return Err(anyhow::Error::new(e).context(format!(
                        "projects/{project}: could not read the lock at {}",
                        branch.label
                    )))
                }
            };
            let Ok(lock) = LockFile::from_json_str(&text) else {
                continue;
            };
            for (path, entry) in lock.iter_entries() {
                if let Some(commit) = entry.version.as_commit_id() {
                    pins.entry(path.clone()).or_default().push(LivePin {
                        commit,
                        project: project.clone(),
                        branch: branch.branch.clone(),
                        held_by: format!("{project} {}", branch.label),
                    });
                }
            }
        }
    }
    Ok(pins)
}

/// The commits a checkout holds beyond its pin.
pub(crate) struct Beyond {
    /// Unpushed, and reached by no live lock: nothing else holds them.
    pub(crate) unpinned: Vec<CommitSummary>,
    /// The live locks that reach an unpushed commit beyond the pin.
    pub(crate) held_by: Vec<String>,
    /// How many commits the checkout holds beyond the pin, pushed or not.
    pub(crate) count: usize,
    /// Whether any of them is unpushed.
    pub(crate) any_unpushed: bool,
}

/// How a checkout's commit stands to the commit a lock pins.
pub(crate) enum Standing {
    At,
    Behind {
        by: usize,
    },
    Diverged,
    /// Beyond the pin, nothing unpinned.
    Ahead(Beyond),
    /// Beyond the pin by commits nothing else holds.
    Unpinned(Beyond),
}

fn reached_by(vcs: &dyn Vcs, dir: &Path, commit: &CommitId, pin: &CommitId) -> bool {
    vcs.is_ancestor(
        dir,
        &ResolvedRevisionId::from(commit.clone()),
        &ResolvedRevisionId::from(pin.clone()),
    )
    .unwrap_or(false)
}

/// Where `head` stands to `pin`, `live` being the live pins of this member.
pub(crate) fn standing(
    vcs: &dyn Vcs,
    dir: &Path,
    head: &ResolvedRevisionId,
    pin: &ResolvedRevisionId,
    live: &[LivePin],
) -> anyhow::Result<Standing> {
    Ok(match vcs.relation(dir, head, pin)? {
        Relation::Even => Standing::At,
        Relation::Behind => Standing::Behind {
            by: vcs.count_commits_in_range(dir, head, pin)?,
        },
        Relation::Diverged => Standing::Diverged,
        Relation::Ahead => {
            let count = vcs.count_commits_in_range(dir, pin, head)?;
            let unpushed = vcs.unpushed_beyond(dir, head, pin)?;
            let any_unpushed = !unpushed.is_empty();
            let mut unpinned = Vec::new();
            let mut held_by = BTreeSet::new();
            for commit in unpushed {
                let holding: Vec<&LivePin> = live
                    .iter()
                    .filter(|p| reached_by(vcs, dir, &commit.id, &p.commit))
                    .collect();
                if holding.is_empty() {
                    unpinned.push(commit);
                } else {
                    held_by.extend(holding.iter().map(|p| p.held_by.clone()));
                }
            }
            let beyond = Beyond {
                unpinned,
                held_by: held_by.into_iter().collect(),
                count,
                any_unpushed,
            };
            if beyond.unpinned.is_empty() {
                Standing::Ahead(beyond)
            } else {
                Standing::Unpinned(beyond)
            }
        }
    })
}

fn commits_text(count: usize) -> String {
    if count == 1 {
        "1 commit".to_owned()
    } else {
        format!("{count} commits")
    }
}

fn listing(commits: &[CommitSummary]) -> Vec<String> {
    let mut lines: Vec<String> = commits
        .iter()
        .take(LISTED_COMMITS_CAP)
        .map(|c| format!("{} {}", c.id.short(), c.subject))
        .collect();
    if commits.len() > LISTED_COMMITS_CAP {
        lines.push(format!(
            "... and {} more",
            commits.len() - LISTED_COMMITS_CAP
        ));
    }
    lines
}

/// The tip of the branch the checkout is on at the remote it publishes to, as
/// last fetched.
fn push_branch_tip(vcs: &dyn Vcs, dir: &Path) -> Option<ResolvedRevisionId> {
    let crate::vcs::HeadAttachment::Attached(attached) = vcs.head_attachment(dir).ok()? else {
        return None;
    };
    let target = vcs.push_target(&attached, None).ok()??;
    vcs.push_branch(&attached, &target).ok()?
}

/// The line for a member standing `standing` to `pin`, `None` where it is at
/// the pin. `head` is the commit the checkout is at.
pub(crate) fn judge(
    repo: &RepoPath,
    dir: &Path,
    vcs: &dyn Vcs,
    standing: &Standing,
    (head, pin): (&ResolvedRevisionId, &ResolvedRevisionId),
    reset: Option<&crate::feature_sync::ResetOffer>,
) -> Option<Judged> {
    judging(repo, dir, vcs, standing, head, pin, reset).map(|judging| judging.judge(vcs))
}

/// [`judge`] for a checkout of the workspace at `workspace`, which a landing
/// judges from its source: its choices run there.
pub(crate) fn judge_elsewhere(
    repo: &RepoPath,
    dir: &Path,
    vcs: &dyn Vcs,
    standing: &Standing,
    head: &ResolvedRevisionId,
    pin: &ResolvedRevisionId,
    workspace: &Path,
) -> Option<Judged> {
    judging(repo, dir, vcs, standing, head, pin, None).map(|judging| {
        judging
            .elsewhere(workspace)
            .only_forms(&[Form::C5Lock, Form::C3UpdateRemote])
            .by_verb("land")
            .judge(vcs)
    })
}

/// A branch `target` declares, checked out nowhere, beyond `target`'s pin by
/// the commits `beyond` lists: refused as `pin` unpinned, recorded by a lock
/// in a workspace on `target`, which the printed `workweave create` opens.
#[allow(clippy::too_many_arguments)]
pub(crate) fn judge_unpinned_branch(
    repo: &RepoPath,
    dir: &Path,
    vcs: &dyn Vcs,
    branch: &str,
    beyond: &Beyond,
    tip: &ResolvedRevisionId,
    pin: &ResolvedRevisionId,
    target: &str,
) -> Judged {
    State::PinUnpinned
        .of(repo)
        .member()
        .checkout(dir)
        .bind("<m>", repo.as_str())
        .bind("<repo>", repo.as_str())
        .bind("<W>", target)
        .bind("<ww>", target.replace('/', "-"))
        .subject(format!(
            "'{branch}', checked out nowhere and moved by this landing, is at {}, {} beyond \
             '{target}''s pin {}",
            tip.id().short(),
            commits_text(beyond.count),
            pin.id().short()
        ))
        .listing(listing(&beyond.unpinned))
        .only_forms(&[Form::OpenWorkweave, Form::C5Lock])
        .by_verb("land")
        .judge(vcs)
}

fn judging(
    repo: &RepoPath,
    dir: &Path,
    vcs: &dyn Vcs,
    standing: &Standing,
    head: &ResolvedRevisionId,
    pin: &ResolvedRevisionId,
    reset: Option<&crate::feature_sync::ResetOffer>,
) -> Option<crate::diagnosis::Judging> {
    let judging = |state: State| {
        state
            .of(repo)
            .member()
            .checkout(dir)
            .bind("<m>", repo.as_str())
            .bind("<repo>", repo.as_str())
    };
    let push_branch = push_branch_tip(vcs, dir);
    let heads = format!(
        "HEAD {} and the pin {}",
        head.id().short(),
        pin.id().short()
    );
    let facts = |beyond: &Beyond| -> Vec<Fact> {
        beyond
            .held_by
            .iter()
            .map(|lock| Fact {
                kind: FactKind::PinnedBy,
                about: Some(lock.clone()),
            })
            .collect()
    };
    let judged = match standing {
        Standing::At => return None,
        Standing::Behind { by } => judging(State::PinBehind).subject(format!(
            "HEAD {} is {} behind the pin {}",
            head.id().short(),
            commits_text(*by),
            pin.id().short()
        )),
        Standing::Diverged => {
            let mut judging = judging(State::PinDiverged).subject(format!(
                "neither HEAD nor the pin is an ancestor of the other: {heads}"
            ));
            let mut only = vec![ChoiceId::C25];
            judging = judging.bind("<rev>", head.id().as_str());
            if push_branch.is_some() {
                only.push(ChoiceId::C3);
            }
            if let Some(reset) = reset.filter(|r| r.landed(vcs, dir, pin.id())) {
                judging = judging.fact(Fact {
                    kind: FactKind::PinLanded,
                    about: None,
                });
                if let Some(fact) = reset.patch_fact(vcs, dir, head.id()) {
                    judging = judging.fact(fact);
                }
                if reset.runs {
                    only.push(ChoiceId::C22);
                }
            }
            judging.only(&only)
        }
        Standing::Ahead(beyond) => {
            let mut judging = judging(State::PinAhead).subject(format!(
                "HEAD {} is {} beyond the pin {}",
                head.id().short(),
                commits_text(beyond.count),
                pin.id().short()
            ));
            for fact in facts(beyond) {
                judging = judging.fact(fact);
            }
            if beyond.any_unpushed {
                judging
            } else {
                judging.only(&[ChoiceId::C5])
            }
        }
        Standing::Unpinned(beyond) => {
            let mut judging = judging(State::PinUnpinned)
                .subject(format!(
                    "HEAD {} is {} beyond the pin {}",
                    head.id().short(),
                    commits_text(beyond.count),
                    pin.id().short()
                ))
                .listing(listing(&beyond.unpinned));
            for fact in facts(beyond) {
                judging = judging.fact(fact);
            }
            judging = judging.bind("<rev>", head.id().as_str());
            if push_branch.as_ref() == Some(pin) {
                judging
            } else {
                judging.only(&[ChoiceId::C5, ChoiceId::C25])
            }
        }
    };
    Some(judged)
}

/// How the checked-out branch's lock holds a pin that sits off the remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Holding {
    /// A feature's lock: the pin is this feature's to publish.
    Feature,
    /// A declared branch's lock: the pin is this branch's to move.
    Declared,
    /// No lock this workspace has checked out holds it.
    NotHeld,
}

/// Whether a remote-tracking branch of any remote of the repo reaches
/// `commit`, as last fetched.
fn reached_by_any_remote(
    vcs: &dyn Vcs,
    dir: &Path,
    commit: &ResolvedRevisionId,
) -> anyhow::Result<bool> {
    for recorded in vcs.recorded_urls(dir)? {
        if vcs.reached_by_remote(dir, commit, &recorded.remote)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The live pins of `repo` that no remote-tracking branch of its clone
/// reaches, one line each, listing the locks that hold the pin. `holding`
/// says how the checked-out branch's lock holds a pin, given its commit.
pub(crate) fn off_remote(
    vcs: &dyn Vcs,
    dir: &Path,
    repo: &RepoPath,
    live: &[LivePin],
    holding: impl Fn(&CommitId) -> Holding,
    reset: impl Fn() -> Option<crate::feature_sync::ResetOffer>,
) -> anyhow::Result<Vec<Judged>> {
    if vcs.recorded_urls(dir)?.is_empty() {
        return Ok(Vec::new());
    }
    let mut by_commit: BTreeMap<&CommitId, Vec<&LivePin>> = BTreeMap::new();
    for pin in live {
        by_commit.entry(&pin.commit).or_default().push(pin);
    }
    let mut lines = Vec::new();
    for (commit, holders) in by_commit {
        let Ok(resolved) = vcs.resolve_revision(dir, commit.as_str()) else {
            continue;
        };
        if reached_by_any_remote(vcs, dir, &resolved)? {
            continue;
        }
        let mut judging = State::PinOffRemote
            .of(repo)
            .member()
            .checkout(dir)
            .bind("<m>", repo.as_str())
            .subject(commit.short())
            .listing(holders.iter().map(|p| p.held_by.clone()));
        let only: &[ChoiceId] = match holding(commit) {
            Holding::Feature => {
                let squashed = reset().and_then(|r| {
                    let fact = r.patch_fact(vcs, dir, commit)?;
                    Some((r, fact))
                });
                match squashed {
                    Some((reset, fact)) => {
                        judging = judging.fact(fact);
                        if reset.runs {
                            &[ChoiceId::C22]
                        } else {
                            &[ChoiceId::C7, ChoiceId::C27]
                        }
                    }
                    None => &[ChoiceId::C7, ChoiceId::C27],
                }
            }
            Holding::Declared => &[ChoiceId::C25],
            Holding::NotHeld => &[ChoiceId::C19],
        };
        lines.push(judging.only(only).judge(vcs));
    }
    Ok(lines)
}

/// One change to one ref of a member's clone, as the pin rule judges it.
pub(crate) struct RefChange<'a> {
    pub(crate) branch: &'a BranchRef,
    /// The tip the ref held.
    pub(crate) was: &'a ResolvedRevisionId,
    /// The tip it is given, `None` when it is deleted.
    pub(crate) now: Option<&'a ResolvedRevisionId>,
    /// A backup holds `was` once the change is made.
    pub(crate) was_backed_up: bool,
}

/// A live pin a ref change would leave reached as the pin rule forbids.
pub(crate) struct Stranded {
    pub(crate) commit: CommitId,
    pub(crate) held_by: Vec<String>,
}

impl Stranded {
    pub(crate) fn line(&self, change: &RefChange<'_>) -> String {
        format!(
            "{} is pinned by {}, and only {} reaches it",
            self.commit.short(),
            self.held_by.join(", "),
            change.branch
        )
    }
}

/// The pin rule for one ref change in the clone at `dir`: each live pin `was`
/// reaches that, after the change, no branch, remote-tracking branch or
/// backup reaches, or, when a remote-tracking lock holds it and the change is
/// to a remote-tracking branch, no remote-tracking branch reaches. Pins held
/// only by locks `exempt` names do not count.
pub(crate) fn stranded_by(
    vcs: &dyn Vcs,
    dir: &Path,
    live: &[LivePin],
    change: &RefChange<'_>,
    exempt: impl Fn(&LivePin) -> bool,
) -> anyhow::Result<Vec<Stranded>> {
    stranded_by_all(vcs, dir, live, std::slice::from_ref(change), exempt)
}

/// [`stranded_by`] for `changes` made together to the clone at `dir`.
pub(crate) fn stranded_by_all(
    vcs: &dyn Vcs,
    dir: &Path,
    live: &[LivePin],
    changes: &[RefChange<'_>],
    exempt: impl Fn(&LivePin) -> bool,
) -> anyhow::Result<Vec<Stranded>> {
    let mut remaining: Vec<(bool, ResolvedRevisionId)> = vcs
        .branch_tips(dir)?
        .into_iter()
        .filter(|tip| !changes.iter().any(|change| &tip.branch == change.branch))
        .map(|tip| (tip.branch.is_remote(), tip.tip))
        .collect();
    let mut backups: Vec<ResolvedRevisionId> = vcs
        .list_backups(dir, crate::vcs::RemoteTracking::Counted)?
        .into_iter()
        .map(|listed| listed.backup.tip)
        .collect();
    for change in changes {
        if let Some(now) = change.now {
            remaining.push((change.branch.is_remote(), now.clone()));
        }
        if change.was_backed_up {
            backups.push(change.was.clone());
        }
    }
    let was: Vec<ResolvedRevisionId> = changes.iter().map(|change| change.was.clone()).collect();
    let left = Left {
        refs: remaining,
        backups,
        remote_change: changes.iter().any(|change| change.branch.is_remote()),
    };
    stranded(vcs, dir, live, &was, &left, exempt)
}

/// The pin rule for dropping the backups `dropping` from the clone at `dir`:
/// each live pin one of them reaches that no branch, remote-tracking branch
/// or other backup does.
pub(crate) fn stranded_by_dropping(
    vcs: &dyn Vcs,
    dir: &Path,
    live: &[LivePin],
    dropping: &[&crate::vcs::Backup],
) -> anyhow::Result<Vec<Stranded>> {
    let refs: Vec<(bool, ResolvedRevisionId)> = vcs
        .branch_tips(dir)?
        .into_iter()
        .map(|tip| (tip.branch.is_remote(), tip.tip))
        .collect();
    let backups: Vec<ResolvedRevisionId> = vcs
        .list_backups(dir, crate::vcs::RemoteTracking::Counted)?
        .into_iter()
        .filter(|listed| !dropping.iter().any(|d| d.name == listed.backup.name))
        .map(|listed| listed.backup.tip)
        .collect();
    let was: Vec<ResolvedRevisionId> = dropping.iter().map(|d| d.tip.clone()).collect();
    let left = Left {
        refs,
        backups,
        remote_change: false,
    };
    stranded(vcs, dir, live, &was, &left, |_| false)
}

/// What still reaches commits once a change is made.
struct Left {
    /// Each branch's tip, and whether it is remote-tracking.
    refs: Vec<(bool, ResolvedRevisionId)>,
    backups: Vec<ResolvedRevisionId>,
    /// The change is to a remote-tracking branch.
    remote_change: bool,
}

fn stranded(
    vcs: &dyn Vcs,
    dir: &Path,
    live: &[LivePin],
    was: &[ResolvedRevisionId],
    left: &Left,
    exempt: impl Fn(&LivePin) -> bool,
) -> anyhow::Result<Vec<Stranded>> {
    let mut by_commit: BTreeMap<&CommitId, Vec<&LivePin>> = BTreeMap::new();
    for pin in live.iter().filter(|pin| !exempt(pin)) {
        by_commit.entry(&pin.commit).or_default().push(pin);
    }
    let mut stranded = Vec::new();
    for (commit, holders) in by_commit {
        let pinned = match vcs.resolve_revision(dir, commit.as_str()) {
            Ok(pinned) => pinned,
            Err(VcsError::RevisionNotFound { .. }) => continue,
            Err(e) => return Err(e.into()),
        };
        let reaches = |tip: &ResolvedRevisionId| vcs.is_ancestor(dir, &pinned, tip);
        let mut reached_by_change = false;
        for tip in was {
            if reaches(tip)? {
                reached_by_change = true;
                break;
            }
        }
        if !reached_by_change {
            continue;
        }
        let mut by_any = false;
        let mut by_remote = false;
        for (remote, tip) in &left.refs {
            if reaches(tip)? {
                by_any = true;
                by_remote |= *remote;
            }
        }
        if !by_any {
            for tip in &left.backups {
                if reaches(tip)? {
                    by_any = true;
                    break;
                }
            }
        }
        let remote_clause = left.remote_change && holders.iter().any(|pin| pin.branch.is_remote());
        if !by_any || (remote_clause && !by_remote) {
            let mut held_by: Vec<String> = holders.iter().map(|pin| pin.held_by.clone()).collect();
            held_by.sort();
            held_by.dedup();
            stranded.push(Stranded {
                commit: commit.clone(),
                held_by,
            });
        }
    }
    Ok(stranded)
}
