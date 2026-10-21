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
/// refuses, how long a holder retries the removal that releases one, and how
/// long a [`replace`] retries a replacement Windows refuses while another
/// handle has the file open.
///
/// A waiter queues behind every holder ahead of it, so this bounds a queue of
/// held sections rather than one, and it has to outlast a full queue on a slow
/// host. Both claims in the tree share the number: two waits that drift apart
/// would make "how long does rwv hang before it complains" depend on which
/// file it was.
pub(crate) const CLAIM_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// How often a wait re-attempts the claim, the removal, or the replacement.
pub(crate) const CLAIM_POLL: std::time::Duration = std::time::Duration::from_millis(5);

/// Why a [`claim`] was not won.
#[derive(Debug)]
pub(crate) enum ClaimError {
    /// A peer still held the claim when [`CLAIM_WAIT`] ran out. Carries what
    /// the claim file records about its holder.
    Held(String),
    Io(std::io::Error),
}

/// A claim file this process created, removed by [`ClaimFile::release`].
#[derive(Debug)]
pub(crate) struct ClaimFile {
    path: PathBuf,
    released: bool,
}

/// Create the claim file at `path`, waiting up to [`CLAIM_WAIT`] for a peer's
/// to go.
pub(crate) fn claim(path: &Path) -> Result<ClaimFile, ClaimError> {
    let holder = format!("pid {}\n", std::process::id());
    let deadline = std::time::Instant::now() + CLAIM_WAIT;
    loop {
        match create_new(path, holder.as_bytes()) {
            Ok(()) => {
                return Ok(ClaimFile {
                    path: path.to_path_buf(),
                    released: false,
                })
            }
            Err(CreateNewError::AlreadyExists) if std::time::Instant::now() >= deadline => {
                let held_by = std::fs::read_to_string(path)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| "an unreadable holder".to_string());
                return Err(ClaimError::Held(held_by));
            }
            Err(CreateNewError::AlreadyExists) => std::thread::sleep(CLAIM_POLL),
            Err(CreateNewError::Io(e)) => return Err(ClaimError::Io(e)),
        }
    }
}

impl ClaimFile {
    /// Remove the claim. A failure is returned only once removal has kept
    /// failing for [`CLAIM_WAIT`].
    pub(crate) fn release(mut self) -> std::io::Result<()> {
        self.released = true;
        self.remove()
    }

    /// On Windows a delete fails while another process — a scanner, an
    /// indexer — holds the file open without sharing deletion, and succeeds
    /// once it closes, so one failed attempt does not yet leave the claim
    /// behind.
    fn remove(&self) -> std::io::Result<()> {
        let deadline = std::time::Instant::now() + CLAIM_WAIT;
        loop {
            match std::fs::remove_file(&self.path) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) if std::time::Instant::now() >= deadline => return Err(e),
                Err(_) => std::thread::sleep(CLAIM_POLL),
            }
        }
    }
}

impl Drop for ClaimFile {
    /// Reached unreleased only when the work under the claim panicked, and
    /// the panic is what reports it.
    fn drop(&mut self) {
        if !self.released {
            let _ = self.remove();
        }
    }
}

/// What work done under a claim returns once the claim's release is known.
///
/// A failed release is an error even after the work succeeded: the claim it
/// leaves behind refuses every later writer.
pub(crate) fn after_release<T>(
    outcome: anyhow::Result<T>,
    released: anyhow::Result<()>,
) -> anyhow::Result<T> {
    match (outcome, released) {
        (outcome, Ok(())) => outcome,
        (Ok(_), Err(release)) => Err(release),
        (Err(e), Err(release)) => Err(e.context(format!("{release:#}"))),
    }
}

/// Publish `bytes` at `path`, replacing whatever is there.
///
/// `rename(2)` is atomic within a filesystem, so a concurrent reader sees
/// either the old file or the new one, never a splice of the two.
pub(crate) fn replace(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = parent_of(path);
    let tmp = staged_temp(path, bytes)
        .with_context(|| format!("failed to stage a write of {}", path.display()))?;
    if let Err(e) = rename_over(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow::Error::new(e).context(format!(
            "failed to rename {} to {}",
            tmp.display(),
            path.display()
        )));
    }
    sync_dir(parent).with_context(|| format!("failed to fsync directory {}", parent.display()))
}

