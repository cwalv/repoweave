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
//! file is a heuristic, not a guarantee. The workweave index is the sharp
//! case — a workweave it records is addressed by name from then on, and git
//! fsyncs the loose ref it writes beside it, so a cached index entry leaves a
//! workweave on disk that no verb can find.
//!
//! ## Why the temp name carries a pid, a process key and a counter and no clock
//!
//! The pid keeps concurrent processes apart; a process-local monotonic
//! counter keeps threads within one process apart; a key drawn once per
//! process keeps a later process that reuses a pid from ever spelling a dead
//! one's temp, so a temp's name is never reused. All three are structural.
//! An earlier revision used a nanosecond timestamp for the intra-process half: two barrier-synchronized threads read the same
//! `SystemTime::now()` under load, collided on the temp path, and each
//! unlinked the other's in-flight temp, so the winner's publish hit ENOENT
//! and *both* calls failed. Uniqueness here must not be re-derived from a
//! clock.
//!
//! ## Who removes a temp a killed writer left
//!
//! A temp is never at a published name, so one left behind blocks nothing.
//! The next publish of a file whose name it begins with removes it, once it
//! can tell no writer still holds it: on unix a writer holds an advisory lock
//! on its temp for as long as the publish runs, which the kernel drops when
//! the process dies; on Windows a writer holds its temp open without sharing
//! deletion until just before it publishes. A temp that may still be held is
//! left. The lock never decides anything else: a writer that cannot take it
//! publishes all the same, and its temp is only the more sweepable. A writer
//! whose temp vanished before it published it stages it again.

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
    /// The filesystem makes neither a hard link nor a rename that refuses an
    /// occupied target, so nothing can be published on it exclusively. Holds
    /// the refusal naming the filesystem.
    NoExclusivePublish(anyhow::Error),
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
    NoExclusivePublish(anyhow::Error),
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
            Err(CreateNewError::AlreadyExists) => {
                crate::test_pause::point("claim-held");
                std::thread::sleep(CLAIM_POLL);
            }
            Err(CreateNewError::Io(e)) => return Err(ClaimError::Io(e)),
            Err(CreateNewError::NoExclusivePublish(e)) => {
                return Err(ClaimError::NoExclusivePublish(e))
            }
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
    replace_with(Publisher::native(), path, bytes)
}

