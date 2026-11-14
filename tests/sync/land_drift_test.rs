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
    init_repo_with(path, &[]);
}

fn init_repo_with(path: &Path, init: &[&str]) {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &[&["init", "-q", "-b", "main"], init].concat());
    common::git_in(path, &["config", "user.email", "test@test.com"]);
    common::git_in(path, &["config", "user.name", "Test"]);
    common::git_in(path, &["config", "core.autocrlf", "false"]);
}

fn fixture() -> Fixture {
    fixture_with_lib_config(&[])
}

fn fixture_with_lib_config(config: &[(&str, &str)]) -> Fixture {
    fixture_with_lib(&[], config)
}

fn fixture_with_lib(init: &[&str], config: &[(&str, &str)]) -> Fixture {
    let tmp = common::tempdir().unwrap();
    let primary = tmp.path().join("primary");
    let primary_lib = primary.join(LIB);
    init_repo_with(&primary_lib, init);
    for (key, value) in config {
        common::git_in(&primary_lib, &["config", key, value]);
    }
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
    assert_eq!(entry["recorded_only"], serde_json::json!([]), "{json:#}");
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

/// The paths listed beneath the first line holding `state`.
fn listing_under(stderr: &str, state: &str) -> Vec<String> {
    stderr
        .lines()
        .skip_while(|l| !l.contains(state))
        .skip(1)
        .take_while(|l| l.starts_with("    "))
        .map(|l| l.trim().to_owned())
        .collect()
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

    let held = phase == Phase::Drifted && shape != MovedShape::Revert;
    let stderr = common::cli::refused(
        &out,
        match held {
            true => State::EditsMovedAndEdited,
            false => State::EditsDirty,
        },
    );
    let listing = match held {
        true => listing_under(&stderr, "edits moved and edited"),
        false => edits_listing(&stderr),
    };
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
/// then lands, refused as `state`, and runs `rwv doctor --fix`, each leaving
/// `kept` exactly as it stood.
fn landing_and_fix_keep(
    side: Side,
    state: State,
    compares: bool,
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

    let stderr = common::cli::refused(&out, state);
    assert_eq!(
        path_state(&checkout, kept),
        before,
        "{side:?}: the landing replaced what was there:\n{stderr}"
    );
    let compare: Vec<String> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("git -C "))
        .map(str::to_owned)
        .collect();
    assert_eq!(
        compare.is_empty(),
        !compares,
        "the compare choice is printed where the VCS does not compare the file:\n{stderr}"
    );
    if state == State::EditsMovedAndEdited && !compares {
        assert!(
            stderr.contains("move out of the way"),
            "something in the way is offered the move-aside first:\n{stderr}"
        );
        assert!(!stderr.contains("set-aside-edited-drift"), "{stderr}");
    }

    let out = fix(&f, side);

    assert_eq!(
        path_state(&checkout, kept),
        before,
        "{side:?}: doctor --fix replaced what was there:\n{}",
        common::cli::describe(&out)
    );
    if !compares {
        return;
    }
    let workspace = match side {
        Side::Source => &f.ww,
        Side::Target => &f.primary,
    };
    for flag in ["--merge-edited-drift", "--set-aside-edited-drift"] {
        let out = common::cli::run(&["doctor", "--fix", flag], workspace);
        let stderr = common::cli::refused(&out, State::EditsMovedAndEdited);
        assert!(!stderr.contains("kept as backup"), "{stderr}");
        assert!(stderr.contains("a backup keeps it"), "{stderr}");
        assert_eq!(
            path_state(&checkout, kept),
            before,
            "{side:?}: {flag} replaced what was there:\n{stderr}"
        );
    }
    for line in &compare {
        let words: Vec<&str> = line.split_whitespace().collect();
        let ran = std::process::Command::new(words[0])
            .args(&words[1..])
            .output()
            .unwrap();
        assert!(
            ran.status.success(),
            "{line}: {}",
            common::cli::describe(&ran)
        );
    }
    let out = common::cli::run(&["doctor", "--fix", "--merge-edited-drift"], workspace);
    let said = common::cli::describe(&out);
    assert!(said.contains("kept as backup"), "{said}");
    assert!(!said.contains("a backup keeps it"), "{said}");
}

/// An untracked file stands where the move puts a directory: git would
/// remove the file to make the directory, and the file is in no commit. It
/// holds the checkout, offered the move-aside alone.
fn untracked_file_where_the_move_puts_a_directory(side: Side) {
    landing_and_fix_keep(
        side,
        State::EditsMovedAndEdited,
        false,
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
/// ignored file as expendable, so only reading it first keeps it. It holds
/// the checkout as a file both moved and edited, offered moving it out of
/// the way.
fn ignored_file_where_the_move_adds_one(side: Side) {
    landing_and_fix_keep(
        side,
        State::EditsMovedAndEdited,
        false,
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
/// move's file would overwrite: skip-worktree, or assume-unchanged. It holds
/// the checkout as a file both moved and edited.
fn unwatched_change_under_a_move(side: Side, flag: &'static str) {
    landing_and_fix_keep(
        side,
        State::EditsMovedAndEdited,
        true,
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
/// move changed already holds HEAD's entry, so the move was taken in a way
/// the checkout's history does not show, and the deletion leaves the path as
/// the old tip held it, which no reading tells from drift. It is kept as it
/// stands, held with the stash alone and no commit.
#[test]
fn a_staged_deletion_after_a_hand_refresh_is_kept_and_held() {
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

    let stderr = common::cli::refused(&out, State::EditsDriftLeftInPlace);
    assert!(stderr.contains("new.txt"), "{stderr}");
    assert!(!stderr.contains(" commit\n"), "{stderr}");
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
        stderr.contains("what the VCS records for these paths was reset")
            && stderr.contains("notes.txt"),
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

/// The workweave with the landing killed in it aborted, so the verbs that
/// refuse while a landing is recorded run.
fn without_the_killed_landing(f: &Fixture) {
    let out = common::cli::run(&["abort"], &f.ww);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    assert!(!f.ww.join(".rwv-op").exists(), "the record stands");
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
/// to land again, and landing again finds the file both moved and edited,
/// which holds the whole checkout.
#[test]
fn an_edit_saved_as_the_refresh_starts_is_kept_and_landing_again_holds_the_checkout() {
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

    let stderr = common::cli::refused(&out, State::EditsMovedAndEdited);
    assert_eq!(
        listing_under(&stderr, "edits moved and edited"),
        vec!["notes.txt".to_owned()],
        "{stderr}"
    );
    assert!(!stderr.contains("refreshed drift"), "{stderr}");
    assert_eq!(read(&f.ww_lib.join("notes.txt")).as_deref(), Some("mine\n"));
    assert_eq!(read(&f.ww_lib.join("new.txt")), None, "{stderr}");
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
    then: State,
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

    let stderr = common::cli::refused(&out, then);
    assert_eq!(path_state(&f.ww_lib, &[path]), before, "{stderr}");
}

#[test]
fn an_ignored_file_put_where_the_move_adds_one_as_the_refresh_starts_is_kept() {
    put_where_the_move_adds_a_file_as_the_refresh_starts(
        "build.log",
        State::EditsMovedAndEdited,
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
        State::EditsMovedAndEdited,
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
        State::EditsMovedAndEdited,
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

    let stderr = common::cli::refused(&out, State::EditsMovedAndEdited);
    assert!(
        listing_under(&stderr, "edits moved and edited")
            .iter()
            .any(|l| l == "notes.txt"),
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
        choices.contains(&status) && choices.iter().any(|l| l == "rwv land"),
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

    let stderr = common::cli::refused(&out, State::EditsMovedAndEdited);
    assert!(
        !stderr.contains("refreshed drift"),
        "a file both moved and edited holds the whole checkout:\n{stderr}"
    );
    assert_eq!(
        listing_under(&stderr, "edits moved and edited"),
        vec!["notes.txt".to_owned()],
        "{stderr}"
    );
}

/// A move of every shape: two files changed, one of them already holding
/// the new content here, a file and a file under a new directory added, a
/// file deleted, a file turned into a directory, a directory turned into a
/// file, and, where links exist, a link added.
fn move_of_every_shape(f: &Fixture) {
    std::fs::create_dir_all(f.ww_lib.join("dir")).unwrap();
    commit_file(&f.ww_lib, "dir/inner.txt", "inner\n", "dir");
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        std::fs::write(elsewhere.join("spare.txt"), "spare moved\n").unwrap();
        std::fs::write(elsewhere.join("new.txt"), "new\n").unwrap();
        std::fs::create_dir_all(elsewhere.join("pkg")).unwrap();
        std::fs::write(elsewhere.join("pkg/mod.rs"), "mod\n").unwrap();
        std::fs::remove_file(elsewhere.join("README.md")).unwrap();
        std::fs::remove_file(elsewhere.join("feature.txt")).unwrap();
        std::fs::create_dir_all(elsewhere.join("feature.txt")).unwrap();
        std::fs::write(elsewhere.join("feature.txt/x"), "x\n").unwrap();
        std::fs::remove_dir_all(elsewhere.join("dir")).unwrap();
        std::fs::write(elsewhere.join("dir"), "dir as a file\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("notes.txt", elsewhere.join("link")).unwrap();
    });
    std::fs::write(f.ww_lib.join("spare.txt"), "spare moved\n").unwrap();
}

/// Every file [`move_of_every_shape`] writes holds the moved tip's content.
fn holds_every_shape(f: &Fixture) -> bool {
    let link = {
        #[cfg(unix)]
        {
            std::fs::read_link(f.ww_lib.join("link")).ok() == Some(PathBuf::from("notes.txt"))
        }
        #[cfg(not(unix))]
        {
            true
        }
    };
    read(&f.ww_lib.join("notes.txt")).as_deref() == Some("three\n")
        && read(&f.ww_lib.join("new.txt")).as_deref() == Some("new\n")
        && read(&f.ww_lib.join("pkg/mod.rs")).as_deref() == Some("mod\n")
        && !f.ww_lib.join("README.md").exists()
        && read(&f.ww_lib.join("feature.txt/x")).as_deref() == Some("x\n")
        && read(&f.ww_lib.join("dir")).as_deref() == Some("dir as a file\n")
        && link
}

/// The refresh writes its files before any index entry, and a landing
/// killed between the two leaves every index entry at the old tip beside
/// files holding HEAD's content, which still reads as drift whatever the
/// move did to each path: the next verb writes the entries.
#[test]
fn a_refresh_killed_between_its_writes_still_reads_as_drift() {
    let f = fixture();
    move_of_every_shape(&f);
    let held = held_landing(&f, "drift-write-records");
    drop(held);
    assert!(holds_every_shape(&f), "the files were written first");
    assert_eq!(
        common::git_in(&f.ww_lib, &["ls-files", "--", "new.txt", "README.md"]),
        "README.md",
        "no index entry was written"
    );

    let out = common::cli::run(&["doctor", "--fix"], &f.ww);

    let said = common::cli::describe(&out);
    assert!(said.contains("refreshed drift"), "{said}");
    assert!(holds_every_shape(&f), "{said}");
    assert_eq!(
        common::git_in(&f.ww_lib, &["status", "--porcelain"]),
        "",
        "{said}"
    );
}

fn drift_scratch(repo: &Path) -> Vec<String> {
    let dir = common::git_in(repo, &["rev-parse", "--absolute-git-dir"]);
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("rwv-drift-"))
        .collect()
}

/// A refresh killed while its copy of the index stands in the git dir: the
/// next refresh removes what it left and finishes the refresh.
#[test]
fn a_refresh_killed_with_its_index_copy_written_leaves_nothing_after_the_next() {
    let f = fixture();
    move_of_every_shape(&f);
    drop(held_landing(&f, "drift-write-spawn"));
    assert!(!drift_scratch(&f.ww_lib).is_empty());

    let out = common::cli::run(&["doctor", "--fix"], &f.ww);

    let said = common::cli::describe(&out);
    assert_eq!(drift_scratch(&f.ww_lib), Vec::<String>::new(), "{said}");
    assert_eq!(
        common::git_in(&f.ww_lib, &["status", "--porcelain"]),
        "",
        "{said}"
    );
}

/// A second refresh while the first stands with its index copy written
/// leaves that copy alone: the first's write still runs through it.
#[test]
fn a_refresh_leaves_a_running_refreshs_index_copy_alone() {
    let f = fixture();
    move_of_every_shape(&f);
    let held = held_landing(&f, "drift-write-spawn");
    let running = drift_scratch(&f.ww_lib);
    assert!(!running.is_empty());

    let out = common::cli::run(&["lock"], &f.ww);

    let said = common::cli::describe(&out);
    let left = drift_scratch(&f.ww_lib);
    assert!(
        running.iter().all(|name| left.contains(name)),
        "{running:?} {left:?}\n{said}"
    );
    let out = held.release();
    let said = common::cli::describe(&out);
    assert!(holds_every_shape(&f), "{said}");
    assert_eq!(drift_scratch(&f.ww_lib), Vec::<String>::new(), "{said}");
}

/// A file write git declines part way, after it wrote the files the move
/// adds: once what stopped it is cleared, the printed choice finishes the
/// refresh, nothing having been edited.
#[cfg(unix)]
#[test]
fn a_file_write_declined_part_way_is_finished_by_the_printed_choice() {
    let f = fixture();
    std::fs::create_dir_all(f.ww_lib.join("ro")).unwrap();
    commit_file(&f.ww_lib, "ro/x.txt", "x\n", "ro");
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("ro/x.txt"), "x2\n").unwrap();
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        std::fs::write(elsewhere.join("new.txt"), "new\n").unwrap();
    });
    set_writable(&f.ww_lib.join("ro"), false);
    let out = common::cli::run(&["lock"], &f.ww);
    set_writable(&f.ww_lib.join("ro"), true);
    let said = common::cli::refused(&out, State::EditsDriftLeftInPlace);
    assert_eq!(
        read(&f.ww_lib.join("new.txt")).as_deref(),
        Some("new\n"),
        "git wrote the added file before it stopped:\n{said}"
    );

    let commands: Vec<String> = choice_lines(&said)
        .into_iter()
        .filter(|l| l.starts_with("rwv ") || l.starts_with("git "))
        .collect();
    assert_eq!(commands, vec!["rwv doctor --fix".to_owned()], "{said}");
    let words: Vec<&str> = commands[0].split_whitespace().collect();

    let out = common::cli::run(&words[1..], &f.ww);

    let said = common::cli::describe(&out);
    assert!(said.contains("[fixed] core: drift in"), "{said}");
    assert_eq!(
        common::git_in(&f.ww_lib, &["status", "--porcelain"]),
        "",
        "{said}"
    );
    assert_eq!(read(&f.ww_lib.join("ro/x.txt")).as_deref(), Some("x2\n"));
    let out = common::cli::run(&["lock"], &f.ww);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
}

/// Another process taking the index between the refresh's two writes: the
/// index write fails, the files the move adds stand written, and once the
/// index is free the next verb finishes the refresh.
#[test]
fn an_index_lock_taken_between_the_writes_reads_as_drift() {
    let f = fixture();
    move_of_every_shape(&f);
    let lock = common::git_in(
        &f.ww_lib,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "index.lock",
        ],
    );
    let held = held_landing(&f, "drift-write-records");
    std::fs::write(&lock, "").unwrap();

    let out = held.release();

    let said = left_in_place(&out, "the write failed");
    assert!(holds_every_shape(&f), "{said}");
    std::fs::remove_file(&lock).unwrap();

    let out = common::cli::run(&["lock"], &f.ww);

    let said = common::cli::describe(&out);
    assert!(out.status.success(), "{said}");
    assert_eq!(
        common::git_in(&f.ww_lib, &["status", "--porcelain"]),
        "",
        "{said}"
    );
}

/// The operator stages an edit of one moved file between the refresh's two
/// writes: that file alone is held as moved and edited; every path the move
/// added, deleted or swapped still reads as drift.
#[test]
fn an_edit_staged_between_the_writes_is_the_only_one_held() {
    let f = fixture();
    move_of_every_shape(&f);
    let held = held_landing(&f, "drift-write-records");
    std::fs::write(f.ww_lib.join("notes.txt"), "operator staged this\n").unwrap();
    common::git_in(&f.ww_lib, &["add", "notes.txt"]);

    let out = held.release();
    common::cli::refused(&out, State::EditsDriftLeftInPlace);

    let out = common::cli::run(&["lock"], &f.ww);

    let said = common::cli::refused(&out, State::EditsMovedAndEdited);
    assert_eq!(
        listing_under(&said, "edits moved and edited"),
        vec!["notes.txt".to_owned()],
        "{said}"
    );
    assert_eq!(
        read(&f.ww_lib.join("notes.txt")).as_deref(),
        Some("operator staged this\n")
    );
}

/// A second verb run while the first stands between its two writes reads
/// the checkout as drift and finishes the refresh; the first then finishes
/// too.
#[test]
fn a_verb_run_between_another_refreshs_writes_finishes_it() {
    let f = fixture();
    move_of_every_shape(&f);
    let held = held_landing(&f, "drift-write-records");

    let out = common::cli::run(&["doctor", "--fix"], &f.ww);

    let said = common::cli::describe(&out);
    assert_eq!(
        common::git_in(&f.ww_lib, &["status", "--porcelain"]),
        "",
        "{said}"
    );
    let out = held.release();
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    assert_eq!(common::git_in(&f.ww_lib, &["status", "--porcelain"]), "");
    assert!(holds_every_shape(&f));
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
/// initial checkout and write every file, the ignored ones standing at paths
/// the move did not touch included (an untracked one makes it refuse
/// whole, so every file here is ignored). Nothing reads as moved under it.
#[test]
fn a_worktree_with_no_index_is_not_refreshed() {
    let f = fixture();
    f.move_from_elsewhere("ww/project", "new.txt", "new\n");
    let common_dir = common::git_in(
        &f.ww_lib,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    std::fs::create_dir_all(Path::new(&common_dir).join("info")).unwrap();
    std::fs::write(Path::new(&common_dir).join("info/exclude"), "*.md\n*.txt\n").unwrap();
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
/// the fix names the drift left in place and its route; running that route
/// finds the file both moved and edited and writes nothing to the checkout,
/// and the saved file is kept throughout.
#[test]
fn doctor_fix_names_a_declined_write_and_running_it_again_holds_the_checkout() {
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

    let said = common::cli::refused(&out, State::EditsMovedAndEdited);
    assert!(!said.contains("refreshed drift"), "{said}");
    assert_eq!(read(&f.ww_lib.join("notes.txt")).as_deref(), Some("mine\n"));
    assert_eq!(read(&f.ww_lib.join("new.txt")), None, "{said}");
}

/// `rwv <args>` in `dir`, held at `point`.
fn held_at(f: &Fixture, dir: &Path, args: &[&str], point: &str) -> common::pause::Paused {
    let mut cmd = common::rwv_std();
    cmd.args(args).current_dir(dir);
    common::pause::Paused::spawn(cmd, point, &f.scratch.join(format!("pause-{point}")))
        .unwrap_or_else(|e| panic!("rwv {args:?} reaches {point}: {e}"))
}

/// Runs the printed choice line holding `holding`, as printed: a `git -C`
/// line through git, an `rwv` line through the binary under test.
fn run_printed(said: &str, holding: &str, dir: &Path) -> std::process::Output {
    let line = said
        .lines()
        .map(str::trim)
        .find(|l| (l.starts_with("git -C ") || l.starts_with("rwv ")) && l.contains(holding))
        .unwrap_or_else(|| panic!("no printed line holds `{holding}`:\n{said}"));
    let words: Vec<&str> = line.split_whitespace().collect();
    match words[0] {
        "rwv" => common::cli::run(&words[1..], dir),
        _ => std::process::Command::new(words[0])
            .args(&words[1..])
            .output()
            .unwrap(),
    }
}

const MERGE: [&str; 3] = ["doctor", "--fix", "--merge-edited-drift"];

/// The seat's `notes.txt` edited while the move changed it and turned
/// `spare.txt` into a directory, the merge then killed once the checkout is
/// set back.
fn set_back_by_a_killed_merge(f: &Fixture) {
    std::fs::write(f.ww_lib.join("notes.txt"), "mine\n").unwrap();
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        std::fs::remove_file(elsewhere.join("spare.txt")).unwrap();
        std::fs::create_dir_all(elsewhere.join("spare.txt")).unwrap();
        std::fs::write(elsewhere.join("spare.txt/x"), "x\n").unwrap();
    });
    drop(held_at(f, &f.ww, &MERGE, "drift-merge-set-back"));
    assert_eq!(
        common::git_in(&f.ww_lib, &["status", "--porcelain"]),
        "M notes.txt",
        "set back, only the edit differs from HEAD"
    );
}

/// A merge killed once set back, where the move swapped a file for a
/// directory: every verb names it `set back` with no commit, and the printed
/// merge finishes it, the directory in place and the edit kept.
#[test]
fn a_merge_killed_where_the_move_swaps_a_file_for_a_directory_is_finished_by_the_flag() {
    let f = fixture();
    set_back_by_a_killed_merge(&f);
    for verb in [&["land"][..], &["lock"], &["doctor", "--fix"]] {
        let out = common::cli::run(verb, &f.ww);
        let stderr = common::cli::refused(&out, State::EditsSetBack);
        assert!(!stderr.contains("commit"), "{verb:?}:\n{stderr}");
        assert!(
            stderr.contains("rwv doctor --fix --merge-edited-drift"),
            "{verb:?}:\n{stderr}"
        );
    }
    let out = common::cli::run(&["land"], &f.ww);
    let out = run_printed(
        &String::from_utf8_lossy(&out.stderr),
        "merge-edited-drift",
        &f.ww,
    );
    let said = common::cli::describe(&out);
    assert!(said.contains("left conflicted"), "{said}");
    assert!(f.ww_lib.join("spare.txt/x").is_file(), "{said}");
    let notes = read(&f.ww_lib.join("notes.txt")).unwrap();
    assert!(notes.contains("mine") && notes.contains("three"), "{notes}");
}

/// Work saved in a set-back checkout before the merge is run again is in
/// the backup that run takes and in the result: nothing is put back over it.
#[test]
fn work_saved_after_a_killed_merge_is_kept_by_the_next_run() {
    let f = fixture();
    set_back_by_a_killed_merge(&f);
    std::fs::write(
        f.ww_lib.join("notes.txt"),
        "mine, and an hour of new work\n",
    )
    .unwrap();
    std::fs::write(f.ww_lib.join("README.md"), "new work beside it\n").unwrap();

    let out = common::cli::run(&MERGE, &f.ww);

    let said = common::cli::describe(&out);
    assert!(said.contains("kept as backup"), "{said}");
    assert!(
        read(&f.ww_lib.join("notes.txt"))
            .unwrap()
            .contains("an hour of new work"),
        "{said}"
    );
    assert_eq!(
        read(&f.ww_lib.join("README.md")).as_deref(),
        Some("new work beside it\n"),
        "{said}"
    );
    let backup = common::git_in(
        &f.ww_lib,
        &[
            "for-each-ref",
            "--sort=-creatordate",
            "--count=1",
            "--format=%(objectname)",
            "refs/rwv/",
        ],
    );
    assert!(
        common::git_in(&f.ww_lib, &["show", &format!("{backup}:notes.txt")])
            .contains("an hour of new work"),
        "the run's backup holds the work: {backup}\n{said}"
    );
}

/// With the reflog entry that marks the checkout set back gone, the checkout
/// reads as detached with edits: the printed stash keeps the edits alone
/// against the tip the files hold, and `rwv update` returns the checkout to
/// its branch, after which the stash merges onto the moved tip.
#[test]
fn a_set_back_checkout_whose_reflog_is_gone_is_exited_by_its_printed_choices() {
    let f = fixture();
    set_back_by_a_killed_merge(&f);
    common::git_in(&f.ww_lib, &["reflog", "expire", "--expire=now", "--all"]);

    let out = common::cli::run(&["lock"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDirty);
    assert!(stderr.contains("alignment detached"), "{stderr}");
    let out = run_printed(&stderr, "stash push", &f.ww);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    assert!(
        !common::git_in(&f.ww_lib, &["stash", "show", "-p"]).contains("three"),
        "the stash holds the edit and not the move's reverse"
    );
    let out = run_printed(&stderr, "rwv update", &f.ww);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    assert_eq!(
        common::git_in(&f.ww_lib, &["symbolic-ref", "--short", "HEAD"]),
        "ww/project"
    );
    assert!(f.ww_lib.join("spare.txt/x").is_file());
    assert!(common::git_in(&f.ww_lib, &["stash", "show", "-p"]).contains("+mine"));
}

/// A set-back checkout whose index a killed git left locked: every verb
/// names the lock and offers only its removal then the merge; the flag run
/// over the lock prints git's own text, and once it is removed finishes.
#[test]
fn a_set_back_checkout_with_a_left_index_lock_names_it_and_offers_no_choice_that_fails_on_it() {
    let f = fixture();
    set_back_by_a_killed_merge(&f);
    let lock = PathBuf::from(common::git_in(
        &f.ww_lib,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "index.lock",
        ],
    ));
    std::fs::write(&lock, "").unwrap();

    let out = common::cli::run(&["land"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsSetBack);
    assert!(stderr.contains(&lock.display().to_string()), "{stderr}");
    assert!(
        stderr.contains("remove the lock on the VCS's record of the checkout"),
        "{stderr}"
    );
    assert!(!stderr.contains("set-aside-edited-drift"), "{stderr}");
    let out = common::cli::run(&MERGE, &f.ww);
    let stderr = common::cli::refused(&out, State::EditsSetBack);
    assert!(stderr.contains("index.lock': File exists"), "{stderr}");
    std::fs::remove_file(&lock).unwrap();
    let out = common::cli::run(&MERGE, &f.ww);
    let said = common::cli::describe(&out);
    assert!(said.contains("left conflicted"), "{said}");
    assert!(f.ww_lib.join("spare.txt/x").is_file(), "{said}");
}

/// Something untracked where the move adds a file or a directory holds the
/// checkout with the move-aside choice alone, and no verb records around it;
/// once moved aside, the merge brings the move's files in.
#[test]
fn untracked_content_where_the_move_adds_a_path_holds_the_checkout_until_moved_aside() {
    let f = fixture();
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        std::fs::write(elsewhere.join("todo"), "theirs\n").unwrap();
        std::fs::create_dir_all(elsewhere.join("cfg")).unwrap();
        std::fs::write(elsewhere.join("cfg/a.txt"), "theirs\n").unwrap();
    });
    std::fs::create_dir_all(f.ww_lib.join("todo")).unwrap();
    std::fs::write(f.ww_lib.join("todo/mine.txt"), "my notes\n").unwrap();
    std::fs::write(f.ww_lib.join("cfg"), "my cfg\n").unwrap();

    let out = common::cli::run(&["lock"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsMovedAndEdited);
    assert!(!stderr.contains("commit"), "{stderr}");
    assert!(stderr.contains("move out of the way"), "{stderr}");
    assert_eq!(read(&f.ww_lib.join("notes.txt")).as_deref(), Some("two\n"));
    std::fs::rename(f.ww_lib.join("todo"), f.scratch.join("todo")).unwrap();
    std::fs::rename(f.ww_lib.join("cfg"), f.scratch.join("cfg")).unwrap();
    let out = run_printed(&stderr, "merge-edited-drift", &f.ww);
    let said = common::cli::describe(&out);
    assert!(said.contains("refreshed drift"), "{said}");
    assert!(!said.contains("inapplicable-flag"), "{said}");
    assert_eq!(read(&f.ww_lib.join("todo")).as_deref(), Some("theirs\n"));
    assert_eq!(
        read(&f.ww_lib.join("notes.txt")).as_deref(),
        Some("three\n")
    );
}

/// A both-file below a link: the first refusal offers the move-aside alone,
/// and nothing is written through the link.
#[cfg(unix)]
#[test]
fn a_file_moved_and_edited_below_a_link_is_offered_the_move_aside_first() {
    let f = fixture();
    let outside = f.scratch.join("outside");
    std::fs::create_dir_all(f.ww_lib.join("d")).unwrap();
    commit_file(&f.ww_lib, "d/f.txt", "one\ntwo\nthree\n", "d");
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("d/f.txt"), "one\ntwo\nTHREE\n").unwrap();
    });
    std::fs::rename(f.ww_lib.join("d"), &outside).unwrap();
    std::os::unix::fs::symlink(&outside, f.ww_lib.join("d")).unwrap();
    std::fs::write(outside.join("f.txt"), "mine\ntwo\nthree\n").unwrap();

    let out = common::cli::run(&["lock"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsMovedAndEdited);
    let choices = choice_lines(&stderr);
    assert!(choices.iter().all(|l| !l.contains("set-aside")), "{stderr}");
    assert!(stderr.contains("move out of the way"), "{stderr}");
    let out = common::cli::run(&MERGE, &f.ww);
    common::cli::refused(&out, State::EditsMovedAndEdited);
    assert_eq!(
        read(&outside.join("f.txt")).as_deref(),
        Some("mine\ntwo\nthree\n")
    );
}

/// A link both moved and edited stays a link under the set-aside: the
/// moved tip's link stands, and the stash merges the operator's back.
#[cfg(unix)]
#[test]
fn a_link_moved_and_edited_stays_a_link_when_set_aside() {
    let f = fixture();
    std::os::unix::fs::symlink("a", f.ww_lib.join("link")).unwrap();
    common::git_in(&f.ww_lib, &["add", "link"]);
    common::git_in(&f.ww_lib, &["commit", "-q", "-m", "link"]);
    f.move_with("ww/project", |elsewhere| {
        std::fs::remove_file(elsewhere.join("link")).unwrap();
        std::os::unix::fs::symlink("b", elsewhere.join("link")).unwrap();
    });
    std::fs::remove_file(f.ww_lib.join("link")).unwrap();
    std::os::unix::fs::symlink("c", f.ww_lib.join("link")).unwrap();

    let out = common::cli::run(&["doctor", "--fix", "--set-aside-edited-drift"], &f.ww);

    let said = common::cli::describe(&out);
    assert!(said.contains("set aside as"), "{said}");
    assert_eq!(
        std::fs::read_link(f.ww_lib.join("link")).unwrap(),
        PathBuf::from("b"),
        "{said}"
    );
    assert_eq!(
        common::git_in(&f.ww_lib, &["status", "--porcelain"]),
        "",
        "{said}"
    );
    assert!(
        common::git_in(&f.ww_lib, &["stash", "show", "-p"]).contains("+c"),
        "{said}"
    );
}

/// A sparse checkout whose move changes a path outside the sparse set:
/// the refresh updates that entry and writes no file, and no verb is left
/// with drift it cannot refresh.
#[test]
fn a_move_outside_the_sparse_set_is_refreshed() {
    let f = fixture();
    std::fs::create_dir_all(f.ww_lib.join("out")).unwrap();
    commit_file(&f.ww_lib, "out/b.txt", "o\n", "out");
    common::git_in(
        &f.ww_lib,
        &["sparse-checkout", "set", "--no-cone", "/*", "!/out/"],
    );
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("out/b.txt"), "o2\n").unwrap();
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
    });

    let out = common::cli::run(&["lock"], &f.ww);

    let said = common::cli::describe(&out);
    assert!(out.status.success(), "{said}");
    assert_eq!(
        read(&f.ww_lib.join("notes.txt")).as_deref(),
        Some("three\n")
    );
    assert!(!f.ww_lib.join("out/b.txt").exists(), "{said}");
    assert_eq!(common::git_in(&f.ww_lib, &["show", ":out/b.txt"]), "o2");
}

/// A landing refused before it knows its target reads drift without
/// refreshing it: pure drift gets no `edits` line, and an edit beside it is
/// listed with the refresh as its one choice, never a commit.
#[test]
fn a_landing_refused_before_its_target_lists_no_drift_as_edits() {
    let f = fixture();
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");

    let out = common::cli::run(&["land", "--target", "nosuch"], &f.ww);

    let said = common::cli::describe(&out);
    assert!(!out.status.success(), "{said}");
    assert!(!said.contains(": edits "), "{said}");
    assert!(!said.contains("commit your edits"), "{said}");
    assert_eq!(read(&f.ww_lib.join("notes.txt")).as_deref(), Some("two\n"));

    std::fs::write(f.ww_lib.join("README.md"), "edited\n").unwrap();
    let out = common::cli::run(&["land", "--target", "nosuch"], &f.ww);
    let stderr = common::cli::refused(&out, State::EditsDriftLeftInPlace);
    assert_eq!(
        listing_under(&stderr, "edits drift left in place"),
        vec!["README.md".to_owned()],
        "{stderr}"
    );
    assert!(!stderr.contains("commit your edits"), "{stderr}");
    let out = run_printed(&stderr, "rwv doctor --fix", &f.ww);
    assert!(
        common::cli::describe(&out).contains("refreshed drift"),
        "{}",
        common::cli::describe(&out)
    );
    let out = common::cli::run(&["land", "--target", "nosuch"], &f.ww);
    let stderr = common::cli::refused(&out, State::EditsDirty);
    assert_eq!(
        listing_under(&stderr, "edits dirty"),
        vec!["README.md".to_owned()],
        "{stderr}"
    );
}

/// A tracked name that is not UTF-8 beside drift elsewhere: the drift is
/// read and refreshed, and an edit beside it is judged as any other.
#[cfg(target_os = "linux")]
#[test]
fn drift_beside_a_name_that_is_not_utf8_is_read_and_refreshed() {
    use std::os::unix::ffi::OsStrExt;
    let f = fixture();
    let odd = std::ffi::OsStr::from_bytes(b"caf\xe9.txt");
    std::fs::write(f.ww_lib.join(odd), "x\n").unwrap();
    common::git_in(&f.ww_lib, &["add", "-A"]);
    common::git_in(&f.ww_lib, &["commit", "-q", "-m", "odd name"]);
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");
    std::fs::write(f.ww_lib.join("README.md"), "edited\n").unwrap();

    let out = common::cli::run(&["lock"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDirty);
    assert!(stderr.contains("refreshed drift"), "{stderr}");
    assert_eq!(edits_listing(&stderr), vec!["README.md".to_owned()]);
    assert_eq!(
        read(&f.ww_lib.join("notes.txt")).as_deref(),
        Some("three\n")
    );
}

/// Drift at a name that is not UTF-8 cannot be read: every verb refuses with
/// no commit, offering the stash that keeps every difference, and once that
/// has run as printed the verb proceeds.
#[cfg(target_os = "linux")]
#[test]
fn drift_at_a_name_that_is_not_utf8_is_exited_by_the_printed_stash() {
    use std::os::unix::ffi::OsStrExt;
    let f = fixture();
    let odd = std::ffi::OsStr::from_bytes(b"caf\xe9.txt");
    std::fs::write(f.ww_lib.join(odd), "x\n").unwrap();
    common::git_in(&f.ww_lib, &["add", "-A"]);
    common::git_in(&f.ww_lib, &["commit", "-q", "-m", "odd name"]);
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join(odd), "x2\n").unwrap();
    });

    for verb in [&["lock"][..], &["land"]] {
        let out = common::cli::run(verb, &f.ww);
        let stderr = common::cli::refused(&out, State::EditsDriftLeftInPlace);
        assert!(stderr.contains("could not be read"), "{verb:?}:\n{stderr}");
        assert!(!stderr.contains("commit"), "{verb:?}:\n{stderr}");
    }
    let said = common::cli::describe(&common::cli::run(&["doctor"], &f.ww));
    assert!(said.contains("could not be read"), "{said}");
    assert!(!said.contains("commit them if they are yours"), "{said}");
    let out = common::cli::run(&["lock"], &f.ww);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let out = run_printed(&stderr, "stash push", &f.ww);
    assert!(out.status.success(), "{}", common::cli::describe(&out));

    let out = common::cli::run(&["lock"], &f.ww);

    assert!(out.status.success(), "{}", common::cli::describe(&out));
    assert_eq!(std::fs::read(f.ww_lib.join(odd)).unwrap(), b"x2\n".to_vec());
}

