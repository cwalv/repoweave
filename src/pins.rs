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

use crate::diagnosis::{ChoiceId, Fact, FactKind, Judged, State};
use crate::manifest::{LockFile, ProjectName, RepoPath};
use crate::vcs::{
    project_vcs, CommitId, CommitSummary, Relation, ResolvedRevisionId, Vcs, VcsError,
};
use crate::workspace::{discover_projects, project_dir};

const LISTED_COMMITS_CAP: usize = 20;

/// A pin some project's live lock holds, and the lock that holds it.
pub(crate) struct LivePin {
    pub(crate) commit: CommitId,
    pub(crate) project: ProjectName,
    /// The project branch whose lock holds it.
    pub(crate) branch: String,
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
                        branch: branch.label.clone(),
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

/// The line for a member standing `standing` to `pin`, `None` where it is at
/// the pin. `head` is the commit the checkout is at.
pub(crate) fn judge(
    repo: &RepoPath,
    dir: &Path,
    vcs: &dyn Vcs,
    standing: &Standing,
    head: &ResolvedRevisionId,
    pin: &ResolvedRevisionId,
) -> Option<Judged> {
    let judging = |state: State| {
        state
            .of(repo)
            .member()
            .checkout(dir)
            .bind("<m>", repo.as_str())
    };
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
        Standing::Diverged => judging(State::PinDiverged).subject(format!(
            "neither HEAD nor the pin is an ancestor of the other: {heads}"
        )),
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
            judging
        }
    };
    Some(judged.judge(vcs))
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
        let only: &[ChoiceId] = match holding(commit) {
            Holding::Feature => &[ChoiceId::C7, ChoiceId::C27],
            Holding::Declared => &[ChoiceId::C25],
            Holding::NotHeld => &[ChoiceId::C19],
        };
        lines.push(
            State::PinOffRemote
                .of(repo)
                .member()
                .checkout(dir)
                .bind("<m>", repo.as_str())
                .subject(commit.short())
                .listing(holders.iter().map(|p| p.held_by.clone()))
                .only(only)
                .judge(vcs),
        );
    }
    Ok(lines)
}
