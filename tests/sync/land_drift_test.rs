//! What `rwv land` does with a member checkout that differs from its HEAD.
//!
//! A checkout whose branch was moved forward by something outside it holds
//! the old tip's files and index entries: that is drift, and the landing
//! refreshes it file by file, reporting each file. Any other difference is an
//! uncommitted edit, and the landing refuses it as `edits` dirty, leaving it
//! exactly as it was. The five edit shapes below each hold only content some
//! commit already has, which is what makes them look like drift to a check
//! that reads content alone.
//!
//! The fixture is a primary weave and a workweave whose member and project
//! checkouts are worktrees of the primary's. `lib`'s history is `init`
//! (README.md), `one` (notes.txt "one") and `two` (notes.txt "two"), which both
//! workspaces start at; the workweave then commits `feature.txt`, so the
//! landing moves `lib` in the primary.

use repoweave::diagnosis::State;
use std::path::{Path, PathBuf};

use crate::common;

const LIB: &str = "github/acme/lib";
const PROJECT: &str = "app";

struct Fixture {
    _tmp: tempfile::TempDir,
    primary: PathBuf,
    primary_lib: PathBuf,
    primary_project: PathBuf,
    ww: PathBuf,
    ww_lib: PathBuf,
    scratch: PathBuf,
}

fn commit_file(repo: &Path, file: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(file), content).unwrap();
    common::git_in(repo, &["add", file]);
    common::git_in(repo, &["commit", "-q", "-m", msg]);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-q", "-b", "main"]);
    common::git_in(path, &["config", "user.email", "test@test.com"]);
    common::git_in(path, &["config", "user.name", "Test"]);
    common::git_in(path, &["config", "core.autocrlf", "false"]);
}

fn fixture() -> Fixture {
    let tmp = common::tempdir().unwrap();
    let primary = tmp.path().join("primary");
    let primary_lib = primary.join(LIB);
    init_repo(&primary_lib);
    commit_file(&primary_lib, "README.md", "init\n", "init");
    commit_file(&primary_lib, "notes.txt", "one\n", "one");
    commit_file(&primary_lib, "spare.txt", "two\n", "spare");
    let two = commit_file(&primary_lib, "notes.txt", "two\n", "two");

    let primary_project = primary.join("projects").join(PROJECT);
    init_repo(&primary_project);
    common::set_up_lock_driver(&primary_project);
    let url = common::file_url(&primary_lib);
    std::fs::write(
        primary_project.join("rwv.toml"),
        format!(
            "[repositories.\"{LIB}\"]\ntype = \"git\"\nurl = \"{url}\"\nbranch = \"main\"\n\
             role = \"owned\"\n"
        ),
    )
    .unwrap();
    common::fixture_lock(&primary_project, &[(LIB, &url, &two)]);
    common::git_in(
        &primary_project,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&primary_project, &["commit", "-q", "-m", "lock: initial"]);
    common::record_trunk(&primary_project);
    std::fs::write(primary.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    let ww = tmp.path().join("ww");
    std::fs::create_dir_all(ww.join("github/acme")).unwrap();
    std::fs::create_dir_all(ww.join("projects")).unwrap();
    let ww_lib = ww.join(LIB);
    common::git_in(
        &primary_lib,
        &[
            "worktree",
            "add",
            "-q",
            ww_lib.to_str().unwrap(),
            "-b",
            "ww/project",
        ],
    );
    let ww_project = ww.join("projects").join(PROJECT);
    common::git_in(
        &primary_project,
        &[
            "worktree",
            "add",
            "-q",
            ww_project.to_str().unwrap(),
            "-b",
            "ww/project",
        ],
    );
    common::record_feature(&primary_project, "ww/project", "main");
    std::fs::write(ww.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    commit_file(&ww_lib, "feature.txt", "feature\n", "feature");

    let scratch = tmp.path().join("scratch");
    Fixture {
        _tmp: tmp,
        primary,
        primary_lib,
        primary_project,
        ww,
        ww_lib,
        scratch,
    }
}

impl Fixture {
    /// Moves `branch` forward by one commit writing `file`, from a checkout of
    /// its own: neither workspace's checkout takes part, as when another
    /// worktree of the same store moves a branch one of them has checked out.
    fn move_from_elsewhere(&self, branch: &str, file: &str, content: &str) -> String {
        let elsewhere = self.scratch.join(branch.replace('/', "-"));
        common::git_in(
            &self.primary_lib,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                elsewhere.to_str().unwrap(),
                branch,
            ],
        );
        let moved = commit_file(&elsewhere, file, content, "moved elsewhere");
        common::git_in(
            &elsewhere,
            &["update-ref", &format!("refs/heads/{branch}"), &moved],
        );
        common::git_in(
            &self.primary_lib,
            &["worktree", "remove", elsewhere.to_str().unwrap()],
        );
        moved
    }

    /// Moves `branch` forward by one commit that `change` makes in a checkout
    /// of its own, as [`Fixture::move_from_elsewhere`] does.
    fn move_with(&self, branch: &str, change: impl FnOnce(&Path)) -> String {
        let elsewhere = self.scratch.join(branch.replace('/', "-"));
        common::git_in(
            &self.primary_lib,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                elsewhere.to_str().unwrap(),
                branch,
            ],
        );
        change(&elsewhere);
        common::git_in(&elsewhere, &["add", "-A"]);
        common::git_in(&elsewhere, &["commit", "-q", "-m", "moved elsewhere"]);
        let moved = common::git_in(&elsewhere, &["rev-parse", "HEAD"]);
        common::git_in(
            &elsewhere,
            &["update-ref", &format!("refs/heads/{branch}"), &moved],
        );
        common::git_in(
            &self.primary_lib,
            &["worktree", "remove", elsewhere.to_str().unwrap()],
        );
        moved
    }

    /// Records `sha` as the primary's pin for `lib`, as the operator who
    /// moved `main` would before landing onto it.
    fn pin_primary(&self, sha: &str) {
        let url = common::file_url(&self.primary_lib);
        common::fixture_lock(&self.primary_project, &[(LIB, &url, sha)]);
        common::git_in(
            &self.primary_project,
            &["commit", "-q", "-am", "lock: moved"],
        );
    }
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Everything about a checkout an edit could live in: what git reports
/// against HEAD and the index, and each file's bytes and mode.
fn snapshot(repo: &Path) -> String {
    let mut out = String::new();
    for args in [
        &["status", "--porcelain=v1", "--untracked-files=all"][..],
        &["diff", "--cached", "--raw"][..],
        &["diff", "--raw"][..],
        &["rev-parse", "HEAD"][..],
    ] {
        out.push_str(&common::git_in(repo, args));
        out.push('\n');
    }
    for file in ["README.md", "notes.txt", "feature.txt"] {
        let path = repo.join(file);
        out.push_str(&format!("{file}: {:?}", read(&path)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).map(|m| m.permissions().mode() & 0o111);
            out.push_str(&format!(" exec {:?}", mode.ok()));
        }
        out.push('\n');
    }
    out
}

/// An uncommitted change whose content some commit already holds.
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// A tracked file deleted.
    Deleted,
    /// A file returned to its content of an earlier commit.
    Reverted,
    /// `git revert --no-commit HEAD`, staged: the index holds the parent's tree.
    StagedRevert,
    /// One tracked file copied over another.
    Copied,
    /// A tracked file made executable.
    #[cfg(unix)]
    ModeChanged,
}