/// With the set-back mark gone, the `dirty` refusal's commit, made on the
/// detached HEAD, records the edit alone on the tip the files hold; `rwv
/// update` then leaves the checkout and names the commit, which no branch
/// reaches, as `detached with unpushed commits`.
#[test]
fn a_commit_made_where_the_set_back_mark_is_gone_records_the_edit_alone() {
    let f = fixture();
    set_back_by_a_killed_merge(&f);
    let at = common::git_in(&f.ww_lib, &["rev-parse", "HEAD"]);
    common::git_in(&f.ww_lib, &["reflog", "expire", "--expire=now", "--all"]);
    let out = common::cli::run(&["lock"], &f.ww);
    let stderr = common::cli::refused(&out, State::EditsDirty);
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("git -C ") && l.ends_with(" commit"))
        .unwrap_or_else(|| panic!("{stderr}"));
    let words: Vec<&str> = line.split_whitespace().collect();
    let ran = std::process::Command::new(words[0])
        .args(&words[1..])
        .args(["-a", "-m", "mine"])
        .output()
        .unwrap();
    assert!(ran.status.success(), "{}", common::cli::describe(&ran));
    assert_eq!(common::git_in(&f.ww_lib, &["rev-parse", "HEAD^"]), at);
    assert_eq!(
        common::git_in(&f.ww_lib, &["diff", "--name-only", "HEAD^", "HEAD"]),
        "notes.txt"
    );
    let mine = common::git_in(&f.ww_lib, &["rev-parse", "HEAD"]);

    let out = common::cli::run(&["update"], &f.ww);

    let said = common::cli::describe(&out);
    assert!(said.contains("detached with unpushed commits"), "{said}");
    assert!(said.contains(&mine[..12]), "{said}");
    assert_eq!(
        common::git_in(&f.ww_lib, &["rev-parse", "HEAD"]),
        mine,
        "{said}"
    );
}

