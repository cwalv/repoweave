//! Publishing a whole file so no reader ever sees it half-written, and so a
//! crash cannot lose what a caller was told had landed.
//!
//! Both entry points write the full content into a sibling temp, fsync it,
//! then publish it in one step and fsync the containing directory. They
//! differ only in what publishing means when something is already at the
//! target path: `create_new` refuses, `replace` overwrites.
//!
//! ## Why the fsyncs are here
//!
//! Atomic is not durable. A temp written with `write(2)` and published leaves
//! both the bytes and the directory entry in the page cache: a power loss can
//! lose either, and ext4's delayed-allocation flush on rename-over-an-existing
//! file is a heuristic, not a guarantee. The ownership receipts in the
//! workweave index are the sharp case — their whole contract is that the
//! receipt reaches disk *before* the ref it describes, and git fsyncs a loose
//! ref it writes, so a cached receipt leaves a ref with no receipt, the one
//! state R2 permanently disowns.
//!
//! ## Why the temp name carries a pid and a counter and no clock
//!
//! The pid keeps concurrent processes apart; a process-local monotonic
//! counter keeps threads within one process apart. Both components are
//! structural. An earlier revision used a nanosecond timestamp for the
//! intra-process half: two barrier-synchronized threads read the same
//! `SystemTime::now()` under load, collided on the temp path, and each
//! unlinked the other's in-flight temp, so the winner's publish hit ENOENT
//! and *both* calls failed. Uniqueness here must not be re-derived from a
//! clock.

use anyhow::Context;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Why a [`create_new`] publish did not happen.
#[derive(Debug)]
pub(crate) enum CreateNewError {
    /// Something was already at the target path. The caller owns what that
    /// means — for op-state it is a peer op holding the workspace.
    AlreadyExists,
    Io(std::io::Error),
}

/// Serial number for temp files, so two threads publishing to the same path
/// in one process cannot pick the same name and clobber each other's temp.
static TMP_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How long a caller waits for a peer's claim on a state file before it
/// refuses.
///
/// A claimed read-modify-write is a read, a small serialization and one
/// durable publish, so a wait this long means the holder is not running rather
/// than merely slow. Both claims in the tree share the number: two waits that
/// drift apart would make "how long does rwv hang before it complains" depend
/// on which file it was.
pub(crate) const CLAIM_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// How often a wait re-attempts the claim.
pub(crate) const CLAIM_POLL: std::time::Duration = std::time::Duration::from_millis(5);

/// Publish `bytes` at `path`, replacing whatever is there.
///
/// `rename(2)` is atomic within a filesystem, so a concurrent reader sees
/// either the old file or the new one, never a splice of the two.
pub(crate) fn replace(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = parent_of(path);
    let tmp = staged_temp(path, bytes)
        .with_context(|| format!("failed to stage a write of {}", path.display()))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow::Error::new(e).context(format!(
            "failed to rename {} to {}",
            tmp.display(),
            path.display()
        )));
    }
    sync_dir(parent).with_context(|| format!("failed to fsync directory {}", parent.display()))
}

/// Publish `bytes` at `path` only if nothing is there.
///
/// `link(2)` is atomic and fails with `EEXIST` on an occupied target —
/// `O_CREAT|O_EXCL` semantics, but applied to an already-populated inode so
/// the loser reads a complete file. With a plain `create_new` open the winner
/// creates the file and then writes it, and a loser that hits `EEXIST` and
/// immediately reads may see it empty: it then fails with a parse error
/// instead of whatever refusal the caller wanted to raise.
///
/// `std::fs::hard_link` works on POSIX and NTFS. A platform without hard
/// links would need a different atomic exclusive publish.
pub(crate) fn create_new(path: &Path, bytes: &[u8]) -> Result<(), CreateNewError> {
    let parent = parent_of(path);
    let tmp = staged_temp(path, bytes).map_err(CreateNewError::Io)?;
    let linked = std::fs::hard_link(&tmp, path);
    // The temp's role ends here whether or not the link took.
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => sync_dir(parent).map_err(CreateNewError::Io),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(CreateNewError::AlreadyExists)
        }
        Err(e) => Err(CreateNewError::Io(e)),
    }
}

fn parent_of(path: &Path) -> &Path {
    path.parent().unwrap_or_else(|| Path::new("."))
}

/// Write `bytes` into a sibling temp of `path` and fsync it, so a publish can
/// only ever expose bytes that have reached the disk. Returns the temp path;
/// the caller publishes it and owns removing it. On failure nothing is left
/// behind and `path` has not been touched.
fn staged_temp(path: &Path, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "durable-file".to_owned());
    let serial = TMP_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp_path = parent_of(path).join(format!(
        "{file_name}.tmp.{pid}.{serial}",
        pid = std::process::id()
    ));

    match (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        f.write_all(bytes)?;
        f.sync_all()
    })() {
        Ok(()) => Ok(tmp_path),
        Err(e) => {
            // Do not leave the temp behind for a later reader to trip over.
            let _ = std::fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

/// fsync the directory so the **publish** is durable, not just the bytes it
/// exposed. Without this a crash can resurrect the pre-publish directory
/// entry with the new file's contents already on disk.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir).and_then(|d| d.sync_all())
}

/// No portable directory-fsync exists off unix; `File::open` on a directory
/// is itself an error on Windows. The atomic publish still holds there.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_overwrites_and_leaves_no_temp() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f");
        replace(&path, b"one").unwrap();
        replace(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(leftover_temps(tmp.path()), Vec::<String>::new());
    }

    #[test]
    fn create_new_refuses_an_occupied_path_and_leaves_it_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f");
        create_new(&path, b"first").unwrap();
        assert!(matches!(
            create_new(&path, b"second"),
            Err(CreateNewError::AlreadyExists)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        assert_eq!(leftover_temps(tmp.path()), Vec::<String>::new());
    }

    #[test]
    fn concurrent_create_new_has_exactly_one_winner() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f");
        let winners: usize = std::thread::scope(|s| {
            let handles: Vec<_> = (0..16)
                .map(|i| {
                    let path = path.clone();
                    s.spawn(move || create_new(&path, format!("{i}").as_bytes()).is_ok())
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|h| h.join().ok())
                .filter(|won| *won)
                .count()
        });
        assert_eq!(winners, 1);
        assert_eq!(leftover_temps(tmp.path()), Vec::<String>::new());
    }

    fn leftover_temps(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        names.sort();
        names
    }
}
