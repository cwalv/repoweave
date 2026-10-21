//! Migrating a workweave whose `reference` members an older release left as
//! symlinks into the canonical clones.
//!
//! The fixture is the shape the operator's seats were found in: one member
//! links to its canonical clone, and one links to where that clone lived
//! before the weave moved, so it dangles — while the canonical clone at the
//! weave's new location holds exactly the revision the workweave's lock pins.
//!
//!   - doctor reports both links and exits non-zero;
//!   - the verbs that walk the workweave's members refuse it, naming
//!     `rwv doctor --fix`, and change nothing;
//!   - `rwv doctor --fix` replaces each link with a worktree at the pinned
//!     revision, touching neither canonical clone nor what a link pointed at;
//!   - a link whose pinned revision the canonical clone lacks is left for
//!     `rwv fetch`, and doctor never fetches it;
//!   - `--worktree-references` is gone.

use assert_cmd::Command as AssertCommand;
use std::path::{Path, PathBuf};

mod common;

const OWNED_PATH: &str = "github/org/owned";
const LINKED_PATH: &str = "github/org/linked";
const DANGLING_PATH: &str = "github/org/dangling";
const PROJECT: &str = "app";
const SEAT: &str = "seat";

fn rwv() -> AssertCommand {
    common::rwv()
}

fn init_repo(path: &Path, file: &str) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    common::git_in(path, &["config", "user.email", "test@test.com"]);
    common::git_in(path, &["config", "user.name", "Test"]);
    std::fs::write(path.join(file), format!("{file}\n")).unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

