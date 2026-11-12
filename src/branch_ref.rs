//! A branch of one repo, by where it lives: this clone's own branches, or
//! its copies of a remote's.

use std::fmt;

use crate::vcs::{RawRefName, RemoteHandle};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BranchRef {
    Local(RawRefName),
    Remote {
        remote: RemoteHandle,
        name: RawRefName,
    },
}

impl BranchRef {
    pub fn name(&self) -> &RawRefName {
        match self {
            Self::Local(name) | Self::Remote { name, .. } => name,
        }
    }

    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }

    pub fn remote(&self) -> Option<&RemoteHandle> {
        match self {
            Self::Local(_) => None,
            Self::Remote { remote, .. } => Some(remote),
        }
    }
}

impl fmt::Display for BranchRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(name) => write!(f, "{name}"),
            Self::Remote { remote, name } => write!(f, "{remote}/{name}"),
        }
    }
}