impl Shape {
    fn apply(self, repo: &Path) {
        match self {
            Shape::Deleted => std::fs::remove_file(repo.join("README.md")).unwrap(),
            Shape::Reverted => std::fs::write(repo.join("notes.txt"), "one\n").unwrap(),
            Shape::StagedRevert => {
                common::git_in(repo, &["revert", "--no-commit", "HEAD"]);
            }
            Shape::Copied => {
                std::fs::copy(repo.join("README.md"), repo.join("notes.txt")).unwrap();
            }
            #[cfg(unix)]
            Shape::ModeChanged => {
                use std::os::unix::fs::PermissionsExt;
                let path = repo.join("notes.txt");
                let mut perms = std::fs::metadata(&path).unwrap().permissions();
                perms.set_mode(perms.mode() | 0o111);
                std::fs::set_permissions(&path, perms).unwrap();
            }
        }
        assert!(
            !common::git_in(repo, &["status", "--porcelain", "--untracked-files=no"]).is_empty(),
            "{self:?} left {} clean, so this tests nothing",
            repo.display()
        );
    }
}

/// The landing refuses `checkout`'s edit as uncommitted work and leaves the
/// checkout, and the primary's `lib`, exactly as they were.
fn refuses_and_keeps(f: &Fixture, shape: Shape, checkout: &Path) {
    shape.apply(checkout);
    let before = snapshot(checkout);
    let primary_lib_head = common::git_in(&f.primary_lib, &["rev-parse", "HEAD"]);

    let out = common::cli::run(&["land"], &f.ww);

    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if stderr.contains("edits mid-operation") {
        common::cli::refused(&out, State::EditsMidOperation);
    } else {
        common::cli::refused(&out, State::EditsDirty);
    }
    assert!(
        !stderr.contains("refreshed"),
        "{shape:?} is an edit, and nothing of it may be refreshed:\n{stderr}"
    );
    assert_eq!(
        snapshot(checkout),
        before,
        "{shape:?}: the refused landing changed the checkout holding the edit:\n{stderr}"
    );
    assert_eq!(
        common::git_in(&f.primary_lib, &["rev-parse", "HEAD"]),
        primary_lib_head,
        "a refused landing moves nothing"
    );
}

/// `rwv land` in the primary, on `main`, which lands into nothing: the
/// landing refuses before it claims any workspace, judging the source's
/// edits beside that refusal, and writes nothing to the checkout.
fn refuses_early_and_keeps(f: &Fixture, shape: Shape) {
    shape.apply(&f.primary_lib);
    let before = snapshot(&f.primary_lib);

    let out = common::cli::run(&["land"], &f.primary);

    let stderr = common::cli::refused(&out, State::TargetNoTarget);
    assert!(
        stderr.contains("edits dirty") || stderr.contains("edits mid-operation"),
        "{shape:?} is judged an edit beside the refusal:\n{stderr}"
    );
    assert!(!stderr.contains("refreshed"), "{stderr}");
    assert_eq!(
        snapshot(&f.primary_lib),
        before,
        "{shape:?}: a landing refused before its claim changed the checkout:\n{stderr}"
    );
}

macro_rules! edit_shapes {
    ($($source:ident, $target:ident, $early:ident: $shape:expr;)*) => {$(
        #[test]
        fn $source() {
            let f = fixture();
            refuses_and_keeps(&f, $shape, &f.ww_lib.clone());
        }

        #[test]
        fn $target() {
            let f = fixture();
            refuses_and_keeps(&f, $shape, &f.primary_lib.clone());
        }

        #[test]
        fn $early() {
            refuses_early_and_keeps(&fixture(), $shape);
        }
    )*};
}

edit_shapes! {
    a_deleted_file_in_the_source_is_an_edit,
    a_deleted_file_in_the_target_is_an_edit,
    a_deleted_file_survives_a_landing_refused_early: Shape::Deleted;
    a_file_returned_to_an_earlier_version_in_the_source_is_an_edit,
    a_file_returned_to_an_earlier_version_in_the_target_is_an_edit,
    a_file_returned_to_an_earlier_version_survives_a_landing_refused_early: Shape::Reverted;
    a_staged_revert_in_the_source_is_an_edit,
    a_staged_revert_in_the_target_is_an_edit,
    a_staged_revert_survives_a_landing_refused_early: Shape::StagedRevert;
    a_copied_file_in_the_source_is_an_edit,
    a_copied_file_in_the_target_is_an_edit,
    a_copied_file_survives_a_landing_refused_early: Shape::Copied;
}

#[cfg(unix)]
edit_shapes! {
    a_mode_change_in_the_source_is_an_edit,
    a_mode_change_in_the_target_is_an_edit,
    a_mode_change_survives_a_landing_refused_early: Shape::ModeChanged;
}

/// Drift too is left alone by a landing refused before its claim: it is
/// judged as the dirt it looks like, and refreshed only by a landing that
/// goes ahead.
#[test]
fn drift_survives_a_landing_refused_early() {
    let f = fixture();
    f.move_from_elsewhere("main", "notes.txt", "three\n");
    let before = snapshot(&f.primary_lib);

    let out = common::cli::run(&["land"], &f.primary);

    let stderr = common::cli::refused(&out, State::TargetNoTarget);
    assert!(!stderr.contains("refreshed"), "{stderr}");
    assert_eq!(snapshot(&f.primary_lib), before, "{stderr}");
}

/// The control the edit shapes stand on: with no edit the fixture lands.
#[test]
fn a_clean_workweave_lands() {
    let f = fixture();
    common::cli::succeed(&["land"], &f.ww);
    assert_eq!(
        read(&f.primary_lib.join("feature.txt")).as_deref(),
        Some("feature\n")
    );
}

/// The source's branch moved under its checkout, changing a file it already
/// had: the stale file is refreshed, reported, and the landing carries the
/// moved content.
#[test]
fn drift_changing_a_file_in_the_source_is_refreshed() {
    let f = fixture();
    let moved = f.move_from_elsewhere("ww/project", "notes.txt", "three\n");
    assert_eq!(
        common::git_in(&f.ww_lib, &["diff", "--cached", "--name-status"]),
        "M\tnotes.txt",
        "the drift must show a changed file, so the content path is what is tested"
    );

    let out = common::cli::succeed(&["land"], &f.ww);

    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("refreshed drift") && stderr.contains("notes.txt"),
        "the refresh is reported with its file:\n{stderr}"
    );
    assert_eq!(
        read(&f.ww_lib.join("notes.txt")).as_deref(),
        Some("three\n")
    );
    assert_eq!(
        common::git_in(&f.ww_lib, &["status", "--porcelain"]),
        "",
        "the refresh leaves the source's checkout clean"
    );
    assert_eq!(
        common::git_in(&f.primary_lib, &["rev-parse", "HEAD"]),
        moved
    );
    assert_eq!(
        read(&f.primary_lib.join("notes.txt")).as_deref(),
        Some("three\n")
    );
}

/// The target's branch moved under the primary's checkout: the landing
/// refreshes that checkout and fast-forwards it, where a dirt check alone
/// would refuse it as edits.
#[test]
fn drift_in_the_target_is_refreshed_and_the_landing_proceeds() {
    let f = fixture();
    let moved = f.move_from_elsewhere("main", "notes.txt", "three\n");
    f.pin_primary(&moved);
    assert_eq!(
        common::git_in(&f.primary_lib, &["diff", "--cached", "--name-status"]),
        "M\tnotes.txt"
    );

    let out = common::cli::succeed(&["land", "--history", "linear"], &f.ww);

    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("refreshed drift") && stderr.contains("notes.txt"),
        "the target's refresh is reported with its file:\n{stderr}"
    );
    assert_eq!(
        read(&f.primary_lib.join("notes.txt")).as_deref(),
        Some("three\n")
    );
    assert_eq!(
        read(&f.primary_lib.join("feature.txt")).as_deref(),
        Some("feature\n")
    );
    assert_eq!(
        common::git_in(&f.primary_lib, &["status", "--porcelain"]),
        ""
    );
}