fn manifest(members: &[(&str, &Path, &str)]) -> String {
    members
        .iter()
        .map(|(path, url, role)| {
            format!(
                "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"file://{url}\"\nversion = \"main\"\nrole = \"{role}\"\n",
                url = common::url_path(url),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn commit_project(project_dir: &Path, manifest: &str, pins: &[(&str, &Path, &str)]) {
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    let urls: Vec<String> = pins
        .iter()
        .map(|(_, url, _)| common::file_url(url))
        .collect();
    let rows: Vec<(&str, &str, &str)> = pins
        .iter()
        .zip(&urls)
        .map(|((path, _, sha), url)| (*path, url.as_str(), *sha))
        .collect();
    common::fixture_lock(project_dir, &rows);
    common::git_in(project_dir, &["add", "rwv.toml", "rwv.lock"]);
    common::git_in(project_dir, &["commit", "-m", "manifest and lock"]);
}

struct Weave {
    primary: PathBuf,
    seat: PathBuf,
    linked_canonical: PathBuf,
    dangling_canonical: PathBuf,
    /// Where the dangling link points: the canonical clone's path under the
    /// weave root it lived at before the move.
    moved_from: PathBuf,
    linked_pin: String,
    dangling_pin: String,
}

/// A workweave created while its project listed only the owned member, whose
/// manifest and lock then gained two `reference` members materialized the
/// way older releases did it: as absolute symlinks.
fn unmigrated_weave(tmp: &Path) -> Weave {
    let primary = tmp.join("primary");
    std::fs::create_dir_all(primary.join("github/org")).unwrap();
    let owned = primary.join(OWNED_PATH);
    let owned_pin = init_repo(&owned, "OWNED");
    let linked_canonical = primary.join(LINKED_PATH);
    let linked_pin = init_repo(&linked_canonical, "LINKED");
    let dangling_canonical = primary.join(DANGLING_PATH);
    let dangling_pin = init_repo(&dangling_canonical, "DANGLING");

    let project_dir = primary.join("projects").join(PROJECT);
    init_repo(&project_dir, "PLACEHOLDER");
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    common::git_in(&project_dir, &["add", ".gitattributes"]);
    commit_project(
        &project_dir,
        &manifest(&[(OWNED_PATH, &owned, "owned")]),
        &[(OWNED_PATH, &owned, &owned_pin)],
    );
    std::fs::write(primary.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    rwv()
        .args(["workweave", PROJECT, "create", SEAT])
        .current_dir(&primary)
        .assert()
        .success();
    let seat = tmp.join(".workweaves").join(format!("{PROJECT}--{SEAT}"));

    let all = manifest(&[
        (OWNED_PATH, &owned, "owned"),
        (LINKED_PATH, &linked_canonical, "reference"),
        (DANGLING_PATH, &dangling_canonical, "reference"),
    ]);
    let pins = [
        (OWNED_PATH, owned.as_path(), owned_pin.as_str()),
        (LINKED_PATH, linked_canonical.as_path(), linked_pin.as_str()),
        (
            DANGLING_PATH,
            dangling_canonical.as_path(),
            dangling_pin.as_str(),
        ),
    ];
    commit_project(&project_dir, &all, &pins);
    commit_project(&seat.join("projects").join(PROJECT), &all, &pins);

    let moved_from = tmp.join("weaveroot").join(DANGLING_PATH);
    std::fs::create_dir_all(seat.join("github/org")).unwrap();
    repoweave::symlink::create(
        &linked_canonical,
        &seat.join(LINKED_PATH),
        repoweave::symlink::LinkTarget::Directory,
    )
    .unwrap();
    repoweave::symlink::create(
        &moved_from,
        &seat.join(DANGLING_PATH),
        repoweave::symlink::LinkTarget::Directory,
    )
    .unwrap();
    assert!(
        seat.join(LINKED_PATH).exists() && !seat.join(DANGLING_PATH).exists(),
        "fixture: one link resolves and one dangles"
    );

    Weave {
        primary,
        seat,
        linked_canonical,
        dangling_canonical,
        moved_from,
        linked_pin,
        dangling_pin,
    }
}

/// Every ref and the checkout of a canonical clone.
fn canonical_state(canonical: &Path) -> (String, String, String) {
    (
        common::git_in(
            canonical,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        ),
        common::git_in(canonical, &["rev-parse", "HEAD"]),
        common::git_in(canonical, &["status", "--porcelain"]),
    )
}

fn store_of(checkout: &Path) -> PathBuf {
    let common_dir = common::git_in(checkout, &["rev-parse", "--git-common-dir"]);
    std::fs::canonicalize(checkout.join(common_dir)).unwrap()
}

fn assert_worktree_at(checkout: &Path, canonical: &Path, pin: &str) {
    assert!(
        !checkout.is_symlink(),
        "{} is still a symlink",
        checkout.display()
    );
    assert!(
        checkout.join(".git").is_file(),
        "{} must be a linked worktree",
        checkout.display()
    );
    assert_eq!(
        store_of(checkout),
        std::fs::canonicalize(canonical.join(".git")).unwrap(),
        "{} must link into its canonical clone's store",
        checkout.display()
    );
    assert_eq!(
        common::git_in(checkout, &["symbolic-ref", "--short", "HEAD"]),
        format!("{PROJECT}--{SEAT}"),
    );
    assert_eq!(common::git_in(checkout, &["rev-parse", "HEAD"]), pin);
}

/// The `workweave-drift` records `rwv doctor --json` emits, as
/// `(path, sub_kind)`.
fn drift_records(seat: &Path) -> Vec<(String, String)> {
    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(seat)
        .output()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "doctor --json must print JSON ({e}); stdout:\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    let mut records: Vec<(String, String)> = doc["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["kind"] == "workweave-drift")
        .map(|v| {
            (
                v["path"].as_str().unwrap().to_owned(),
                v["sub_kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    records.sort();
    records
}

fn assert_routes_to(stderr: &str, token: &str) {
    let routes: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(routes, vec![token], "stderr was:\n{stderr}");
    assert!(
        stderr.ends_with(&format!("\n\nrwv explain {token}\n")),
        "the route line must be the last line, after a blank one, stderr was:\n{stderr}"
    );
}

#[test]
fn doctor_reports_a_valid_and_a_dangling_reference_symlink() {
    let tmp = common::tempdir().unwrap();
    let weave = unmigrated_weave(tmp.path());

    let out = rwv()
        .args(["doctor"])
        .current_dir(&weave.seat)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a workweave whose members are symlinks is broken, and doctor must say so \
         with its exit status; stdout:\n{stdout}"
    );
    for path in [LINKED_PATH, DANGLING_PATH] {
        assert!(
            stdout.contains(&format!("symlink instead of a worktree at {path}")),
            "doctor must name {path}; stdout:\n{stdout}"
        );
    }

    assert_eq!(
        drift_records(&weave.seat),
        vec![
            (DANGLING_PATH.to_owned(), "symlink".to_owned()),
            (LINKED_PATH.to_owned(), "symlink".to_owned()),
        ],
        "both links are repairable without the network: the canonical clones hold \
         the pinned revisions"
    );
}

#[test]
fn doctor_fix_replaces_each_symlink_with_a_worktree_at_the_pinned_revision() {
    let tmp = common::tempdir().unwrap();
    let weave = unmigrated_weave(tmp.path());
    let linked_before = canonical_state(&weave.linked_canonical);
    let dangling_before = canonical_state(&weave.dangling_canonical);

    let out = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&weave.seat)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "doctor --fix must repair the workweave; stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains(&format!(
            "[fixed] core: created the worktree for {LINKED_PATH} in workweave {PROJECT}--{SEAT}"
        )),
        "stdout:\n{stdout}"
    );

    assert_worktree_at(
        &weave.seat.join(LINKED_PATH),
        &weave.linked_canonical,
        &weave.linked_pin,
    );
    assert_worktree_at(
        &weave.seat.join(DANGLING_PATH),
        &weave.dangling_canonical,
        &weave.dangling_pin,
    );
    assert!(
        !weave.moved_from.exists() && !weave.moved_from.parent().unwrap().exists(),
        "nothing may be created where the dangling link pointed"
    );

    for (canonical, before) in [
        (&weave.linked_canonical, linked_before),
        (&weave.dangling_canonical, dangling_before),
    ] {
        let after = canonical_state(canonical);
        assert_eq!(
            (&after.1, &after.2),
            (&before.1, &before.2),
            "the canonical clone's checkout is not touched"
        );
        let added: Vec<&str> = after
            .0
            .lines()
            .filter(|l| !before.0.lines().any(|b| b == *l))
            .collect();
        assert_eq!(
            added,
            vec![format!("refs/heads/{PROJECT}--{SEAT} {}", before.1)],
            "the only change to the canonical clone is the workweave's own branch"
        );
    }

    assert_eq!(
        drift_records(&weave.seat),
        vec![],
        "doctor reports no drift once the links are gone"
    );
    rwv()
        .args(["status"])
        .current_dir(&weave.seat)
        .assert()
        .success();
}

#[test]
fn doctor_at_the_primary_reports_and_repairs_a_workweaves_symlinks() {
    let tmp = common::tempdir().unwrap();
    let weave = unmigrated_weave(tmp.path());

    assert_eq!(
        drift_records(&weave.primary),
        vec![
            (DANGLING_PATH.to_owned(), "symlink".to_owned()),
            (LINKED_PATH.to_owned(), "symlink".to_owned()),
        ],
        "doctor at the primary answers for every workweave it already scans"
    );

    let out = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&weave.primary)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_worktree_at(
        &weave.seat.join(LINKED_PATH),
        &weave.linked_canonical,
        &weave.linked_pin,
    );
    assert_worktree_at(
        &weave.seat.join(DANGLING_PATH),
        &weave.dangling_canonical,
        &weave.dangling_pin,
    );
}

#[test]
fn verbs_refuse_an_unmigrated_workweave_and_name_doctor_fix() {
    let tmp = common::tempdir().unwrap();
    let weave = unmigrated_weave(tmp.path());
    let remedy = format!(
        "rwv -C {} doctor --fix",
        repoweave::path_spelling::operator_path(&weave.seat)
    );
    let primary_arg = weave.primary.to_string_lossy().into_owned();
    let seat_arg = weave.seat.to_string_lossy().into_owned();
    let linked_before = canonical_state(&weave.linked_canonical);

    let refusals: Vec<(&Path, Vec<&str>)> = vec![
        (&weave.seat, vec!["status"]),
        (&weave.seat, vec!["lock"]),
        (&weave.seat, vec!["update"]),
        (&weave.seat, vec!["sync", "primary"]),
        (&weave.seat, vec!["sync-to", primary_arg.as_str()]),
        (&weave.primary, vec!["sync", seat_arg.as_str()]),
        (&weave.primary, vec!["workweave", PROJECT, "delete", SEAT]),
        (
            &weave.primary,
            vec!["workweave", PROJECT, "create", "next", "--from", &seat_arg],
        ),
    ];
    for (dir, args) in refusals {
        let out = rwv().args(&args).current_dir(dir).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(1),
            "`rwv {}` must refuse; stderr:\n{stderr}",
            args.join(" ")
        );
        assert!(
            stderr.contains(&remedy) && stderr.contains(LINKED_PATH),
            "`rwv {}` must name the member and `{remedy}`; stderr:\n{stderr}",
            args.join(" ")
        );
        assert_routes_to(&stderr, "workweave-drift");
    }

    assert!(weave.seat.join(LINKED_PATH).is_symlink());
    assert!(weave.seat.join(DANGLING_PATH).is_symlink());
    assert!(
        !weave.seat.join(".rwv-op").exists(),
        "no op may have started"
    );
    assert!(
        !tmp.path().join(".workweaves").join("app--next").exists(),
        "the refused create must not have made a workweave"
    );
    assert_eq!(
        canonical_state(&weave.linked_canonical),
        linked_before,
        "a refused verb reaches nothing through the link"
    );
}

#[test]
fn abort_refuses_an_unmigrated_workweave_before_restoring_anything() {
    let tmp = common::tempdir().unwrap();
    let weave = unmigrated_weave(tmp.path());
    let op_id = "1700000000000000000";
    let pin = common::git_in(&weave.linked_canonical, &["rev-parse", "HEAD"]);
    common::git_in(
        &weave.linked_canonical,
        &["update-ref", &format!("refs/rwv/pre-op/{op_id}"), &pin],
    );
    let record = format!(
        "{{\"id\": \"{op_id}\", \"verb\": \"sync\", \"strategy\": \"rebase\", \"project\": \"{PROJECT}\", \
         \"source\": \"{root}\", \"target\": \"{root}\", \"retire\": false, \"phase\": \"replay\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \"overrides\": [], \
         \"started_at\": \"2026-05-27T10:00:00Z\"}}",
        root = common::json_escaped(&weave.seat),
    );
    std::fs::write(weave.seat.join(".rwv-op"), record).unwrap();

    let out = rwv()
        .args(["abort"])
        .current_dir(&weave.seat)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr:\n{stderr}");
    assert_routes_to(&stderr, "workweave-drift");
    assert_eq!(
        common::git_in(
            &weave.linked_canonical,
            &["for-each-ref", "--format=%(refname)", "refs/rwv/pre-abort/"],
        ),
        "",
        "abort must not write into the canonical clone through the link"
    );
}

/// The refusal's own remedy is runnable while an op is parked: abort refuses
/// on the link and names `doctor --fix`; `doctor --fix` migrates the member
/// with the op still parked; abort then restores. An op-in-progress guard on
/// the repair would leave the two verbs naming each other.
#[test]
fn doctor_fix_migrates_a_member_under_a_parked_op_and_abort_then_restores() {
    let tmp = common::tempdir().unwrap();
    let primary = tmp.path().join("primary");
    std::fs::create_dir_all(primary.join("github/org")).unwrap();
    let owned = primary.join(OWNED_PATH);
    let owned_pin = init_repo(&owned, "OWNED");
    let linked = primary.join(LINKED_PATH);
    let linked_pin = init_repo(&linked, "LINKED");
    let project_dir = primary.join("projects").join(PROJECT);
    init_repo(&project_dir, "PLACEHOLDER");
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    common::git_in(&project_dir, &["add", ".gitattributes"]);
    commit_project(
        &project_dir,
        &manifest(&[
            (OWNED_PATH, &owned, "owned"),
            (LINKED_PATH, &linked, "reference"),
        ]),
        &[
            (OWNED_PATH, &owned, &owned_pin),
            (LINKED_PATH, &linked, &linked_pin),
        ],
    );
    std::fs::write(primary.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    rwv()
        .args(["workweave", PROJECT, "create", SEAT])
        .current_dir(&primary)
        .assert()
        .success();
    let seat = tmp
        .path()
        .join(".workweaves")
        .join(format!("{PROJECT}--{SEAT}"));

    std::fs::write(owned.join("OWNED"), "primary\n").unwrap();
    common::git_in(&owned, &["commit", "-qam", "primary edit"]);
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&primary)
        .assert()
        .success();
    let seat_owned = seat.join(OWNED_PATH);
    std::fs::write(seat_owned.join("OWNED"), "seat\n").unwrap();
    common::git_in(&seat_owned, &["commit", "-qam", "seat edit"]);
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&seat)
        .assert()
        .success();
    let seat_owned_before = common::git_in(&seat_owned, &["rev-parse", "HEAD"]);
    let primary_owned_before = common::git_in(&owned, &["rev-parse", "main"]);

    let parked = rwv()
        .args(["sync-to", &primary.to_string_lossy(), "--strategy=rebase"])
        .current_dir(&seat)
        .output()
        .unwrap();
    assert!(
        !parked.status.success() && seat.join(".rwv-op").exists(),
        "fixture: the conflicting owned edit must park the sync-to; stderr:\n{}",
        String::from_utf8_lossy(&parked.stderr)
    );

    // What an older release left: the reference member as a link into the
    // canonical clone, with no worktree or branch of the workweave's own.
    let seat_linked = seat.join(LINKED_PATH);
    common::git_in(
        &linked,
        &[
            "worktree",
            "remove",
            "--force",
            &seat_linked.to_string_lossy(),
        ],
    );
    common::git_in(&linked, &["branch", "-D", &format!("{PROJECT}--{SEAT}")]);
    repoweave::symlink::create(
        &linked,
        &seat_linked,
        repoweave::symlink::LinkTarget::Directory,
    )
    .unwrap();

    let refused = rwv().args(["abort"]).current_dir(&seat).output().unwrap();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(refused.status.code(), Some(1), "stderr:\n{stderr}");
    assert!(
        stderr.contains(&format!(
            "rwv -C {} doctor --fix",
            repoweave::path_spelling::operator_path(&seat)
        )),
        "abort must name the repair; stderr:\n{stderr}"
    );
    assert_routes_to(&stderr, "workweave-drift");

    let fixed = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&seat)
        .output()
        .unwrap();
    assert!(
        seat.join(".rwv-op").exists(),
        "fixture: the op is still parked while doctor repairs"
    );
    assert!(
        String::from_utf8_lossy(&fixed.stdout).contains(&format!(
            "[fixed] core: created the worktree for {LINKED_PATH} in workweave {PROJECT}--{SEAT}"
        )),
        "doctor --fix must migrate the member under a parked op; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&fixed.stdout),
        String::from_utf8_lossy(&fixed.stderr)
    );
    assert_worktree_at(&seat_linked, &linked, &linked_pin);

    let aborted = rwv().args(["abort"]).current_dir(&seat).output().unwrap();
    assert!(
        aborted.status.success(),
        "abort must restore once the member is a worktree; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&aborted.stdout),
        String::from_utf8_lossy(&aborted.stderr)
    );
    assert!(!seat.join(".rwv-op").exists(), "abort clears the op");
    assert_eq!(
        common::git_in(&seat_owned, &["rev-parse", "HEAD"]),
        seat_owned_before,
        "abort restores the workweave's owned member to its pre-op tip"
    );
    assert_eq!(
        common::git_in(&owned, &["rev-parse", "main"]),
        primary_owned_before,
        "the target never moved"
    );
    assert_worktree_at(&seat_linked, &linked, &linked_pin);
}

