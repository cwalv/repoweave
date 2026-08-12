//! The two halves of "no path outside `crate::git` encodes git's on-disk
//! layout or git argv", pinned where a gate cannot reach.
//!
//! The argv half is closed by the compiler: `git_command` is private to
//! `src/git.rs`, so a frame elsewhere that tries to assemble git argv is
//! `error[E0603]` and never reaches a test. What the compiler cannot notice
//! is someone *widening* the declaration back to `pub(crate)` to add one more
//! call site — the tree would build, and the prohibition would be prose again.
//! The first test below is that tripwire.
//!
//! The layout half is closed by the vcs-seam check in
//! `src/bin/generate-explain.rs`, which has its own seeded-failure tests. What
//! it cannot see is whether the derivation those call sites now share is
//! *right*. The second test is the case that motivated sharing it, and it is
//! the one a plausible refactor silently breaks.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// The argv half: `git_command` stays private to the seam
// ---------------------------------------------------------------------------

#[test]
fn git_command_is_private_to_the_seam() {
    let git_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/git.rs");
    let text = std::fs::read_to_string(&git_rs).expect("readable src/git.rs");

    let declarations: Vec<&str> = text
        .lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("fn git_command") || line.contains("fn git_command("))
        .filter(|line| !line.starts_with("//"))
        .collect();

    // Vacuity guard. A rename or a move leaves the visibility assertion below
    // matching nothing at all, which is green and worthless — it would report
    // a seam that had stopped existing as a seam in good health.
    assert_eq!(
        declarations.len(),
        1,
        "expected exactly one `git_command` declaration in src/git.rs; if it \
         was renamed or moved, update this test rather than deleting it — the \
         assertion below is vacuous without it. Found:\n{}",
        declarations.join("\n")
    );

    assert!(
        declarations[0].starts_with("fn git_command"),
        "src/git.rs declares `git_command` as `{}`; it must stay private to \
         this module, because that privacy is what makes a git argv assembled \
         anywhere else a compile error instead of a convention. A caller that \
         needs git behaviour takes a `&dyn Vcs`, or its command moves into \
         src/git.rs beside the others.",
        declarations[0]
    );
}

// ---------------------------------------------------------------------------
// The layout half: the shared derivation still detects a deleted store
// ---------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A canonical clone with one commit, plus a linked worktree beside it.
fn canonical_with_worktree(root: &Path) -> (PathBuf, PathBuf) {
    let canonical = root.join("canonical");
    std::fs::create_dir_all(&canonical).unwrap();
    git(&canonical, &["init", "-q"]);
    git(&canonical, &["config", "user.email", "t@example.invalid"]);
    git(&canonical, &["config", "user.name", "t"]);
    std::fs::write(canonical.join("f.txt"), "x\n").unwrap();
    git(&canonical, &["add", "f.txt"]);
    git(&canonical, &["commit", "-qm", "init"]);

    let worktree = root.join("linked");
    git(
        &canonical,
        &["worktree", "add", "-q", worktree.to_str().unwrap()],
    );
    (canonical, worktree)
}

/// A live worktree reports nothing missing.
///
/// The control for the test below: without it, a derivation that returned
/// `None` unconditionally would pass that one and look like a working guard.
#[test]
fn a_worktree_whose_canonical_is_present_reports_nothing() {
    let tmp = common::tempdir().expect("tempdir");
    let (_canonical, worktree) = canonical_with_worktree(tmp.path());

    assert_eq!(
        repoweave::check::worktree_canonical_clone_missing(&worktree),
        None,
        "a worktree whose canonical clone is present is not a finding"
    );
}

/// The canonical clone is deleted out of band; the worktree still names it.
///
/// This is the case the shared derivation must not lose. `commondir` lives
/// *inside* the canonical clone, so deleting the clone deletes git's own
/// record of where the store was — and a resolution that only follows
/// `commondir` answers `None` here, which reads as "healthy" and silently
/// retires the finding. The fallback to the `<store>/worktrees/<name>` layout
/// is what keeps the answer available, and it is only reachable in exactly
/// this situation.
#[test]
fn a_worktree_whose_canonical_was_deleted_names_the_missing_clone() {
    let tmp = common::tempdir().expect("tempdir");
    let (canonical, worktree) = canonical_with_worktree(tmp.path());

    std::fs::remove_dir_all(&canonical).expect("delete the canonical clone");
    assert!(
        !canonical.exists(),
        "the canonical clone must really be gone for this to test anything"
    );

    let reported = repoweave::check::worktree_canonical_clone_missing(&worktree)
        .expect("a worktree pointing at a deleted canonical clone is a finding");
    assert_eq!(
        reported, canonical,
        "the finding must name the clone directory, which is what doctor \
         offers to repair — not its store"
    );
}

/// The canonical clone itself is not a linked worktree.
#[test]
fn a_canonical_clone_is_not_a_worktree_with_a_missing_canonical() {
    let tmp = common::tempdir().expect("tempdir");
    let (canonical, _worktree) = canonical_with_worktree(tmp.path());

    assert_eq!(
        repoweave::check::worktree_canonical_clone_missing(&canonical),
        None,
        "a clone that owns its store has no canonical to be missing"
    );
}
