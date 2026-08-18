//! The files rwv writes as its own record of a weave, and the one way to
//! publish them.
//!
//! What separates these from the content rwv generates for an ecosystem tool
//! is what a torn one costs. A half-written `Cargo.toml` is regenerable from
//! the weave and the managed-file axis reports it. A half-written record here
//! is what that axis reads in order to decide, so tearing one takes the
//! detector down with it instead of being caught by it.

use std::path::Path;

/// A file rwv writes as its own record and later parses back, published by
/// replacement.
///
/// The op-state pair and the two `.lock` claims are absent by construction
/// rather than by oversight: those are published with
/// `durable_file::create_new`, whose refusal on an occupied path is the mutual
/// exclusion `acquire_op`, the owned-digest ledger and the workweave index are
/// each built from. Replacing one would overwrite a peer's claim, which is the
/// opposite of what it is for. [`EXCLUSIVE_CREATE`] holds them so a census over
/// rwv's state files can account for every one without granting them a publish
/// that would be wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateFile {
    OwnedDigests,
    ActiveProject,
    WorkweaveMarker,
    ProjectLock,
    WorkweaveIndex,
    HealthFloor,
}

/// State files whose publish is an exclusive create, not a replacement.
pub const EXCLUSIVE_CREATE: [&str; 4] = [
    crate::op_state::OP_STATE_FILE,
    crate::op_state::OP_LEASE_FILE,
    crate::owned_state::OWNED_DIGESTS_CLAIM_FILE,
    crate::workweave_index::INDEX_CLAIM_FILE,
];

/// Files in rwv's namespace that rwv reads and rewrites but does not own: an
/// operator authors them and may hold the only copy of what they mean. They
/// are not state rwv attests, so they are not published through [`StateFile`].
pub const OPERATOR_AUTHORED: [&str; 2] = [
    crate::manifest::Manifest::FILE_NAME,
    crate::manifest::Manifest::LEGACY_FILE_NAME,
];

impl StateFile {
    pub const ALL: [StateFile; 6] = [
        StateFile::OwnedDigests,
        StateFile::ActiveProject,
        StateFile::WorkweaveMarker,
        StateFile::ProjectLock,
        StateFile::WorkweaveIndex,
        StateFile::HealthFloor,
    ];

    pub fn file_name(self) -> &'static str {
        match self {
            StateFile::OwnedDigests => crate::owned_state::OWNED_DIGESTS_FILE,
            StateFile::ActiveProject => crate::workspace::ACTIVE_PROJECT_FILE,
            StateFile::WorkweaveMarker => crate::workspace::WORKWEAVE_MARKER_FILE,
            StateFile::ProjectLock => crate::manifest::LockFile::FILE_NAME,
            StateFile::WorkweaveIndex => crate::workweave_index::INDEX_FILENAME,
            StateFile::HealthFloor => crate::health_floor::FLOOR_FILE,
        }
    }

    /// Publish `bytes` as this file inside `dir`, atomically and durably.
    pub fn publish_in(self, dir: &Path, bytes: &[u8]) -> anyhow::Result<()> {
        crate::durable_file::replace(&dir.join(self.file_name()), bytes)
    }

    /// Publish `bytes` at `path`, for a caller that was handed the whole path
    /// rather than the directory holding it.
    ///
    /// The file name is checked rather than trusted: a caller passing a
    /// directory where a path was wanted, or the wrong state file, would
    /// otherwise publish to a path this type has vouched for without ever
    /// looking at it.
    pub fn publish_at(self, path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
        anyhow::ensure!(
            path.file_name().is_some_and(|n| n == self.file_name()),
            "refusing to publish {} as {}",
            path.display(),
            self.file_name()
        );
        crate::durable_file::replace(path, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn names_are_distinct_and_do_not_overlap_the_exclusive_set() {
        let names: BTreeSet<&str> = StateFile::ALL.iter().map(|f| f.file_name()).collect();
        assert_eq!(names.len(), StateFile::ALL.len());
        for name in EXCLUSIVE_CREATE {
            assert!(!names.contains(name), "{name} is published both ways");
        }
    }

    #[test]
    fn publish_in_replaces_and_leaves_no_temp() {
        let tmp = tempfile::tempdir().unwrap();
        StateFile::ActiveProject
            .publish_in(tmp.path(), b"first\n")
            .unwrap();
        StateFile::ActiveProject
            .publish_in(tmp.path(), b"second\n")
            .unwrap();
        let path = tmp.path().join(StateFile::ActiveProject.file_name());
        assert_eq!(std::fs::read(&path).unwrap(), b"second\n");
        let leftovers: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert_eq!(leftovers, Vec::<String>::new());
    }
}