/// Drift in one file beside an edit in a file the move did not touch: the
/// edit alone is refused, and the drifted file is refreshed so that the
/// commit the refusal offers cannot commit the move's reverse.
#[test]
fn drift_beside_an_edit_refuses_the_edit_alone() {
    let f = fixture();
    std::fs::write(f.ww_lib.join("README.md"), "edited\n").unwrap();
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");

    let out = common::cli::run(&["land"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDirty);
    let listing: Vec<&str> = stderr
        .lines()
        .skip_while(|l| !l.ends_with("edits dirty"))
        .take_while(|l| !l.trim().is_empty())
        .collect();
    assert!(
        listing.iter().any(|l| l.contains("README.md"))
            && !listing.iter().any(|l| l.contains("notes.txt")),
        "the edits listing names the edit and not the drift: {listing:?}\n{stderr}"
    );
    assert!(
        stderr.contains("refreshed drift") && stderr.contains("notes.txt"),
        "{stderr}"
    );
    assert_eq!(
        read(&f.ww_lib.join("README.md")).as_deref(),
        Some("edited\n")
    );
    assert_eq!(
        read(&f.ww_lib.join("notes.txt")).as_deref(),
        Some("three\n")
    );
    assert_eq!(
        common::git_in(&f.ww_lib, &["status", "--porcelain"]),
        "M README.md",
        "only the edit is left, unstaged"
    );
}

/// A landing refused on a checkout other than the drifted one changes
/// nothing: the drift waits for a landing that proceeds.
#[test]
fn drift_is_left_alone_when_another_checkout_refuses() {
    let f = fixture();
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");
    let manifest = f.primary_project.join("rwv.toml");
    let mut text = std::fs::read_to_string(&manifest).unwrap();
    text.push_str("# scratch\n");
    std::fs::write(&manifest, text).unwrap();
    let before = snapshot(&f.ww_lib);

    let out = common::cli::run(&["land"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDirty);
    assert!(!stderr.contains("refreshed"), "{stderr}");
    assert_eq!(snapshot(&f.ww_lib), before, "{stderr}");
}

/// `--json` names each refreshed file, with the checkout and the move.
#[test]
fn json_carries_what_was_refreshed() {
    let f = fixture();
    let moved = f.move_from_elsewhere("main", "notes.txt", "three\n");
    f.pin_primary(&moved);

    let out = common::cli::succeed(&["land", "--history", "linear", "--json"], &f.ww);

    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let refreshed: Vec<&serde_json::Value> = json["reports"]
        .as_array()
        .expect("a `reports` array")
        .iter()
        .filter(|r| r["kind"] == "refreshed")
        .collect();
    assert_eq!(refreshed.len(), 1, "{json:#}");
    let entry = refreshed[0];
    assert_eq!(entry["path"], LIB, "{json:#}");
    assert_eq!(
        entry["absolute_path"].as_str().map(PathBuf::from),
        Some(repoweave::path_spelling::resolve(&f.primary_lib).unwrap()),
        "{json:#}"
    );
    assert_eq!(entry["moved_to"], moved.as_str(), "{json:#}");
    assert_eq!(entry["files"], serde_json::json!(["notes.txt"]), "{json:#}");
    assert_eq!(entry["index_entries"], serde_json::json!([]), "{json:#}");
}

// ---------------------------------------------------------------------------
// Edits in a checkout whose branch really moved under it
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Source,
    Target,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The branch moved under the checkout and nothing has refreshed it.
    Drifted,
    /// As `Drifted`, then a refused landing refreshed the checkout.
    Refreshed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Staging {
    Staged,
    Unstaged,
}

/// An edit on the one path a move under the checkout changed. After a
/// refresh each returns that path to what the moved-from tip held, which is
/// exactly what a stale path holds, so only the evidence that the checkout
/// already took the move tells it from drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MovedShape {
    /// The file deleted: after a refresh, the file the move added.
    Deleted,
    /// The file given an earlier commit's content: after a refresh, the
    /// moved-from tip's.
    Reverted,
    /// A commit's change undone: after a refresh, the move's, by `git revert
    /// --no-commit` ended with `--quit` (staged) or a reversed patch (unstaged).
    Revert,
    /// Another tracked file copied over it: after a refresh, one holding the
    /// moved-from tip's content.
    Copied,
    /// Its mode changed: after a refresh, back to the moved-from tip's.
    #[cfg(unix)]
    ModeChanged,
}

#[cfg(unix)]
fn set_exec(path: &Path, exec: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    let mode = if exec {
        perms.mode() | 0o111
    } else {
        perms.mode() & !0o111
    };
    perms.set_mode(mode);
    std::fs::set_permissions(path, perms).unwrap();
}

/// Applies the reverse of `commit`'s change to `repo`, to the index as well
/// when `staging` says so.
fn apply_reversed(repo: &Path, scratch: &Path, commit: &str, staging: Staging) {
    std::fs::create_dir_all(scratch).unwrap();
    let patch = scratch.join("reverse.patch");
    let diff = common::git()
        .args(["show", "--format=", "--binary", commit])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(diff.status.success());
    std::fs::write(&patch, &diff.stdout).unwrap();
    let patch = patch.to_str().unwrap();
    match staging {
        Staging::Staged => common::git_in(repo, &["apply", "-R", "--index", patch]),
        Staging::Unstaged => common::git_in(repo, &["apply", "-R", patch]),
    };
}

impl MovedShape {
    /// The move made from elsewhere, and the path it changes.
    fn make_move(self, phase: Phase, elsewhere: &Path) -> &'static str {
        match (self, phase) {
            (MovedShape::Deleted, Phase::Refreshed) => {
                std::fs::write(elsewhere.join("new.txt"), "new\n").unwrap();
                "new.txt"
            }
            #[cfg(unix)]
            (MovedShape::ModeChanged, Phase::Refreshed) => {
                set_exec(&elsewhere.join("notes.txt"), true);
                "notes.txt"
            }
            _ => {
                std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
                "notes.txt"
            }
        }
    }

    /// Makes the edit in `repo`, returning every path it touches.
    fn apply(
        self,
        phase: Phase,
        staging: Staging,
        repo: &Path,
        scratch: &Path,
    ) -> Vec<&'static str> {
        let stage = |path: &str| {
            if staging == Staging::Staged {
                common::git_in(repo, &["add", "-A", "--", path]);
            }
        };
        let touched = match (self, phase) {
            (MovedShape::Deleted, phase) => {
                let path = match phase {
                    Phase::Refreshed => "new.txt",
                    Phase::Drifted => "notes.txt",
                };
                std::fs::remove_file(repo.join(path)).unwrap();
                stage(path);
                vec![path]
            }
            (MovedShape::Reverted, phase) => {
                let content = match phase {
                    Phase::Refreshed => "two\n",
                    Phase::Drifted => "one\n",
                };
                std::fs::write(repo.join("notes.txt"), content).unwrap();
                stage("notes.txt");
                vec!["notes.txt"]
            }
            (MovedShape::Revert, Phase::Refreshed) => {
                match staging {
                    Staging::Staged => {
                        common::git_in(repo, &["revert", "--no-commit", "HEAD"]);
                        common::git_in(repo, &["revert", "--quit"]);
                    }
                    Staging::Unstaged => apply_reversed(repo, scratch, "HEAD", staging),
                }
                vec!["notes.txt"]
            }
            (MovedShape::Revert, Phase::Drifted) => {
                let root = common::git_in(repo, &["rev-list", "--max-parents=0", "HEAD"]);
                apply_reversed(repo, scratch, &root, staging);
                vec!["README.md"]
            }
            (MovedShape::Copied, phase) => {
                let from = match phase {
                    Phase::Refreshed => "spare.txt",
                    Phase::Drifted => "README.md",
                };
                std::fs::copy(repo.join(from), repo.join("notes.txt")).unwrap();
                stage("notes.txt");
                vec!["notes.txt"]
            }
            #[cfg(unix)]
            (MovedShape::ModeChanged, phase) => {
                set_exec(&repo.join("notes.txt"), phase == Phase::Drifted);
                stage("notes.txt");
                vec!["notes.txt"]
            }
        };
        if staging == Staging::Staged {
            assert!(
                !common::git_in(repo, &["diff", "--cached", "--name-only"]).is_empty(),
                "{self:?} staged nothing in {}",
                repo.display()
            );
        }
        assert!(
            common::git()
                .args(["rev-parse", "-q", "--verify", "REVERT_HEAD"])
                .current_dir(repo)
                .output()
                .unwrap()
                .stdout
                .is_empty(),
            "{self:?} must not leave an operation in progress, or the mid-operation guard \
             answers before the moved-from tip is read"
        );
        touched
    }
}

