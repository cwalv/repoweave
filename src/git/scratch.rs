use super::GitVcs;
use crate::liveness::{self, Hold};
use crate::vcs::VcsError;
use std::path::{Path, PathBuf};

const PREFIX: &str = "rwv-scratch-";

/// The scratch files of one call in a checkout's git dir, each named for the
/// process beside an owner file the process holds locked while it runs. The
/// lock ends with the process, so what a killed call left is removed by the
/// next call to start. Where the filesystem has no locks a call is never
/// refused for want of one; a dead holder is then recognized by its pid on
/// this host only.
pub(super) struct Scratch {
    dir: PathBuf,
    stem: String,
    _owner: std::fs::File,
}

impl Scratch {
    pub(super) fn take(repo: &Path) -> Result<Self, VcsError> {
        let stem = format!("{PREFIX}{}", std::process::id());
        let owner = PathBuf::from(GitVcs::run_trimmed(
            ["rev-parse", "--path-format=absolute", "--git-path", &stem],
            repo,
        )?);
        let dir = owner.parent().map(Path::to_path_buf).unwrap_or_default();
        let io = |e: std::io::Error| VcsError::Io {
            ctx: format!("failed to hold {}", owner.display()),
            source: e,
        };
        let abandoned: Vec<String> = std::fs::read_dir(&dir)
            .map_err(io)?
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| *name != stem && is_owner_name(name))
            .collect();
        for name in abandoned {
            liveness::with_abandoned(&dir.join(&name), || remove_scratch(repo, &dir, &name));
        }
        let file = hold_owner(&owner).map_err(io)?;
        crate::test_pause::point("scratch-taken");
        Ok(Self {
            dir,
            stem,
            _owner: file,
        })
    }

    pub(super) fn path(&self, name: &str) -> String {
        self.dir
            .join(format!("{}-{name}", self.stem))
            .display()
            .to_string()
    }
}

fn is_owner_name(name: &str) -> bool {
    name.strip_prefix(PREFIX)
        .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
}

/// Create `owner` and lock it. A sweep that found the file between its
/// creation and the lock may have removed it, so the lock counts only on the
/// file still standing.
fn hold_owner(owner: &Path) -> std::io::Result<std::fs::File> {
    loop {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(owner)?;
        match liveness::try_hold(&file, owner) {
            Hold::Taken if !liveness::is_current(&file, owner) => continue,
            Hold::Busy => std::thread::sleep(crate::durable_file::CLAIM_POLL),
            Hold::Taken | Hold::Unavailable => {
                std::io::Write::write_all(&mut file, liveness::holder_body().as_bytes())?;
                return Ok(file);
            }
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        remove_scratch_files(&self.dir, &self.stem);
    }
}

/// Remove the owner file `stem` in `dir` and every scratch file or checkout
/// named for it.
fn remove_scratch_files(dir: &Path, stem: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let named = format!("{stem}-");
    let mut removed_checkout = false;
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&named) {
            if entry.path().is_dir() {
                removed_checkout |= std::fs::remove_dir_all(entry.path()).is_ok();
            } else {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let _ = std::fs::remove_file(dir.join(stem));
    removed_checkout
}

fn remove_scratch(repo: &Path, dir: &Path, stem: &str) {
    if remove_scratch_files(dir, stem) {
        let _ = GitVcs::run(["worktree", "prune"], repo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let tmp = crate::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        tmp
    }

    fn plant(dir: &Path, pid: u32) -> (PathBuf, PathBuf) {
        let owner = dir.join(format!("{PREFIX}{pid}"));
        std::fs::write(&owner, liveness::holder_body()).unwrap();
        let index = dir.join(format!("{PREFIX}{pid}-index"));
        std::fs::write(&index, "").unwrap();
        let checkout = dir.join(format!("{PREFIX}{pid}-merge"));
        std::fs::create_dir(&checkout).unwrap();
        std::fs::write(checkout.join("file"), "").unwrap();
        (owner, checkout)
    }

    #[test]
    fn a_dead_runs_files_and_checkouts_are_swept_and_a_running_ones_are_left() {
        let tmp = repo();
        let git_dir = tmp.path().join(".git");
        let (dead_owner, dead_checkout) = plant(&git_dir, 4_000_001);
        let (live_owner, live_checkout) = plant(&git_dir, 4_000_002);
        let live = std::fs::OpenOptions::new()
            .write(true)
            .open(&live_owner)
            .unwrap();
        assert!(matches!(
            liveness::try_hold(&live, &live_owner),
            Hold::Taken
        ));

        let scratch = Scratch::take(tmp.path()).unwrap();

        assert!(!dead_owner.exists() && !dead_checkout.exists());
        assert!(live_owner.exists() && live_checkout.exists());
        assert!(git_dir
            .join(format!("{PREFIX}{}", std::process::id()))
            .exists());
        drop(scratch);
        assert!(!git_dir
            .join(format!("{PREFIX}{}", std::process::id()))
            .exists());
    }

    #[test]
    fn names_that_are_not_an_owner_are_never_swept() {
        assert!(is_owner_name("rwv-scratch-123"));
        assert!(!is_owner_name("rwv-scratch-123-index"));
        assert!(!is_owner_name("rwv-scratch-"));
        assert!(!is_owner_name("rwv-replayed-123"));
    }
}