// ---------------------------------------------------------------------------
// The project repo's own drift, before a verb commits there
// ---------------------------------------------------------------------------

const MOVED_NOTE: &str = "# moved note\n";

impl Fixture {
    fn ww_project(&self) -> PathBuf {
        self.ww.join("projects").join(PROJECT)
    }

    /// Moves the workweave's project branch forward by one commit that
    /// `change` makes in a checkout of its own.
    fn move_project_with(&self, change: impl FnOnce(&Path)) -> String {
        let elsewhere = self.scratch.join("project-elsewhere");
        common::git_in(
            &self.primary_project,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                elsewhere.to_str().unwrap(),
                "ww/project",
            ],
        );
        change(&elsewhere);
        common::git_in(&elsewhere, &["add", "-A"]);
        common::git_in(&elsewhere, &["commit", "-q", "-m", "moved elsewhere"]);
        let moved = common::git_in(&elsewhere, &["rev-parse", "HEAD"]);
        common::git_in(&elsewhere, &["update-ref", "refs/heads/ww/project", &moved]);
        common::git_in(
            &self.primary_project,
            &["worktree", "remove", elsewhere.to_str().unwrap()],
        );
        moved
    }

    /// The project branch moves under the workweave, appending a note to
    /// `rwv.toml` and adding `NOTES.md`.
    fn move_project_manifest(&self) -> String {
        self.move_project_with(|elsewhere| {
            let manifest = elsewhere.join("rwv.toml");
            let mut text = std::fs::read_to_string(&manifest).unwrap();
            text.push_str(MOVED_NOTE);
            std::fs::write(&manifest, text).unwrap();
            std::fs::write(elsewhere.join("NOTES.md"), "notes\n").unwrap();
        })
    }
}

fn committed_manifest(project: &Path) -> String {
    common::git_in(project, &["show", "HEAD:rwv.toml"])
}

