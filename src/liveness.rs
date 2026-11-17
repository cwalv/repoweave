//! What a process holds only while it runs, and how another process tells a
//! live holder from a dead one.
//!
//! A holder keeps an advisory lock on a file for as long as it runs; the
//! kernel drops it when the process dies, so a file whose lock can be taken
//! has no holder. A filesystem with no advisory locks (NFS without a lock
//! daemon, some FUSE and SMB mounts) cannot say: the holder's file then
//! names its pid and host, and a process on the same host reads the pid.
//! A holder on another host cannot be shown gone from here, so its file
//! counts as held.

use std::fs::File;
use std::path::Path;

/// The outcome of asking for the lock on a holder's file.
pub(crate) enum Hold {
    /// The lock is taken, and kept until the file handle drops.
    Taken,
    /// Another process holds it.
    Busy,
    /// The filesystem has no advisory locks, so nothing is learned.
    Unavailable,
}

pub(crate) fn try_hold(file: &File, at: &Path) -> Hold {
    if crate::test_pause::withheld_at("advisory-lock", at) {
        return Hold::Unavailable;
    }
    match try_lock(file) {
        Ok(()) => Hold::Taken,
        Err(std::fs::TryLockError::WouldBlock) => Hold::Busy,
        Err(std::fs::TryLockError::Error(_)) => Hold::Unavailable,
    }
}

#[cfg(not(windows))]
fn try_lock(file: &File) -> Result<(), std::fs::TryLockError> {
    file.try_lock()
}

/// A Windows lock is mandatory: one over the holder's own bytes would refuse
/// every reader of the file that names its holder. The byte locked lies past
/// anything a holder's file holds, which a lock may.
#[cfg(windows)]
fn try_lock(file: &File) -> Result<(), std::fs::TryLockError> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{ERROR_LOCK_VIOLATION, HANDLE};
    use windows::Win32::Storage::FileSystem::LockFile;
    const HOLD_BYTE: u64 = 1 << 62;
    // SAFETY: the handle is open for the duration of the call.
    match unsafe {
        LockFile(
            HANDLE(file.as_raw_handle()),
            HOLD_BYTE as u32,
            (HOLD_BYTE >> 32) as u32,
            1,
            0,
        )
    }
    .map_err(crate::durable_file::win32_io_error)
    {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(ERROR_LOCK_VIOLATION.0 as i32) => {
            Err(std::fs::TryLockError::WouldBlock)
        }
        Err(e) => Err(std::fs::TryLockError::Error(e)),
    }
}

/// What a holder's file says about its holder: this process and host.
pub(crate) fn holder_body() -> String {
    format!("pid {} host {}\n", std::process::id(), host())
}

#[cfg(unix)]
fn host() -> String {
    nix::unistd::gethostname()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(windows)]
fn host() -> String {
    use windows::core::PWSTR;
    use windows::Win32::System::SystemInformation::{ComputerNameDnsHostname, GetComputerNameExW};
    let mut len = 0u32;
    // SAFETY: a null buffer with a length asks only for the length.
    let _ = unsafe { GetComputerNameExW(ComputerNameDnsHostname, None, &mut len) };
    let mut name = vec![0u16; len as usize];
    // SAFETY: `name` holds `len` units and outlives the call.
    match unsafe {
        GetComputerNameExW(
            ComputerNameDnsHostname,
            Some(PWSTR(name.as_mut_ptr())),
            &mut len,
        )
    } {
        Ok(()) => String::from_utf16_lossy(&name[..len as usize]),
        Err(_) => String::new(),
    }
}

#[cfg(not(any(unix, windows)))]
fn host() -> String {
    String::new()
}

/// The pid in `body`, as [`holder_body`] wrote it, when it names this host.
fn local_pid(body: &str) -> Option<i32> {
    let mut words = body.split_whitespace();
    let (Some("pid"), Some(pid), Some("host"), Some(host_name)) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        return None;
    };
    (host_name == host()).then(|| pid.parse().ok()).flatten()
}

/// Whether `body`, as [`holder_body`] wrote it, names a process on this host
/// that no longer runs.
#[cfg(unix)]
pub(crate) fn holder_is_gone(body: &str) -> bool {
    local_pid(body).is_some_and(|pid| {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None)
            == Err(nix::errno::Errno::ESRCH)
    })
}

