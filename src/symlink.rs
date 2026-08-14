//! The one place a platform symlink API is called.
//!
//! Unix creates a symbolic link with a single call that takes no
//! directory-vs-file argument; Windows has two calls, and a link made with the
//! wrong one does not resolve. Every rwv site that creates a link therefore
//! makes a kind decision, whether or not it looks like one, and routing them
//! all through [`create`] puts that decision in code Unix compiles, reads and
//! tests rather than inside a `#[cfg(windows)]` arm nothing here can run.
//!
//! Failure returns rather than warning. rwv reads its own links back as
//! structural facts — `classify_checkout` answers "is this a shared read-only
//! alias" by asking the filesystem what is at the path — so a link that
//! silently failed to appear is a workspace that misreports what it contains.
//! The argument is in `docs/explanation/joints/symlinks-as-structure.md`.

use std::path::Path;

/// Which of Windows' two symlink constructors a link needs.
///
/// Carried on every platform, and read only on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkTarget {
    Directory,
    File,
}

impl LinkTarget {
    /// Classify by what `source` is on disk at this moment.
    ///
    /// A path that does not exist yet is a [`LinkTarget::File`]: the links rwv
    /// creates ahead of their target are ecosystem lock files, written later
    /// through the dangling link. A target that changes kind after the link is
    /// made is out of reach of any creation-time rule.
    pub fn on_disk(source: &Path) -> LinkTarget {
        if source.is_dir() {
            LinkTarget::Directory
        } else {
            LinkTarget::File
        }
    }
}

/// Remove the symlink at `link`, whatever it points at.
///
/// Windows types a symlink at creation: one made for a directory is refused
/// by `remove_file` (`Access is denied`) and unlinked with `remove_dir`.
/// Both spellings are one unlink everywhere else, so the fallback fires only
/// where the path really is a symlink — a real directory is never removed
/// here, and a real file keeps `remove_file`'s exact behavior and error.
pub fn remove(link: &Path) -> std::io::Result<()> {
    let Err(primary_err) = std::fs::remove_file(link) else {
        return Ok(());
    };
    let is_symlink = std::fs::symlink_metadata(link)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if cfg!(windows) && is_symlink {
        std::fs::remove_dir(link).map_err(|_| primary_err)
    } else {
        Err(primary_err)
    }
}

/// What an operator does when Windows refuses to create a symbolic link.
///
/// Developer Mode is a machine-wide policy an administrator sets once, not a
/// privilege a process can ask for, so this offers only what a person can act
/// on and never suggests rwv could grant itself the capability.
pub const WINDOWS_PERMISSION_REMEDY: &str =
    "Windows creates symbolic links only for an elevated process, or on a machine with Developer \
     Mode enabled: enable Developer Mode (Settings > System > For developers; one-time, requires \
     an administrator) or re-run rwv from an elevated prompt";

/// Create a symbolic link at `link` pointing at `target`.
pub fn create(target: &Path, link: &Path, kind: LinkTarget) -> anyhow::Result<()> {
    platform_symlink(target, link, kind)
        .map_err(|e| anyhow::Error::msg(refusal(&e, target, link, cfg!(windows))))
}

/// The sentence an operator reads when a link could not be created.
///
/// `on_windows` names the platform the refusal is *for*, not the one this is
/// compiled on. A permission failure only happens where these tests never run,
/// so taking the platform as an argument is what lets a Linux run read the
/// text a Windows operator would get.
fn refusal(error: &std::io::Error, target: &Path, link: &Path, on_windows: bool) -> String {
    let mut message = format!(
        "failed to create symlink {} -> {}: {error}",
        link.display(),
        target.display()
    );
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        message.push_str("; ");
        message.push_str(&occupied_path_remedy(link));
    } else if on_windows {
        message.push_str("; ");
        message.push_str(WINDOWS_PERMISSION_REMEDY);
    }
    message
}

/// What an operator does when something already sits where a link belongs.
///
/// rwv never overwrites what it did not put there, so removing it is the one
/// thing that moves the state forward, and the sentence has to name which
/// path it means. `EEXIST` does not distinguish a file from a directory or
/// a foreign link, so the occupant is classified here by looking again —
/// which of those it is decides what the operator just protected.
pub fn occupied_path_remedy(link: &Path) -> String {
    let occupant = match std::fs::symlink_metadata(link) {
        Ok(meta) if meta.file_type().is_symlink() => match std::fs::read_link(link) {
            Ok(target) => format!("a symlink to {}", target.display()),
            Err(_) => "a symlink whose target could not be read".to_string(),
        },
        Ok(meta) if meta.file_type().is_dir() => "a directory".to_string(),
        Ok(_) => "a regular file".to_string(),
        Err(_) => "no longer observable".to_string(),
    };
    format!(
        "rwv does not overwrite what is already at {} ({occupant}) — remove it and re-run",
        link.display()
    )
}