fn replace_with(publisher: Publisher, path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = parent_of(path);
    sweep_abandoned_temps(path);
    let mut restage = Restage::new();
    loop {
        let mut staged = staged_temp(path, bytes, publisher.lock)
            .with_context(|| format!("failed to stage a write of {}", path.display()))?;
        let tmp = staged.ready_to_publish();
        match rename_over(tmp, path) {
            Ok(()) => break,
            Err(e) if vanished(&e) => restage.again(path)?,
            Err(e) => {
                let _ = std::fs::remove_file(tmp);
                return Err(anyhow::Error::new(e).context(format!(
                    "failed to rename {} to {}",
                    tmp.display(),
                    path.display()
                )));
            }
        }
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

/// Publish `bytes` at `path` only if nothing is there, and atomically: a
/// reader sees nothing there or all of `bytes`, never an empty or partial
/// file, and a loser of a race reads the winner's complete file.
///
/// Two publish modes, each atomic and failing on an occupied target: a hard
/// link of the fully written temp, and where the filesystem makes no hard
/// link, a rename of it that refuses to replace. The link is tried first on
/// every call and the rename is reached only on an error that says this
/// filesystem makes no links, never on one that could mean a permission was
/// denied. Where neither is made the publish is refused, naming the
/// filesystem: an exclusive create followed by a write is never a fallback,
/// because a writer killed between the two leaves an empty file that every
/// reader of an op record or a claim refuses.
pub(crate) fn create_new(path: &Path, bytes: &[u8]) -> Result<(), CreateNewError> {
    create_new_with(Publisher::native(), path, bytes)
}

fn create_new_with(publisher: Publisher, path: &Path, bytes: &[u8]) -> Result<(), CreateNewError> {
    let parent = parent_of(path);
    sweep_abandoned_temps(path);
    let mut restage = Restage::new();
    let published = loop {
        let mut staged = staged_temp(path, bytes, publisher.lock).map_err(CreateNewError::Io)?;
        let tmp = staged.ready_to_publish();
        let published = publisher.publish(tmp, path);
        // The temp's role ends here whether or not the publish took.
        let _ = std::fs::remove_file(tmp);
        match published {
            Err(Published::Io(e)) if vanished(&e) => {
                restage.again(path).map_err(CreateNewError::Io)?
            }
            published => break published,
        }
    };
    match published {
        Ok(()) => sync_dir(parent).map_err(CreateNewError::Io),
        Err(Published::Occupied) => Err(CreateNewError::AlreadyExists),
        Err(Published::Io(e)) => Err(CreateNewError::Io(e)),
        Err(Published::Unsupported { link, rename }) => Err(CreateNewError::NoExclusivePublish(
            no_exclusive_publish(path, &link, &rename),
        )),
    }
}

/// How a publish stages and publishes a temp: the calls a filesystem makes or
/// refuses, apart so a test can stand in a filesystem that lacks one.
#[derive(Clone, Copy)]
struct Publisher {
    link: fn(&Path, &Path) -> std::io::Result<()>,
    rename_no_replace: fn(&Path, &Path) -> std::io::Result<()>,
    /// The non-blocking lock a unix writer takes on its temp.
    lock: fn(&std::fs::File) -> std::io::Result<()>,
}

enum Published {
    Occupied,
    Io(std::io::Error),
    Unsupported {
        link: std::io::Error,
        rename: std::io::Error,
    },
}

impl Publisher {
    fn native() -> Self {
        Self {
            link: if crate::test_pause::withheld("hard-link") {
                |_, _| Err(no_hard_links_error())
            } else {
                |from, to| std::fs::hard_link(from, to)
            },
            rename_no_replace: if crate::test_pause::withheld("no-replace-rename") {
                |_, _| Err(no_rename_no_replace_error())
            } else {
                rename_no_replace
            },
            lock: |f| f.try_lock().map_err(std::io::Error::from),
        }
    }

    fn publish(self, tmp: &Path, path: &Path) -> Result<(), Published> {
        let link = match (self.link)(tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(Published::Occupied)
            }
            Err(e) if makes_no_hard_links(&e) => e,
            Err(e) => return Err(Published::Io(e)),
        };
        match (self.rename_no_replace)(tmp, path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(Published::Occupied),
            Err(rename) if makes_no_rename_no_replace(&rename) => {
                Err(Published::Unsupported { link, rename })
            }
            Err(e) => Err(Published::Io(e)),
        }
    }
}

/// `link(2)` answers EPERM where the filesystem has no link operation (vfat,
/// exfat on Linux) and EOPNOTSUPP or ENOSYS on some FUSE and SMB mounts. A
/// denied permission is EACCES, and the temp beside the target was created a
/// moment ago, so EPERM here is not a permission rwv could be granted.
#[cfg(unix)]
fn makes_no_hard_links(e: &std::io::Error) -> bool {
    use nix::libc;
    e.raw_os_error().is_some_and(|code| {
        [libc::EPERM, libc::EOPNOTSUPP, libc::ENOTSUP, libc::ENOSYS].contains(&code)
    })
}

/// `CreateHardLinkW` answers these where the volume has no hard links (FAT,
/// exFAT); a denied permission is `ERROR_ACCESS_DENIED`, which is not among
/// them.
#[cfg(windows)]
fn makes_no_hard_links(e: &std::io::Error) -> bool {
    windows_unsupported(e)
}

/// `renameat2` answers EINVAL where the filesystem takes no
/// `RENAME_NOREPLACE` (NFS, FUSE without rename2) and ENOSYS on a kernel
/// before 3.15; `renamex_np` answers ENOTSUP where the volume has no
/// `RENAME_EXCL`.
#[cfg(unix)]
fn makes_no_rename_no_replace(e: &std::io::Error) -> bool {
    use nix::libc;
    e.raw_os_error().is_some_and(|code| {
        [libc::EINVAL, libc::ENOSYS, libc::EOPNOTSUPP, libc::ENOTSUP].contains(&code)
    })
}

#[cfg(windows)]
fn makes_no_rename_no_replace(e: &std::io::Error) -> bool {
    windows_unsupported(e)
}

#[cfg(windows)]
fn windows_unsupported(e: &std::io::Error) -> bool {
    use windows::Win32::Foundation::{ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED};
    [ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED]
        .iter()
        .any(|code| e.raw_os_error() == Some(code.0 as i32))
}

#[cfg(unix)]
fn no_hard_links_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(nix::libc::EPERM)
}

