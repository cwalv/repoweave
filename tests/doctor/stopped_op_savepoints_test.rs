//! A seat stopped in `rwv sync` keeps its savepoints through `rwv doctor
//! --fix` run anywhere else in the weave, so its `rwv abort` still puts every
//! repo back: from the primary over a seat made with `--dir`, which no
//! container holds, and from a sibling seat, whose own checkout holds no
//! record of the operation.

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::common;

const PROJECT: &str = "app";
const LIB: &str = "local/owner/lib";

struct Side {
    root: PathBuf,
    project: PathBuf,
    lib: PathBuf,
}

fn side(root: PathBuf) -> Side {
    Side {
        project: root.join("projects").join(PROJECT),
        lib: common::under(&root, LIB),
        root,
    }
}

struct Stopped {
    _tmp: tempfile::TempDir,
    primary: Side,
    seat: Side,
    sibling: Side,
    seat_lib: String,
    seat_project: String,
}

fn head(repo: &Path) -> String {
    common::git_in(repo, &["rev-parse", "HEAD"])
        .trim()
        .to_owned()
}

fn commit_file(repo: &Path, file: &str, text: &str, message: &str) {
    std::fs::write(repo.join(file), text).unwrap();
    common::git_in(repo, &["add", file]);
    common::git_in(repo, &["commit", "-m", message]);
}

fn rwv(dir: &Path, args: &[&str]) -> Output {
    common::rwv().args(args).current_dir(dir).output().unwrap()
}

fn succeed(dir: &Path, args: &[&str]) -> Output {
    let out = rwv(dir, args);
    assert!(
        out.status.success(),
        "rwv {args:?} in {} failed:\n{}",
        dir.display(),
        common::cli::describe(&out)
    );
    out
}

fn savepoints(repo: &Path) -> String {
    common::git_in(
        repo,
        &["for-each-ref", "--format=%(refname)", "refs/rwv/pre-op/"],
    )
}

/// A primary with one member, a seat at `seat_dir` (made with `--dir`) and a
/// sibling seat in the container. The seat and the primary commit to the same
/// file of the member and the primary relocks, so the seat's `rwv sync` stops
/// on the member's rebase conflict with a current record and its savepoints.
fn stop_a_sync() -> Stopped {
    let tmp = common::tempdir().unwrap();
    let upstream = tmp.path().join("upstream/owner/lib");
    std::fs::create_dir_all(&upstream).unwrap();
    common::git_in(&upstream, &["init", "-b", "main"]);
    commit_file(&upstream, "README.md", "init\n", "initial");

    let primary = side(tmp.path().join("ws"));
    common::git_in(
        tmp.path(),
        &[
            "clone",
            &common::file_url(&upstream),
            &primary.lib.to_string_lossy(),
        ],
    );
    std::fs::create_dir_all(&primary.project).unwrap();
    common::git_in(&primary.project, &["init", "-b", "main"]);
    common::set_up_lock_driver(&primary.project);
    std::fs::write(primary.project.join(".gitignore"), "*.code-workspace\n").unwrap();
    std::fs::write(
        primary.project.join("rwv.toml"),
        format!(
            "history = \"linear\"\n\n[repositories.\"{LIB}\"]\ntype = \"git\"\nurl = \"{}\"\nbranch = \"main\"\nrole = \"owned\"\n",
            common::file_url(&upstream)
        ),
    )
    .unwrap();
    std::fs::write(primary.root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    common::record_trunk(&primary.project);
    common::pin_every_member(&primary.root);
    common::git_in(&primary.project, &["add", "-A"]);
    common::git_in(&primary.project, &["commit", "-m", "initial"]);

    let seat_dir = tmp.path().join("elsewhere/seat");
    succeed(
        &primary.root,
        &[
            "workweave",
            "create",
            "--project",
            PROJECT,
            "s1",
            "--dir",
            &seat_dir.to_string_lossy(),
        ],
    );
    succeed(
        &primary.root,
        &["workweave", "create", "--project", PROJECT, "s2"],
    );
    let seat = side(seat_dir);
    let sibling = side(
        tmp.path()
            .join(".workweaves")
            .join(format!("{PROJECT}--s2")),
    );
    commit_file(&seat.lib, "c.txt", "seat\n", "seat: lib");
    succeed(&seat.root, &["lock", "--commit"]);
    commit_file(&primary.lib, "c.txt", "primary\n", "primary: lib");
    succeed(&primary.root, &["lock", "--commit"]);

    let seat_lib = head(&seat.lib);
    let seat_project = head(&seat.project);
    let out = rwv(&seat.root, &["sync"]);
    assert!(
        !out.status.success() && seat.root.join(".rwv-op").exists(),
        "the member conflict must stop the sync:\n{}",
        common::cli::describe(&out)
    );
    assert!(!savepoints(&primary.lib).is_empty());
    Stopped {
        _tmp: tmp,
        primary,
        seat,
        sibling,
        seat_lib,
        seat_project,
    }
}

/// `rwv doctor --fix` from `at` drops none of the stopped sync's savepoints,
/// and the seat's `rwv abort` then puts its member and project repo back on
/// their branches at the tips they had before the sync.
fn doctor_fix_elsewhere_leaves_the_abort_whole(at: impl Fn(&Stopped) -> &Path, recorded: bool) {
    let stopped = stop_a_sync();
    if !recorded {
        unrecord_the_seat(&stopped);
    }
    let lib_before = savepoints(&stopped.primary.lib);
    let project_before = savepoints(&stopped.primary.project);

    let fixed = rwv(at(&stopped), &["doctor", "--fix"]);
    let report = common::cli::describe(&fixed);
    assert!(!report.contains("dropped op_id"), "{report}");
    assert_eq!(savepoints(&stopped.primary.lib), lib_before, "{report}");
    assert_eq!(
        savepoints(&stopped.primary.project),
        project_before,
        "{report}"
    );

    succeed(&stopped.seat.root, &["abort"]);
    assert_eq!(head(&stopped.seat.lib), stopped.seat_lib);
    assert_eq!(head(&stopped.seat.project), stopped.seat_project);
    common::assert_on_branch(&stopped.seat.lib, &format!("{PROJECT}--s1"));
    common::assert_on_branch(&stopped.seat.project, &format!("{PROJECT}--s1"));
}

/// Drop the stopped seat's entry from the project's index, as an index
/// rebuilt while its marker disagreed would leave it: the project clone still
/// registers the seat's checkout.
fn unrecord_the_seat(stopped: &Stopped) {
    let index = common::index_file_of(&stopped.primary.project);
    let mut recorded: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&index).unwrap()).unwrap();
    recorded["workweaves"]
        .as_object_mut()
        .unwrap()
        .remove("s1")
        .expect("the index records the seat");
    std::fs::write(&index, serde_json::to_string(&recorded).unwrap()).unwrap();
}