/// Rust's `symlink_file` and `symlink_dir` both pass
/// `SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE`, which is the opt-in that
/// makes Developer Mode apply, so the happy path needs no privilege code here.
#[cfg(windows)]
fn platform_symlink(target: &Path, link: &Path, kind: LinkTarget) -> std::io::Result<()> {
    match kind {
        LinkTarget::Directory => std::os::windows::fs::symlink_dir(target, link),
        LinkTarget::File => std::os::windows::fs::symlink_file(target, link),
    }
}

#[cfg(unix)]
fn platform_symlink(target: &Path, link: &Path, _kind: LinkTarget) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_source_yields_a_directory_link() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");
        std::fs::create_dir(&dir).unwrap();
        assert_eq!(LinkTarget::on_disk(&dir), LinkTarget::Directory);
    }

    #[test]
    fn a_file_source_yields_a_file_link() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("Cargo.toml");
        std::fs::write(&file, "").unwrap();
        assert_eq!(LinkTarget::on_disk(&file), LinkTarget::File);
    }

    #[test]
    fn an_absent_source_yields_a_file_link() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            LinkTarget::on_disk(&tmp.path().join("Cargo.lock")),
            LinkTarget::File,
            "surfacing creates links ahead of lock files that do not exist yet"
        );
    }

    #[test]
    fn a_symlink_to_a_directory_yields_a_directory_link() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("canonical");
        std::fs::create_dir(&dir).unwrap();
        let alias = tmp.path().join("alias");
        create(&dir, &alias, LinkTarget::Directory).unwrap();
        assert_eq!(
            LinkTarget::on_disk(&alias),
            LinkTarget::Directory,
            "a nested workweave classifies a parent's alias, which resolves to a directory"
        );
    }

    #[test]
    fn an_occupied_link_path_returns_the_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("occupied");
        std::fs::write(&link, "user content").unwrap();
        let err = create(Path::new("target"), &link, LinkTarget::File).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("occupied") && msg.contains("target"),
            "the refusal must name both paths: {msg}"
        );
        assert_eq!(
            std::fs::read_to_string(&link).unwrap(),
            "user content",
            "a refused link must leave what was there alone"
        );
    }

    fn denied() -> std::io::Error {
        std::io::Error::from(std::io::ErrorKind::PermissionDenied)
    }

    fn exists() -> std::io::Error {
        std::io::Error::from(std::io::ErrorKind::AlreadyExists)
    }

    #[test]
    fn a_windows_permission_failure_carries_the_remedy() {
        let message = refusal(&denied(), Path::new("t"), Path::new("l"), true);
        assert!(
            message.contains(WINDOWS_PERMISSION_REMEDY),
            "a Windows operator must be told what to do: {message}"
        );
    }

    #[test]
    fn a_unix_permission_failure_carries_no_windows_remedy() {
        let message = refusal(&denied(), Path::new("t"), Path::new("l"), false);
        assert!(
            !message.contains("Developer Mode"),
            "without this the test above passes on a remedy pasted onto every \
             failure, which is not what it claims: {message}"
        );
    }

    #[test]
    fn an_occupied_path_reads_the_same_on_either_platform() {
        for on_windows in [true, false] {
            let message = refusal(&exists(), Path::new("t"), Path::new("l"), on_windows);
            assert!(
                message.contains(&occupied_path_remedy(Path::new("l"))),
                "on_windows={on_windows}: {message}"
            );
            assert!(
                !message.contains("Developer Mode"),
                "an occupied path is not a privilege problem, and offering \
                 Developer Mode for it sends the operator somewhere useless: \
                 {message}"
            );
        }
    }

    #[test]
    fn create_refuses_through_the_shared_sentence() {
        // The helper above is only evidence if the real error path is the same
        // path. Both calls hit one occupied link, so the two errors agree and
        // the strings may be compared whole.
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("occupied");
        std::fs::write(&link, "user content").unwrap();
        let target = Path::new("target");

        let raw = platform_symlink(target, &link, LinkTarget::File).unwrap_err();
        let err = create(target, &link, LinkTarget::File).unwrap_err();

        assert_eq!(err.to_string(), refusal(&raw, target, &link, cfg!(windows)));
    }

    #[test]
    fn the_windows_remedy_names_both_actions_and_claims_neither_for_rwv() {
        let remedy = WINDOWS_PERMISSION_REMEDY.to_lowercase();
        assert!(remedy.contains("developer mode"));
        assert!(remedy.contains("elevated"));
        assert!(
            remedy.contains("administrator"),
            "enabling Developer Mode is an administrator action, and saying so is what \
             stops a reader hunting for a per-user setting"
        );
    }
}