/// What a path holds in a checkout: its index entry, its file's bytes and,
/// on unix, the file's exec bits.
fn path_state(repo: &Path, paths: &[&str]) -> String {
    let mut out = String::new();
    for path in paths {
        out.push_str(&common::git_in(repo, &["ls-files", "-s", "--", path]));
        out.push_str(&format!(
            "\n{path}: {:?}",
            std::fs::read(repo.join(path)).ok()
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(repo.join(path)).map(|m| m.permissions().mode() & 0o111);
            out.push_str(&format!(" exec {:?}", mode.ok()));
        }
        out.push('\n');
    }
    out
}

/// The paths an `edits dirty` refusal lists.
fn edits_listing(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .skip_while(|l| !l.ends_with("edits dirty"))
        .skip(1)
        .take_while(|l| l.starts_with("    "))
        .map(|l| l.trim().to_owned())
        .collect()
}

/// `checkout`'s branch moves under it from elsewhere; with `Phase::Refreshed`
/// a landing refused on an unrelated edit then refreshes it. The edit made
/// next is refused as `edits dirty` and left exactly as it stands.
fn moved_edit_is_kept(shape: MovedShape, staging: Staging, phase: Phase, side: Side) {
    let f = fixture();
    let (checkout, branch) = match side {
        Side::Source => (f.ww_lib.clone(), "ww/project"),
        Side::Target => (f.primary_lib.clone(), "main"),
    };
    let mut moved_path = "";
    let moved = f.move_with(branch, |elsewhere| {
        moved_path = shape.make_move(phase, elsewhere)
    });
    if side == Side::Target {
        f.pin_primary(&moved);
    }
    assert!(
        common::git_in(&checkout, &["diff", "--cached", "--name-only"])
            .lines()
            .any(|l| l == moved_path),
        "the move left {moved_path} stale, so the checkout really drifted"
    );
    if phase == Phase::Refreshed {
        std::fs::write(checkout.join("README.md"), "edited\n").unwrap();
        let out = common::cli::run(&["land"], &f.ww);
        let stderr = common::cli::refused(&out, State::EditsDirty);
        assert!(
            stderr.contains("refreshed drift") && stderr.contains(moved_path),
            "the first landing refreshes the drift:\n{stderr}"
        );
        std::fs::write(checkout.join("README.md"), "init\n").unwrap();
        assert_eq!(
            common::git_in(&checkout, &["status", "--porcelain"]),
            "",
            "after the refresh the checkout stands at its HEAD"
        );
    }
    let touched = shape.apply(phase, staging, &checkout, &f.scratch);
    let before = path_state(&checkout, &touched);
    let primary_head = common::git_in(&f.primary_lib, &["rev-parse", "HEAD"]);

    let out = common::cli::run(&["land"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDirty);
    let listing = edits_listing(&stderr);
    for path in &touched {
        assert!(
            listing.iter().any(|l| l == path),
            "{shape:?} {staging:?} {phase:?} {side:?}: {path} is listed as an edit: \
             {listing:?}\n{stderr}"
        );
    }
    assert_eq!(
        path_state(&checkout, &touched),
        before,
        "{shape:?} {staging:?} {phase:?} {side:?}: the refused landing replaced the edit:\n{stderr}"
    );
    assert_eq!(
        common::git_in(&f.primary_lib, &["rev-parse", "HEAD"]),
        primary_head
    );
}

macro_rules! moved_edits {
    ($($name:ident: $shape:expr, $staging:expr, $phase:expr, $side:expr;)*) => {$(
        #[test]
        fn $name() {
            moved_edit_is_kept($shape, $staging, $phase, $side);
        }
    )*};
}

moved_edits! {
    moved_deletion_staged_drifted_source: MovedShape::Deleted, Staging::Staged, Phase::Drifted, Side::Source;
    moved_deletion_staged_drifted_target: MovedShape::Deleted, Staging::Staged, Phase::Drifted, Side::Target;
    moved_deletion_staged_refreshed_source: MovedShape::Deleted, Staging::Staged, Phase::Refreshed, Side::Source;
    moved_deletion_staged_refreshed_target: MovedShape::Deleted, Staging::Staged, Phase::Refreshed, Side::Target;
    moved_deletion_unstaged_drifted_source: MovedShape::Deleted, Staging::Unstaged, Phase::Drifted, Side::Source;
    moved_deletion_unstaged_drifted_target: MovedShape::Deleted, Staging::Unstaged, Phase::Drifted, Side::Target;
    moved_deletion_unstaged_refreshed_source: MovedShape::Deleted, Staging::Unstaged, Phase::Refreshed, Side::Source;
    moved_deletion_unstaged_refreshed_target: MovedShape::Deleted, Staging::Unstaged, Phase::Refreshed, Side::Target;
    moved_earlier_version_staged_drifted_source: MovedShape::Reverted, Staging::Staged, Phase::Drifted, Side::Source;
    moved_earlier_version_staged_drifted_target: MovedShape::Reverted, Staging::Staged, Phase::Drifted, Side::Target;
    moved_earlier_version_staged_refreshed_source: MovedShape::Reverted, Staging::Staged, Phase::Refreshed, Side::Source;
    moved_earlier_version_staged_refreshed_target: MovedShape::Reverted, Staging::Staged, Phase::Refreshed, Side::Target;
    moved_earlier_version_unstaged_drifted_source: MovedShape::Reverted, Staging::Unstaged, Phase::Drifted, Side::Source;
    moved_earlier_version_unstaged_drifted_target: MovedShape::Reverted, Staging::Unstaged, Phase::Drifted, Side::Target;
    moved_earlier_version_unstaged_refreshed_source: MovedShape::Reverted, Staging::Unstaged, Phase::Refreshed, Side::Source;
    moved_earlier_version_unstaged_refreshed_target: MovedShape::Reverted, Staging::Unstaged, Phase::Refreshed, Side::Target;
    moved_revert_staged_drifted_source: MovedShape::Revert, Staging::Staged, Phase::Drifted, Side::Source;
    moved_revert_staged_drifted_target: MovedShape::Revert, Staging::Staged, Phase::Drifted, Side::Target;
    moved_revert_staged_refreshed_source: MovedShape::Revert, Staging::Staged, Phase::Refreshed, Side::Source;
    moved_revert_staged_refreshed_target: MovedShape::Revert, Staging::Staged, Phase::Refreshed, Side::Target;
    moved_revert_unstaged_drifted_source: MovedShape::Revert, Staging::Unstaged, Phase::Drifted, Side::Source;
    moved_revert_unstaged_drifted_target: MovedShape::Revert, Staging::Unstaged, Phase::Drifted, Side::Target;
    moved_revert_unstaged_refreshed_source: MovedShape::Revert, Staging::Unstaged, Phase::Refreshed, Side::Source;
    moved_revert_unstaged_refreshed_target: MovedShape::Revert, Staging::Unstaged, Phase::Refreshed, Side::Target;
    moved_copy_staged_drifted_source: MovedShape::Copied, Staging::Staged, Phase::Drifted, Side::Source;
    moved_copy_staged_drifted_target: MovedShape::Copied, Staging::Staged, Phase::Drifted, Side::Target;
    moved_copy_staged_refreshed_source: MovedShape::Copied, Staging::Staged, Phase::Refreshed, Side::Source;
    moved_copy_staged_refreshed_target: MovedShape::Copied, Staging::Staged, Phase::Refreshed, Side::Target;
    moved_copy_unstaged_drifted_source: MovedShape::Copied, Staging::Unstaged, Phase::Drifted, Side::Source;
    moved_copy_unstaged_drifted_target: MovedShape::Copied, Staging::Unstaged, Phase::Drifted, Side::Target;
    moved_copy_unstaged_refreshed_source: MovedShape::Copied, Staging::Unstaged, Phase::Refreshed, Side::Source;
    moved_copy_unstaged_refreshed_target: MovedShape::Copied, Staging::Unstaged, Phase::Refreshed, Side::Target;
}

#[cfg(unix)]
moved_edits! {
    moved_mode_staged_drifted_source: MovedShape::ModeChanged, Staging::Staged, Phase::Drifted, Side::Source;
    moved_mode_staged_drifted_target: MovedShape::ModeChanged, Staging::Staged, Phase::Drifted, Side::Target;
    moved_mode_staged_refreshed_source: MovedShape::ModeChanged, Staging::Staged, Phase::Refreshed, Side::Source;
    moved_mode_staged_refreshed_target: MovedShape::ModeChanged, Staging::Staged, Phase::Refreshed, Side::Target;
    moved_mode_unstaged_drifted_source: MovedShape::ModeChanged, Staging::Unstaged, Phase::Drifted, Side::Source;
    moved_mode_unstaged_drifted_target: MovedShape::ModeChanged, Staging::Unstaged, Phase::Drifted, Side::Target;
    moved_mode_unstaged_refreshed_source: MovedShape::ModeChanged, Staging::Unstaged, Phase::Refreshed, Side::Source;
    moved_mode_unstaged_refreshed_target: MovedShape::ModeChanged, Staging::Unstaged, Phase::Refreshed, Side::Target;
}

// ---------------------------------------------------------------------------
// What a refresh never replaces, through `rwv land` and `rwv doctor --fix`
// ---------------------------------------------------------------------------

fn checkout_of(f: &Fixture, side: Side) -> (PathBuf, &'static str) {
    match side {
        Side::Source => (f.ww_lib.clone(), "ww/project"),
        Side::Target => (f.primary_lib.clone(), "main"),
    }
}

/// `rwv doctor --fix` in the workspace holding `side`'s checkout.
fn fix(f: &Fixture, side: Side) -> std::process::Output {
    let workspace = match side {
        Side::Source => &f.ww,
        Side::Target => &f.primary,
    };
    common::cli::run(&["doctor", "--fix"], workspace)
}

/// Moves `side`'s branch with `change` after `prepare` ran in its checkout,
/// then lands and runs `rwv doctor --fix`, each refusing or leaving `kept`
/// exactly as it stood.
fn landing_and_fix_keep(
    side: Side,
    kept: &[&str],
    prepare: impl FnOnce(&Path),
    change: impl FnOnce(&Path),
) {
    let f = fixture();
    let (checkout, branch) = checkout_of(&f, side);
    prepare(&checkout);
    let moved = f.move_with(branch, change);
    if side == Side::Target {
        f.pin_primary(&moved);
    }
    let before = path_state(&checkout, kept);

    let out = common::cli::run(&["land"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDirty);
    assert_eq!(
        path_state(&checkout, kept),
        before,
        "{side:?}: the landing replaced what was there:\n{stderr}"
    );

    let out = fix(&f, side);

    assert_eq!(
        path_state(&checkout, kept),
        before,
        "{side:?}: doctor --fix replaced what was there:\n{}",
        common::cli::describe(&out)
    );
}

/// An untracked file stands where the move puts a directory: git would
/// remove the file to make the directory, and the file is in no commit.
fn untracked_file_where_the_move_puts_a_directory(side: Side) {
    landing_and_fix_keep(
        side,
        &["docs"],
        |checkout| std::fs::write(checkout.join("docs"), "never committed\n").unwrap(),
        |elsewhere| {
            std::fs::create_dir_all(elsewhere.join("docs")).unwrap();
            std::fs::write(elsewhere.join("docs/x.txt"), "x\n").unwrap();
        },
    );
}

#[test]
fn an_untracked_file_where_the_move_puts_a_directory_is_kept_in_the_source() {
    untracked_file_where_the_move_puts_a_directory(Side::Source);
}

#[test]
fn an_untracked_file_where_the_move_puts_a_directory_is_kept_in_the_target() {
    untracked_file_where_the_move_puts_a_directory(Side::Target);
}

/// An ignored file stands where the move adds a file: git's merge treats an
/// ignored file as expendable, so only reading it first keeps it.
fn ignored_file_where_the_move_adds_one(side: Side) {
    landing_and_fix_keep(
        side,
        &["build.log"],
        |checkout| {
            let exclude = common::git_in(
                checkout,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            );
            let exclude = Path::new(&exclude).join("info");
            std::fs::create_dir_all(&exclude).unwrap();
            std::fs::write(exclude.join("exclude"), "build.log\n").unwrap();
            std::fs::write(checkout.join("build.log"), "never committed\n").unwrap();
        },
        |elsewhere| {
            std::fs::write(elsewhere.join("build.log"), "committed\n").unwrap();
            common::git_in(elsewhere, &["add", "-f", "build.log"]);
        },
    );
}

#[test]
fn an_ignored_file_where_the_move_adds_one_is_kept_in_the_source() {
    ignored_file_where_the_move_adds_one(Side::Source);
}

#[test]
fn an_ignored_file_where_the_move_adds_one_is_kept_in_the_target() {
    ignored_file_where_the_move_adds_one(Side::Target);
}

/// A file git does not compare with its index entry carries a change the
/// move's file would overwrite: skip-worktree, or assume-unchanged.
fn unwatched_change_under_a_move(side: Side, flag: &'static str) {
    landing_and_fix_keep(
        side,
        &["notes.txt"],
        |checkout| {
            common::git_in(checkout, &["update-index", flag, "notes.txt"]);
            std::fs::write(checkout.join("notes.txt"), "local tweak\n").unwrap();
        },
        |elsewhere| std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap(),
    );
}

#[test]
fn a_skip_worktree_change_is_kept_in_the_source() {
    unwatched_change_under_a_move(Side::Source, "--skip-worktree");
}

#[test]
fn a_skip_worktree_change_is_kept_in_the_target() {
    unwatched_change_under_a_move(Side::Target, "--skip-worktree");
}

#[test]
fn an_assume_unchanged_change_is_kept_in_the_source() {
    unwatched_change_under_a_move(Side::Source, "--assume-unchanged");
}

#[test]
fn an_assume_unchanged_change_is_kept_in_the_target() {
    unwatched_change_under_a_move(Side::Target, "--assume-unchanged");
}

/// After `rwv doctor --fix` refreshed a move that added a file, the file's
/// staged deletion is the person's: neither the next fix nor a landing
/// restores it.
fn staged_deletion_after_a_fix(side: Side) {
    let f = fixture();
    let (checkout, branch) = checkout_of(&f, side);
    let moved = f.move_with(branch, |elsewhere| {
        std::fs::write(elsewhere.join("new.txt"), "new\n").unwrap()
    });
    if side == Side::Target {
        f.pin_primary(&moved);
    }
    let out = fix(&f, side);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        stdout.contains("refreshed drift") && stdout.contains("new.txt"),
        "{side:?}: the first fix refreshes the drift:\n{}",
        common::cli::describe(&out)
    );
    common::git_in(&checkout, &["rm", "-q", "new.txt"]);
    let before = path_state(&checkout, &["new.txt"]);

    let out = fix(&f, side);

    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("refreshed drift"),
        "{}",
        common::cli::describe(&out)
    );
    assert_eq!(path_state(&checkout, &["new.txt"]), before);

    let out = common::cli::run(&["land"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDirty);
    assert!(
        edits_listing(&stderr).iter().any(|l| l == "new.txt"),
        "{stderr}"
    );
    assert_eq!(path_state(&checkout, &["new.txt"]), before, "{stderr}");
}

#[test]
fn a_staged_deletion_after_a_fix_is_kept_in_the_source() {
    staged_deletion_after_a_fix(Side::Source);
}

#[test]
fn a_staged_deletion_after_a_fix_is_kept_in_the_target() {
    staged_deletion_after_a_fix(Side::Target);
}

/// A checkout refreshed by hand, which leaves no record of where it stands,
/// then given a staged deletion of a file the move added: the other path the
/// move changed already holds HEAD's entry, so the checkout is no longer one
/// left behind, and the deletion is an edit.
#[test]
fn a_staged_deletion_after_a_hand_refresh_is_kept() {
    let f = fixture();
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        std::fs::write(elsewhere.join("new.txt"), "new\n").unwrap();
    });
    common::git_in(&f.ww_lib, &["checkout", "HEAD", "--", "."]);
    assert_eq!(common::git_in(&f.ww_lib, &["status", "--porcelain"]), "");
    common::git_in(&f.ww_lib, &["rm", "-q", "new.txt"]);
    let before = path_state(&f.ww_lib, &["new.txt"]);

    let out = common::cli::run(&["land"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDirty);
    assert!(
        edits_listing(&stderr).iter().any(|l| l == "new.txt"),
        "{stderr}"
    );
    assert!(!stderr.contains("refreshed"), "{stderr}");
    assert_eq!(path_state(&f.ww_lib, &["new.txt"]), before, "{stderr}");
}

/// A file already holding HEAD's content beside its old-tip index entry has
/// only the entry reset, and the landing proceeds.
#[test]
fn a_file_already_holding_heads_content_has_its_index_entry_reset() {
    let f = fixture();
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");
    std::fs::write(f.ww_lib.join("notes.txt"), "three\n").unwrap();

    let out = common::cli::succeed(&["land"], &f.ww);

    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("these index entries were reset") && stderr.contains("notes.txt"),
        "{stderr}"
    );
    assert_eq!(common::git_in(&f.ww_lib, &["status", "--porcelain"]), "");
}