/// `OpenProcess` refuses a pid no process holds with
/// `ERROR_INVALID_PARAMETER`; one it opens has ended once its exit code is
/// other than `STILL_ACTIVE`. Any other refusal (a process of another user)
/// is a process that runs.
#[cfg(windows)]
pub(crate) fn holder_is_gone(body: &str) -> bool {
    use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, STILL_ACTIVE};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let Some(pid) = local_pid(body).and_then(|pid| u32::try_from(pid).ok()) else {
        return false;
    };
    // SAFETY: OpenProcess takes no pointers; the handle it returns is closed below.
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Err(e) => e.code() == ERROR_INVALID_PARAMETER.to_hresult(),
        Ok(process) => {
            let mut code = 0u32;
            // SAFETY: `process` is open and `code` outlives the call.
            let read = unsafe { GetExitCodeProcess(process, &mut code) };
            // SAFETY: `process` was opened above and is closed once.
            let _ = unsafe { CloseHandle(process) };
            read.is_ok() && code != STILL_ACTIVE.0 as u32
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn holder_is_gone(_body: &str) -> bool {
    false
}

/// What the file at `path` says of its holder when this host cannot tell
/// whether it still runs: the filesystem has no advisory locks, and the file
/// names a process on another host or no process at all. `None` wherever the
/// lock is the evidence, and wherever a process on this host answers.
pub(crate) fn unreachable_holder(path: &Path) -> Option<String> {
    let file = std::fs::OpenOptions::new().write(true).open(path).ok()?;
    if !matches!(try_hold(&file, path), Hold::Unavailable) {
        return None;
    }
    let body = std::fs::read_to_string(path).unwrap_or_default();
    (local_pid(&body).is_none()).then(|| body.trim().to_owned())
}

/// Remove the file at `path` while it still reads as `seen` from
/// [`unreachable_holder`].
pub(crate) fn release_unreachable(path: &Path, seen: &str) -> std::io::Result<bool> {
    if unreachable_holder(path).as_deref() != Some(seen) {
        return Ok(false);
    }
    std::fs::remove_file(path).map(|()| true)
}

/// Whether the file at `path` has no live holder, taking its lock to say so.
/// Opened for writing: an NFS client grants an exclusive lock only on a file
/// open for writing. The lock is released when this returns; a caller that
/// removes the file must do so through [`with_abandoned`].
pub(crate) fn with_abandoned(path: &Path, then: impl FnOnce()) {
    let Ok(file) = std::fs::OpenOptions::new().write(true).open(path) else {
        return;
    };
    let gone = match try_hold(&file, path) {
        Hold::Taken => true,
        Hold::Busy => false,
        Hold::Unavailable => std::fs::read_to_string(path).is_ok_and(|body| holder_is_gone(&body)),
    };
    if gone {
        then();
    }
}

/// Whether `file` is still what stands at `path`: a holder removes its file
/// before it lets go of the lock, so a lock taken on the removed file holds
/// nothing.
///
/// Where the filesystem names no file identity the lock counts. Exclusion
/// then rests on the name: a filesystem that deletes a still-open file
/// without reserving its name until the last handle closes (Windows' POSIX
/// delete) is one that names identities, so on the rest no new file can
/// stand at the name while the lock's handle is open.
pub(crate) fn is_current(file: &File, path: &Path) -> bool {
    !matches!(
        standing_of(identity(file), identity_at(path)),
        Standing::Replaced
    )
}

type Identity = (u64, u128);

#[derive(Debug, PartialEq, Eq)]
enum Standing {
    Current,
    Replaced,
    Unidentified,
}

/// `Ok(None)` is a filesystem that names no identity; an error reading the
/// path is a name that no longer opens to the held file.
fn standing_of(
    held: std::io::Result<Option<Identity>>,
    at_path: std::io::Result<Option<Identity>>,
) -> Standing {
    match (held, at_path) {
        (Err(_), _) | (_, Err(_)) => Standing::Replaced,
        (Ok(Some(held)), Ok(Some(at_path))) if held == at_path => Standing::Current,
        (Ok(Some(_)), Ok(Some(_))) => Standing::Replaced,
        _ => Standing::Unidentified,
    }
}

#[cfg(unix)]
fn identity(file: &File) -> std::io::Result<Option<Identity>> {
    use std::os::unix::fs::MetadataExt;
    file.metadata()
        .map(|m| Some((m.dev(), u128::from(m.ino()))))
}

#[cfg(unix)]
fn identity_at(path: &Path) -> std::io::Result<Option<Identity>> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).map(|m| Some((m.dev(), u128::from(m.ino()))))
}

