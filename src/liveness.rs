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
    match file.try_lock() {
        Ok(()) => Hold::Taken,
        Err(std::fs::TryLockError::WouldBlock) => Hold::Busy,
        Err(std::fs::TryLockError::Error(_)) => Hold::Unavailable,
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

#[cfg(not(unix))]
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

#[cfg(not(unix))]
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
#[cfg(unix)]
pub(crate) fn is_current(file: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), std::fs::metadata(path)) {
        (Ok(held), Ok(at_path)) => held.dev() == at_path.dev() && held.ino() == at_path.ino(),
        _ => false,
    }
}

/// A Windows delete leaves the name reserved until every handle to the old
/// file closes, so a lock taken on it cannot be raced by a new file.
#[cfg(not(unix))]
pub(crate) fn is_current(_file: &File, _path: &Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let held = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        assert!(matches!(try_hold(&held, &path), Hold::Taken));
        let mut removed = false;
        with_abandoned(&path, || removed = true);
        assert!(!removed, "a file whose lock is held is not abandoned");
    }
}