/// A checkout stopped in a revert, whose branch then moved under it: the
/// landing refuses the operation and refreshes nothing, the drifted path
/// included.
fn stopped_in_a_revert_under_a_move(side: Side) {
    let f = fixture();
    let (checkout, branch) = checkout_of(&f, side);
    let spare = common::git_in(&checkout, &["log", "-n1", "--format=%H", "--", "spare.txt"]);
    common::git_in(&checkout, &["revert", "--no-commit", &spare]);
    let moved = f.move_from_elsewhere(branch, "notes.txt", "three\n");
    if side == Side::Target {
        f.pin_primary(&moved);
    }
    let kept = ["notes.txt", "spare.txt"];
    let before = path_state(&checkout, &kept);

    let out = common::cli::run(&["land"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsMidOperation);
    assert!(!stderr.contains("refreshed"), "{stderr}");
    assert_eq!(path_state(&checkout, &kept), before, "{stderr}");
    assert!(
        !common::git_in(&checkout, &["rev-parse", "-q", "--verify", "REVERT_HEAD"]).is_empty(),
        "the revert is still in progress"
    );
}

#[test]
fn drift_in_a_source_stopped_in_a_revert_is_left() {
    stopped_in_a_revert_under_a_move(Side::Source);
}

#[test]
fn drift_in_a_target_stopped_in_a_revert_is_left() {
    stopped_in_a_revert_under_a_move(Side::Target);
}

// ---------------------------------------------------------------------------
// A refresh that cannot write: what the landing prints, and the route back
// ---------------------------------------------------------------------------

/// `rwv land` in the workweave, held at `point` once it has read the drift a
/// second time and is about to write it.
fn held_landing(f: &Fixture, point: &str) -> common::pause::Paused {
    let mut land = common::rwv_std();
    land.args(["land"]).current_dir(&f.ww);
    common::pause::Paused::spawn(land, point, &f.scratch.join(format!("pause-{point}")))
        .unwrap_or_else(|e| panic!("the landing reaches {point}: {e}"))
}

/// The lines the `choices:` block prints.
fn choice_lines(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .skip_while(|l| *l != "choices:")
        .skip(1)
        .take_while(|l| l.starts_with(' '))
        .map(|l| l.trim().to_owned())
        .collect()
}

/// The landing refused with the drift left in place: the state's line, the
/// one route that reads the checkout again, and no choice that commits.
fn left_in_place(out: &std::process::Output, why: &str) -> String {
    let stderr = common::cli::refused(out, State::EditsDriftLeftInPlace);
    assert!(
        stderr.contains(why),
        "the reason is named, `{why}`:\n{stderr}"
    );
    let choices = choice_lines(&stderr);
    assert!(
        choices.iter().any(|l| l == "rwv land"),
        "the route is to land again: {choices:?}\n{stderr}"
    );
    assert!(
        !choices
            .iter()
            .any(|l| l.contains("commit") || l.contains("stash")),
        "nothing offers to commit or stash the old tip's content: {choices:?}\n{stderr}"
    );
    stderr
}

fn source_head(f: &Fixture) -> String {
    common::git_in(&f.primary_lib, &["rev-parse", "HEAD"])
}

/// A file saved while the refresh is about to write is git's to refuse over:
/// nothing is written, the landing names the drift left in place and offers
/// to land again, and landing again refreshes everything but the edit.
#[test]
fn an_edit_saved_as_the_refresh_starts_is_kept_and_landing_again_refreshes_the_rest() {
    let f = fixture();
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        std::fs::write(elsewhere.join("new.txt"), "new\n").unwrap();
    });
    let target = source_head(&f);
    let held = held_landing(&f, "drift-write");
    std::fs::write(f.ww_lib.join("notes.txt"), "mine\n").unwrap();

    let out = held.release();

    let stderr = left_in_place(&out, "a path changed after it was read");
    assert!(!stderr.contains("refreshed drift"), "{stderr}");
    assert_eq!(read(&f.ww_lib.join("notes.txt")).as_deref(), Some("mine\n"));
    assert_eq!(read(&f.ww_lib.join("new.txt")), None, "{stderr}");
    assert_eq!(source_head(&f), target, "a refused landing moves nothing");

    let out = common::cli::run(&["land"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDirty);
    assert_eq!(
        edits_listing(&stderr),
        vec!["notes.txt".to_owned()],
        "{stderr}"
    );
    assert!(
        stderr.contains("refreshed drift") && stderr.contains("new.txt"),
        "{stderr}"
    );
    assert_eq!(read(&f.ww_lib.join("notes.txt")).as_deref(), Some("mine\n"));
    assert_eq!(read(&f.ww_lib.join("new.txt")).as_deref(), Some("new\n"));
}

