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
//! Both populations are driven, because the remedy the finding names is not
//! one mechanism: in a project that generates a fully-owned file the record is
//! rebuilt as a side effect of the generation, and in one that generates
//! nothing there is no generation to ride on and the record is emptied
//! instead. A single-population suite here reported a live remedy for a
//! project it had never built.
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

/// Run `rwv` with the machine's own `PATH`. For a fixture where no enabled
/// integration has a generation to run, which tools are reachable decides
/// nothing, and narrowing would only cost the run its `git`.
fn rwv_here(args: &[&str], cwd: &Path) -> (bool, String) {
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
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
///
/// Every project it builds generates a fully-owned file, which is the input
/// that decides whether the remedy the finding names can do anything at all.
/// [`weave_with_nothing_to_generate`] is the fixture that diverges it.
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

/// The other population: a weave whose one member carries no `Cargo.toml`, so
/// cargo-workspace has no active cargo work and no enabled integration here
/// generates a fully-owned file at all. Nothing ever stamps the record.
///
/// What it holds constant, beside [`weave`]: one member, `owned` role, git,
/// and an ecosystem no builtin integration claims. It samples no project that
/// generates through a DIFFERENT ecosystem — the npm and uv integrations reach
/// the same ledger through the same stamp, and a project generating through
/// one of those is the cargo population as far as this file is concerned.
fn weave_with_nothing_to_generate(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let member = ws.join("github/acme/notes");
    std::fs::create_dir_all(&member).unwrap();
    std::fs::write(member.join("README.md"), "notes\n").unwrap();
    git_init_with_commit(&member);

    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/notes\"]\ntype = \"git\"\nurl = \"https://github.com/acme/notes.git\"\nversion = \"main\"\nrole = \"owned\"\n",
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
    assert_eq!(
        repoweave::owned_state::attested_owned_files(&ws.join("projects/app")),
        vec!["Cargo.lock".to_owned()],
        "and it must rebuild the record from the generation it just ran — a \
         repair that cleared the finding by emptying the record would pass the \
         assertion above while throwing away everything this project attests"
    );
}

/// The same remedy in the population that generates nothing. Here no hook
/// stamps, so nothing rwv does incidentally rewrites the record, and every
/// earlier step of materialize decides from a READ of the record it would have
/// to write: an unreadable one reads as empty, the steps find nothing to do,
/// and the file does not move. Before this arm, the one verb the finding names
/// exited 0 and left the finding standing, for as long as the operator kept
/// re-running it.
///
/// Not `#[cfg(unix)]`, unlike the arm above: that one needs a stand-in `cargo`
/// so the generation it measures happens on a machine with no toolchain, and
/// the stand-in is a shell script resolved off `PATH`. This fixture runs no
/// generation, so there is no tool for the machine to be missing.
#[test]
fn materialize_clears_the_finding_where_no_generator_can_rebuild_the_record() {
    let tmp = common::tempdir().unwrap();
    let ws = weave_with_nothing_to_generate(tmp.path());
    let project_dir = ws.join("projects/app");
    let ledger = project_dir.join(LEDGER);

    let (ok, out) = rwv_here(&["materialize"], &ws);
    assert!(ok, "{out}");
    assert!(
        !ledger.exists(),
        "the fixture is only the population it claims to be if a full \
         materialize leaves no record at all — a record written here would mean \
         something DID generate, and every assertion below would be measuring \
         the cargo path under another name"
    );

    std::fs::write(&ledger, "not a ledger {{{").unwrap();
    assert!(
        kinds(&ws).contains(&"unreadable-owned-state".to_owned()),
        "precondition: {:?}",
        kinds(&ws)
    );

    let (ok, out) = rwv_here(&["materialize"], &ws);
    assert!(ok, "{out}");
    assert!(
        !kinds(&ws).contains(&"unreadable-owned-state".to_owned()),
        "`rwv materialize` is the remedy the finding names here too: {:?}",
        kinds(&ws)
    );
    let rebuilt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&ledger).unwrap())
            .expect("what materialize leaves must be a record, not more unparseable bytes");
    assert_eq!(
        rebuilt,
        serde_json::json!({}),
        "and it must be the empty one: this project attests nothing, and a \
         record claiming otherwise would be a derivation nobody performed"
    );
    assert!(
        out.contains(LEDGER),
        "emptying the record destroys bytes the operator might have wanted to \
         read, so the run says which file it did that to:\n{out}"
    );
}

/// How a record arrives in the population above, so that arm is a shape
/// production writes rather than one only a test can build.
///
/// `rwv remove` is an intent verb: it rewrites the manifest and does not touch
/// the record, which is correct — dropping an attestation is materialize's job
/// and it announces each one. So between the removal of the last cargo member
/// and the next materialize, a live record naming `Cargo.lock` sits in a
/// project that can no longer generate one. That window is where the remedy
/// used to die.
#[test]
fn the_record_outlives_the_member_whose_generation_it_attests() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    let project_dir = ws.join("projects/app");

    let (ok, out) = rwv_here(&["remove", "github/acme/lib"], &ws);
    assert!(ok, "{out}");
    assert_eq!(
        repoweave::owned_state::attested_owned_files(&project_dir),
        vec!["Cargo.lock".to_owned()],
        "the record must survive the member, or this file's other population is \
         unreachable and the arm that drives it is fiction"
    );

    std::fs::write(project_dir.join(LEDGER), "not a ledger {{{").unwrap();
    assert!(
        kinds(&ws).contains(&"unreadable-owned-state".to_owned()),
        "precondition: {:?}",
        kinds(&ws)
    );
    let (ok, out) = rwv_here(&["materialize"], &ws);
    assert!(ok, "{out}");
    assert!(
        !kinds(&ws).contains(&"unreadable-owned-state".to_owned()),
        "and the remedy must reach it by the route an operator actually takes \
         to get here: {:?}",
        kinds(&ws)
    );
}

