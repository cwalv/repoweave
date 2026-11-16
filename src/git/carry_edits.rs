//! Edits standing over a branch that moved under the checkout, handed to
//! git's own machinery: HEAD is pointed at the tip the files hold, which
//! leaves exactly the edits differing from it, then `stash apply` merges them
//! onto the branch after a plain checkout, or `stash push` keeps them before
//! one.

use super::{git_command, Argv, DERIVED_CONTENT_DRIVER_NAME};
use crate::path_spelling::printed_path;
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
    let taken_out = taken_out(repo, &at, branch)?;
    Ok(Some(SetBack {
        at,
        branch: branch.to_owned(),
        taken_out,
    }))
}

/// The edits `branch`'s taken-out slot keeps over `at`; a slot kept over
/// another commit is from a merge that finished, or that the operator moved
/// the checkout off, and is left to `rwv backup list`.
fn taken_out(repo: &Path, at: &CommitId, branch: &str) -> Result<Option<CommitId>, VcsError> {
    let slot = super::backup::taken_out_slot(branch);
    let output = git(["rev-parse", &format!("{slot}^1"), &slot], repo)?;
    if !output.status.success() {
        return Ok(None);
    }
    let listed = text(output.stdout);
    let mut ids = listed.lines().map(CommitId::parse);
    match (ids.next(), ids.next()) {
        (Some(Ok(base)), Some(Ok(kept))) if &base == at => Ok(Some(kept)),
        _ => Ok(None),
    }
}

/// The paths whose edits `kept`, a commit `git stash create` made over `at`,
/// holds: no path is added, since a staged change refuses the merge.
pub(super) fn taken_out_paths(
    repo: &Path,
    at: &CommitId,
    kept: &CommitId,
) -> Result<Vec<String>, VcsError> {
    Ok(fields(&succeeded(
        [
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            at.as_str(),
            kept.as_str(),
            "--",
        ],
        repo,
    )?)
    .collect())
}

/// `git restore` of `paths` from `source` into the working copy alone, with
/// no hook run: it is no checkout the operator asked for, and a
/// post-checkout hook writing a file would stop the merge of the edits.
fn restore_from(repo: &Path, source: &str, paths: &[String]) -> Result<Output, VcsError> {
    let scratch = super::Scratch::take(repo)?;
    let no_hooks = format!("core.hooksPath={}", scratch.path("no-hooks"));
    let mut restore = Argv::from([
        "-c",
        no_hooks.as_str(),
        "--literal-pathspecs",
        "restore",
        "--worktree",
        "--source",
        source,
        "--",
    ]);
    for path in paths {
        restore = restore.text(path);
    }
    git(restore, repo)
}

fn drop_taken_out(repo: &Path, branch: &str, kept: &CommitId) -> Result<(), VcsError> {
    let slot = super::backup::taken_out_slot(branch);
    succeeded(["update-ref", "-d", slot.as_str(), kept.as_str()], repo).map(drop)
}