#[cfg(not(windows))]
fn rename_over(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

/// Windows refuses to rename onto a file while any handle to it is open, with
/// `ERROR_ACCESS_DENIED` or `ERROR_SHARING_VIOLATION`. POSIX rename semantics
/// lift that for holders that share deletion, but only on a filesystem that
/// has them. Readers of rwv's state files take no claim, and scanners and
/// indexers open what rwv has just written; each lets go in moments, so the
/// replacement is retried rather than reported.
#[cfg(windows)]
fn rename_over(from: &Path, to: &Path) -> std::io::Result<()> {
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};
    let held_open = |e: &std::io::Error| {
        [ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION]
            .iter()
            .any(|code| e.raw_os_error() == Some(code.0 as i32))
    };
    let deadline = std::time::Instant::now() + CLAIM_WAIT;
    loop {
        match std::fs::rename(from, to) {
            Err(e) if held_open(&e) && std::time::Instant::now() < deadline => {
                std::thread::sleep(CLAIM_POLL)
            }
            outcome => return outcome,
        }
    }
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

    /// As long as the slowest held section seen on a Windows host with eight
    /// writers contending. Spelled out rather than derived from
    /// [`CLAIM_WAIT`], so the budget cannot shrink without this going red.
    const SLOW_HOST_SECTION: std::time::Duration = std::time::Duration::from_millis(550);

    /// The last of eight queued holders waits out the seven sections ahead of
    /// it, so the budget has to cover the queue, not one holder.
    #[test]
    fn every_holder_in_a_queue_of_slow_sections_gets_its_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.lock");
        let refused: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    s.spawn(|| {
                        let held = claim(&path)?;
                        std::thread::sleep(SLOW_HOST_SECTION);
                        held.release().map_err(ClaimError::Io)
                    })
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|h| h.join().unwrap().err())
                .map(|e| format!("{e:?}"))
                .collect()
        });
        assert_eq!(
            refused,
            Vec::<String>::new(),
            "a holder queued behind slow sections must wait its turn, not refuse"
        );
        assert!(!path.exists());
    }

    /// A delete that fails only until another process lets go of the file is
    /// retried rather than reported, and leaves no claim behind.
    #[test]
    #[cfg(unix)]
    fn a_release_that_fails_for_a_moment_is_retried() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.lock");
        let held = claim(&path).unwrap();
        let writable = std::fs::metadata(tmp.path()).unwrap().permissions();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

        let released = std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(200));
                std::fs::set_permissions(tmp.path(), writable.clone()).unwrap();
            });
            held.release()
        });
        std::fs::set_permissions(tmp.path(), writable).unwrap();

        released.expect("a removal that fails only for a moment must be retried");
        assert!(!path.exists(), "the retried release must remove the claim");
    }

    /// Holds `f` open under `share_mode` while replacing it, and lets go
    /// `HOLD` after the staged temp appears, so the first attempt meets the
    /// open handle unless staging outlasts it.
    #[cfg(windows)]
    fn replace_while_held_open(share_mode: u32) {
        use std::os::windows::fs::OpenOptionsExt;
        const HOLD: std::time::Duration = std::time::Duration::from_millis(300);

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f");
        replace(&path, b"one").unwrap();
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(share_mode)
            .open(&path)
            .unwrap();

        let replaced = std::thread::scope(|s| {
            s.spawn(|| {
                let deadline = std::time::Instant::now() + CLAIM_WAIT;
                while leftover_temps(tmp.path()).is_empty() && std::time::Instant::now() < deadline
                {
                    std::thread::sleep(CLAIM_POLL);
                }
                std::thread::sleep(HOLD);
                drop(holder);
            });
            replace(&path, b"two")
        });

        replaced
            .unwrap_or_else(|e| panic!("the replace failed while the holder had it open: {e:#}"));
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(leftover_temps(tmp.path()), Vec::<String>::new());
    }

    #[cfg(windows)]
    const FILE_SHARE_READ: u32 = 0x1;
    #[cfg(windows)]
    const FILE_SHARE_WRITE: u32 = 0x2;
    #[cfg(windows)]
    const FILE_SHARE_DELETE: u32 = 0x4;

    /// Shares deletion, as rwv's own readers do. Windows refuses the
    /// replacement where the filesystem lacks POSIX rename semantics, and Wine
    /// refuses it everywhere.
    #[test]
    #[cfg(windows)]
    fn a_replace_waits_out_a_reader_holding_the_target_open() {
        replace_while_held_open(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    }

    /// Does not share deletion, as a scanner may not. Every Windows
    /// filesystem refuses the replacement.
    #[test]
    #[cfg(windows)]
    fn a_replace_waits_out_a_holder_that_does_not_share_deletion() {
        replace_while_held_open(FILE_SHARE_READ);
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