#[cfg(windows)]
fn no_hard_links_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(windows::Win32::Foundation::ERROR_INVALID_FUNCTION.0 as i32)
}

#[cfg(unix)]
fn no_rename_no_replace_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(nix::libc::EINVAL)
}

#[cfg(windows)]
fn no_rename_no_replace_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(windows::Win32::Foundation::ERROR_NOT_SUPPORTED.0 as i32)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use nix::libc;
    let from = c_path(from)?;
    let to = c_path(to)?;
    // SAFETY: both paths are NUL-terminated and outlive the call.
    let renamed = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if renamed == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_vendor = "apple")]
fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use nix::libc;
    let from = c_path(from)?;
    let to = c_path(to)?;
    // SAFETY: both paths are NUL-terminated and outlive the call.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))
))]
fn rename_no_replace(_from: &Path, _to: &Path) -> std::io::Result<()> {
    Err(std::io::Error::from_raw_os_error(nix::libc::ENOSYS))
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn c_path(path: &Path) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))
}

/// `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` fails with
/// `ERROR_ALREADY_EXISTS` on an occupied target.
#[cfg(windows)]
fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVE_FILE_FLAGS};
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe { MoveFileExW(&HSTRING::from(from), &HSTRING::from(to), MOVE_FILE_FLAGS(0)) }
        .map_err(win32_io_error)
}

/// `windows::core::Error` carries an HRESULT; a Win32 code rides in its low
/// word under `FACILITY_WIN32`, and `std::io::Error` classifies the bare code.
#[cfg(windows)]
fn win32_io_error(e: windows::core::Error) -> std::io::Error {
    let hresult = e.code().0 as u32;
    if hresult & 0xFFFF_0000 == 0x8007_0000 {
        std::io::Error::from_raw_os_error((hresult & 0xFFFF) as i32)
    } else {
        std::io::Error::other(e)
    }
}

fn no_exclusive_publish(
    path: &Path,
    link: &std::io::Error,
    rename: &std::io::Error,
) -> anyhow::Error {
    let dir = parent_of(path);
    crate::refusal::refusal(
        crate::refusal::RefusalKind::FilesystemWithoutExclusivePublish,
        format!(
            "cannot publish {path} exclusively: {dir} is on {fs}, which makes neither a hard \
             link ({link}) nor a rename that refuses an occupied target ({rename}). rwv \
             records its operations and claims through one of the two, and writes nothing \
             else first. Move this workspace to a filesystem that makes hard links (ext4, \
             XFS, Btrfs, APFS, NTFS) or a no-replace rename, then rerun.",
            path = path.display(),
            dir = dir.display(),
            fs = filesystem_of(dir),
        ),
    )
}

/// The filesystem type of `dir` as the OS names it, for a refusal to print.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn filesystem_of(dir: &Path) -> String {
    use nix::libc;
    let Ok(c_dir) = c_path(dir) else {
        return "a filesystem the OS did not name".to_owned();
    };
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the path is NUL-terminated; `stat` is written in full on success.
    if unsafe { libc::statfs(c_dir.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return "a filesystem the OS did not name".to_owned();
    }
    // SAFETY: statfs returned 0.
    let magic = unsafe { stat.assume_init() }.f_type as u64;
    let name = match magic {
        0x4d44 => "vfat (FAT)",
        0x2011_bab0 => "exfat",
        0x6573_5546 => "a FUSE mount",
        0xfe53_4d42 => "SMB2",
        0xff53_4d42 => "CIFS",
        0x517b => "SMB",
        0x6969 => "NFS",
        0x5346_544e => "ntfs3",
        0x9123_683e => "btrfs",
        0x5846_5342 => "xfs",
        0xef53 => "ext4",
        0x0102_1994 => "tmpfs",
        _ => return format!("a filesystem of type 0x{magic:x}"),
    };
    name.to_owned()
}