/// The checkout takes part of the move by hand after the landing first read
/// it: read again before the write, it no longer shows the move under it, and
/// nothing is written, the path the move added included.
#[test]
fn a_hand_refresh_after_the_first_reading_voids_the_drift_before_the_write() {
    let f = fixture();
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        std::fs::write(elsewhere.join("new.txt"), "new\n").unwrap();
    });
    let held = held_landing(&f, "drift-read");
    common::git_in(&f.ww_lib, &["checkout", "HEAD", "--", "notes.txt"]);

    let out = held.release();

    let stderr = left_in_place(&out, "a path changed after it was read");
    assert!(!stderr.contains("refreshed drift"), "{stderr}");
    assert_eq!(read(&f.ww_lib.join("new.txt")), None, "{stderr}");
}

/// Moves `ww/project` adding `path` as a tracked file and runs `prepare` in
/// the source checkout; then lands, `put` putting something at the path while
/// the landing is about to write. That something is kept, and the drift left.
fn put_where_the_move_adds_a_file_as_the_refresh_starts(
    path: &str,
    prepare: impl FnOnce(&Path),
    put: impl FnOnce(&Path),
) {
    let f = fixture();
    prepare(&f.ww_lib);
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join(path), "committed\n").unwrap();
        common::git_in(elsewhere, &["add", "-f", path]);
    });
    let held = held_landing(&f, "drift-write");
    put(&f.ww_lib);
    let before = path_state(&f.ww_lib, &[path]);

    let out = held.release();

    let stderr = left_in_place(&out, "a path changed after it was read");
    assert_eq!(path_state(&f.ww_lib, &[path]), before, "{stderr}");

    let out = common::cli::run(&["land"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDirty);
    assert_eq!(path_state(&f.ww_lib, &[path]), before, "{stderr}");
}

