//! The disk scan must reach a member in every registry segment a manifest can
//! name, not only the built-in three.
//!
//! `rwv add` mints the first path segment from the source URL: `local` for a
//! `file://` source, the URL's own host for anything else no built-in matched.
//! So the segments a manifest can hold are an open set, and a scan that walks
//! a fixed list of them cannot see a member outside it. Everything `rwv
//! doctor` keys on that scan then goes quiet for such a member at once — its
//! manifest entry reads as a dangling reference, and its lock is never
//! compared against its HEAD.
//!
//! The `local/` half of each pin is driven through `rwv add` from a `file://`
//! source, the production route. The host-derived half is written into the
//! manifest directly, in the shape `rwv add https://codeberg.org/acme/lib`
//! writes: minting that segment through `rwv add` needs the host to be
//! reachable, and the suite runs offline.

use std::path::{Path, PathBuf};

mod common;

/// A weave holding one project, one `file://` member added through `rwv add`,
/// and a lock covering it.
///
/// Returns the weave root and the origin the member was cloned from.
fn weave_with_a_file_url_member(tmp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
    let origin = tmp.path().join("origin").join("acme").join("widgets.git");
    std::fs::create_dir_all(origin.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&origin);

    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();

    common::rwv()
        .args(["init", "demo"])
        .current_dir(&ws)
        .assert()
        .success();
    common::rwv()
        .args(["add", &common::file_url(&origin)])
        .current_dir(&ws)
        .assert()
        .success();
    common::rwv()
        .args(["lock"])
        .current_dir(&ws)
        .assert()
        .success();

    (ws, origin)
}

