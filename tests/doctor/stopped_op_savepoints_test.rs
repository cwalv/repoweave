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
    let ready = ready_to_sync(true);
    let out = rwv(&ready.seat.root, &["sync"]);
    assert!(
        !out.status.success() && ready.seat.root.join(".rwv-op").exists(),
        "the member conflict must stop the sync:\n{}",
        common::cli::describe(&out)
    );
    assert!(!savepoints(&ready.primary.lib).is_empty());
    ready
}

/// [`stop_a_sync`] up to the seat's `rwv sync`, which has not run; with
/// `conflicting` false the primary does not touch the seat's file, so the
/// seat lands without a stop.
fn ready_to_sync(conflicting: bool) -> Stopped {
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
    if conflicting {
        commit_file(&primary.lib, "c.txt", "primary\n", "primary: lib");
        succeed(&primary.root, &["lock", "--commit"]);
    }

    let seat_lib = head(&seat.lib);
    let seat_project = head(&seat.project);
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

/// What makes a workspace that may hold an operation unreadable.
#[derive(Debug, Clone, Copy)]
enum Unreadable {
    /// A workweave the index records, its directory gone.
    RecordedGone,
    /// A workspace a checkout of the project clone is registered in, which
    /// no index records, its directory gone.
    UnrecordedGone,
    /// A checkout registered with a hold on it, its directory gone.
    HeldRegistrationGone,
    /// The index does not parse.
    IndexUnreadable,
    /// A workweave's record names no operation this rwv reads.
    RecordNamesNoOperation,
}

impl Unreadable {
    const ALL: [Self; 5] = [
        Self::RecordedGone,
        Self::UnrecordedGone,
        Self::HeldRegistrationGone,
        Self::IndexUnreadable,
        Self::RecordNamesNoOperation,
    ];

    fn make(self, stopped: &Stopped) {
        let index = common::index_file_of(&stopped.primary.project);
        match self {
            Self::RecordedGone => std::fs::remove_dir_all(&stopped.sibling.root).unwrap(),
            Self::UnrecordedGone => {
                let mut recorded: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(&index).unwrap()).unwrap();
                recorded["workweaves"].as_object_mut().unwrap().remove("s2");
                std::fs::write(&index, serde_json::to_string(&recorded).unwrap()).unwrap();
                std::fs::remove_dir_all(&stopped.sibling.root).unwrap();
            }
            Self::HeldRegistrationGone => {
                let hand = stopped.primary.root.parent().unwrap().join("hand/a");
                let checkout = hand.join("projects").join(PROJECT);
                common::git_in(
                    &stopped.primary.project,
                    &[
                        "worktree",
                        "add",
                        "-q",
                        "-b",
                        "hand",
                        &checkout.to_string_lossy(),
                    ],
                );
                common::git_in(
                    &stopped.primary.project,
                    &["worktree", "lock", &checkout.to_string_lossy()],
                );
                std::fs::remove_dir_all(&hand).unwrap();
            }
            Self::IndexUnreadable => std::fs::write(&index, "{ not json").unwrap(),
            Self::RecordNamesNoOperation => {
                std::fs::write(stopped.sibling.root.join(".rwv-op"), "{}").unwrap();
            }
        }
    }
}

