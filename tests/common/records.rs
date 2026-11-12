//! Commits on the records branch made the way a hand edit makes them: with
//! git's plumbing, no working tree, and no rwv.

use std::io::Write as _;
use std::path::Path;
use std::process::Stdio;

/// The records branch's name.
pub const BRANCH: &str = "rwv-records";

/// The file holding `branch`'s entry: its name with `%` and `/` escaped.
pub fn path(branch: &str) -> String {
    branch.replace('%', "%25").replace('/', "%2F")
}

fn git_stdin(dir: &Path, args: &[&str], input: &[u8]) -> String {
    let mut command = super::git();
    command
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Hand")
        .env("GIT_AUTHOR_EMAIL", "hand@example.invalid")
        .env("GIT_COMMITTER_NAME", "Hand")
        .env("GIT_COMMITTER_EMAIL", "hand@example.invalid")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A commit on `parent` (none: an orphan) whose tree is `parent`'s without
/// `removing` and with `files` laid over it, made with `message`. Returns its
/// id; the caller moves a ref to it.
pub fn commit(
    dir: &Path,
    parent: Option<&str>,
    files: &[(&str, &str)],
    removing: &[&str],
    message: &str,
) -> String {
    let mut listing = String::new();
    if let Some(parent) = parent {
        let existing = super::git_in(dir, &["ls-tree", "-r", parent]);
        for line in existing.lines() {
            let (meta, name) = line.split_once('\t').unwrap();
            let replaced = files.iter().any(|(file, _)| *file == name);
            if !replaced && !removing.contains(&name) {
                let oid = meta.split(' ').nth(2).unwrap();
                listing.push_str(&format!("100644 blob {oid}\t{name}\n"));
            }
        }
    }
    for (name, content) in files {
        let oid = git_stdin(dir, &["hash-object", "-w", "--stdin"], content.as_bytes());
        listing.push_str(&format!("100644 blob {oid}\t{name}\n"));
    }
    let tree = git_stdin(dir, &["mktree"], listing.as_bytes());
    let mut args = vec!["commit-tree", tree.as_str(), "-m", message];
    if let Some(parent) = parent {
        args.extend(["-p", parent]);
    }
    super::git_in(dir, &args)
}

/// Move the local records branch of the repo at `dir` to a commit holding
/// `files` over its tip and without `removing`.
pub fn edit_by_hand(dir: &Path, files: &[(&str, &str)], removing: &[&str], message: &str) {
    let refname = format!("refs/heads/{BRANCH}");
    let tip = super::git_in(dir, &["rev-parse", &refname]);
    let hand = commit(dir, Some(&tip), files, removing, message);
    super::git_in(dir, &["update-ref", &refname, &hand]);
}