#[cfg(target_vendor = "apple")]
fn filesystem_of(dir: &Path) -> String {
    use nix::libc;
    let Ok(c_dir) = c_path(dir) else {
        return "a filesystem the OS did not name".to_owned();
    };
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the path is NUL-terminated; `stat` is written in full on success.
    if unsafe { libc::statfs(c_dir.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return "a filesystem the OS did not name".to_owned();
    }
    // SAFETY: statfs returned 0, so `stat` is initialized.
    let stat = unsafe { stat.assume_init() };
    // SAFETY: f_fstypename is NUL-terminated and `stat` outlives the borrow.
    let name = unsafe { std::ffi::CStr::from_ptr(stat.f_fstypename.as_ptr()) };
    name.to_string_lossy().into_owned()
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))
))]
fn filesystem_of(_dir: &Path) -> String {
    "a filesystem the OS did not name".to_owned()
}

#[cfg(windows)]
fn filesystem_of(dir: &Path) -> String {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::{GetVolumeInformationW, GetVolumePathNameW};
    let mut root = [0u16; 261];
    let mut name = [0u16; 261];
    // SAFETY: every buffer outlives its call, and each length is its slice's.
    let named = unsafe {
        GetVolumePathNameW(&HSTRING::from(dir), &mut root).and_then(|()| {
            GetVolumeInformationW(
                windows::core::PCWSTR(root.as_ptr()),
                None,
                None,
                None,
                None,
                Some(&mut name),
            )
        })
    };
    match named {
        Ok(()) => {
            let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
            String::from_utf16_lossy(&name[..end])
        }
        Err(_) => "a filesystem the OS did not name".to_owned(),
    }
}

/// Whether a publish failed because its staged temp was gone: removed by a
/// [`sweep_abandoned_temps`] that met it before its writer held it.
fn vanished(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound
}

/// How long a publish keeps staging again a temp a sweep removed. The first
/// restage is always made, however long the staging that preceded it took;
/// giving up is an error of its own kind, so a caller reads `NotFound` from a
/// publish only as "the directory is gone".
struct Restage {
    deadline: Option<std::time::Instant>,
}

impl Restage {
    fn new() -> Self {
        Self { deadline: None }
    }

    fn again(&mut self, path: &Path) -> std::io::Result<()> {
        crate::test_pause::point("durable-file-restage");
        let now = std::time::Instant::now();
        match self.deadline {
            None => {
                self.deadline = Some(now + CLAIM_WAIT);
                Ok(())
            }
            Some(deadline) if now < deadline => Ok(()),
            Some(_) => Err(std::io::Error::other(format!(
                "could not publish {}: another process removed the copy staged beside it each \
                 time it was staged, for {}s, as the temp sweep of a host that does not see \
                 this host's file locks does. Nothing was published there; rerunning is safe.",
                path.display(),
                CLAIM_WAIT.as_secs()
            ))),
        }
    }
}

/// Remove a lock file rwv created at `path` with [`create_new`]; one already
/// gone is no error.
pub(crate) fn remove_named(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn parent_of(path: &Path) -> &Path {
    path.parent().unwrap_or_else(|| Path::new("."))
}

/// A temp written and fsynced beside the path it will be published at, held
/// by its writer so no sweep takes it for abandoned.
struct Staged {
    path: PathBuf,
    held: Option<std::fs::File>,
}

impl Staged {
    /// Let go of what marks the temp as held where holding it would stop the
    /// publish, and return its path. A unix lock is advisory and is kept until
    /// this value drops, after the publish; a Windows handle that does not
    /// share deletion would refuse the publish itself.
    fn ready_to_publish(&mut self) -> &Path {
        crate::test_pause::point("durable-file-staged");
        if cfg!(not(unix)) {
            self.held = None;
        }
        &self.path
    }
}

/// Drawn once per process, so no later process that reuses this pid spells
/// one of this process's temp names.
fn process_key() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    static KEY: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *KEY.get_or_init(|| {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u32(std::process::id());
        hasher.finish()
    })
}

