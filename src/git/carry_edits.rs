//! Edits standing over a branch that moved under the checkout, handed to
//! git's own machinery: HEAD is pointed at the tip the files hold, which
//! leaves exactly the edits differing from it, then `checkout -m` merges them
//! onto the branch, or `stash push` keeps them before a plain checkout.

use super::{git_command, Argv, DERIVED_CONTENT_DRIVER_NAME};
use crate::vcs::Vcs;
use crate::vcs::{CommitId, Refusable, SetBack, UncomparedPaths, VcsError};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

const SET_BACK: &str = "rwv: set back to the tip its files hold, from ";
const SET_ASIDE: &str = "rwv: edits set aside from ";

fn git(args: impl Into<Argv>, repo: &Path) -> Result<Output, VcsError> {
    let args = args.into();
    git_command()
        .args(&args)
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| VcsError::Io {
            ctx: format!("failed to run git {args:?}"),
            source: e,
        })
}

fn succeeded(args: impl Into<Argv>, repo: &Path) -> Result<Vec<u8>, VcsError> {
    let args = args.into();
    let output = git(args.clone(), repo)?;
    if !output.status.success() {
        return Err(VcsError::CommandFailed {
            args: args.into_words(),
            repo: repo.to_path_buf(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(output.stdout)
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

fn fields(listing: &[u8]) -> impl Iterator<Item = String> + '_ {
    listing
        .split(|b| *b == 0)
        .filter(|l| !l.is_empty())
        .map(|l| String::from_utf8_lossy(l).into_owned())
}

fn refusable(output: Output) -> Refusable<()> {
    if output.status.success() {
        return Refusable::Done(());
    }
    Refusable::Refused(String::from_utf8_lossy(&output.stderr).trim().to_owned())
}

pub(super) fn set_back(repo: &Path, at: &CommitId) -> Result<(), VcsError> {
    let crate::vcs::HeadAttachment::Attached(on) = super::GitVcs.head_attachment(repo)? else {
        return Err(VcsError::CommandFailed {
            args: vec!["set back".to_owned()],
            repo: repo.to_path_buf(),
            stderr: "HEAD is on no branch with commits".to_owned(),
        });
    };
    let branch = on.to_string();

    let message = format!("{SET_BACK}{branch}");
    succeeded(
        [
            "update-ref",
            "--no-deref",
            "-m",
            message.as_str(),
            "HEAD",
            at.as_str(),
        ],
        repo,
    )
    .map(drop)
}

pub(super) fn set_back_from(repo: &Path) -> Result<Option<SetBack>, VcsError> {
    if git(["symbolic-ref", "-q", "HEAD"], repo)?.status.success() {
        return Ok(None);
    }
    let output = git(
        [
            "log",
            "-g",
            "-n1",
            "--no-show-signature",
            "--format=%H %gs",
            "HEAD",
        ],
        repo,
    )?;
    if !output.status.success() {
        return Ok(None);
    }
    let recorded = text(output.stdout);
    let Some((at, subject)) = recorded.split_once(' ') else {
        return Ok(None);
    };
    let Some(branch) = subject.strip_prefix(SET_BACK) else {
        return Ok(None);
    };
    let Ok(at) = CommitId::parse(at) else {
        return Ok(None);
    };
    let head = text(succeeded(["rev-parse", "--verify", "-q", "HEAD"], repo)?);
    if head != at.as_str() {
        return Ok(None);
    }
    Ok(Some(SetBack {
        at,
        branch: branch.to_owned(),
    }))
}

pub(super) fn carry_edits_onto(repo: &Path, branch: &str) -> Result<Refusable<()>, VcsError> {
    let derived = format!("merge.{DERIVED_CONTENT_DRIVER_NAME}.driver=true");
    git(
        ["-c", derived.as_str(), "checkout", "-m", branch, "--"],
        repo,
    )
    .map(refusable)
}

/// `git checkout` without `-m`: its fallback resets the files before merging
/// the edits back, which a stop in between would leave only in an
/// unreferenced object, and once the edits are stashed nothing is left to
/// merge.
pub(super) fn check_out_branch(repo: &Path, branch: &str) -> Result<Refusable<()>, VcsError> {
    git(["checkout", branch, "--"], repo).map(refusable)
}

pub(super) fn set_edits_aside(
    repo: &Path,
    paths: &[String],
    branch: &str,
) -> Result<Refusable<CommitId>, VcsError> {
    let message = format!("{SET_ASIDE}{branch}");
    let mut args = Argv::from([
        "--literal-pathspecs",
        "stash",
        "push",
        "-m",
        message.as_str(),
        "--",
    ]);
    for path in paths {
        args = args.text(path);
    }
    match refusable(git(args, repo)?) {
        Refusable::Refused(account) => return Ok(Refusable::Refused(account)),
        Refusable::Done(()) => {}
    }
    let kept = text(succeeded(
        ["rev-parse", "--verify", "-q", "refs/stash"],
        repo,
    )?);
    CommitId::parse(&kept)
        .map(Refusable::Done)
        .map_err(|_| VcsError::Io {
            ctx: "reading the stash just made".to_owned(),
            source: std::io::Error::other(kept),
        })
}

pub(super) fn set_aside_from(
    repo: &Path,
    at: &CommitId,
    branch: &str,
) -> Result<Vec<CommitId>, VcsError> {
    let listing = git(
        [
            "log",
            "-g",
            "--no-show-signature",
            "--format=%H %P %gs",
            "refs/stash",
        ],
        repo,
    )?;
    if !listing.status.success() {
        return Ok(Vec::new());
    }
    let ours = format!(": {SET_ASIDE}{branch}");
    let mut kept: Vec<CommitId> = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter_map(|record| {
            let mut fields = record.splitn(3, ' ');
            let (kept, base, subject) = (fields.next()?, fields.next()?, fields.next()?);
            (base == at.as_str() && subject.ends_with(&ours))
                .then(|| CommitId::parse(kept).ok())
                .flatten()
        })
        .collect();
    kept.reverse();
    Ok(kept)
}

pub(super) fn staged_against_head(repo: &Path) -> Result<Vec<String>, VcsError> {
    Ok(fields(&succeeded(
        [
            "diff",
            "--cached",
            "--name-only",
            "--no-renames",
            "--ignore-submodules=all",
            "-z",
            "HEAD",
            "--",
        ],
        repo,
    )?)
    .collect())
}

pub(super) fn index_lock(repo: &Path) -> Result<Option<PathBuf>, VcsError> {
    let lock = PathBuf::from(text(succeeded(
        [
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "index.lock",
        ],
        repo,
    )?));
    Ok(lock.exists().then_some(lock))
}

/// The paths of `paths` git does not compare with what the index records:
/// ignored files the index lacks, and entries marked assume-unchanged or
/// skip-worktree.
pub(super) fn uncompared(repo: &Path, paths: &[String]) -> Result<UncomparedPaths, VcsError> {
    let mut flagged = Argv::from(["--literal-pathspecs", "ls-files", "-v", "-z", "--"]);
    let mut ignored = Argv::from([
        "--literal-pathspecs",
        "ls-files",
        "-z",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--",
    ]);
    for path in paths {
        flagged = flagged.text(path);
        ignored = ignored.text(path);
    }
    let mut assumed = Vec::new();
    let mut skipped = Vec::new();
    for record in fields(&succeeded(flagged, repo)?) {
        let Some((tag, path)) = record.split_once(' ') else {
            continue;
        };
        if !paths.iter().any(|p| p == path) {
            continue;
        }
        if tag.chars().any(|c| c.is_ascii_lowercase()) {
            assumed.push(path.to_owned());
        }
        if tag.eq_ignore_ascii_case("s") {
            skipped.push(path.to_owned());
        }
    }
    let ignored: Vec<String> = fields(&succeeded(ignored, repo)?)
        .filter(|path| paths.contains(path))
        .collect();
    Ok(UncomparedPaths {
        ignored,
        assumed,
        skipped,
    })
}
