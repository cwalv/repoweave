//! rwv's record of the generations it accepted, when it is present and cannot
//! be read.
//!
//! The record is what `managed-file-drift` and `derived-state-stale` decide
//! from. Both read an unreadable one as "nothing is attested", so both report
//! nothing — and the project then looks exactly like a clean one, including
//! when a generated file had already drifted. What is pinned here is that the
//! fault is reported instead, and that the two silences the record is
//! *supposed* to keep are still kept.
//!
//! Driven through the shipped binary, and the assertions parse the `--json`
//! envelope rather than grepping prose: an agent branching on `kind` is the
//! consumer this exists for.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

mod common;

const LEDGER: &str = ".rwv-owned-digests";
const LOCK: &str = "version = 4\n";
fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

/// `path` replaces the child's whole `PATH`, so which tools the run can reach
/// is the caller's decision rather than the machine's.
fn rwv_on_path(args: &[&str], cwd: &Path, path: &OsStr) -> (bool, String) {
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
        .env("PATH", path)
        .output()
        .expect("rwv should run");
    (
        output.status.success(),
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

/// `Some(path)` narrows the doctor run the same way; `None` inherits.
fn doctor_json(ws: &Path, path: Option<&OsStr>) -> serde_json::Value {
    let mut cmd = common::rwv();
    cmd.args(["doctor", "--json"]).current_dir(ws);
    if let Some(path) = path {
        cmd.env("PATH", path);
    }
    let output = cmd.output().expect("rwv should run");
    serde_json::from_slice(&output.stdout).expect("`--json` must emit parseable JSON")
}

/// Every finding `kind` doctor raises, across the channels it raises them on.
fn kinds_with_path(ws: &Path, path: Option<&OsStr>) -> Vec<String> {
    let report = doctor_json(ws, path);
    let mut out = Vec::new();
    for channel in ["violations", "issues", "advisories"] {
        for finding in report[channel]
            .as_array()
            .expect("every channel is present, empty rather than absent")
        {
            if let Some(kind) = finding["kind"].as_str() {
                out.push(kind.to_owned());
            }
        }
    }
    out.sort();
    out
}

fn kinds(ws: &Path) -> Vec<String> {
    kinds_with_path(ws, None)
}

/// A weave with a real owned member, so the cargo-workspace integration is
/// active and its managed-file axis actually runs — the axis whose silence is
/// the subject here. Its `Cargo.lock` is attested through the library's own
/// stamp, so the fixture cannot drift from the shape production writes.
fn weave(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let member = ws.join("github/acme/lib");
    std::fs::create_dir_all(member.join("src")).unwrap();
    std::fs::write(
        member.join("Cargo.toml"),
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(member.join("src/lib.rs"), "").unwrap();
    git_init_with_commit(&member);

    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/lib\"]\ntype = \"git\"\nurl = \"https://github.com/acme/lib.git\"\nversion = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();
    common::fixture_lock(&project_dir, &[]);
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

    let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        "app",
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("intent activation should author the workspace manifest");

    std::fs::write(project_dir.join("Cargo.lock"), LOCK).unwrap();
    let project = repoweave::manifest::ProjectName::new("app").unwrap();
    repoweave::owned_state::stamp_owned_generation(
        &project_dir,
        "Cargo.lock",
        LOCK.as_bytes(),
        repoweave::owned_state::ObservedInputs::observe(&project_dir, &project, &ws),
    )
    .unwrap();
    ws
}

/// The control every assertion below rests on. A finding that fired
/// unconditionally would satisfy them all.
#[test]
fn an_attested_project_raises_nothing_about_its_record() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    assert!(
        !kinds(&ws).contains(&"unreadable-owned-state".to_owned()),
        "a readable record must not be reported: {:?}",
        kinds(&ws)
    );
}

/// The silence that must survive: a weave that has never stamped anything has
/// no record, and absence is not a fault. Reporting here would fire on every
/// fresh and every pre-upgrade weave.
#[test]
fn an_absent_record_is_not_a_finding() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    std::fs::remove_file(ws.join("projects/app").join(LEDGER)).unwrap();
    assert!(
        !kinds(&ws).contains(&"unreadable-owned-state".to_owned()),
        "an absent record is a legitimate state: {:?}",
        kinds(&ws)
    );
}

/// The defect. A record that is present and is not a record takes the checks
/// that read it silent; the finding is what says so.
#[test]
fn a_present_but_unparseable_record_is_reported() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    std::fs::write(ws.join("projects/app").join(LEDGER), "not a ledger {{{").unwrap();

    assert!(
        kinds(&ws).contains(&"unreadable-owned-state".to_owned()),
        "a present-but-unparseable record must not pass as an absent one: {:?}",
        kinds(&ws)
    );
}

/// The reason the finding is worth its noise: without it, losing the record
/// also loses every drift it had already been reporting, and the report goes
/// from naming a drifted file to naming nothing at all.
#[test]
fn losing_the_record_does_not_silently_lose_a_drift_it_was_reporting() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    let project_dir = ws.join("projects/app");

    // A generated file rewritten behind rwv's back, with the record intact.
    std::fs::write(project_dir.join("Cargo.lock"), "version = 3\n").unwrap();
    let with_record = kinds(&ws);
    assert!(
        with_record.contains(&"managed-file-drift".to_owned()),
        "precondition: the drift is reported while the record is readable: {with_record:?}"
    );

    // The same drift, with the record no longer readable.
    std::fs::write(project_dir.join(LEDGER), "not a ledger {{{").unwrap();
    let without_record = kinds(&ws);
    assert!(
        !without_record.contains(&"managed-file-drift".to_owned()),
        "MEASURED: the drift axis cannot run without the record — if this ever \
         starts reporting, the finding below is no longer carrying the silence \
         and this test should say so instead: {without_record:?}"
    );
    assert!(
        without_record.contains(&"unreadable-owned-state".to_owned()),
        "so the report must say the check did not run, rather than going quiet: \
         {without_record:?}"
    );
}

/// The named remedy actually clears it. A finding whose fix does not work
/// leaves the operator worse off than the silence did.
///
/// The claim is that materialize REBUILDS THE RECORD, which is rwv's own
/// write; the ecosystem tool is reached only because the run regenerates the
/// managed set on the way there. So the run gets a stand-in `cargo` and no
/// other tool, and the test measures the same thing on a machine with no
/// toolchain installed as on one with every toolchain.
///
/// Unix only, since the stand-in is a script resolved off `PATH` —
/// `common::cargo_stand_in_path` states why that does not port.
#[cfg(unix)]
#[test]
fn materialize_rebuilds_the_record_and_clears_the_finding() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    let stand_in = common::cargo_stand_in_path(&tmp.path().join("stand-in-bin"));
    let path = stand_in.as_os_str();
    std::fs::write(ws.join("projects/app").join(LEDGER), "not a ledger {{{").unwrap();
    assert!(kinds_with_path(&ws, Some(path)).contains(&"unreadable-owned-state".to_owned()));

    let (ok, out) = rwv_on_path(&["materialize"], &ws, path);
    assert!(ok, "{out}");
    assert!(
        !kinds_with_path(&ws, Some(path)).contains(&"unreadable-owned-state".to_owned()),
        "`rwv materialize` is the remedy the finding names: {:?}",
        kinds_with_path(&ws, Some(path))
    );
}