#[test]
fn a_symlinked_member_whose_canonical_clone_is_gone_waits_for_fetch() {
    let tmp = common::tempdir().unwrap();
    let weave = unmigrated_weave(tmp.path());
    let upstream = tmp.path().join("upstream-dangling");
    common::git_in(
        tmp.path(),
        &[
            "clone",
            "--bare",
            &weave.dangling_canonical.to_string_lossy(),
            &upstream.to_string_lossy(),
        ],
    );
    std::fs::remove_dir_all(&weave.dangling_canonical).unwrap();
    for project_dir in [
        weave.primary.join("projects").join(PROJECT),
        weave.seat.join("projects").join(PROJECT),
    ] {
        let manifest_path = project_dir.join("rwv.toml");
        let lock_path = project_dir.join("rwv.lock");
        let from = format!(
            "file://{}",
            common::url_path(weave.primary.join(DANGLING_PATH))
        );
        let to = format!("file://{}", common::url_path(&upstream));
        for file in [&manifest_path, &lock_path] {
            let body = std::fs::read_to_string(file).unwrap();
            std::fs::write(file, body.replace(&from, &to)).unwrap();
        }
        common::git_in(&project_dir, &["commit", "-am", "upstream moved"]);
    }

    assert_eq!(
        drift_records(&weave.seat),
        vec![
            (DANGLING_PATH.to_owned(), "symlink-unfetched".to_owned()),
            (LINKED_PATH.to_owned(), "symlink".to_owned()),
        ]
    );

    let fixed = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&weave.seat)
        .output()
        .unwrap();
    assert_eq!(
        fixed.status.code(),
        Some(1),
        "the link doctor cannot repair is still an error; stdout:\n{}",
        String::from_utf8_lossy(&fixed.stdout)
    );
    assert!(
        weave.seat.join(DANGLING_PATH).is_symlink(),
        "doctor --fix must leave a link whose revision is not local for `rwv fetch`"
    );
    assert!(
        !weave.dangling_canonical.exists(),
        "doctor never clones: the network stays behind `rwv fetch`"
    );
    assert_worktree_at(
        &weave.seat.join(LINKED_PATH),
        &weave.linked_canonical,
        &weave.linked_pin,
    );

    let out = rwv()
        .args(["fetch"])
        .current_dir(&weave.seat)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fetch must clone the canonical and create the worktree; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_worktree_at(
        &weave.seat.join(DANGLING_PATH),
        &weave.dangling_canonical,
        &weave.dangling_pin,
    );
    assert_eq!(
        drift_records(&weave.seat),
        vec![],
        "fetch and doctor agree once fetch has run"
    );
}

#[test]
fn worktree_references_exits_2_saying_it_is_now_the_only_behavior() {
    let tmp = common::tempdir().unwrap();
    let out = rwv()
        .args(["workweave", PROJECT, "create", "x", "--worktree-references"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr:\n{stderr}");
    assert!(
        stderr.contains("`--worktree-references` has been removed")
            && stderr.contains("every member, `reference` included, is now a worktree"),
        "stderr:\n{stderr}"
    );
}