/// A savepoint no readable record claims while a workspace that may hold its
/// operation cannot be read: `--fix` keeps it and says why, with the line
/// that ends the doubt; that line, run as printed, lets the next `--fix`
/// judge it.
#[test]
fn each_unreadable_holder_keeps_unclaimed_savepoints_and_prints_the_line_that_ends_it() {
    for reason in Unreadable::ALL {
        let stopped = stop_a_sync();
        std::fs::remove_file(stopped.seat.root.join(".rwv-op")).unwrap();
        reason.make(&stopped);
        let before = savepoints(&stopped.primary.lib);
        assert!(!before.is_empty());

        let fixed = rwv(&stopped.primary.root, &["doctor", "--fix"]);
        let report = common::cli::describe(&fixed);
        assert!(!report.contains("dropped op_id"), "{reason:?}: {report}");
        assert_eq!(
            savepoints(&stopped.primary.lib),
            before,
            "{reason:?}: {report}"
        );

        let text = String::from_utf8_lossy(&fixed.stdout).into_owned();
        let at = text
            .find("[warning: unreadable-holder]")
            .unwrap_or_else(|| panic!("{reason:?}: no unreadable-holder in:\n{text}"));
        let line = text[at..]
            .lines()
            .nth(1)
            .map(str::trim)
            .filter(|line| line.starts_with("rwv "))
            .unwrap_or_else(|| panic!("{reason:?}: no line under the finding:\n{text}"))
            .to_owned();
        let ran = common::run_advised(&line, &stopped.primary.root);
        assert!(
            ran.status.success(),
            "{reason:?}: {line}\n{}",
            common::cli::describe(&ran)
        );

        let judged = rwv(&stopped.primary.root, &["doctor", "--fix"]);
        let report = common::cli::describe(&judged);
        assert!(
            savepoints(&stopped.primary.lib).is_empty(),
            "{reason:?}: the next --fix judges the savepoints once the line ran:\n{report}"
        );
        assert!(
            !report.contains("unreadable-holder"),
            "{reason:?}: {report}"
        );
    }
}

/// An operation that starts while `rwv doctor --fix` runs elsewhere: doctor
/// lists the savepoints, then reads which operations stand, so one that
/// starts between the two is either not yet listed or read as standing, and
/// its abort still puts every repo back.
#[test]
fn an_operation_that_starts_while_doctor_runs_keeps_its_savepoints() {
    if !cfg!(feature = "test-pause") {
        common::report_skip("holds rwv doctor at a pause point, which needs --features test-pause");
        return;
    }
    for at in [Elsewhere::Primary, Elsewhere::Sibling] {
        let ready = ready_to_sync(true);
        let mut doctor = common::rwv_std();
        doctor.args(["doctor", "--fix"]).current_dir(match at {
            Elsewhere::Primary => &ready.primary.root,
            Elsewhere::Sibling => &ready.sibling.root,
        });
        let held = common::pause::Paused::spawn(
            doctor,
            "doctor-savepoints-listed",
            &ready._tmp.path().join("pause"),
        )
        .expect("doctor lists the savepoints");
        let out = rwv(&ready.seat.root, &["sync"]);
        assert!(
            !out.status.success() && ready.seat.root.join(".rwv-op").exists(),
            "the member conflict must stop the sync:\n{}",
            common::cli::describe(&out)
        );
        let lib_before = savepoints(&ready.primary.lib);
        let project_before = savepoints(&ready.primary.project);
        assert!(!lib_before.is_empty());

        let fixed = held.release();
        let report = common::cli::describe(&fixed);
        assert!(!report.contains("dropped op_id"), "{at:?}: {report}");
        assert_eq!(
            savepoints(&ready.primary.lib),
            lib_before,
            "{at:?}: {report}"
        );
        assert_eq!(savepoints(&ready.primary.project), project_before, "{at:?}");

        succeed(&ready.seat.root, &["abort"]);
        assert_eq!(head(&ready.seat.lib), ready.seat_lib, "{at:?}");
        assert_eq!(head(&ready.seat.project), ready.seat_project, "{at:?}");
    }
}

#[derive(Debug, Clone, Copy)]
enum Elsewhere {
    Primary,
    Sibling,
}