/// The 128-bit id where the filesystem answers one, else the 64-bit file
/// index, else none.
#[cfg(windows)]
fn identity(file: &File) -> std::io::Result<Option<Identity>> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO,
    };
    let handle = HANDLE(file.as_raw_handle());
    let mut id = FILE_ID_INFO::default();
    // SAFETY: the handle is open for the call and `id` is as large as stated.
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&raw mut id).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .is_ok()
    {
        return Ok(Some((
            id.VolumeSerialNumber,
            u128::from_ne_bytes(id.FileId.Identifier),
        )));
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is open for the call and `info` outlives it.
    Ok(unsafe { GetFileInformationByHandle(handle, &mut info) }
        .is_ok()
        .then(|| {
            (
                u64::from(info.dwVolumeSerialNumber),
                (u128::from(info.nFileIndexHigh) << 32) | u128::from(info.nFileIndexLow),
            )
        }))
}

/// Stable Rust reads no file identity from a path on Windows, so the path is
/// opened: a file deleted while still open refuses that, and reads as gone.
#[cfg(windows)]
fn identity_at(path: &Path) -> std::io::Result<Option<Identity>> {
    identity(&File::open(path)?)
}

#[cfg(not(any(unix, windows)))]
fn identity(_file: &File) -> std::io::Result<Option<Identity>> {
    Ok(None)
}

#[cfg(not(any(unix, windows)))]
fn identity_at(path: &Path) -> std::io::Result<Option<Identity>> {
    std::fs::metadata(path).map(|_| None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_counts_unless_the_name_opens_to_another_file_or_to_none() {
        let gone = || Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        let id = |n: u128| Ok(Some((1, n)));
        assert_eq!(standing_of(id(7), id(7)), Standing::Current);
        assert_eq!(standing_of(id(7), id(8)), Standing::Replaced);
        assert_eq!(standing_of(id(7), gone()), Standing::Replaced);
        assert_eq!(standing_of(gone(), id(7)), Standing::Replaced);
        assert_eq!(standing_of(Ok(None), Ok(None)), Standing::Unidentified);
        assert_eq!(standing_of(id(7), Ok(None)), Standing::Unidentified);
        assert_eq!(standing_of(Ok(None), gone()), Standing::Replaced);
    }

    #[cfg(unix)]
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[test]
    #[cfg(unix)]
    fn a_holder_on_this_host_is_gone_once_its_process_is() {
        let dead = format!("pid {} host {}\n", dead_pid(), host());
        assert!(holder_is_gone(&dead));
        assert!(!holder_is_gone(&holder_body()));
    }

    #[test]
    #[cfg(unix)]
    fn a_holder_on_another_host_or_unreadable_is_never_shown_gone() {
        assert!(!holder_is_gone(&format!(
            "pid {} host another-host.invalid\n",
            dead_pid()
        )));
        assert!(!holder_is_gone("pid 1\n"));
        assert!(!holder_is_gone(""));
    }

    #[test]
    fn a_file_whose_lock_is_free_is_abandoned_and_a_held_one_is_not() {
        let tmp = crate::tempdir().unwrap();
        let path = tmp.path().join("held");
        std::fs::write(&path, holder_body()).unwrap();
        let mut removed = false;
        with_abandoned(&path, || removed = true);
        assert!(removed);

        // A child another test thread forks inherits the descriptor
        // `with_abandoned` locked until it execs, and the lock lasts while
        // any copy is open, so it can read as held for that instant.
        let held = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        while !matches!(try_hold(&held, &path), Hold::Taken) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut removed = false;
        with_abandoned(&path, || removed = true);
        assert!(!removed, "a file whose lock is held is not abandoned");
    }
}
