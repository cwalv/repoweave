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

    let out = common::cli::succeed(&["land"], &f.ww);

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

    let out = common::cli::succeed(&["land", "--json"], &f.ww);

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