/// `rwv lock --commit` over a project repo whose branch moved under it, with
/// no edit there: the drift is refreshed before the manifest is read, and
/// the commit carries the lock alone, the move's lines kept.
#[test]
fn lock_commit_over_a_moved_project_branch_refreshes_it_and_commits_the_lock_alone() {
    let f = fixture();
    let moved = f.move_project_manifest();
    let project = f.ww_project();

    let out = common::cli::run(&["lock", "--commit"], &f.ww);

    let said = common::cli::describe(&out);
    assert!(out.status.success(), "{said}");
    assert!(said.contains("refreshed drift"), "{said}");
    assert_eq!(
        common::git_in(&project, &["rev-parse", "HEAD^"]),
        moved,
        "{said}"
    );
    assert_eq!(
        common::git_in(&project, &["diff", "--name-only", "HEAD^", "HEAD"]),
        "rwv.lock",
        "{said}"
    );
    assert!(
        committed_manifest(&project).contains(MOVED_NOTE.trim()),
        "{said}"
    );
    assert_eq!(read(&project.join("NOTES.md")).as_deref(), Some("notes\n"));
    assert_eq!(common::git_in(&project, &["status", "--porcelain"]), "");
}

/// Plain `rwv lock` refreshes the project repo's drift too.
#[test]
fn lock_over_a_moved_project_branch_refreshes_it() {
    let f = fixture();
    f.move_project_manifest();
    let project = f.ww_project();

    let out = common::cli::run(&["lock"], &f.ww);

    let said = common::cli::describe(&out);
    assert!(out.status.success(), "{said}");
    assert!(said.contains("refreshed drift"), "{said}");
    assert!(
        read(&project.join("rwv.toml"))
            .unwrap()
            .contains(MOVED_NOTE.trim()),
        "{said}"
    );
    assert_eq!(
        common::git_in(
            &project,
            &["status", "--porcelain", "--", "rwv.toml", "NOTES.md"]
        ),
        "",
        "{said}"
    );
}