/// Every violation `rwv doctor` reports, as `(kind, path)` pairs.
///
/// `path` is empty for a finding that names no repo, which keeps a caller's
/// filter honest: a kind that stopped carrying a path shows up as a
/// mismatched pair rather than silently dropping out of the list.
fn doctor_findings(ws: &Path, args: &[&str]) -> Vec<(String, String)> {
    let out = common::rwv()
        .arg("doctor")
        .args(args)
        .arg("--json")
        .current_dir(ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json must parse ({e}):\n{stdout}"));
    report["violations"]
        .as_array()
        .expect("the report carries a violations array")
        .iter()
        .map(|v| {
            (
                v["kind"].as_str().unwrap_or_default().to_owned(),
                v["path"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// A member `rwv add` placed under `local/` is on disk, so `rwv doctor`
/// straight after the add reports nothing at all.
///
/// The defect: the manifest entry reported as `dangling-reference` — "listed
/// in rwv.toml but not cloned on disk" — for a clone `rwv add` had just
/// written, with `rwv fetch` printed as the repair. That advice cannot clear
/// it; fetch skips a member whose directory already exists.
#[test]
fn a_file_url_member_is_clean_immediately_after_add() {
    let tmp = common::tempdir().unwrap();
    let (ws, _origin) = weave_with_a_file_url_member(&tmp);

    assert!(
        ws.join("local/acme/widgets").is_dir(),
        "`rwv add` from a file:// source places the clone under local/"
    );
    assert_eq!(
        doctor_findings(&ws, &[]),
        Vec::new(),
        "a just-added member is on disk and pinned by the lock"
    );
}

/// The scan reaches the `local/` member, rather than the member having
/// dropped out of every scan.
///
/// A vanished `dangling-reference` is consistent with both, so this drives a
/// finding that only fires for a repo the scan found: `stale-lock` compares
/// the lock against a HEAD read from the on-disk repos, and skips any member
/// with no HEAD recorded.
#[test]
fn a_file_url_member_reports_stale_lock_when_it_drifts() {
    let tmp = common::tempdir().unwrap();
    let (ws, _origin) = weave_with_a_file_url_member(&tmp);

    let member = ws.join("local/acme/widgets");
    common::git_in(&member, &["commit", "--allow-empty", "-m", "past the lock"]);

    let findings = doctor_findings(&ws, &[]);
    assert!(
        findings.contains(&("stale-lock".to_owned(), "local/acme/widgets".to_owned())),
        "a drifted local/ member must be compared against its lock, got: {findings:?}"
    );
}

/// A member in a host-derived segment is discovered too, and so is a stray
/// clone sitting beside it.
///
/// `local` is the segment reachable offline, so a walk special-cased to that
/// one name would satisfy the pins above while leaving every self-hosted host
/// invisible. The two findings here are opposite verdicts on two clones in a
/// segment no built-in registry names, and a walk that cannot see the
/// directory can render neither.
#[test]
fn a_host_derived_segment_is_walked_for_both_verdicts() {
    let tmp = common::tempdir().unwrap();
    let (ws, _origin) = weave_with_a_file_url_member(&tmp);

    let declared_origin = tmp.path().join("origin").join("acme").join("lib.git");
    common::init_bare_repo_with_commit(&declared_origin);
    let stray_origin = tmp.path().join("origin").join("acme").join("stray.git");
    common::init_bare_repo_with_commit(&stray_origin);

    let segment = ws.join("codeberg.org").join("acme");
    std::fs::create_dir_all(&segment).unwrap();
    for (origin, name) in [(&declared_origin, "lib"), (&stray_origin, "stray")] {
        common::git_in(
            &segment,
            &[
                "clone",
                origin.to_str().unwrap(),
                segment.join(name).to_str().unwrap(),
            ],
        );
    }

    let manifest_path = ws.join("projects").join("demo").join("rwv.toml");
    let mut manifest = std::fs::read_to_string(&manifest_path).unwrap();
    manifest.push_str(&format!(
        "\n[repositories.\"codeberg.org/acme/lib\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"owned\"\n",
        common::file_url(&declared_origin)
    ));
    std::fs::write(&manifest_path, manifest).unwrap();

    common::rwv()
        .args(["activate", "demo"])
        .current_dir(&ws)
        .assert()
        .success();
    common::rwv()
        .args(["lock"])
        .current_dir(&ws)
        .assert()
        .success();

    let findings = doctor_findings(&ws, &["--all"]);
    assert!(
        !findings.contains(&(
            "dangling-reference".to_owned(),
            "codeberg.org/acme/lib".to_owned()
        )),
        "the declared clone is on disk, so it is not a dangling reference: {findings:?}"
    );
    assert!(
        findings.contains(&(
            "orphaned-clone".to_owned(),
            "codeberg.org/acme/stray".to_owned()
        )),
        "the undeclared clone beside it is an orphan, which only a walk that \
         reaches the directory can say: {findings:?}"
    );
}

/// A built-in segment is walked even when no manifest names a member there.
///
/// The manifests are what open the walk past the built-in three, so a walk
/// derived from them alone would stop reaching `github/` in a weave that
/// declares nothing under it — and the orphan finding, which exists to report
/// exactly what no manifest mentions, would go silent in the one weave that
/// most needs it.
#[test]
fn a_builtin_segment_is_walked_with_no_member_declared_there() {
    let tmp = common::tempdir().unwrap();
    let (ws, _origin) = weave_with_a_file_url_member(&tmp);

    let stray_origin = tmp.path().join("origin").join("acme").join("stray.git");
    common::init_bare_repo_with_commit(&stray_origin);
    let owner = ws.join("github").join("acme");
    std::fs::create_dir_all(&owner).unwrap();
    common::git_in(
        &owner,
        &[
            "clone",
            stray_origin.to_str().unwrap(),
            owner.join("stray").to_str().unwrap(),
        ],
    );

    let manifest = std::fs::read_to_string(ws.join("projects").join("demo").join("rwv.toml"))
        .expect("the project manifest is readable");
    assert!(
        !manifest.contains("github/"),
        "the pin needs a manifest that names nothing under github/, got:\n{manifest}"
    );

    let findings = doctor_findings(&ws, &["--all"]);
    assert!(
        findings.contains(&("orphaned-clone".to_owned(), "github/acme/stray".to_owned())),
        "a stray clone under a built-in segment is an orphan whatever the \
         manifests declare, got: {findings:?}"
    );
}

/// The repair `dangling-reference` prints still works on the population the
/// finding is left with.
///
/// The finding is correct for a member whose clone is genuinely absent, and
/// `rwv fetch` re-clones it from the manifest URL — including a `file://` one.
#[test]
fn fetch_clears_a_dangling_file_url_member() {
    let tmp = common::tempdir().unwrap();
    let (ws, _origin) = weave_with_a_file_url_member(&tmp);

    std::fs::remove_dir_all(ws.join("local/acme/widgets")).unwrap();
    assert!(
        doctor_findings(&ws, &[]).contains(&(
            "dangling-reference".to_owned(),
            "local/acme/widgets".to_owned()
        )),
        "a member whose clone is gone is a dangling reference"
    );

    common::rwv()
        .args(["fetch"])
        .current_dir(&ws)
        .assert()
        .success();

    assert_eq!(
        doctor_findings(&ws, &[]),
        Vec::new(),
        "the advice the finding prints re-materializes the member"
    );
}