const TEMP_MARK: &str = ".tmp.";

fn temp_name(file_name: &str, serial: u64) -> String {
    format!(
        "{file_name}{TEMP_MARK}{pid}.{key:016x}.{serial}",
        pid = std::process::id(),
        key = process_key()
    )
}

/// Whether `name` spells a temp [`staged_temp`] makes for a file whose name
/// begins with `published`.
fn is_temp_of(name: &str, published: &str) -> bool {
    let Some((base, tail)) = name.rsplit_once(TEMP_MARK) else {
        return false;
    };
    let parts: Vec<&str> = tail.split('.').collect();
    base.starts_with(published)
        && matches!(parts.as_slice(), [pid, key, serial]
            if pid.parse::<u32>().is_ok()
                && key.len() == 16
                && u64::from_str_radix(key, 16).is_ok()
                && serial.parse::<u64>().is_ok())
}

/// Write `bytes` into a sibling temp of `path`, fsync it, and hold it. On
/// failure nothing is left behind and `path` has not been touched.
fn staged_temp(
    path: &Path,
    bytes: &[u8],
    lock: fn(&std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<Staged> {
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "durable-file".to_owned());
    let serial = TMP_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp_path = parent_of(path).join(temp_name(&file_name, serial));

    match (|| -> std::io::Result<std::fs::File> {
        let mut f = open_held(&tmp_path, lock)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(f)
    })() {
        Ok(held) => Ok(Staged {
            path: tmp_path,
            held: Some(held),
        }),
        Err(e) => {
            // Do not leave the temp behind for a later reader to trip over.
            let _ = std::fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

/// The lock only tells a sweep this temp is not garbage. Whether it is taken
/// changes nothing else: a filesystem that refuses it (NFS `ENOLCK`, macOS
/// `ENOTSUP`) or a sweep that took it first leaves the temp sweepable, and a
/// temp swept from under its writer is staged again.
#[cfg(unix)]
fn open_held(
    tmp: &Path,
    lock: fn(&std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<std::fs::File> {
    let f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)?;
    let _ = lock(&f);
    Ok(f)
}

/// A Windows lock is mandatory, so it would stop a reader of the published
/// file: the temp is held by a handle that shares no deletion instead.
#[cfg(windows)]
fn open_held(
    tmp: &Path,
    _lock: fn(&std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(tmp)
}

/// Remove each temp beside `path` for a file whose name begins with its own
/// that no writer holds: one a writer killed mid-publish left.
fn sweep_abandoned_temps(path: &Path) {
    let Some(published) = path.file_name().map(|s| s.to_string_lossy().into_owned()) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(parent_of(path)) else {
        return;
    };
    for entry in entries.flatten() {
        if is_temp_of(&entry.file_name().to_string_lossy(), &published) {
            remove_if_abandoned(&entry.path());
        }
    }
}

/// Opened for writing: an NFS client grants an exclusive `flock` only on a
/// file open for writing.
#[cfg(unix)]
fn remove_if_abandoned(tmp: &Path) {
    let Ok(f) = std::fs::OpenOptions::new().write(true).open(tmp) else {
        return;
    };
    if f.try_lock().is_ok() {
        let _ = std::fs::remove_file(tmp);
    }
}

/// A delete fails with a sharing violation while the writer holds the temp
/// open, since it shares no deletion.
#[cfg(windows)]
fn remove_if_abandoned(tmp: &Path) {
    let _ = std::fs::remove_file(tmp);
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
        let tmp = crate::tempdir().unwrap();
        let path = tmp.path().join("f");
        replace(&path, b"one").unwrap();
        replace(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(leftover_temps(tmp.path()), Vec::<String>::new());
    }

    #[test]
    fn create_new_refuses_an_occupied_path_and_leaves_it_intact() {
        let tmp = crate::tempdir().unwrap();
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
        let tmp = crate::tempdir().unwrap();
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

    fn without_hard_links() -> Publisher {
        Publisher {
            link: |_, _| Err(no_hard_links_error()),
            ..Publisher::native()
        }
    }

    #[test]
    fn where_no_hard_link_is_made_the_rename_publishes_exclusively() {
        let tmp = crate::tempdir().unwrap();
        let path = tmp.path().join("f");
        create_new_with(without_hard_links(), &path, b"first").unwrap();
        assert!(matches!(
            create_new_with(without_hard_links(), &path, b"second"),
            Err(CreateNewError::AlreadyExists)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        assert_eq!(leftover_temps(tmp.path()), Vec::<String>::new());
    }

    #[test]
    fn concurrent_create_new_without_hard_links_has_exactly_one_winner() {
        let tmp = crate::tempdir().unwrap();
        let path = tmp.path().join("f");
        let winners: usize = std::thread::scope(|s| {
            let handles: Vec<_> = (0..16)
                .map(|i| {
                    let path = path.clone();
                    s.spawn(move || {
                        match create_new_with(
                            without_hard_links(),
                            &path,
                            format!("{i}").as_bytes(),
                        ) {
                            Ok(()) => true,
                            Err(CreateNewError::AlreadyExists) => false,
                            Err(e) => panic!("a loser must find the path occupied: {e:?}"),
                        }
                    })
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

    #[test]
    #[cfg(unix)]
    fn a_link_refused_for_permission_is_reported_and_never_renamed() {
        let tmp = crate::tempdir().unwrap();
        let path = tmp.path().join("f");
        let denied = Publisher {
            link: |_, _| Err(std::io::Error::from_raw_os_error(nix::libc::EACCES)),
            rename_no_replace: |_, _| panic!("a denied permission must not reach the rename"),
            ..Publisher::native()
        };
        match create_new_with(denied, &path, b"x") {
            Err(CreateNewError::Io(e)) => {
                assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied)
            }
            other => panic!("expected the permission error itself, got {other:?}"),
        }
        assert!(!path.exists());
        assert_eq!(leftover_temps(tmp.path()), Vec::<String>::new());
    }

    #[test]
    fn a_filesystem_with_links_never_reaches_the_rename() {
        let tmp = crate::tempdir().unwrap();
        let path = tmp.path().join("f");
        let linking = Publisher {
            rename_no_replace: |_, _| panic!("a filesystem that links must publish by link"),
            ..Publisher::native()
        };
        create_new_with(linking, &path, b"x").unwrap();
        assert!(matches!(
            create_new_with(linking, &path, b"y"),
            Err(CreateNewError::AlreadyExists)
        ));
    }

    #[test]
    fn where_neither_mode_is_made_the_publish_is_refused_naming_the_filesystem() {
        let tmp = crate::tempdir().unwrap();
        let path = tmp.path().join("f");
        let neither = Publisher {
            link: |_, _| Err(no_hard_links_error()),
            rename_no_replace: |_, _| Err(no_rename_no_replace_error()),
            ..Publisher::native()
        };
        let Err(CreateNewError::NoExclusivePublish(e)) = create_new_with(neither, &path, b"x")
        else {
            panic!("expected the named refusal");
        };
        assert_eq!(
            crate::refusal::kind_of(&e),
            Some(crate::refusal::RefusalKind::FilesystemWithoutExclusivePublish)
        );
        let text = format!("{e:#}");
        assert!(text.contains(&filesystem_of(tmp.path())), "{text}");
        assert!(text.contains("hard links"), "{text}");
        assert!(!path.exists());
        assert_eq!(leftover_temps(tmp.path()), Vec::<String>::new());
    }

    /// Each way a writer's lock on its temp can fail: taken first by a sweep,
    /// NFS's failed lock protocol, macOS's and Linux's "not supported", an
    /// interrupted call. None of them may fail or stall a publish.
    #[cfg(unix)]
    const LOCK_FAILURES: [fn(&std::fs::File) -> std::io::Result<()>; 5] = [
        |_| Err(std::io::Error::from_raw_os_error(nix::libc::EWOULDBLOCK)),
        |_| Err(std::io::Error::from_raw_os_error(nix::libc::ENOLCK)),
        |_| Err(std::io::Error::from_raw_os_error(nix::libc::ENOTSUP)),
        |_| Err(std::io::Error::from_raw_os_error(nix::libc::EOPNOTSUPP)),
        |_| Err(std::io::Error::from_raw_os_error(nix::libc::EINTR)),
    ];

    #[test]
    #[cfg(unix)]
    fn a_lock_that_fails_never_fails_a_publish_in_either_mode() {
        for (i, lock) in LOCK_FAILURES.into_iter().enumerate() {
            let tmp = crate::tempdir().unwrap();
            for (mode, publisher) in [
                ("link", Publisher::native()),
                ("rename", without_hard_links()),
            ] {
                let publisher = Publisher { lock, ..publisher };
                let path = tmp.path().join(format!("created-{mode}"));
                create_new_with(publisher, &path, b"x")
                    .unwrap_or_else(|e| panic!("lock failure {i}, {mode}: {e:?}"));
                assert_eq!(std::fs::read(&path).unwrap(), b"x");
                let path = tmp.path().join(format!("replaced-{mode}"));
                replace_with(publisher, &path, b"one").unwrap();
                replace_with(publisher, &path, b"two")
                    .unwrap_or_else(|e| panic!("lock failure {i}, {mode}: {e:#}"));
                assert_eq!(std::fs::read(&path).unwrap(), b"two");
            }
            assert_eq!(leftover_temps(tmp.path()), Vec::<String>::new());
        }
    }

    #[test]
    fn the_first_restage_is_always_made_and_giving_up_is_never_read_as_a_missing_directory() {
        let path = Path::new("f");
        let mut restage = Restage {
            deadline: Some(std::time::Instant::now()),
        };
        let gave_up = restage.again(path).unwrap_err();
        assert_ne!(gave_up.kind(), std::io::ErrorKind::NotFound);
        assert!(
            gave_up.to_string().contains("rerunning is safe"),
            "{gave_up}"
        );

        let mut restage = Restage::new();
        std::thread::sleep(std::time::Duration::from_millis(1));
        restage
            .again(path)
            .expect("a first vanish is staged again however long staging took");
    }

    #[test]
    fn a_temp_no_writer_holds_is_removed_by_the_next_publish_of_its_name() {
        let tmp = crate::tempdir().unwrap();
        let path = tmp.path().join("f.lock");
        let abandoned = tmp.path().join(temp_name("f.lock.takeover.7", 99));
        std::fs::write(&abandoned, b"half").unwrap();
        let unrelated = tmp.path().join(temp_name("g", 99));
        std::fs::write(&unrelated, b"other").unwrap();
        let foreign = tmp.path().join("f.lock.tmp.notes");
        std::fs::write(&foreign, b"a user's").unwrap();

        create_new(&path, b"x").unwrap();

        assert!(!abandoned.exists(), "an abandoned temp must be removed");
        assert!(
            unrelated.exists(),
            "a temp of another file is not this publish's"
        );
        assert!(
            foreign.exists(),
            "a name rwv does not make is never removed"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_temp_its_writer_still_holds_is_left() {
        let tmp = crate::tempdir().unwrap();
        let path = tmp.path().join("f");
        let staged = staged_temp(&path, b"in flight", Publisher::native().lock).unwrap();
        replace(&path, b"x").unwrap();
        assert!(staged.path.exists(), "a held temp must not be swept");
        drop(staged);
    }

    /// As long as the slowest held section seen on a Windows host with eight
    /// writers contending. Spelled out rather than derived from
    /// [`CLAIM_WAIT`], so the budget cannot shrink without this going red.
    const SLOW_HOST_SECTION: std::time::Duration = std::time::Duration::from_millis(550);

    /// The last of eight queued holders waits out the seven sections ahead of
    /// it, so the budget has to cover the queue, not one holder.
    #[test]
    fn every_holder_in_a_queue_of_slow_sections_gets_its_turn() {
        let tmp = crate::tempdir().unwrap();
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

        let tmp = crate::tempdir().unwrap();
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

        let tmp = crate::tempdir().unwrap();
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