#[test]
fn the_primarys_doctor_fix_keeps_the_savepoints_of_a_dir_seats_stopped_sync() {
    doctor_fix_elsewhere_leaves_the_abort_whole(|stopped| &stopped.primary.root, true);
}

#[test]
fn a_sibling_seats_doctor_fix_keeps_the_savepoints_of_a_stopped_sync() {
    doctor_fix_elsewhere_leaves_the_abort_whole(|stopped| &stopped.sibling.root, true);
}

#[test]
fn the_primarys_doctor_fix_keeps_the_savepoints_of_an_unrecorded_seats_stopped_sync() {
    doctor_fix_elsewhere_leaves_the_abort_whole(|stopped| &stopped.primary.root, false);
}

#[test]
fn a_sibling_seats_doctor_fix_keeps_the_savepoints_of_an_unrecorded_seats_stopped_sync() {
    doctor_fix_elsewhere_leaves_the_abort_whole(|stopped| &stopped.sibling.root, false);
}

/// The primary's doctor reads the workweaves the index records wherever
/// their directories are: the `--dir` seat's stopped sync is reported from
/// the primary as from the seat.
#[test]
fn the_primarys_doctor_reports_the_op_of_a_seat_no_container_holds() {
    let stopped = stop_a_sync();
    let out = rwv(&stopped.primary.root, &["doctor", "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let seat = repoweave::path_spelling::resolve(&stopped.seat.root).unwrap();
    let reported =
        json["violations"].as_array().unwrap().iter().any(|v| {
            v["kind"] == "stale-op-state" && v.to_string().contains(&*seat.to_string_lossy())
        });
    assert!(reported, "{json:#}");
}

/// A workweave the index records whose directory is gone may have held the
/// operation a savepoint belongs to: `--fix` keeps the savepoint and says why,
/// rather than judging it orphaned.
#[test]
fn a_recorded_workweave_that_is_gone_keeps_every_unclaimed_savepoint() {
    let stopped = stop_a_sync();
    std::fs::remove_file(stopped.seat.root.join(".rwv-op")).unwrap();
    std::fs::remove_dir_all(&stopped.sibling.root).unwrap();
    let before = savepoints(&stopped.primary.lib);

    let fixed = rwv(&stopped.primary.root, &["doctor", "--fix"]);
    let report = common::cli::describe(&fixed);
    assert!(!report.contains("dropped op_id"), "{report}");
    assert!(
        report.contains("are kept, not judged, because a workspace that may hold the operation")
            && report.contains("for workweave `s2` is not there"),
        "{report}"
    );
    assert_eq!(savepoints(&stopped.primary.lib), before);
}
