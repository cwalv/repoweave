//! The files rwv writes as its own record of a weave, where each one lives,
//! and the one way to touch them.
//!
//! What separates these from the content rwv generates for an ecosystem tool
//! is what a torn one costs. A half-written `Cargo.toml` is regenerable from
//! the weave and the managed-file axis reports it. A half-written record here
//! is what that axis reads in order to decide, so tearing one takes the
//! detector down with it instead of being caught by it.
//!
//! A [`StatePath`] is the location of one of them. It converts to no path type
//! and hands none out, so the only way to write the file is one of its own
//! methods, and each of those publishes durably.

use crate::durable_file::{ClaimError, ClaimFile, CreateNewError};
use crate::path_spelling::WirePath;
use std::path::{Path, PathBuf};

/// A file rwv writes as its own record, and how it is written.
///
/// What a file may be asked for is the protocol of the module that owns it,
/// stated per variant. A replacement overwrites, so it is the right publish
/// only for a file no peer is racing to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StateFile {
    /// Replaced, by a writer holding [`StateFile::OwnedDigestsClaim`].
    OwnedDigests,
    /// Replaced.
    ActiveProject,
    /// Replaced.
    WorkweaveMarker,
    /// Replaced.
    ProjectLock,
    /// Replaced, by a writer holding [`StateFile::IndexClaim`].
    WorkweaveIndex,
    /// Replaced.
    HealthFloor,
    /// Created exclusively by the op that acquires its workspace, then
    /// replaced by that op as it advances. The create's refusal on an
    /// occupied path is the mutual exclusion between ops.
    OpRecord,
    /// Created exclusively, and never replaced.
    OpLease,
    /// Claimed, and removed on release.
    OwnedDigestsClaim,
    /// Claimed, and removed on release.
    IndexClaim,
}

/// Files in rwv's namespace that rwv reads and rewrites but does not own: an
/// operator authors them and may hold the only copy of what they mean. They
/// are not state rwv attests, so they have no [`StateFile`].
pub const OPERATOR_AUTHORED: [&str; 2] = [
    crate::manifest::Manifest::FILE_NAME,
    crate::manifest::Manifest::LEGACY_FILE_NAME,
];

impl StateFile {
    pub const ALL: [StateFile; 10] = [
        StateFile::OwnedDigests,
        StateFile::ActiveProject,
        StateFile::WorkweaveMarker,
        StateFile::ProjectLock,
        StateFile::WorkweaveIndex,
        StateFile::HealthFloor,
        StateFile::OpRecord,
        StateFile::OpLease,
        StateFile::OwnedDigestsClaim,
        StateFile::IndexClaim,
    ];

    pub fn file_name(self) -> &'static str {
        match self {
            StateFile::OwnedDigests => crate::owned_state::OWNED_DIGESTS_FILE,
            StateFile::ActiveProject => crate::workspace::ACTIVE_PROJECT_FILE,
            StateFile::WorkweaveMarker => crate::workspace::WORKWEAVE_MARKER_FILE,
            StateFile::ProjectLock => crate::manifest::LockFile::FILE_NAME,
            StateFile::WorkweaveIndex => crate::workweave_index::INDEX_FILENAME,
            StateFile::HealthFloor => crate::health_floor::FLOOR_FILE,
            StateFile::OpRecord => crate::op_state::OP_STATE_FILE,
            StateFile::OpLease => crate::op_state::OP_LEASE_FILE,
            StateFile::OwnedDigestsClaim => crate::owned_state::OWNED_DIGESTS_CLAIM_FILE,
            StateFile::IndexClaim => crate::workweave_index::INDEX_CLAIM_FILE,
        }
    }

    /// This file as it sits directly inside `dir`.
    pub fn in_dir(self, dir: &Path) -> StatePath {
        StatePath {
            dir: dir.to_path_buf(),
            file: self,
        }
    }
}