/// An edit of the operator's own in a `rwv.toml` the move also changed: both
/// lock verbs refuse it as moved and edited, commit nothing, and the printed
/// merge brings the edit onto the moved tip, after which the commit keeps
/// both.
#[test]
fn a_project_manifest_moved_and_edited_is_held_until_merged() {
    let f = fixture();
    let moved = f.move_project_manifest();
    let project = f.ww_project();
    let manifest = project.join("rwv.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, format!("# mine\n{text}")).unwrap();

    for verb in [&["lock"][..], &["lock", "--commit"]] {
        let out = common::cli::run(verb, &f.ww);
        let stderr = common::cli::refused(&out, State::EditsMovedAndEdited);
        assert!(
            listing_under(&stderr, "edits moved and edited").contains(&"rwv.toml".to_owned()),
            "{verb:?}:\n{stderr}"
        );
        assert!(!stderr.contains("commit"), "{verb:?}:\n{stderr}");
        assert_eq!(common::git_in(&project, &["rev-parse", "HEAD"]), moved);
    }
    let out = common::cli::run(&["lock", "--commit"], &f.ww);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let out = run_printed(&stderr, "merge-edited-drift", &f.ww);
    let said = common::cli::describe(&out);
    assert!(
        said.contains("drift in projects/app") && said.contains("merged onto"),
        "{said}"
    );

    let out = common::cli::run(&["lock", "--commit"], &f.ww);

    let said = common::cli::describe(&out);
    assert!(out.status.success(), "{said}");
    let committed = committed_manifest(&project);
    assert!(
        committed.contains("# mine") && committed.contains(MOVED_NOTE.trim()),
        "{committed}\n{said}"
    );
    assert!(
        common::git_in(&project, &["ls-files", "--", "NOTES.md"]) == "NOTES.md"
            && common::git_in(&project, &["status", "--porcelain", "--", "NOTES.md"]).is_empty(),
        "{said}"
    );
}

/// The move is another workspace's member add: the manifest and lock gain an
/// entry. `rwv lock --commit` here never commits the entry's removal.
#[test]
fn a_member_added_by_a_move_of_the_project_branch_is_not_removed_by_lock_commit() {
    let f = fixture();
    let url = common::file_url(&f.primary_lib);
    let added = "github/acme/other";
    f.move_project_with(|elsewhere| {
        let manifest = elsewhere.join("rwv.toml");
        let mut text = std::fs::read_to_string(&manifest).unwrap();
        text.push_str(&format!(
            "\n[repositories.\"{added}\"]\ntype = \"git\"\nurl = \"{url}\"\nbranch = \"main\"\n\
             role = \"reference\"\n"
        ));
        std::fs::write(&manifest, text).unwrap();
    });
    let project = f.ww_project();

    let out = common::cli::run(&["lock", "--commit"], &f.ww);

    let said = common::cli::describe(&out);
    assert!(committed_manifest(&project).contains(added), "{said}");
    assert!(
        read(&project.join("rwv.toml")).unwrap().contains(added),
        "{said}"
    );
}

/// `rwv pull`, `rwv update --remote` and `rwv cherry-pick` judge the project
/// repo's drift before they read its manifest: an edit of `rwv.toml` the
/// branch's move also changed refuses each as moved and edited, and nothing
/// in the project repo is written.
#[test]
fn pull_update_remote_and_cherry_pick_hold_a_moved_and_edited_project_manifest() {
    let verbs: [&[&str]; 3] = [&["pull"], &["update", "--remote"], &["cherry-pick", "HEAD"]];
    for verb in verbs {
        let f = fixture();
        let moved = f.move_project_manifest();
        let project = f.ww_project();
        let manifest = project.join("rwv.toml");
        let text = std::fs::read_to_string(&manifest).unwrap();
        std::fs::write(&manifest, format!("# mine\n{text}")).unwrap();
        let before = std::fs::read_to_string(&manifest).unwrap();

        let out = common::cli::run(verb, &f.ww);

        let stderr = common::cli::refused(&out, State::EditsMovedAndEdited);
        assert!(
            listing_under(&stderr, "edits moved and edited").contains(&"rwv.toml".to_owned()),
            "{verb:?}:\n{stderr}"
        );
        assert_eq!(
            common::git_in(&project, &["rev-parse", "HEAD"]),
            moved,
            "{verb:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&manifest).unwrap(),
            before,
            "{verb:?}"
        );
    }
}

/// The printed merge leaves `rwv.toml` conflicted, the edit here and the
/// move's both at its end: the next verb names it `unresolved` with its
/// choice, before reading the manifest.
#[test]
fn a_project_manifest_left_conflicted_by_the_merge_is_unresolved() {
    let f = fixture();
    f.move_project_manifest();
    let project = f.ww_project();
    let manifest = project.join("rwv.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, format!("{text}# seat note\n")).unwrap();
    let out = common::cli::run(&MERGE, &f.ww);
    let said = common::cli::describe(&out);
    assert!(said.contains("left conflicted"), "{said}");

    let out = common::cli::run(&["lock"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsUnresolved);
    assert!(
        listing_under(&stderr, "edits unresolved").contains(&"rwv.toml".to_owned()),
        "{stderr}"
    );
    assert!(!stderr.contains("failed to parse"), "{stderr}");
}

/// A merge of the operator's own stopped on a conflict in `rwv.toml`: the
/// verb names the operation in progress, not the conflict alone.
#[test]
fn a_project_repo_stopped_in_a_merge_is_named_mid_operation() {
    let f = fixture();
    let project = f.ww_project();
    let manifest = project.join("rwv.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    common::git_in(&project, &["branch", "side"]);
    std::fs::write(&manifest, format!("{text}# mine\n")).unwrap();
    common::git_in(&project, &["commit", "-q", "-am", "mine"]);
    common::git_in(&project, &["checkout", "-q", "side"]);
    std::fs::write(&manifest, format!("{text}# side\n")).unwrap();
    common::git_in(&project, &["commit", "-q", "-am", "side"]);
    common::git_in(&project, &["checkout", "-q", "ww/project"]);
    let merged = common::git()
        .args(["merge", "side"])
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(!merged.status.success());

    let out = common::cli::run(&["lock", "--commit"], &f.ww);

    common::cli::refused(&out, State::EditsCommitMidOperation);
}

/// A project repo a merge of its edits left set back is named `set back`
/// alone by `rwv status`, `rwv cherry-pick` and `rwv switch`: no judgment
/// calls it detached or offers committing its manifest beside that line.
#[test]
fn a_set_back_project_repo_is_named_set_back_alone() {
    let f = fixture();
    f.move_project_manifest();
    let project = f.ww_project();
    let manifest = project.join("rwv.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, format!("# mine\n{text}")).unwrap();
    drop(held_at(&f, &f.ww, &MERGE, "drift-merge-set-back"));
    assert!(!on_branch(&project));
    let head = common::git_in(&project, &["rev-parse", "HEAD"]);

    for verb in [
        &["status"][..],
        &["cherry-pick", "HEAD"],
        &["switch", "ww/project"],
        &["doctor"],
    ] {
        let out = common::cli::run(verb, &f.ww);

        let said = common::cli::describe(&out);
        assert!(
            said.contains("projects/app: edits set back"),
            "{verb:?}:\n{said}"
        );
        assert!(
            !said.contains("project detached") && !said.contains("project-detached"),
            "{verb:?}:\n{said}"
        );
        assert!(!said.contains("uncommitted"), "{verb:?}:\n{said}");
        assert_eq!(common::git_in(&project, &["rev-parse", "HEAD"]), head);
    }
}

/// Each run of every leaf of rwv's command tree the sweep below makes, as the
/// words after `rwv`; `<other>` is a repository no weave holds.
const SWEPT: &[(&str, &[&[&str]])] = &[
    (
        "activate",
        &[&["activate", PROJECT], &["activate", PROJECT, "--update"]],
    ),
    ("materialize", &[&["materialize"]]),
    ("prime", &[&["prime"]]),
    ("resolve", &[&["resolve"]]),
    ("clone", &[&["clone", "<other>"]]),
    ("init", &[&["init", "fresh"]]),
    ("member add", &[&["member", "add", "<other>"]]),
    ("member remove", &[&["member", "remove", LIB]]),
    ("member set-role", &[&["member", "set-role", LIB, "fork"]]),
    ("member pin", &[&["member", "pin", LIB, "HEAD"]]),
    (
        "member set-branch",
        &[&["member", "set-branch", LIB, "main"]],
    ),
    ("member set-url", &[&["member", "set-url", LIB, "<other>"]]),
    (
        "member set-recurse-submodules",
        &[&["member", "set-recurse-submodules", LIB, "true"]],
    ),
    ("branch list", &[&["branch", "list"]]),
    ("branch set-mode", &[&["branch", "set-mode", "declared"]]),
    ("branch set-history", &[&["branch", "set-history", "merge"]]),
    ("branch rename", &[&["branch", "rename", "nb"]]),
    ("branch create", &[&["branch", "create", "nb"]]),
    ("branch delete", &[&["branch", "delete", "main"]]),
    (
        "switch",
        &[&["switch", "main"], &["switch", "--detach", "HEAD"]],
    ),
    ("workweave create", &[&["workweave", "create", "w2"]]),
    ("workweave delete", &[&["workweave", "delete", "ww"]]),
    ("workweave list", &[&["workweave", "list"]]),
    (
        "workweave set-default-create-basedir",
        &[&["workweave", "set-default-create-basedir", "elsewhere"]],
    ),
    ("doctor", &[&["doctor"], &["doctor", "--fix"]]),
    (
        "lock",
        &[&["lock"], &["lock", "--commit"], &["lock", "--dirty"]],
    ),
    ("status", &[&["status"]]),
    ("log", &[&["log"]]),
    ("abort", &[&["abort"]]),
    ("backup list", &[&["backup", "list"]]),
    ("backup drop", &[&["backup", "drop", LIB, "--all"]]),
    ("backup prune", &[&["backup", "prune"]]),
    ("sync", &[&["sync"]]),
    ("land", &[&["land"]]),
    ("cherry-pick", &[&["cherry-pick", "HEAD"]]),
    ("fetch", &[&["fetch"]]),
    ("pull", &[&["pull"]]),
    (
        "update",
        &[
            &["update"],
            &["update", "--remote"],
            &["update", "--remote", "--commit"],
        ],
    ),
    ("push", &[&["push"], &["push", "--dry-run"]]),
    ("completions", &[&["completions", "bash"]]),
    ("explain", &[&["explain", "edits"]]),
    ("setup claude", &[&["setup", "claude"]]),
];

/// Every leaf of rwv's command tree, hidden ones included, as the words that
/// name it.
fn leaf_verbs() -> Vec<String> {
    use clap::CommandFactory;
    fn walk(cmd: &clap::Command, prefix: &str, out: &mut Vec<String>) {
        let subs: Vec<&clap::Command> = cmd
            .get_subcommands()
            .filter(|sub| sub.get_name() != "help")
            .collect();
        if subs.is_empty() && !prefix.is_empty() {
            out.push(prefix.to_owned());
        }
        for sub in subs {
            let name = match prefix {
                "" => sub.get_name().to_owned(),
                _ => format!("{prefix} {}", sub.get_name()),
            };
            walk(sub, &name, out);
        }
    }
    let mut out = Vec::new();
    walk(&repoweave::cli::Cli::command(), "", &mut out);
    out
}

/// Which checkout of the workweave the sweep moves the branch of.
#[derive(Debug, Clone, Copy)]
enum Swept {
    Project,
    Member,
}

/// What a run left in `checkout`, whose branch moved to `moved` adding
/// `witness`, that reads the move's reverse as the checkout's own: a commit
/// containing the move whose tree lacks the witness, or the checkout's
/// record naming HEAD as where it stands while what it holds still differs
/// from HEAD.
fn reverse_taken(checkout: &Path, moved: &str, witness: &str) -> Option<String> {
    let head = common::git_in(checkout, &["rev-parse", "HEAD"]);
    let contains = common::git()
        .args(["merge-base", "--is-ancestor", moved, &head])
        .current_dir(checkout)
        .status()
        .unwrap()
        .success();
    if contains && common::git_in(checkout, &["ls-tree", "--name-only", "HEAD", witness]).is_empty()
    {
        return Some(format!("HEAD {head} records the move's reverse"));
    }
    let newest = common::git_in(checkout, &["log", "-g", "-n1", "--format=%H", "HEAD"]);
    let standing = common::git_in(
        checkout,
        &[
            "status",
            "--porcelain",
            "--untracked-files=no",
            "--",
            ".",
            ":!rwv.lock",
        ],
    );
    (newest == head && !standing.is_empty())
        .then(|| format!("its record says it stands at HEAD {head}, beside:\n{standing}"))
}

/// Every leaf verb, run over a workweave whose project branch, or whose
/// member's branch, was moved forward under it: none commits the move's
/// reverse, and none leaves the checkout reading as standing at HEAD while it
/// still holds the old tip's content. A verb that moves, renames or switches a
/// branch, or commits, settles the drift first or refuses with nothing
/// changed. The population is the command tree itself, so a new verb fails
/// here until it is given its runs above.
fn sweep(swept: Swept) {
    let leaves = leaf_verbs();
    assert!(leaves.contains(&"branch rename".to_owned()), "{leaves:?}");
    let unlisted: Vec<&String> = leaves
        .iter()
        .filter(|leaf| !SWEPT.iter().any(|(name, _)| name == leaf))
        .collect();
    assert!(
        unlisted.is_empty(),
        "give these verbs their runs in SWEPT: {unlisted:?}"
    );
    let mut taken = Vec::new();
    for (_, runs) in SWEPT {
        for args in *runs {
            let f = fixture();
            let other = f.scratch.join("other");
            init_repo(&other);
            commit_file(&other, "o.txt", "o\n", "o");
            let other_url = common::file_url(&other);
            let (checkout, moved, witness) = match swept {
                Swept::Project => (f.ww_project(), f.move_project_manifest(), "NOTES.md"),
                Swept::Member => (
                    f.ww_lib.clone(),
                    f.move_with("ww/project", |elsewhere| {
                        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
                        std::fs::write(elsewhere.join("witness.txt"), "w\n").unwrap();
                    }),
                    "witness.txt",
                ),
            };
            let args: Vec<&str> = args
                .iter()
                .map(|a| match *a {
                    "<other>" => other_url.as_str(),
                    a => a,
                })
                .collect();
            let home = f.scratch.join("home");
            std::fs::create_dir_all(&home).unwrap();

            let out = common::rwv()
                .args(&args)
                .current_dir(&f.ww)
                .env("HOME", &home)
                .output()
                .unwrap();

            if let Some(why) = reverse_taken(&checkout, &moved, witness) {
                taken.push(format!("{args:?}: {why}\n{}", common::cli::describe(&out)));
            }
        }
    }
    assert!(taken.is_empty(), "{}", taken.join("\n\n"));
}

#[test]
fn no_verb_takes_the_reverse_of_a_project_branch_move() {
    sweep(Swept::Project);
}

#[test]
fn no_verb_takes_the_reverse_of_a_member_branch_move() {
    sweep(Swept::Member);
}

const RENAME: [&str; 3] = ["branch", "rename", "nb"];
const CREATE: [&str; 3] = ["branch", "create", "nb"];

/// Whether `repo` has a local branch `nb`.
fn has_nb(repo: &Path) -> bool {
    !common::git_in(repo, &["branch", "--list", "nb"]).is_empty()
}

/// `rwv branch rename` and `rwv branch create` over a member whose branch
/// moved under it: the drift is refreshed before any branch is renamed or
/// switched, so the checkout stands at the moved tip, renamed where the
/// rename goes ahead. (The create refuses this fixture's member for its
/// alignment, after the refresh.)
#[test]
fn renaming_or_creating_a_branch_refreshes_a_member_left_behind_first() {
    for verb in [RENAME, CREATE] {
        let f = fixture();
        let moved = f.move_with("ww/project", |elsewhere| {
            std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        });

        let out = common::cli::run(&verb, &f.ww);

        let said = common::cli::describe(&out);
        assert_eq!(out.status.success(), verb == RENAME, "{verb:?}:\n{said}");
        assert!(said.contains("refreshed drift"), "{verb:?}:\n{said}");
        assert_eq!(common::git_in(&f.ww_lib, &["rev-parse", "HEAD"]), moved);
        assert_eq!(
            common::git_in(&f.ww_lib, &["status", "--porcelain"]),
            "",
            "{verb:?}:\n{said}"
        );
        assert_eq!(
            read(&f.ww_lib.join("notes.txt")).as_deref(),
            Some("three\n")
        );
    }
}

/// The member states a refresh cannot finish refuse `rwv branch rename` and
/// `rwv branch create` with no branch renamed or created anywhere: files both
/// the move and an edit changed, a checkout whose record of where it stood is
/// gone, and one a merge of its edits left set back.
#[test]
fn renaming_or_creating_a_branch_refuses_a_member_held_by_its_drift() {
    for verb in [RENAME, CREATE] {
        let f = fixture();
        f.move_with("ww/project", |elsewhere| {
            std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        });
        std::fs::write(f.ww_lib.join("notes.txt"), "mine\n").unwrap();
        let out = common::cli::run(&verb, &f.ww);
        common::cli::refused(&out, State::EditsMovedAndEdited);
        assert!(!has_nb(&f.ww_lib) && !has_nb(&f.ww_project()), "{verb:?}");
        assert_eq!(read(&f.ww_lib.join("notes.txt")).as_deref(), Some("mine\n"));

        let f = fixture_without_entry(NoEntry::ExpiredByHand);
        move_notes(&f);
        let out = common::cli::run(&verb, &f.ww);
        common::cli::refused(&out, State::EditsDriftLeftInPlace);
        assert!(!has_nb(&f.ww_lib) && !has_nb(&f.ww_project()), "{verb:?}");

        let f = fixture();
        f.move_with("ww/project", |elsewhere| {
            std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        });
        std::fs::write(f.ww_lib.join("notes.txt"), "mine\n").unwrap();
        drop(held_at(&f, &f.ww, &MERGE, "drift-merge-set-back"));
        assert!(!on_branch(&f.ww_lib));
        let out = common::cli::run(&verb, &f.ww);
        common::cli::refused(&out, State::EditsSetBack);
        assert!(!has_nb(&f.ww_lib) && !has_nb(&f.ww_project()), "{verb:?}");
    }
}

/// `rwv branch rename` and `rwv branch create` over a project branch moved
/// under the workweave: the drift is refreshed before the branch is renamed
/// or switched, and the next `rwv lock --commit` keeps the move's lines.
/// Where `rwv.toml` was edited too, both refuse as moved and edited.
#[test]
fn renaming_or_creating_a_branch_settles_the_project_repo_first() {
    for verb in [RENAME, CREATE] {
        let f = fixture();
        f.move_project_manifest();
        let project = f.ww_project();

        let out = common::cli::run(&verb, &f.ww);

        let said = common::cli::describe(&out);
        assert!(out.status.success(), "{verb:?}:\n{said}");
        assert!(said.contains("refreshed drift"), "{verb:?}:\n{said}");
        assert_eq!(
            common::git_in(&project, &["symbolic-ref", "--short", "HEAD"]),
            "nb"
        );
        let out = common::cli::run(&["lock", "--commit"], &f.ww);
        let said = common::cli::describe(&out);
        assert!(
            committed_manifest(&project).contains(MOVED_NOTE.trim()),
            "{verb:?}:\n{said}"
        );

        let f = fixture();
        f.move_project_manifest();
        let project = f.ww_project();
        let manifest = project.join("rwv.toml");
        let text = std::fs::read_to_string(&manifest).unwrap();
        std::fs::write(&manifest, format!("# mine\n{text}")).unwrap();
        let out = common::cli::run(&verb, &f.ww);
        common::cli::refused(&out, State::EditsMovedAndEdited);
        assert!(!has_nb(&project), "{verb:?}");
    }
}

// ---------------------------------------------------------------------------
// A refresh stopped between its writes
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn set_writable(dir: &Path, writable: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if writable { 0o755 } else { 0o555 };
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A refresh git declines part way through its file write, then a hand
/// refresh and a staged deletion and revert: the checkout took the move in a
/// way its history does not show, and the two files hold the old tip again,
/// which no reading tells from drift a refresh never wrote. They are kept as
/// they stand, held with the stash alone and no commit.
#[cfg(unix)]
#[test]
fn staged_returns_after_a_declined_refresh_and_a_hand_refresh_are_held() {
    let f = fixture();
    std::fs::create_dir_all(f.ww_lib.join("ro")).unwrap();
    commit_file(&f.ww_lib, "ro/x.txt", "x\n", "ro");
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("ro/x.txt"), "x2\n").unwrap();
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        std::fs::write(elsewhere.join("new.txt"), "new\n").unwrap();
    });
    set_writable(&f.ww_lib.join("ro"), false);
    let out = common::cli::run(&["lock"], &f.ww);
    set_writable(&f.ww_lib.join("ro"), true);
    common::cli::refused(&out, State::EditsDriftLeftInPlace);
    common::git_in(&f.ww_lib, &["checkout", "HEAD", "--", "."]);
    assert_eq!(common::git_in(&f.ww_lib, &["status", "--porcelain"]), "");
    common::git_in(&f.ww_lib, &["rm", "-q", "new.txt"]);
    common::git_in(&f.ww_lib, &["checkout", "HEAD~1", "--", "notes.txt"]);
    let kept = ["new.txt", "notes.txt"];
    let before = path_state(&f.ww_lib, &kept);

    let out = common::cli::run(&["lock"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDriftLeftInPlace);
    assert!(stderr.contains("new.txt, notes.txt"), "{stderr}");
    assert!(!stderr.contains("refreshed"), "{stderr}");
    assert!(!stderr.contains(" commit\n"), "{stderr}");
    assert_eq!(path_state(&f.ww_lib, &kept), before, "{stderr}");
}

/// A refresh git declines part way through its file write, then a hand
/// `git add` of one file it wrote: the file it never wrote still holds the
/// old tip, which no reading tells from a return to it once part of the move
/// is taken by hand. It is held with the stash alone, never offered a
/// commit, and the printed stash leaves the checkout at HEAD.
#[cfg(unix)]
#[test]
fn a_file_a_declined_refresh_never_wrote_is_held_after_a_hand_add() {
    let f = fixture();
    std::fs::create_dir_all(f.ww_lib.join("ro")).unwrap();
    commit_file(&f.ww_lib, "ro/x.txt", "x\n", "ro");
    commit_file(&f.ww_lib, "zz.txt", "z\n", "zz");
    let moved = f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        std::fs::write(elsewhere.join("ro/x.txt"), "x2\n").unwrap();
        std::fs::write(elsewhere.join("zz.txt"), "z2\n").unwrap();
    });
    set_writable(&f.ww_lib.join("ro"), false);
    let out = common::cli::run(&["lock"], &f.ww);
    set_writable(&f.ww_lib.join("ro"), true);
    common::cli::refused(&out, State::EditsDriftLeftInPlace);
    assert_eq!(read(&f.ww_lib.join("ro/x.txt")).as_deref(), Some("x\n"));
    common::git_in(&f.ww_lib, &["add", "notes.txt"]);

    let out = common::cli::run(&["lock"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDriftLeftInPlace);
    assert!(stderr.contains("ro/x.txt"), "{stderr}");
    assert!(!stderr.contains(" commit\n"), "{stderr}");
    assert_eq!(read(&f.ww_lib.join("ro/x.txt")).as_deref(), Some("x\n"));
    run_every_choice(&stderr, &f.ww);
    assert_eq!(common::git_in(&f.ww_lib, &["rev-parse", "HEAD"]), moved);
    assert_eq!(common::git_in(&f.ww_lib, &["status", "--porcelain"]), "");
    let out = common::cli::run(&["lock"], &f.ww);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
}

/// A refresh killed once its files are written, then a second move of the
/// branch before any verb: nothing of the first move is ever offered as a
/// commit. A path only the first move changed reads as drift in the index
/// alone; one both moves changed holds the first move's content, which no
/// reading tells from an edit, so it is held as moved and edited, and the
/// printed merge, the first move's change being part of the second tip,
/// brings the checkout to that tip.
#[test]
fn a_refresh_killed_between_its_writes_then_a_second_move_offers_no_commit() {
    let f = fixture();
    commit_file(&f.ww_lib, "u.txt", "a\nm\nm\nm\nb\n", "u");
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        std::fs::write(elsewhere.join("u.txt"), "a2\nm\nm\nm\nb\n").unwrap();
    });
    drop(held_landing(&f, "drift-write-records"));
    assert_eq!(
        read(&f.ww_lib.join("u.txt")).as_deref(),
        Some("a2\nm\nm\nm\nb\n")
    );
    let second = f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("u.txt"), "a2\nm\nm\nm\nb2\n").unwrap();
    });

    let out = common::cli::run(&["doctor", "--fix"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsMovedAndEdited);
    assert_eq!(
        listing_under(&stderr, "edits moved and edited"),
        vec!["u.txt".to_owned()],
        "{stderr}"
    );
    assert!(!stderr.contains("commit"), "{stderr}");
    let out = run_printed(&stderr, "merge-edited-drift", &f.ww);
    let said = common::cli::describe(&out);
    assert_eq!(common::git_in(&f.ww_lib, &["rev-parse", "HEAD"]), second);
    assert_eq!(
        common::git_in(&f.ww_lib, &["status", "--porcelain"]),
        "",
        "{said}"
    );
    without_the_killed_landing(&f);
    let out = common::cli::run(&["lock"], &f.ww);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
}