/// Put back into a set-back checkout the edits a stopped merge had out of
/// it, wherever it stopped, from its taken-out slot: the files the edits or
/// the merge touched take the edits' content, and the index HEAD's.
pub(super) fn put_back_taken_out(repo: &Path, back: &SetBack) -> Result<(), VcsError> {
    let Some(kept) = &back.taken_out else {
        return Ok(());
    };
    let mut paths = taken_out_paths(repo, &back.at, kept)?;
    for path in fields(&succeeded(
        [
            "diff",
            "--cached",
            "--name-only",
            "--no-renames",
            "-z",
            "HEAD",
            "--",
        ],
        repo,
    )?) {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    if !paths.is_empty() {
        let restored = restore_from(repo, kept.as_str(), &paths)?;
        if !restored.status.success() {
            return Err(VcsError::CommandFailed {
                args: vec![
                    "restore".to_owned(),
                    "--source".to_owned(),
                    kept.to_string(),
                ],
                repo: repo.to_path_buf(),
                stderr: String::from_utf8_lossy(&restored.stderr).into_owned(),
            });
        }
    }
    succeeded(["read-tree", "HEAD"], repo)?;
    drop_taken_out(repo, &back.branch, kept)
}

/// The paths among `paths`, which `kept` changes over HEAD, where taking the
/// edits out would remove what `kept` does not hold: something standing
/// where a path it deletes was, or a file where a directory of its paths
/// is.
fn in_the_way(repo: &Path, kept: &CommitId, paths: &[String]) -> Result<Vec<String>, VcsError> {
    let mut held = Argv::from([
        "--literal-pathspecs",
        "ls-tree",
        "--name-only",
        "-z",
        kept.as_str(),
        "--",
    ]);
    for path in paths {
        held = held.text(path);
    }
    let held: Vec<String> = fields(&succeeded(held, repo)?).collect();
    let mut blocked = Vec::new();
    for path in paths {
        let deleted = !held.contains(path);
        let stands = std::fs::symlink_metadata(repo.join(path)).is_ok();
        let mut ancestor = Path::new(path).parent();
        let mut over_a_dir = false;
        while let Some(dir) = ancestor.filter(|d| !d.as_os_str().is_empty()) {
            if std::fs::symlink_metadata(repo.join(dir)).is_ok_and(|m| !m.is_dir()) {
                over_a_dir = true;
            }
            ancestor = dir.parent();
        }
        if (deleted && stands) || over_a_dir {
            blocked.push(path.clone());
        }
    }
    Ok(blocked)
}

/// Not `checkout -m`: from git 2.55 it no longer refuses staged changes, and
/// merges them into the working tree, losing the staged version; and it
/// leaves a conflicted merge's edits as a `refs/stash` entry, which every
/// worktree of the clone shares.
///
/// HEAD stays where the files hold until the last step, and the edits stay
/// in the taken-out slot until they stand in the working copy again, so a
/// stop anywhere leaves the checkout set back with the slot naming them.
pub(super) fn carry_edits_onto(repo: &Path, branch: &str) -> Result<Refusable<()>, VcsError> {
    let staged = staged_against_head(repo, "--ita-visible-in-index")?;
    if !staged.is_empty() {
        return Ok(Refusable::Refused(format!(
            "cannot continue with staged changes in the following files:\n{}",
            staged.join("\n")
        )));
    }
    if let Refusable::Refused(said) = refusable(git(["add", "--refresh", "--", "."], repo)?) {
        return Ok(Refusable::Refused(said));
    }
    let target = format!("refs/heads/{branch}");
    let tip = git(
        [
            "rev-parse",
            "--verify",
            "-q",
            &format!("{target}^{{commit}}"),
        ],
        repo,
    )?;
    if !tip.status.success() {
        return Ok(Refusable::Refused(format!(
            "branch {branch} no longer exists"
        )));
    }
    let tip = text(tip.stdout);
    let here = crate::path_spelling::CanonicalPath::of(repo);
    let elsewhere = super::GitVcs
        .checkouts_of_branch(repo, &crate::vcs::RawRefName::new(branch.to_owned()))?
        .into_iter()
        .find(|other| crate::path_spelling::CanonicalPath::of(other) != here);
    if let Some(other) = elsewhere {
        return Ok(Refusable::Refused(format!(
            "branch {branch} is checked out at {}",
            other.display()
        )));
    }
    let created = git(["stash", "create"], repo)?;
    if !created.status.success() {
        return Ok(refusable(created));
    }
    let created = text(created.stdout);
    if created.is_empty() {
        return check_out_branch(repo, branch);
    }
    let kept = CommitId::parse(&created).map_err(|_| VcsError::Io {
        ctx: "reading the commit `git stash create` made".to_owned(),
        source: std::io::Error::other(created.clone()),
    })?;
    let at = text(succeeded(["rev-parse", "--verify", "-q", "HEAD"], repo)?);
    let paths = taken_out_paths(
        repo,
        &CommitId::parse(&at).map_err(|_| VcsError::Io {
            ctx: "reading HEAD".to_owned(),
            source: std::io::Error::other(at.clone()),
        })?,
        &kept,
    )?;
    let blocked = in_the_way(repo, &kept, &paths)?;
    if !blocked.is_empty() {
        return Ok(Refusable::Refused(format!(
            "something not tracked stands where an edit removed a tracked path, and taking the \
             edits out to merge them would remove it:\n{}",
            blocked.join("\n")
        )));
    }
    let slot = super::backup::taken_out_slot(branch);
    succeeded(
        [
            "update-ref",
            "--create-reflog",
            "-m",
            "rwv doctor --fix --merge-edited-drift",
            slot.as_str(),
            kept.as_str(),
        ],
        repo,
    )?;
    crate::test_pause::point("drift-merge-edits-kept");
    let taken = restore_from(repo, "HEAD", &paths)?;
    if !taken.status.success() {
        let back = restore_from(repo, kept.as_str(), &paths)?;
        if back.status.success() {
            drop_taken_out(repo, branch, &kept)?;
        }
        return Ok(refusable(taken));
    }
    crate::test_pause::point("drift-merge-taken-out");
    succeeded(["update-index", "-q", "--refresh"], repo)?;
    if let Refusable::Refused(said) =
        refusable(git(["read-tree", "-m", "-u", "HEAD", tip.as_str()], repo)?)
    {
        let back = restore_from(repo, kept.as_str(), &paths)?;
        if back.status.success() {
            drop_taken_out(repo, branch, &kept)?;
        }
        return Ok(Refusable::Refused(said));
    }
    crate::test_pause::point("drift-merge-switched");
    let derived = format!("merge.{DERIVED_CONTENT_DRIVER_NAME}.driver=true");
    let applied = git(
        [
            "-c",
            derived.as_str(),
            "-c",
            "rerere.enabled=false",
            "stash",
            "apply",
            kept.as_str(),
        ],
        repo,
    )?;
    let conflicted = super::GitVcs::unmerged(repo)?;
    if !applied.status.success() && conflicted.is_empty() {
        return Err(VcsError::CommandFailed {
            args: vec!["stash".to_owned(), "apply".to_owned(), kept.to_string()],
            repo: repo.to_path_buf(),
            stderr: String::from_utf8_lossy(&applied.stderr).into_owned(),
        });
    }
    let merged: Vec<String> = fields(&succeeded(
        [
            "diff",
            "--cached",
            "--name-only",
            "--no-renames",
            "-z",
            tip.as_str(),
            "--",
        ],
        repo,
    )?)
    .filter(|path| !conflicted.contains(path))
    .collect();
    if !merged.is_empty() {
        let mut unstage = Argv::from(["--literal-pathspecs", "reset", "-q", tip.as_str(), "--"]);
        for path in &merged {
            unstage = unstage.text(path);
        }
        succeeded(unstage, repo)?;
    }
    let moved = format!("rwv: merged the edits onto {branch}");
    succeeded(
        [
            "symbolic-ref",
            "-m",
            moved.as_str(),
            "HEAD",
            target.as_str(),
        ],
        repo,
    )?;
    drop_taken_out(repo, branch, &kept)?;
    Ok(Refusable::Done(()))
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

pub(super) fn staged_against_head(repo: &Path, ita: &str) -> Result<Vec<String>, VcsError> {
    Ok(fields(&succeeded(
        [
            "diff",
            "--cached",
            ita,
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
    let lock = printed_path(text(succeeded(
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
