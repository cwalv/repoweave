//! Where a member's checked-out branch stands to the branch it publishes to:
//! the `push-branch` axis, read from the seam's roles.

use crate::diagnosis::{Judged, State};
use crate::vcs::{
    short_revision, AttachedRef, CommitSummary, RawRefName, Relation, RemoteHandle, TrackingRef,
    Vcs, VcsError,
};
use std::fmt;
use std::path::Path;

/// How a branch stands to its push branch, as last fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Standing {
    /// Nowhere to publish: no push remote is configured and the repo has no
    /// url remote.
    NoPushTarget,
    /// The branch takes in from a branch of another name.
    ForeignUpstream {
        upstream: String,
    },
    /// The push target has no branch of this name, and none is recorded as
    /// the branch's upstream.
    Unpublished,
    /// The branch's upstream is gone from its remote.
    Gone {
        unpushed: Vec<CommitSummary>,
    },
    Even,
    Ahead,
    Behind,
    Diverged {
        unpushed: Vec<CommitSummary>,
    },
}

/// `attached`'s push target, in the VCS's order, ending with `url_remote`, and
/// where the branch stands to the same-named branch there.
pub(crate) fn standing(
    vcs: &dyn Vcs,
    attached: &AttachedRef,
    url_remote: Option<&RemoteHandle>,
) -> Result<(Standing, Option<RemoteHandle>), VcsError> {
    standing_of_branch(
        vcs,
        attached.repo(),
        &RawRefName::new(attached.to_string()),
        url_remote,
    )
}

/// [`standing`] for the local branch `branch` of `repo`, checked out or not.
pub(crate) fn standing_of_branch(
    vcs: &dyn Vcs,
    repo: &Path,
    branch: &RawRefName,
    url_remote: Option<&RemoteHandle>,
) -> Result<(Standing, Option<RemoteHandle>), VcsError> {
    let Some(target) = vcs.push_target_of(repo, branch, url_remote)? else {
        return Ok((Standing::NoPushTarget, None));
    };
    let upstream = vcs.upstream_of(repo, branch)?;
    if let Some(upstream) = &upstream {
        if upstream.branch != *branch {
            return Ok((
                Standing::ForeignUpstream {
                    upstream: format!("{}/{}", upstream.remote, upstream.branch),
                },
                Some(target),
            ));
        }
    }
    let unpushed = || vcs.unpushed_commits(repo, branch);
    let copies = vcs.remote_copies(repo, branch)?;
    let standing = match copies.iter().find(|copy| copy.remote == target) {
        None if upstream
            .as_ref()
            .is_some_and(|up| !copies.iter().any(|copy| copy.remote == up.remote)) =>
        {
            Standing::Gone {
                unpushed: unpushed()?,
            }
        }
        None => Standing::Unpublished,
        Some(pushed) => {
            let tip = vcs.resolve_local_branch_tip(repo, branch)?.ok_or_else(|| {
                VcsError::RevisionNotFound {
                    repo: repo.to_path_buf(),
                    rev: branch.as_str().to_owned(),
                }
            })?;
            match vcs.relation(repo, &tip, &pushed.tip)? {
                Relation::Even => Standing::Even,
                Relation::Ahead => Standing::Ahead,
                Relation::Behind => Standing::Behind,
                Relation::Diverged => Standing::Diverged {
                    unpushed: unpushed()?,
                },
            }
        }
    };
    Ok((standing, Some(target)))
}

impl Standing {
    /// The state this standing is judged as, where it is one worth a line:
    /// not the even branch, not one the push target has never had, and not a
    /// gone upstream whose branch holds nothing the remote lacks.
    pub(crate) fn state(&self) -> Option<State> {
        match self {
            Standing::NoPushTarget => Some(State::PushBranchNoPushTarget),
            Standing::ForeignUpstream { .. } => Some(State::PushBranchForeignUpstream),
            Standing::Gone { unpushed } if !unpushed.is_empty() => Some(State::PushBranchGone),
            Standing::Gone { .. } => None,
            Standing::Ahead => Some(State::PushBranchAhead),
            Standing::Behind => Some(State::PushBranchBehind),
            Standing::Diverged { .. } => Some(State::PushBranchDiverged),
            Standing::Even | Standing::Unpublished => None,
        }
    }

    /// The commits only the branch holds, for the standings that list them.
    fn listed(&self) -> &[CommitSummary] {
        match self {
            Standing::Gone { unpushed } | Standing::Diverged { unpushed } => unpushed,
            _ => &[],
        }
    }

    /// Whether publishing the branch would lose nothing the remote has: it
    /// holds no commit of its own that the push branch lacks and others lack.
    pub(crate) fn diverged_with_unpushed(&self) -> bool {
        matches!(self, Standing::Diverged { unpushed } if !unpushed.is_empty())
    }
}

/// What a member's judged line is about: the repo and the branch checked out.
pub(crate) struct Subject<'a> {
    pub(crate) repo: &'a dyn fmt::Display,
    pub(crate) checkout: &'a Path,
    pub(crate) branch: &'a str,
    /// The branch the manifest declares, where the commits of a gone branch
    /// would be copied.
    pub(crate) declared: Option<&'a TrackingRef>,
    pub(crate) member: bool,
}

/// `standing` judged for `subject`, when it is worth a line.
pub(crate) fn judge(vcs: &dyn Vcs, subject: &Subject<'_>, standing: &Standing) -> Option<Judged> {
    let state = standing.state()?;
    let mut judging = state.of(subject.repo).checkout(subject.checkout);
    judging = if subject.member {
        judging.member().bind("<m>", subject.repo)
    } else {
        judging.project_repo()
    };
    judging = match standing {
        Standing::ForeignUpstream { upstream } => {
            judging.subject(format!("{} takes in from {upstream}", subject.branch))
        }
        Standing::NoPushTarget => judging.subject(subject.branch),
        _ => judging,
    };
    if let Some(declared) = subject.declared {
        judging = judging.bind("<b>", declared);
    }
    judging = judging.listing(
        standing
            .listed()
            .iter()
            .map(|c| format!("{} {}", short_revision(c.id.as_str()), c.subject)),
    );
    Some(judging.judge(vcs))
}