/// A refresh killed once its files are written, then a second move that
/// puts a file back to the content it had before the first: the move from
/// the first tip to the second no longer touches that file, so the first
/// move's content it still holds reads as an edit made here, offered a
/// commit like any other. That is the one shape a stop between the writes
/// leaves that reads as the operator's own edit.
#[test]
fn a_file_the_second_move_puts_back_after_a_killed_refresh_reads_as_an_edit() {
    let f = fixture();
    commit_file(&f.ww_lib, "z.txt", "z\n", "z");
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        std::fs::write(elsewhere.join("z.txt"), "z2\n").unwrap();
    });
    drop(held_landing(&f, "drift-write-records"));
    assert_eq!(read(&f.ww_lib.join("z.txt")).as_deref(), Some("z2\n"));
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("z.txt"), "z\n").unwrap();
    });

    without_the_killed_landing(&f);
    let out = common::cli::run(&["lock"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDirty);
    assert!(stderr.contains("refreshed drift"), "{stderr}");
    assert_eq!(edits_listing(&stderr), vec!["z.txt".to_owned()], "{stderr}");
    assert!(stderr.contains("commit"), "{stderr}");
    assert_eq!(read(&f.ww_lib.join("z.txt")).as_deref(), Some("z2\n"));
    assert_eq!(
        read(&f.ww_lib.join("notes.txt")).as_deref(),
        Some("three\n")
    );
}

// ---------------------------------------------------------------------------
// The set-aside stash: whose it is, what it holds, and its take-back lines
// ---------------------------------------------------------------------------

const SET_ASIDE: [&str; 3] = ["doctor", "--fix", "--set-aside-edited-drift"];

fn stashes(repo: &Path) -> Vec<String> {
    common::git_in(repo, &["stash", "list", "--format=%H"])
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The seat and the primary stand at one tip on different branches. A
/// set-aside finished in the seat leaves a stash based on that tip; the
/// primary, set back over the same tip by a merge git refused, neither names
/// that stash nor prints a line that applies it.
#[test]
fn a_set_back_checkout_never_names_another_workspaces_stash() {
    let f = fixture();
    common::git_in(&f.ww_lib, &["reset", "-q", "--hard", "HEAD~1"]);
    let fork = common::git_in(&f.ww_lib, &["rev-parse", "HEAD"]);
    assert_eq!(common::git_in(&f.primary_lib, &["rev-parse", "HEAD"]), fork);
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");
    std::fs::write(f.ww_lib.join("notes.txt"), "seat's\n").unwrap();
    let out = common::cli::run(&SET_ASIDE, &f.ww);
    assert!(
        common::cli::describe(&out).contains("set aside as"),
        "{}",
        common::cli::describe(&out)
    );
    let seats = stashes(&f.ww_lib);
    assert_eq!(seats.len(), 1);

    f.move_from_elsewhere("main", "notes.txt", "main moved\n");
    std::fs::write(f.primary_lib.join("notes.txt"), "primary's\n").unwrap();
    std::fs::write(f.primary_lib.join("st.txt"), "staged\n").unwrap();
    common::git_in(&f.primary_lib, &["add", "st.txt"]);
    let out = common::cli::run(&MERGE, &f.primary);
    let said = common::cli::describe(&out);
    common::cli::refused(&out, State::EditsSetBack);
    assert!(!said.contains(&seats[0][..12]), "{said}");
    assert!(!said.contains("stash apply"), "{said}");

    let out = common::cli::run(&["lock"], &f.primary);
    let said = common::cli::describe(&out);
    common::cli::refused(&out, State::EditsSetBack);
    assert!(!said.contains(&seats[0][..12]), "{said}");
}

/// A finished set-aside's stash, kept, and a later merge in the same
/// workspace set back over a newer tip of the same branch: the judgment names
/// no stash, since none was made against the tip the files hold now.
#[test]
fn a_set_back_checkout_never_names_a_stash_made_against_another_tip() {
    let f = fixture();
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");
    std::fs::write(f.ww_lib.join("notes.txt"), "first\n").unwrap();
    let out = common::cli::run(&SET_ASIDE, &f.ww);
    assert!(common::cli::describe(&out).contains("set aside as"));
    let first = stashes(&f.ww_lib);
    assert_eq!(first.len(), 1);
    f.move_from_elsewhere("ww/project", "notes.txt", "four\n");
    std::fs::write(f.ww_lib.join("notes.txt"), "second\n").unwrap();
    drop(held_at(&f, &f.ww, &MERGE, "drift-merge-set-back"));

    let out = common::cli::run(&["lock"], &f.ww);

    let said = common::cli::describe(&out);
    common::cli::refused(&out, State::EditsSetBack);
    assert!(!said.contains(&first[0][..12]), "{said}");
    assert!(!said.contains("stash apply"), "{said}");
}

/// The set-aside stashes the listed files alone: an edit to a file the move
/// did not change stays in the working tree and is not in the stash.
#[test]
fn a_set_aside_keeps_an_unrelated_edit_in_place() {
    let f = fixture();
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");
    std::fs::write(f.ww_lib.join("notes.txt"), "mine\n").unwrap();
    std::fs::write(f.ww_lib.join("README.md"), "unrelated\n").unwrap();

    let out = common::cli::run(&SET_ASIDE, &f.ww);

    let said = common::cli::describe(&out);
    assert!(said.contains("set aside as"), "{said}");
    assert_eq!(
        read(&f.ww_lib.join("README.md")).as_deref(),
        Some("unrelated\n"),
        "{said}"
    );
    assert_eq!(
        common::git_in(&f.ww_lib, &["stash", "show", "--name-only"]),
        "notes.txt"
    );
    assert_eq!(
        read(&f.ww_lib.join("notes.txt")).as_deref(),
        Some("three\n")
    );
}

/// A set-aside stopped once its stash is made has already printed the line
/// that takes it back. Edited again and set aside again, the finished report
/// prints both take-back lines, the earlier first, each set-aside holding an
/// edit of the same file. Run as printed, with what the first brings back
/// staged before the next as the report says, both edits come back.
#[test]
fn two_set_asides_from_one_set_back_print_both_take_back_lines_oldest_first() {
    let f = fixture();
    commit_file(&f.ww_lib, "u.txt", "a\nm\nm\nm\nb\nm\nm\nm\nc\n", "u");
    f.move_from_elsewhere("ww/project", "u.txt", "A\nm\nm\nm\nb\nm\nm\nm\nc\n");
    std::fs::write(f.ww_lib.join("u.txt"), "a\nm\nm\nm\nb1\nm\nm\nm\nc\n").unwrap();
    let stopped = held_at(&f, &f.ww, &SET_ASIDE, "drift-merge-carry").kill();
    let first = stashes(&f.ww_lib);
    assert_eq!(first.len(), 1);
    let said = common::cli::describe(&stopped);
    assert!(
        said.contains(&format!("stash apply {}", first[0])),
        "the take-back line is printed as the stash is made: {said}"
    );
    std::fs::write(f.ww_lib.join("u.txt"), "a\nm\nm\nm\nb\nm\nm\nm\nc2\n").unwrap();

    let out = common::cli::run(&SET_ASIDE, &f.ww);

    let said = common::cli::describe(&out);
    let all = stashes(&f.ww_lib);
    assert_eq!(all.len(), 2, "{said}");
    let (newest, oldest) = (&all[0], &all[1]);
    assert_eq!(oldest, &first[0]);
    let at_oldest = said
        .find(&format!("stash apply {oldest}"))
        .unwrap_or_else(|| panic!("{said}"));
    let at_newest = said
        .rfind(&format!("stash apply {newest}"))
        .unwrap_or_else(|| panic!("{said}"));
    assert!(at_oldest < at_newest, "{said}");
    assert_eq!(
        common::git_in(&f.ww_lib, &["symbolic-ref", "--short", "HEAD"]),
        "ww/project"
    );
    assert!(
        said.contains("record or commit what each brings back before the next"),
        "{said}"
    );

    let out = run_printed(&said, &format!("stash apply {oldest}"), &f.ww);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    common::git_in(&f.ww_lib, &["add", "-u"]);
    let out = run_printed(&said, &format!("stash apply {newest}"), &f.ww);

    assert!(out.status.success(), "{}", common::cli::describe(&out));
    assert_eq!(
        read(&f.ww_lib.join("u.txt")).as_deref(),
        Some("A\nm\nm\nm\nb1\nm\nm\nm\nc2\n")
    );
}

/// An entry only intended for adding beside a both-file, merged: git refuses
/// it, and every printed line that shows a staged version from the backup
/// runs; the entry has no staged version, so none names it.
#[test]
fn an_intent_to_add_entry_beside_a_refused_merge_is_shown_by_no_failing_line() {
    let f = fixture();
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");
    std::fs::write(f.ww_lib.join("notes.txt"), "mine\n").unwrap();
    std::fs::write(f.ww_lib.join("README.md"), "staged\n").unwrap();
    common::git_in(&f.ww_lib, &["add", "README.md"]);
    std::fs::write(f.ww_lib.join("newf.txt"), "new work\n").unwrap();
    common::git_in(&f.ww_lib, &["add", "-N", "newf.txt"]);

    let out = common::cli::run(&MERGE, &f.ww);

    common::cli::refused(&out, State::EditsSetBack);
    let said = common::cli::describe(&out);
    let shown: Vec<&str> = said
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("git -C ") && l.contains(" show "))
        .collect();
    assert!(!shown.is_empty(), "{said}");
    assert!(!shown.iter().any(|l| l.contains("newf.txt")), "{said}");
    for line in shown {
        let out = run_printed(line, " show ", &f.ww);
        assert!(
            out.status.success(),
            "{line}: {}",
            common::cli::describe(&out)
        );
    }
}

/// An entry only intended for adding beside a both-file: the stash refuses
/// it, and the set-back judgment prints the unstage that clears it before
/// the merge; run as printed, the two finish, the new file kept.
#[test]
fn an_intent_to_add_entry_in_a_set_back_checkout_is_cleared_by_the_printed_unstage() {
    let f = fixture();
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");
    std::fs::write(f.ww_lib.join("notes.txt"), "mine\n").unwrap();
    std::fs::write(f.ww_lib.join("newf.txt"), "new work\n").unwrap();
    common::git_in(&f.ww_lib, &["add", "-N", "newf.txt"]);

    let out = common::cli::run(&SET_ASIDE, &f.ww);

    let stderr = common::cli::refused(&out, State::EditsSetBack);
    assert!(stderr.contains("restore --staged"), "{stderr}");
    let said = common::cli::describe(&out);
    for shown in said.lines().map(str::trim).filter(|l| l.contains(" show ")) {
        let out = run_printed(shown, " show ", &f.ww);
        assert!(
            out.status.success(),
            "{shown}: {}",
            common::cli::describe(&out)
        );
    }
    let out = run_printed(&stderr, "restore --staged", &f.ww);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    let out = run_printed(&stderr, "merge-edited-drift", &f.ww);
    let said = common::cli::describe(&out);
    assert!(said.contains("drift in github/acme/lib"), "{said}");
    assert_eq!(
        common::git_in(&f.ww_lib, &["symbolic-ref", "--short", "HEAD"]),
        "ww/project",
        "{said}"
    );
    let notes = read(&f.ww_lib.join("notes.txt")).unwrap();
    assert!(notes.contains("mine") && notes.contains("three"), "{notes}");
    assert_eq!(
        read(&f.ww_lib.join("newf.txt")).as_deref(),
        Some("new work\n")
    );
}

/// A merge stopped once set back, then a second move of the branch adding a
/// file where an ignored one of the operator's stands: the rerun holds the
/// checkout as the first run would, writing nothing over the ignored file,
/// and its printed choices, run as printed, finish with the file kept.
#[test]
fn a_rerun_from_set_back_holds_over_an_ignored_file_the_branch_now_adds() {
    let f = fixture();
    std::fs::write(f.ww_lib.join("notes.txt"), "mine\n").unwrap();
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");
    drop(held_at(&f, &f.ww, &MERGE, "drift-merge-carry"));
    f.move_from_elsewhere("ww/project", "x.log", "theirs log\n");
    let exclude = PathBuf::from(common::git_in(
        &f.ww_lib,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ))
    .join("info/exclude");
    std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    std::fs::write(&exclude, "x.log\n").unwrap();
    std::fs::write(f.ww_lib.join("x.log"), "my log\n").unwrap();

    let out = common::cli::run(&MERGE, &f.ww);

    let stderr = common::cli::refused(&out, State::EditsSetBack);
    assert!(stderr.contains("move out of the way"), "{stderr}");
    assert!(
        listing_under(&stderr, "edits set back").contains(&"x.log".to_owned()),
        "{stderr}"
    );
    assert_eq!(read(&f.ww_lib.join("x.log")).as_deref(), Some("my log\n"));
    std::fs::create_dir_all(&f.scratch).unwrap();
    std::fs::rename(f.ww_lib.join("x.log"), f.scratch.join("x.log")).unwrap();
    let out = run_printed(&stderr, "merge-edited-drift", &f.ww);

    let said = common::cli::describe(&out);
    assert!(on_branch(&f.ww_lib), "{said}");
    assert_eq!(
        read(&f.ww_lib.join("x.log")).as_deref(),
        Some("theirs log\n")
    );
    assert_eq!(read(&f.scratch.join("x.log")).as_deref(), Some("my log\n"));
}

