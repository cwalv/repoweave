//! Which members of a manifest a verb acts on.

use crate::manifest::{RepoPath, Role};
use crate::selector::RepoFilter;

/// The members `--role` and `--repo` name, less the `reference`-role ones
/// under `--no-reference`. Decided once, where the flags are parsed; every
/// step of the verb asks it.
#[derive(Debug, Clone)]
pub struct MemberSelection {
    filter: RepoFilter,
    skip_reference: bool,
}

impl MemberSelection {
    pub fn new(filter: RepoFilter, no_reference: bool) -> Self {
        Self {
            filter,
            skip_reference: no_reference,
        }
    }

    /// Whether `--role` / `--repo` name this member, or name none at all.
    pub fn names(&self, repo_path: &RepoPath, role: Role) -> bool {
        self.filter.matches(repo_path, role)
    }

    /// Whether `--no-reference` opts this member out of the verb.
    pub fn leaves_out(&self, role: Role) -> bool {
        self.skip_reference && role == Role::Reference
    }

    /// Whether the verb acts on this member.
    pub fn covers(&self, repo_path: &RepoPath, role: Role) -> bool {
        self.names(repo_path, role) && !self.leaves_out(role)
    }

    /// Whether no `--role` or `--repo` narrows the verb.
    pub fn names_every_member(&self) -> bool {
        self.filter.is_empty()
    }
}
