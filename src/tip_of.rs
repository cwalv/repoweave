//! A branch's tip as the newer of the branch and its upstream.

use crate::vcs::{RawRefName, Relation, RemoteHandle, ResolvedRevisionId, Vcs, VcsError};
use std::path::Path;

/// A branch read locally and on its upstream, as last fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TipOf {
    pub(crate) branch: RawRefName,
    pub(crate) local: Option<ResolvedRevisionId>,
    pub(crate) upstream: Option<(RemoteHandle, ResolvedRevisionId)>,
    /// The local tip against the upstream's, where both exist.
    pub(crate) relation: Option<Relation>,
}

/// The branch and its upstream each hold commits the other lacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Diverged {
    pub(crate) local: ResolvedRevisionId,
    pub(crate) remote: RemoteHandle,
    pub(crate) upstream: ResolvedRevisionId,
}

impl TipOf {
    /// The newer of the two; `None` where the branch is nowhere.
    pub(crate) fn newer(&self) -> Result<Option<&ResolvedRevisionId>, Diverged> {
        match (&self.local, &self.upstream, self.relation) {
            (Some(local), Some((remote, upstream)), Some(Relation::Diverged)) => Err(Diverged {
                local: local.clone(),
                remote: remote.clone(),
                upstream: upstream.clone(),
            }),
            (_, Some((_, upstream)), Some(Relation::Behind)) => Ok(Some(upstream)),
            (Some(local), _, _) => Ok(Some(local)),
            (None, Some((_, upstream)), _) => Ok(Some(upstream)),
            (None, None, _) => Ok(None),
        }
    }

    /// Whether the branch exists nowhere this clone knows of.
    pub(crate) fn is_nowhere(&self) -> bool {
        self.local.is_none() && self.upstream.is_none()
    }
}

/// `branch` in `repo`, with its upstream; a branch with none, or with no
/// local branch, is read on `home` where that remote has a copy of the name.
pub(crate) fn tip_of(
    vcs: &dyn Vcs,
    repo: &Path,
    branch: &RawRefName,
    home: Option<&RemoteHandle>,
) -> Result<TipOf, VcsError> {
    let local = vcs.resolve_local_branch_tip(repo, branch)?;
    let configured = match &local {
        Some(_) => vcs.upstream_of(repo, branch)?,
        None => None,
    };
    let (remote, name) = match (configured, home) {
        (Some(upstream), _) => (Some(upstream.remote), upstream.branch),
        (None, Some(home)) if local.is_none() => (Some(home.clone()), branch.clone()),
        _ => (None, branch.clone()),
    };
    let upstream = match remote {
        Some(remote) => vcs
            .remote_copies(repo, &name)?
            .into_iter()
            .find(|copy| copy.remote == remote)
            .map(|copy| (copy.remote, copy.tip)),
        None => None,
    };
    let relation = match (&local, &upstream) {
        (Some(local), Some((_, upstream))) => Some(vcs.relation(repo, local, upstream)?),
        _ => None,
    };
    Ok(TipOf {
        branch: branch.clone(),
        local,
        upstream,
        relation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::CommitId;

    fn rev(n: u8) -> ResolvedRevisionId {
        ResolvedRevisionId::from(CommitId::parse(&format!("{n:02x}").repeat(20)).unwrap())
    }

    fn tips(local: Option<u8>, upstream: Option<u8>, relation: Option<Relation>) -> TipOf {
        TipOf {
            branch: RawRefName::new("main"),
            local: local.map(rev),
            upstream: upstream.map(|n| (RemoteHandle::new("origin"), rev(n))),
            relation,
        }
    }

    #[test]
    fn the_upstream_is_the_tip_where_the_branch_is_behind_it() {
        assert_eq!(
            tips(Some(1), Some(2), Some(Relation::Behind)).newer(),
            Ok(Some(&rev(2)))
        );
    }

    #[test]
    fn the_local_branch_is_the_tip_where_it_is_ahead_or_even() {
        for relation in [Relation::Ahead, Relation::Even] {
            assert_eq!(
                tips(Some(1), Some(2), Some(relation)).newer(),
                Ok(Some(&rev(1)))
            );
        }
    }

    #[test]
    fn a_diverged_branch_has_no_tip() {
        assert!(tips(Some(1), Some(2), Some(Relation::Diverged))
            .newer()
            .is_err());
    }

    #[test]
    fn a_branch_only_on_a_remote_is_read_there() {
        assert_eq!(tips(None, Some(2), None).newer(), Ok(Some(&rev(2))));
        assert_eq!(tips(None, None, None).newer(), Ok(None));
    }
}