/// An edit beside an ignored file of the operator's where the move adds
/// one: the first run offers the choice the rerun does, moving the file out
/// of the way, and run as printed it finishes, both copies kept.
#[test]
fn an_ignored_file_where_the_move_adds_one_is_moved_aside_then_merged() {
    let f = fixture();
    commit_file(&f.ww_lib, "u.txt", "a\nm\nm\nm\nb\n", "u");
    let exclude = PathBuf::from(common::git_in(
        &f.ww_lib,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ))
    .join("info/exclude");
    std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    std::fs::write(&exclude, "x.log\n").unwrap();
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("u.txt"), "a2\nm\nm\nm\nb\n").unwrap();
        std::fs::write(elsewhere.join("x.log"), "theirs log\n").unwrap();
        common::git_in(elsewhere, &["add", "-f", "x.log"]);
    });
    std::fs::write(f.ww_lib.join("u.txt"), "a\nm\nm\nm\nb2\n").unwrap();
    std::fs::write(f.ww_lib.join("x.log"), "my log\n").unwrap();

    let out = common::cli::run(&MERGE, &f.ww);

    let stderr = common::cli::refused(&out, State::EditsMovedAndEdited);
    assert!(stderr.contains("move out of the way"), "{stderr}");
    assert!(!stderr.contains("add -f"), "{stderr}");
    assert!(
        listing_under(&stderr, "edits moved and edited").contains(&"x.log".to_owned()),
        "{stderr}"
    );
    std::fs::create_dir_all(&f.scratch).unwrap();
    std::fs::rename(f.ww_lib.join("x.log"), f.scratch.join("x.log")).unwrap();
    let out = run_printed(&stderr, "merge-edited-drift", &f.ww);

    let said = common::cli::describe(&out);
    assert!(said.contains("merged onto"), "{said}");
    assert!(on_branch(&f.ww_lib), "{said}");
    assert_eq!(
        read(&f.ww_lib.join("u.txt")).as_deref(),
        Some("a2\nm\nm\nm\nb2\n")
    );
    assert_eq!(
        read(&f.ww_lib.join("x.log")).as_deref(),
        Some("theirs log\n")
    );
    assert_eq!(read(&f.scratch.join("x.log")).as_deref(), Some("my log\n"));
}

/// `rwv` run in `dir` with a `git` first on `PATH` that fails every listing
/// of ignored files and passes every other call to the real one.
#[cfg(unix)]
fn run_with_ignored_listing_failing(f: &Fixture, args: &[&str], dir: &Path) -> String {
    let bin = f.scratch.join("failing-git");
    std::fs::create_dir_all(&bin).unwrap();
    let real = String::from_utf8(
        std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let wrapper = bin.join("git");
    common::write_executable(
        &wrapper,
        format!(
            "#!/bin/sh\ncase \" $* \" in *\" --others --ignored --exclude-standard \"*) \
             echo 'fatal: the listing failed' >&2; exit 128;; esac\nexec {} \"$@\"\n",
            real.trim()
        ),
    );
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = common::rwv_std()
        .args(args)
        .current_dir(dir)
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", common::cli::describe(&out));
    common::cli::describe(&out)
}

/// The files the VCS does not compare cannot be listed: the merge holds the
/// checkout on its first run and on a rerun from set back, writing nothing,
/// and says why; once the listing works the merge finishes.
#[cfg(unix)]
#[test]
fn a_failed_listing_of_uncompared_files_holds_the_merge_first_and_on_a_rerun() {
    let f = fixture();
    std::fs::write(f.ww_lib.join("notes.txt"), "mine\n").unwrap();
    f.move_from_elsewhere("ww/project", "notes.txt", "three\n");
    let head = common::git_in(&f.ww_lib, &["rev-parse", "HEAD"]);

    let said = run_with_ignored_listing_failing(&f, &MERGE, &f.ww);

    assert!(said.contains("edits moved and edited"), "{said}");
    assert!(said.contains("could not be listed, so no backup"), "{said}");
    assert!(
        said.contains("clear what the VCS says stopped it"),
        "{said}"
    );
    assert_eq!(common::git_in(&f.ww_lib, &["rev-parse", "HEAD"]), head);
    assert_eq!(read(&f.ww_lib.join("notes.txt")).as_deref(), Some("mine\n"));

    drop(held_at(&f, &f.ww, &MERGE, "drift-merge-carry"));
    assert!(!on_branch(&f.ww_lib));
    let back = common::git_in(&f.ww_lib, &["rev-parse", "HEAD"]);

    let said = run_with_ignored_listing_failing(&f, &MERGE, &f.ww);

    assert!(said.contains("edits set back"), "{said}");
    assert!(said.contains("could not be listed, so no backup"), "{said}");
    assert!(
        said.contains("clear what the VCS says stopped it"),
        "{said}"
    );
    assert_eq!(common::git_in(&f.ww_lib, &["rev-parse", "HEAD"]), back);
    assert!(!on_branch(&f.ww_lib), "{said}");
    let out = common::cli::run(&MERGE, &f.ww);
    let said = common::cli::describe(&out);
    assert!(on_branch(&f.ww_lib), "{said}");
}

fn on_branch(repo: &Path) -> bool {
    common::git()
        .args(["symbolic-ref", "-q", "HEAD"])
        .current_dir(repo)
        .output()
        .unwrap()
        .status
        .success()
}

/// The branch a set-back checkout left is taken by another worktree: the
/// merge's refusal names where, offers freeing it, and once freed the
/// printed merge finishes.
#[test]
fn a_set_back_checkout_whose_branch_is_taken_is_offered_freeing_it() {
    let f = fixture();
    set_back_by_a_killed_merge(&f);
    let other = f.scratch.join("taker");
    common::git_in(
        &f.primary_lib,
        &[
            "worktree",
            "add",
            "-q",
            other.to_str().unwrap(),
            "ww/project",
        ],
    );

    let out = common::cli::run(&MERGE, &f.ww);

    let stderr = common::cli::refused(&out, State::EditsSetBack);
    assert!(stderr.contains("is checked out at"), "{stderr}");
    assert!(stderr.contains("out of the other checkout"), "{stderr}");
    assert!(!stderr.contains("move out of the way"), "{stderr}");
    common::git_in(
        &f.primary_lib,
        &["worktree", "remove", other.to_str().unwrap()],
    );
    let out = run_printed(&stderr, "merge-edited-drift", &f.ww);
    let said = common::cli::describe(&out);
    assert!(on_branch(&f.ww_lib), "{said}");
}

/// The branch a set-back checkout left was deleted: the merge's refusal says
/// so and offers creating it again; once created, the printed merge finishes.
#[test]
fn a_set_back_checkout_whose_branch_is_gone_is_offered_creating_it_again() {
    let f = fixture();
    set_back_by_a_killed_merge(&f);
    let tip = common::git_in(&f.primary_lib, &["rev-parse", "ww/project"]);
    common::git_in(&f.primary_lib, &["branch", "-q", "-D", "ww/project"]);

    let out = common::cli::run(&MERGE, &f.ww);

    let stderr = common::cli::refused(&out, State::EditsSetBack);
    assert!(stderr.contains("no longer exists"), "{stderr}");
    assert!(stderr.contains("create the branch it left"), "{stderr}");
    assert!(!stderr.contains("move out of the way"), "{stderr}");
    common::git_in(&f.primary_lib, &["branch", "-q", "ww/project", &tip]);
    let out = run_printed(&stderr, "merge-edited-drift", &f.ww);
    let said = common::cli::describe(&out);
    assert!(on_branch(&f.ww_lib), "{said}");
}

/// A set-back checkout under `rwv update`, `rwv switch` and `rwv status`:
/// each names it set back, beside no `alignment detached` line and no choice
/// that runs `rwv update` or `rwv lock --commit`.
#[test]
fn update_switch_and_status_name_a_set_back_checkout_alone() {
    let f = fixture();
    set_back_by_a_killed_merge(&f);
    for verb in [&["update"][..], &["switch", "ww/project"], &["status"]] {
        let out = common::cli::run(verb, &f.ww);
        let said = common::cli::describe(&out);
        assert!(said.contains("set back"), "{verb:?}:\n{said}");
        assert!(!said.contains("alignment detached"), "{verb:?}:\n{said}");
        let offered = choice_lines(&said);
        assert!(
            !offered
                .iter()
                .any(|l| l == "rwv update" || l.starts_with("rwv lock --commit")),
            "{verb:?}:\n{said}"
        );
        assert!(!said.contains("rwv lock --commit"), "{verb:?}:\n{said}");
    }
}

// ---------------------------------------------------------------------------
// A checkout whose reflog holds no entry
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum NoEntry {
    ExpiredByHand,
    ExpiredWhileIdle,
    NeverLogged,
}

const NO_ENTRY: [NoEntry; 3] = [
    NoEntry::ExpiredByHand,
    NoEntry::ExpiredWhileIdle,
    NoEntry::NeverLogged,
];

fn fixture_without_entry(how: NoEntry) -> Fixture {
    let f = match how {
        NoEntry::NeverLogged => fixture_with_lib_config(&[("core.logAllRefUpdates", "false")]),
        _ => fixture(),
    };
    match how {
        NoEntry::ExpiredByHand => {
            common::git_in(&f.ww_lib, &["reflog", "expire", "--expire=now", "--all"]);
        }
        NoEntry::ExpiredWhileIdle => {
            let two_hundred_days_on = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 200 * 86400;
            let out = std::process::Command::new("git")
                .args([
                    "reflog",
                    "expire",
                    "--expire=90.days.ago",
                    "--expire-unreachable=30.days.ago",
                    "--all",
                ])
                .env("GIT_TEST_DATE_NOW", two_hundred_days_on.to_string())
                .current_dir(&f.ww_lib)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", common::cli::describe(&out));
        }
        NoEntry::NeverLogged => {}
    }
    assert_eq!(
        std::fs::metadata(f.ww_lib.join(common::git_in(
            &f.ww_lib,
            &["rev-parse", "--git-path", "logs/HEAD"]
        )))
        .map(|m| m.len())
        .unwrap_or(0),
        0,
        "{how:?}: the checkout's HEAD log holds nothing"
    );
    f
}

fn move_notes(f: &Fixture) {
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        std::fs::write(elsewhere.join("new.txt"), "new\n").unwrap();
    });
}

/// The commands the `choices:` block prints.
fn printed_commands(stderr: &str) -> Vec<String> {
    choice_lines(stderr)
        .into_iter()
        .filter(|l| l.starts_with("git -C ") || l.starts_with("rwv "))
        .collect()
}

/// Run every printed command as printed.
fn run_every_choice(stderr: &str, dir: &Path) {
    let commands = printed_commands(stderr);
    assert!(!commands.is_empty(), "{stderr}");
    for line in &commands {
        let out = run_printed(stderr, line, dir);
        assert!(
            out.status.success(),
            "{line}:\n{}",
            common::cli::describe(&out)
        );
    }
}

/// A branch moved from another worktree under a checkout whose reflog holds
/// no entry: rwv cannot tell the move's reverse from edits, so every verb
/// holds the checkout with no choice that commits, and the printed stash
/// leaves it at HEAD.
#[test]
fn a_move_under_a_checkout_with_no_reflog_entry_is_held_with_no_commit_offered() {
    for how in NO_ENTRY {
        let f = fixture_without_entry(how);
        move_notes(&f);
        let before = path_state(&f.ww_lib, &["notes.txt", "new.txt"]);

        for verb in [&["lock"][..], &["land"]] {
            let out = common::cli::run(verb, &f.ww);
            let stderr = common::cli::refused(&out, State::EditsDriftLeftInPlace);
            assert!(
                stderr.contains("where this checkout of"),
                "{how:?} {verb:?}:\n{stderr}"
            );
            assert!(!stderr.contains("commit"), "{how:?} {verb:?}:\n{stderr}");
            assert_eq!(
                path_state(&f.ww_lib, &["notes.txt", "new.txt"]),
                before,
                "{how:?} {verb:?}: nothing was written"
            );
        }
        let said = common::cli::describe(&common::cli::run(&["doctor"], &f.ww));
        assert!(said.contains("where this checkout of"), "{how:?}:\n{said}");
        assert!(
            !said.contains("commit them if they are yours"),
            "{how:?}:\n{said}"
        );

        let out = common::cli::run(&["lock"], &f.ww);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        run_every_choice(&stderr, &f.ww);
        assert_eq!(common::git_in(&f.ww_lib, &["status", "--porcelain"]), "");
        let out = common::cli::run(&["lock"], &f.ww);
        assert!(
            out.status.success(),
            "{how:?}:\n{}",
            common::cli::describe(&out)
        );
    }
}

/// A clean checkout with no reflog entry has nothing to tell apart.
#[test]
fn a_clean_checkout_with_no_reflog_entry_is_not_held() {
    for how in NO_ENTRY {
        let f = fixture_without_entry(how);
        assert_eq!(common::git_in(&f.ww_lib, &["status", "--porcelain"]), "");
        for verb in [&["lock"][..], &["land"], &["doctor"]] {
            let out = common::cli::run(verb, &f.ww);
            assert!(
                !common::cli::describe(&out).contains("where this checkout of"),
                "{how:?} {verb:?}:\n{}",
                common::cli::describe(&out)
            );
        }
    }
}