/// A weave with two projects — `app`, selected by `.rwv-active`, and `other`,
/// present but not selected — so the advice can be pinned against a project
/// `rwv materialize` reaches and one it does not, in the same run. Neither
/// project generates anything: [`weave_with_nothing_to_generate`]'s
/// population, so the fixture needs no stand-in `cargo` and the two-project
/// shape is the only thing under test.
fn two_project_weave(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    for name in ["app", "other"] {
        let project_dir = ws.join("projects").join(name);
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("rwv.toml"), "[repositories]\n").unwrap();
        common::fixture_lock(&project_dir, &[]);
        git_init_with_commit(&project_dir);
    }
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
    ws
}

/// The `active` field for the sole `unreadable-owned-state` finding naming
/// `project`, off `--json`'s `violations`.
fn unreadable_owned_state_active(ws: &Path, project: &str) -> bool {
    let report = doctor_json(ws, None);
    report["violations"]
        .as_array()
        .expect("violations array present")
        .iter()
        .find(|v| {
            v["kind"].as_str() == Some("unreadable-owned-state")
                && v["project"].as_str() == Some(project)
        })
        .unwrap_or_else(|| panic!("no unreadable-owned-state violation for `{project}`: {report}"))
        ["active"]
        .as_bool()
        .expect("active is a bool")
}

/// Rendered advice text does not reach `--json` at all: its `issues` channel
/// is integration hook output only, and the core-finding prose is rendered
/// text-mode-only. So the plain `rwv doctor` text an operator actually reads
/// is what these tests read too.
fn doctor_text(ws: &Path) -> String {
    let (_, out) = rwv_here(&["doctor"], ws);
    out
}

/// The defect this fixes: `other`'s record is corrupt, `app` is active,
/// and the advice for `other` must name the route that actually clears it —
/// `rwv materialize` takes no project argument, so it cannot.
///
/// Driven end to end, matching the bug's own repro: `rwv materialize` at
/// `app` leaves the `other` finding standing, and the named route
/// (`rwv activate other` then `rwv materialize`) is what clears it.
#[test]
fn inactive_project_advice_names_the_activation_route() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    std::fs::write(ws.join("projects/other").join(LEDGER), "not a ledger {{{").unwrap();

    let message = doctor_text(&ws);
    assert!(
        !unreadable_owned_state_active(&ws, "other"),
        "`other` is not `.rwv-active`'s selection: {message}"
    );
    assert!(
        message.contains("`other` is not the active project here"),
        "the advice must say `other` is not what `rwv materialize` would reach: {message}"
    );
    assert!(
        message.contains("rwv activate other"),
        "and must name the activation route the adjudication settled on: {message}"
    );
    assert!(
        !message.contains("Run `rwv materialize` to rebuild it: it re-derives"),
        "must not still hand out the remedy that cannot reach `other`: {message}"
    );

    // The bug's own repro: materialize at the active project leaves the
    // inactive one's finding standing.
    let (ok, out) = rwv_here(&["materialize"], &ws);
    assert!(ok, "{out}");
    assert!(
        kinds(&ws).contains(&"unreadable-owned-state".to_owned()),
        "materialize at `app` must not have touched `other`'s record: {:?}",
        kinds(&ws)
    );

    // The route the advice names, driven end to end.
    let (ok, out) = rwv_here(&["activate", "other"], &ws);
    assert!(ok, "{out}");
    let (ok, out) = rwv_here(&["materialize"], &ws);
    assert!(ok, "{out}");
    assert!(
        !kinds(&ws).contains(&"unreadable-owned-state".to_owned()),
        "once `other` is active, `rwv materialize` must reach it: {:?}",
        kinds(&ws)
    );
}

/// The half of the split that must not move: in the same multi-project weave,
/// the active project's advice is exactly what a single-project weave already
/// got — [`materialize_rebuilds_the_record_and_clears_the_finding`] and
/// [`materialize_clears_the_finding_where_no_generator_can_rebuild_the_record`]
/// pin that it is live; this pins that the wording survives a sibling project
/// existing at all.
#[test]
fn active_project_advice_is_unchanged_in_a_multi_project_weave() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    std::fs::write(ws.join("projects/app").join(LEDGER), "not a ledger {{{").unwrap();

    let message = doctor_text(&ws);
    assert!(
        unreadable_owned_state_active(&ws, "app"),
        "`app` is `.rwv-active`'s selection: {message}"
    );
    assert!(
        message.contains(
            "Run `rwv materialize` to rebuild it: it \
             re-derives the generated files this project has and \
             records them afresh, and leaves an empty record where it \
             has none"
        ),
        "the active-project advice must be byte-for-byte what it was before \
         `other` existed: {message}"
    );
    assert!(
        !message.contains("is not the active project here"),
        "the active project must not be told to activate itself: {message}"
    );
}