/// A seat that is away while `--fix` runs (a volume unmounted, a rename in
/// progress) still holds its operation: whether it stands cannot be told, so
/// the lease on the primary and the landing claim stay, and so do the
/// savepoints, the registrations of a seat no index records among them, over
/// two runs; back, the seat's abort puts every repo back.
#[test]
fn a_seat_that_is_away_keeps_its_lease_claim_registrations_and_savepoints() {
    if !cfg!(feature = "test-pause") {
        common::report_skip("kills rwv land at a pause point, which needs --features test-pause");
        return;
    }
    for recorded in [true, false] {
        let ready = ready_to_sync(false);
        let primary_lib = head(&ready.primary.lib);
        let primary_project = head(&ready.primary.project);
        let mut land = common::rwv_std();
        land.args(["land"]).current_dir(&ready.seat.root);
        common::pause::Paused::spawn(land, "aligning-target", &ready._tmp.path().join("pause"))
            .expect("the landing reaches its target")
            .kill();
        if !recorded {
            unrecord_the_seat(&ready);
        }
        let lease = ready.primary.root.join(".rwv-op-lease");
        assert!(lease.exists() && ready.seat.root.join(".rwv-op").exists());
        let claims = |root: &Path| {
            common::git_in(
                root,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
        };
        let claims_dir = PathBuf::from(claims(&ready.primary.project).trim()).join("rwv-landings");
        let claimed = std::fs::read_dir(&claims_dir).map_or(0, |entries| entries.count());
        assert!(claimed > 0, "fixture: the landing holds its claim");
        let lib_before = savepoints(&ready.primary.lib);
        assert!(
            !lib_before.is_empty(),
            "fixture: the landing filed its savepoints"
        );

        let away = ready._tmp.path().join("unmounted");
        std::fs::rename(&ready.seat.root, &away).unwrap();
        for _ in 0..2 {
            let fixed = rwv(&ready.primary.root, &["doctor", "--fix"]);
            let report = common::cli::describe(&fixed);
            assert!(!report.contains("dropped op_id"), "{recorded}: {report}");
            assert!(!report.contains("pruned"), "{recorded}: {report}");
            assert!(!report.contains("removed lease"), "{recorded}: {report}");
            assert!(
                !report.contains("released the claim"),
                "{recorded}: {report}"
            );
            assert!(report.contains("unreadable-holder"), "{recorded}: {report}");
            assert!(lease.exists(), "{recorded}: {report}");
            assert_eq!(
                std::fs::read_dir(&claims_dir).map_or(0, |entries| entries.count()),
                claimed,
                "{recorded}: {report}"
            );
            assert_eq!(
                savepoints(&ready.primary.lib),
                lib_before,
                "{recorded}: {report}"
            );
        }
        let held = rwv(&ready.primary.root, &["lock", "--commit"]);
        assert!(
            !held.status.success(),
            "{recorded}: the lease holds the primary"
        );
        std::fs::rename(&away, &ready.seat.root).unwrap();

        succeed(&ready.seat.root, &["abort"]);
        assert_eq!(head(&ready.primary.lib), primary_lib, "{recorded}");
        assert_eq!(head(&ready.primary.project), primary_project, "{recorded}");
        assert_eq!(head(&ready.seat.lib), ready.seat_lib, "{recorded}");
        assert!(!lease.exists(), "{recorded}");
    }
}

/// A landing killed after it moved the target's branch and before it moved
/// the target's checkout: what that checkout holds is what the landing's
/// abort restores, so `--fix` anywhere refreshes nothing in it, and the abort
/// puts the target back.
#[test]
fn doctor_refreshes_nothing_in_a_checkout_an_operation_holds() {
    if !cfg!(feature = "test-pause") {
        common::report_skip("kills rwv land at a pause point, which needs --features test-pause");
        return;
    }
    let ready = ready_to_sync(false);
    let primary_lib = head(&ready.primary.lib);
    let primary_project = head(&ready.primary.project);
    let mut land = common::rwv_std();
    land.args(["land"]).current_dir(&ready.seat.root);
    common::pause::Paused::spawn(
        land,
        "advance-target-move",
        &ready._tmp.path().join("pause"),
    )
    .expect("the landing moves the target's branch")
    .kill();
    assert!(ready.seat.root.join(".rwv-op").exists());
    let status =
        |dir: &Path| common::git_in(dir, &["status", "--porcelain", "--untracked-files=all"]);
    let held: Vec<(PathBuf, String)> = [&ready.primary.lib, &ready.primary.project]
        .into_iter()
        .map(|dir| (dir.clone(), status(dir)))
        .collect();

    for at in [&ready.primary.root, &ready.sibling.root] {
        let fixed = rwv(at, &["doctor", "--fix"]);
        let report = common::cli::describe(&fixed);
        assert!(!report.contains("drift in"), "{report}");
        for (dir, was) in &held {
            assert_eq!(&status(dir), was, "{}: {report}", dir.display());
        }
    }

    succeed(&ready.seat.root, &["abort"]);
    assert_eq!(head(&ready.primary.lib), primary_lib);
    assert_eq!(head(&ready.primary.project), primary_project);
    assert!(
        status(&ready.primary.lib).trim().is_empty(),
        "{}",
        status(&ready.primary.lib)
    );
}