/// Edits in a checkout with no reflog entry are held the same way, whatever
/// the branch did: the drift cannot be ruled out, so the one choice is the
/// stash that keeps them.
#[test]
fn an_edit_in_a_checkout_with_no_reflog_entry_is_held_with_the_stash_alone() {
    let f = fixture_without_entry(NoEntry::ExpiredByHand);
    std::fs::write(f.ww_lib.join("README.md"), "edited\n").unwrap();

    let out = common::cli::run(&["lock"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDriftLeftInPlace);
    assert!(stderr.contains("where this checkout of"), "{stderr}");
    assert_eq!(printed_commands(&stderr).len(), 1, "{stderr}");
    run_every_choice(&stderr, &f.ww);
    assert_eq!(common::git_in(&f.ww_lib, &["status", "--porcelain"]), "");
}

/// Whether the installed git creates repositories on the reftable ref storage.
fn makes_reftable() -> bool {
    let version = common::git_in(Path::new("."), &["version"]);
    let release: Vec<u32> = version
        .trim_start_matches("git version ")
        .split('.')
        .take(2)
        .filter_map(|n| n.parse().ok())
        .collect();
    let makes = release.as_slice() >= [2, 45].as_slice();
    if !makes {
        common::report_skip(&format!("{version} creates no reftable repository"));
    }
    makes
}

/// On the reftable ref storage, which keeps no log file per worktree, a
/// checkout whose own log was emptied while the branch's log kept the move
/// is held as one, with no commit offered, as on the files storage.
#[test]
fn a_move_under_a_checkout_with_an_emptied_log_is_held_on_reftable() {
    if !makes_reftable() {
        return;
    }
    let f = fixture_with_lib(&["--ref-format=reftable"], &[]);
    assert_eq!(
        common::git_in(&f.ww_lib, &["rev-parse", "--show-ref-format"]),
        "reftable"
    );
    move_notes(&f);
    common::git_in(&f.ww_lib, &["reflog", "expire", "--expire=now", "HEAD"]);

    let out = common::cli::run(&["lock"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDriftLeftInPlace);
    assert!(
        stderr.contains("holds no record of where this checkout"),
        "{stderr}"
    );
    assert!(!stderr.contains("commit"), "{stderr}");
    run_every_choice(&stderr, &f.ww);
    let out = common::cli::run(&["lock"], &f.ww);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
}

/// A checkout with no record of where it stood, clean but for a path marked
/// to be added with no content: the stash cannot run over that mark, so the
/// hold unmarks it first, and every printed line runs as printed.
#[test]
fn a_held_checkout_with_a_path_marked_to_be_added_is_offered_unmarking_it_first() {
    let f = fixture_without_entry(NoEntry::ExpiredByHand);
    std::fs::write(f.ww_lib.join("marked.txt"), "marked\n").unwrap();
    common::git_in(&f.ww_lib, &["add", "-N", "marked.txt"]);

    let out = common::cli::run(&["lock"], &f.ww);

    let stderr = common::cli::refused(&out, State::EditsDriftLeftInPlace);
    assert_eq!(printed_commands(&stderr).len(), 2, "{stderr}");
    assert!(!stderr.contains("commit"), "{stderr}");
    run_every_choice(&stderr, &f.ww);
    assert_eq!(
        read(&f.ww_lib.join("marked.txt")).as_deref(),
        Some("marked\n")
    );
    let out = common::cli::run(&["lock"], &f.ww);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
}

/// The branch rewritten under a checkout that edited nothing, moved to a
/// sibling commit or back to an ancestor: the checkout's record names a
/// commit its branch no longer contains, so its differences cannot be told
/// from edits. Every verb holds it naming both commits, with no commit
/// offered, and the printed stash leaves it at the branch's new tip and ends
/// the hold.
#[test]
fn a_branch_rewritten_under_a_checkout_is_held_with_no_commit_offered() {
    for sideways in [true, false] {
        let f = fixture();
        let stood = common::git_in(&f.ww_lib, &["rev-parse", "HEAD"]);
        let rewritten = if sideways {
            let elsewhere = f.scratch.join("rewrite");
            common::git_in(
                &f.primary_lib,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "--detach",
                    elsewhere.to_str().unwrap(),
                    "ww/project~1",
                ],
            );
            commit_file(&elsewhere, "other.txt", "other\n", "rewritten")
        } else {
            common::git_in(&f.ww_lib, &["rev-parse", "HEAD~1"])
        };
        common::git_in(
            &f.primary_lib,
            &["update-ref", "refs/heads/ww/project", &rewritten],
        );
        let before = path_state(&f.ww_lib, &["feature.txt", "other.txt"]);

        for verb in [&["lock"][..], &["land"]] {
            let out = common::cli::run(verb, &f.ww);
            let stderr = common::cli::refused(&out, State::EditsDriftLeftInPlace);
            assert!(
                stderr.contains(&stood[..12]) && stderr.contains(&rewritten[..12]),
                "{verb:?}:\n{stderr}"
            );
            assert!(stderr.contains("does not contain"), "{verb:?}:\n{stderr}");
            assert!(!stderr.contains(" commit\n"), "{verb:?}:\n{stderr}");
            assert_eq!(
                path_state(&f.ww_lib, &["feature.txt", "other.txt"]),
                before,
                "{verb:?}"
            );
        }
        let said = common::cli::describe(&common::cli::run(&["doctor"], &f.ww));
        assert!(said.contains("does not contain"), "{said}");

        let out = common::cli::run(&["lock"], &f.ww);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(printed_commands(&stderr).len(), 1, "{stderr}");
        run_every_choice(&stderr, &f.ww);
        assert_eq!(common::git_in(&f.ww_lib, &["rev-parse", "HEAD"]), rewritten);
        assert_eq!(common::git_in(&f.ww_lib, &["status", "--porcelain"]), "");
        let out = common::cli::run(&["lock"], &f.ww);
        let said = common::cli::describe(&out);
        assert!(!said.contains("drift left in place"), "{said}");
    }
}

/// With `core.fileMode=false` git reads no exec bit from disk: a refresh
/// killed between its writes, then files reading executable as on a mount
/// that records none, is finished by the next verb, not held as edited.
#[cfg(unix)]
#[test]
fn a_killed_refresh_where_the_checkout_reads_no_exec_bit_is_finished() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture_with_lib_config(&[("core.fileMode", "false")]);
    move_notes(&f);
    drop(held_landing(&f, "drift-write-records"));
    for file in ["notes.txt", "new.txt"] {
        let path = f.ww_lib.join(file);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    without_the_killed_landing(&f);

    let out = common::cli::run(&["lock"], &f.ww);

    let said = common::cli::describe(&out);
    assert!(out.status.success(), "{said}");
    assert!(said.contains("refreshed drift"), "{said}");
    assert_eq!(common::git_in(&f.ww_lib, &["status", "--porcelain"]), "");
}

// ---------------------------------------------------------------------------
// A refresh stopped between its writes, and index-only returns after a hand
// refresh
// ---------------------------------------------------------------------------

fn move_changing_two_and_adding_one(f: &Fixture) {
    f.move_with("ww/project", |elsewhere| {
        std::fs::write(elsewhere.join("notes.txt"), "three\n").unwrap();
        std::fs::write(elsewhere.join("spare.txt"), "spare moved\n").unwrap();
        std::fs::write(elsewhere.join("new.txt"), "new\n").unwrap();
    });
}

fn index_entry(repo: &Path, path: &str) -> String {
    common::git_in(repo, &["ls-files", "-s", "--", path])
}

/// A landing killed between the refresh's two writes, then one moved file
/// staged by hand: the files that are still unstaged are the refresh's to
/// finish, with no commit offered that would record the move's reverse.
#[test]
fn a_hand_add_of_one_file_after_a_refresh_killed_between_its_writes_is_finished_by_the_next_verb() {
    let f = fixture();
    move_changing_two_and_adding_one(&f);
    drop(held_landing(&f, "drift-write-records"));
    common::git_in(&f.ww_lib, &["add", "notes.txt"]);
    assert_eq!(
        read(&f.ww_lib.join("spare.txt")).as_deref(),
        Some("spare moved\n")
    );

    without_the_killed_landing(&f);
    let out = common::cli::run(&["lock"], &f.ww);

    let said = common::cli::describe(&out);
    assert!(said.contains("refreshed drift"), "{said}");
    assert!(said.contains("spare.txt"), "{said}");
    assert!(!said.contains("commit"), "{said}");
    assert_eq!(
        common::git_in(&f.ww_lib, &["status", "--porcelain"]),
        "",
        "{said}"
    );
    assert_eq!(
        read(&f.ww_lib.join("spare.txt")).as_deref(),
        Some("spare moved\n")
    );
}

/// After a refresh by hand, a staged-only return of one moved file
/// (`git reset <old> -- <file>`) leaves its index entry at the old tip beside
/// HEAD's file, the shape a stopped refresh leaves, so the next verb resets
/// the entry. The file's bytes are untouched and the content the staging held
/// is in history.
#[test]
fn a_staged_only_return_of_one_file_after_a_hand_refresh_has_its_entry_reset() {
    let f = fixture();
    move_changing_two_and_adding_one(&f);
    common::git_in(&f.ww_lib, &["checkout", "HEAD", "--", "."]);
    common::git_in(&f.ww_lib, &["reset", "-q", "HEAD~1", "--", "spare.txt"]);
    assert_eq!(
        common::git_in(&f.ww_lib, &["status", "--porcelain"]),
        "MM spare.txt"
    );

    let out = common::cli::run(&["lock"], &f.ww);

    let said = common::cli::describe(&out);
    assert!(out.status.success(), "{said}");
    assert!(
        said.contains("what the VCS records for these paths was reset"),
        "{said}"
    );
    assert_eq!(common::git_in(&f.ww_lib, &["status", "--porcelain"]), "");
    assert_eq!(
        read(&f.ww_lib.join("spare.txt")).as_deref(),
        Some("spare moved\n")
    );
}

/// After a refresh by hand, `git rm --cached` of a file the move added
/// leaves no index entry beside HEAD's file, which reads as a stopped refresh
/// too: the next verb tracks the file again and leaves its bytes.
#[test]
fn an_untracked_added_file_after_a_hand_refresh_is_tracked_again() {
    let f = fixture();
    move_changing_two_and_adding_one(&f);
    common::git_in(&f.ww_lib, &["checkout", "HEAD", "--", "."]);
    common::git_in(&f.ww_lib, &["rm", "-q", "--cached", "new.txt"]);
    assert_eq!(index_entry(&f.ww_lib, "new.txt"), "");

    let out = common::cli::run(&["lock"], &f.ww);

    let said = common::cli::describe(&out);
    assert!(out.status.success(), "{said}");
    assert!(said.contains("new.txt"), "{said}");
    assert_ne!(index_entry(&f.ww_lib, "new.txt"), "", "{said}");
    assert_eq!(read(&f.ww_lib.join("new.txt")).as_deref(), Some("new\n"));
    assert_eq!(common::git_in(&f.ww_lib, &["status", "--porcelain"]), "");
}

/// Edits held for want of a reflog entry, stashed as printed and taken back:
/// where git writes an entry for that, the next verb judges them as any
/// edits, with the commit choice back; where the setting stops git writing
/// one, the hold names the setting, and turning it on is the exit.
#[test]
fn the_printed_stash_and_its_pop_end_the_hold_unless_the_setting_forbids_an_entry() {
    for how in NO_ENTRY {
        let f = fixture_without_entry(how);
        std::fs::write(f.ww_lib.join("README.md"), "edited\n").unwrap();
        let out = common::cli::run(&["lock"], &f.ww);
        let stderr = common::cli::refused(&out, State::EditsDriftLeftInPlace);
        let off = matches!(how, NoEntry::NeverLogged);
        assert_eq!(
            stderr.contains("core.logAllRefUpdates"),
            off,
            "{how:?}:\n{stderr}"
        );
        assert!(
            stderr.contains("taking back what was set aside"),
            "{how:?}:\n{stderr}"
        );
        run_every_choice(&stderr, &f.ww);
        common::git_in(&f.ww_lib, &["stash", "pop", "-q"]);

        let out = common::cli::run(&["lock"], &f.ww);

        if off {
            let stderr = common::cli::refused(&out, State::EditsDriftLeftInPlace);
            assert!(stderr.contains("core.logAllRefUpdates"), "{stderr}");
            let printed = stderr
                .split("VCS action (git: `")
                .nth(1)
                .and_then(|rest| rest.split('`').next())
                .unwrap_or_else(|| panic!("no VCS action printed:\n{stderr}"));
            let words: Vec<&str> = printed.split_whitespace().collect();
            let ran = std::process::Command::new(words[0])
                .args(&words[1..])
                .output()
                .unwrap();
            assert!(
                ran.status.success(),
                "{printed}:\n{}",
                common::cli::describe(&ran)
            );
            common::git_in(&f.ww_lib, &["stash", "push", "-q"]);
            common::git_in(&f.ww_lib, &["stash", "pop", "-q"]);
            let out = common::cli::run(&["lock"], &f.ww);
            common::cli::refused(&out, State::EditsDirty);
        } else {
            let stderr = common::cli::refused(&out, State::EditsDirty);
            assert!(stderr.contains(" commit"), "{how:?}:\n{stderr}");
        }
    }
}