#[test]
fn an_ignored_file_put_where_the_move_adds_one_as_the_refresh_starts_is_kept() {
    put_where_the_move_adds_a_file_as_the_refresh_starts(
        "build.log",
        |checkout| {
            let common_dir = common::git_in(
                checkout,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            );
            let info = Path::new(&common_dir).join("info");
            std::fs::create_dir_all(&info).unwrap();
            std::fs::write(info.join("exclude"), "build.log\n").unwrap();
        },
        |checkout| std::fs::write(checkout.join("build.log"), "never committed\n").unwrap(),
    );
}

#[test]
fn an_ignored_directory_put_where_the_move_adds_a_file_as_the_refresh_starts_is_kept() {
    put_where_the_move_adds_a_file_as_the_refresh_starts(
        "out",
        |checkout| {
            let common_dir = common::git_in(
                checkout,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            );
            let info = Path::new(&common_dir).join("info");
            std::fs::create_dir_all(&info).unwrap();
            std::fs::write(info.join("exclude"), "out/\n").unwrap();
        },
        |checkout| {
            std::fs::create_dir_all(checkout.join("out")).unwrap();
            std::fs::write(checkout.join("out/a.o"), "never committed\n").unwrap();
        },
    );
}

#[test]
fn an_untracked_file_put_where_the_move_adds_one_as_the_refresh_starts_is_kept() {
    put_where_the_move_adds_a_file_as_the_refresh_starts(
        "new.txt",
        |_| {},
        |checkout| std::fs::write(checkout.join("new.txt"), "never committed\n").unwrap(),
    );
}

/// A stale file deleted while the refresh is about to write stays deleted:
/// git's merge counts a missing file as up to date and would write it again.
#[test]
fn a_file_deleted_as_the_refresh_starts_stays_deleted() {
    let f = fixture();
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");
    let held = held_landing(&f, "drift-write");
    std::fs::remove_file(f.ww_lib.join("notes.txt")).unwrap();

    let out = held.release();

    let stderr = left_in_place(&out, "a path changed after it was read");
    assert_eq!(read(&f.ww_lib.join("notes.txt")), None, "{stderr}");

    let out = common::cli::run(&["land"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDirty);
    assert!(
        edits_listing(&stderr).iter().any(|l| l == "notes.txt"),
        "{stderr}"
    );
    assert_eq!(read(&f.ww_lib.join("notes.txt")), None, "{stderr}");
}

/// Another process holding the index when the refresh writes: the landing
/// says the write failed, with git's account, and landing again once the
/// index is free refreshes and lands.
#[test]
fn a_write_that_fails_is_named_and_landing_again_refreshes() {
    let f = fixture();
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");
    let lock = common::git_in(
        &f.ww_lib,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "index.lock",
        ],
    );
    let held = held_landing(&f, "drift-write");
    std::fs::write(&lock, "").unwrap();

    let out = held.release();

    let stderr = left_in_place(&out, "the write failed");
    assert!(
        stderr.contains("index.lock"),
        "git's own account is carried: {stderr}"
    );
    assert_eq!(read(&f.ww_lib.join("notes.txt")).as_deref(), Some("two\n"));
    std::fs::remove_file(&lock).unwrap();

    let out = common::cli::succeed(&["land"], &f.ww);

    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("refreshed drift"), "{stderr}");
    assert_eq!(
        read(&f.primary_lib.join("notes.txt")).as_deref(),
        Some("three\n")
    );
    assert_eq!(common::git_in(&f.ww_lib, &["status", "--porcelain"]), "");
}

/// The branch moved again while the refresh was about to write: nothing is
/// written or recorded, and landing again refreshes to the newer tip.
#[test]
fn a_second_move_as_the_refresh_starts_is_left_for_the_next_landing() {
    let f = fixture();
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");
    let held = held_landing(&f, "drift-write");
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("notes.txt"), "four\n").unwrap();
        std::fs::write(elsewhere.join("README.md"), "moved again\n").unwrap();
    });

    let out = held.release();

    let stderr = left_in_place(&out, "the branch moved again");
    assert!(!stderr.contains("refreshed drift"), "{stderr}");
    assert_eq!(read(&f.ww_lib.join("notes.txt")).as_deref(), Some("two\n"));

    let out = common::cli::succeed(&["land"], &f.ww);

    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("refreshed drift"), "{stderr}");
    assert_eq!(read(&f.ww_lib.join("notes.txt")).as_deref(), Some("four\n"));
    assert_eq!(
        read(&f.ww_lib.join("README.md")).as_deref(),
        Some("moved again\n")
    );
    assert_eq!(common::git_in(&f.ww_lib, &["status", "--porcelain"]), "");
}

/// Unresolved conflicts in the index: the drift is left, its route is to look
/// at them with the VCS's status and land again, and the checkout's other
/// edits are listed with no choice of their own. Resolving them as that
/// status says and landing again refreshes the drift and lands.
#[test]
fn unresolved_conflicts_leave_the_drift_and_resolving_them_then_landing_again_lands() {
    let f = fixture();
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");
    let blob = common::git_in(&f.ww_lib, &["rev-parse", "HEAD:spare.txt"]);
    let zero = "0".repeat(blob.len());
    let info = format!(
        "0 {zero}\tspare.txt\n100644 {blob} 1\tspare.txt\n100644 {blob} 2\tspare.txt\n\
         100644 {blob} 3\tspare.txt\n"
    );
    let mut stage = common::git();
    stage
        .args(["update-index", "--index-info"])
        .current_dir(&f.ww_lib)
        .stdin(std::process::Stdio::piped());
    let mut child = stage.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), info.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());

    let out = common::cli::run(&["land"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDriftLeftInPlace);
    assert!(stderr.contains("unresolved conflicts"), "{stderr}");
    let status = format!("git -C {} status", f.ww_lib.display());
    let choices = choice_lines(&stderr);
    assert!(
        choices.iter().any(|l| *l == status) && choices.iter().any(|l| l == "rwv land"),
        "{choices:?}\n{stderr}"
    );
    assert!(
        !choices
            .iter()
            .any(|l| l.contains("commit") || l.contains("stash")),
        "{choices:?}\n{stderr}"
    );
    assert_eq!(read(&f.ww_lib.join("notes.txt")).as_deref(), Some("two\n"));

    let shown = common::git_in(&f.ww_lib, &["status"]);
    assert!(
        shown.contains("spare.txt") && shown.contains("git add"),
        "{shown}"
    );
    common::git_in(&f.ww_lib, &["add", "spare.txt"]);
    let out = common::cli::succeed(&["land"], &f.ww);

    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("refreshed drift") && stderr.contains("notes.txt"),
        "{stderr}"
    );
    assert_eq!(
        read(&f.primary_lib.join("notes.txt")).as_deref(),
        Some("three\n")
    );
}

/// The move changes two files, one of which already holds the new content:
/// that one takes its index entry alone. A file write declined leaves that
/// index entry as it was and records nothing, so the next landing still
/// reads it as drift and resets it.
#[test]
fn a_declined_file_write_leaves_the_index_entries_and_records_nothing() {
    let f = fixture();
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        std::fs::write(elsewhere.join("spare.txt"), "spare moved\n").unwrap();
    });
    std::fs::write(f.ww_lib.join("spare.txt"), "spare moved\n").unwrap();
    let held = held_landing(&f, "drift-write");
    std::fs::write(f.ww_lib.join("notes.txt"), "mine\n").unwrap();

    let out = held.release();

    let stderr = left_in_place(&out, "a path changed after it was read");
    assert!(
        common::git_in(&f.ww_lib, &["diff", "--cached", "--name-only"])
            .lines()
            .any(|l| l == "spare.txt"),
        "the index entry is not reset alone:\n{stderr}"
    );

    let out = common::cli::run(&["land"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDirty);
    assert!(
        stderr.contains("these index entries were reset") && stderr.contains("spare.txt"),
        "the next landing still reads the index entry as drift:\n{stderr}"
    );
    assert_eq!(
        edits_listing(&stderr),
        vec!["notes.txt".to_owned()],
        "{stderr}"
    );
}