/// Where one of rwv's state files lives, and the operations on it that cannot
/// tear it.
///
/// It is not `AsRef<Path>`, does not deref to a path and has no accessor for
/// one, so `std::fs::write` and every other path-taking write refuse it at
/// compile time. What it offers is the durable writes, the reads, and
/// the spellings a message or a `--json` field takes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StatePath {
    dir: PathBuf,
    file: StateFile,
}

impl StatePath {
    pub fn file(&self) -> StateFile {
        self.file
    }

    /// The directory holding the file.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn located(&self) -> PathBuf {
        self.dir.join(self.file.file_name())
    }

    /// Publish `bytes` as the whole file, atomically and durably, replacing
    /// whatever is there.
    pub fn publish(&self, bytes: &[u8]) -> anyhow::Result<()> {
        crate::durable_file::replace(&self.located(), bytes)
    }

    /// Publish `bytes` only if nothing is there.
    pub(crate) fn create(&self, bytes: &[u8]) -> Result<(), CreateNewError> {
        crate::durable_file::create_new(&self.located(), bytes)
    }

    pub(crate) fn claim(&self) -> Result<ClaimFile, ClaimError> {
        crate::durable_file::claim(&self.located())
    }

    pub fn read(&self) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.located())
    }

    pub fn read_to_string(&self) -> std::io::Result<String> {
        std::fs::read_to_string(self.located())
    }

    pub fn exists(&self) -> bool {
        self.located().exists()
    }

    pub(crate) fn symlink_metadata(&self) -> std::io::Result<std::fs::Metadata> {
        self.located().symlink_metadata()
    }

    pub fn remove(&self) -> std::io::Result<()> {
        std::fs::remove_file(self.located())
    }

    pub fn wire(&self) -> WirePath {
        WirePath::of(&self.located())
    }

    /// The location as a value a finding carries to be printed or sent: a
    /// path every writer refuses by construction here is writable once it is
    /// a `PathBuf`, so only a report built to be read takes one.
    pub(crate) fn reported(&self) -> PathBuf {
        self.located()
    }
}

impl std::fmt::Display for StatePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let located = self.located();
        write!(f, "{}", located.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn names_are_distinct() {
        let names: BTreeSet<&str> = StateFile::ALL.iter().map(|f| f.file_name()).collect();
        assert_eq!(names.len(), StateFile::ALL.len());
    }

    #[test]
    fn publish_replaces_and_leaves_no_temp() {
        let tmp = tempfile::tempdir().unwrap();
        let active = StateFile::ActiveProject.in_dir(tmp.path());
        active.publish(b"first\n").unwrap();
        active.publish(b"second\n").unwrap();
        assert_eq!(active.read().unwrap(), b"second\n");
        let leftovers: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert_eq!(leftovers, Vec::<String>::new());
    }

    #[test]
    fn create_refuses_an_occupied_path_and_leaves_it() {
        let tmp = tempfile::tempdir().unwrap();
        let record = StateFile::OpRecord.in_dir(tmp.path());
        record.create(b"first").unwrap();
        assert!(matches!(
            record.create(b"second"),
            Err(CreateNewError::AlreadyExists)
        ));
        assert_eq!(record.read_to_string().unwrap(), "first");
    }

    #[test]
    fn a_state_path_names_its_file_inside_its_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = StateFile::ProjectLock.in_dir(tmp.path());
        assert_eq!(lock.file(), StateFile::ProjectLock);
        assert_eq!(lock.dir(), tmp.path());
        assert!(!lock.exists());
        lock.publish(b"{}\n").unwrap();
        assert!(lock.exists());
        assert!(tmp.path().join(StateFile::ProjectLock.file_name()).exists());
        lock.remove().unwrap();
        assert!(!lock.exists());
    }

    #[test]
    fn display_and_wire_spell_the_same_location() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = StateFile::WorkweaveMarker.in_dir(tmp.path());
        let located = tmp.path().join(StateFile::WorkweaveMarker.file_name());
        assert_eq!(marker.to_string(), located.display().to_string());
        assert_eq!(marker.wire(), WirePath::of(&located));
    }
}