/// An index entry staged while the refresh is between its file write and its
/// index-entry write: the index-entry write is declined, and the files it had
/// written are put back, so the checkout is not left half refreshed.
#[test]
fn a_declined_index_entry_write_puts_the_written_files_back() {
    let f = fixture();
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        std::fs::write(elsewhere.join("spare.txt"), "spare moved\n").unwrap();
    });
    std::fs::write(f.ww_lib.join("spare.txt"), "spare moved\n").unwrap();
    let held = held_landing(&f, "drift-write-index");
    std::fs::write(f.ww_lib.join("spare.txt"), "mine\n").unwrap();
    common::git_in(&f.ww_lib, &["add", "spare.txt"]);

    let out = held.release();

    let stderr = left_in_place(&out, "a path changed after it was read");
    assert!(!stderr.contains("refreshed drift"), "{stderr}");
    assert_eq!(read(&f.ww_lib.join("notes.txt")).as_deref(), Some("two\n"));
    assert_eq!(
        common::git_in(&f.ww_lib, &["diff", "--name-only", "--", "notes.txt"]),
        "",
        "the put-back leaves notes.txt's file and index entry together"
    );

    let out = common::cli::run(&["land"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDirty);
    assert!(
        stderr.contains("refreshed drift") && stderr.contains("notes.txt"),
        "{stderr}"
    );
    assert_eq!(
        edits_listing(&stderr),
        vec!["spare.txt".to_owned()],
        "{stderr}"
    );
    assert_eq!(read(&f.ww_lib.join("spare.txt")).as_deref(), Some("mine\n"));
}

/// An edit refused beside drift whose refresh was declined is listed with no
/// choice: committing there would commit the old tip's content with it.
#[test]
fn an_edit_beside_drift_left_in_place_is_offered_no_commit() {
    let f = fixture();
    std::fs::write(f.ww_lib.join("README.md"), "edited\n").unwrap();
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");
    let held = held_landing(&f, "drift-write");
    std::fs::write(f.ww_lib.join("notes.txt"), "mine\n").unwrap();

    let out = held.release();

    let stderr = left_in_place(&out, "a path changed after it was read");
    common::cli::refused(&out, State::EditsDirty);
    assert!(
        edits_listing(&stderr).iter().any(|l| l == "README.md"),
        "{stderr}"
    );
}

// ---------------------------------------------------------------------------
// Checkouts the merge would treat otherwise than as a refresh
// ---------------------------------------------------------------------------

/// A worktree whose index file is gone: git's merge would read it as an
/// initial checkout and write every file, an ignored one standing at a path
/// the move did not touch included. Nothing reads as moved under it.
#[test]
fn a_worktree_with_no_index_is_not_refreshed() {
    let f = fixture();
    let common_dir = common::git_in(
        &f.ww_lib,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    std::fs::create_dir_all(Path::new(&common_dir).join("info")).unwrap();
    std::fs::write(Path::new(&common_dir).join("info/exclude"), "README.md\n").unwrap();
    f.move_from_elsewhere("ww/project", "new.txt", "new\n");
    let index = common::git_in(
        &f.ww_lib,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    );
    std::fs::remove_file(&index).unwrap();
    std::fs::write(f.ww_lib.join("README.md"), "mine, ignored\n").unwrap();

    let out = common::cli::run(&["land"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDirty);
    assert!(!stderr.contains("refreshed"), "{stderr}");
    assert_eq!(
        read(&f.ww_lib.join("README.md")).as_deref(),
        Some("mine, ignored\n")
    );
    assert_eq!(read(&f.ww_lib.join("new.txt")), None);

    let out = fix(&f, Side::Source);

    assert_eq!(
        read(&f.ww_lib.join("README.md")).as_deref(),
        Some("mine, ignored\n"),
        "{}",
        common::cli::describe(&out)
    );
}

/// A move that turns a file into a directory is refreshed whole: the file
/// the move removes does not stand in the way of the directory.
#[test]
fn a_move_turning_a_file_into_a_directory_is_refreshed_whole() {
    let f = fixture();
    f.move_with("ww/project", |elsewhere| {
        std::fs::remove_file(elsewhere.join("spare.txt")).unwrap();
        std::fs::create_dir_all(elsewhere.join("spare.txt")).unwrap();
        std::fs::write(elsewhere.join("spare.txt/inner.txt"), "inner\n").unwrap();
    });

    let out = common::cli::succeed(&["land"], &f.ww);

    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("refreshed drift"), "{stderr}");
    assert_eq!(
        read(&f.ww_lib.join("spare.txt/inner.txt")).as_deref(),
        Some("inner\n")
    );
    assert_eq!(common::git_in(&f.ww_lib, &["status", "--porcelain"]), "");
}

/// A move that turns a directory into a file is refreshed whole: the files
/// the move removes from the directory do not stand in the way of the file.
#[test]
fn a_move_turning_a_directory_into_a_file_is_refreshed_whole() {
    let f = fixture();
    f.move_with("ww/project", |elsewhere| {
        std::fs::create_dir_all(elsewhere.join("docs")).unwrap();
        std::fs::write(elsewhere.join("docs/a.txt"), "a\n").unwrap();
    });
    let out = fix(&f, Side::Source);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("refreshed drift"),
        "{}",
        common::cli::describe(&out)
    );
    f.move_with("ww/project", |elsewhere| {
        std::fs::remove_dir_all(elsewhere.join("docs")).unwrap();
        std::fs::write(elsewhere.join("docs"), "now a file\n").unwrap();
    });

    let out = common::cli::succeed(&["land"], &f.ww);

    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("refreshed drift"), "{stderr}");
    assert_eq!(
        read(&f.ww_lib.join("docs")).as_deref(),
        Some("now a file\n")
    );
    assert_eq!(common::git_in(&f.ww_lib, &["status", "--porcelain"]), "");
}

/// `rwv doctor --fix` held as it is about to write, a file saved meanwhile:
/// the fix names the drift left in place and its route, running that route
/// refreshes the rest, and the saved file is kept throughout.
#[test]
fn doctor_fix_names_a_declined_write_and_running_it_again_refreshes_the_rest() {
    let f = fixture();
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        std::fs::write(elsewhere.join("new.txt"), "new\n").unwrap();
    });
    let mut doctor = common::rwv_std();
    doctor.args(["doctor", "--fix"]).current_dir(&f.ww);
    let held = common::pause::Paused::spawn(doctor, "drift-write", &f.scratch.join("pause"))
        .unwrap_or_else(|e| panic!("the fix reaches its write: {e}"));
    std::fs::write(f.ww_lib.join("notes.txt"), "mine\n").unwrap();

    let out = held.release();

    let said = common::cli::describe(&out);
    assert!(
        said.contains("drift left in place") && said.contains("run `rwv doctor --fix`"),
        "{said}"
    );
    assert_eq!(read(&f.ww_lib.join("new.txt")), None, "{said}");

    let out = fix(&f, Side::Source);

    let said = common::cli::describe(&out);
    assert!(
        said.contains("refreshed drift") && said.contains("new.txt"),
        "{said}"
    );
    assert_eq!(read(&f.ww_lib.join("notes.txt")).as_deref(), Some("mine\n"));
    assert_eq!(read(&f.ww_lib.join("new.txt")).as_deref(), Some("new\n"));
}
