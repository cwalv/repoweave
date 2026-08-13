//! Generated state that no longer follows from the inputs it was derived from.
//!
//! `rwv sync` announces the condition once, at a moment the operator may not be
//! reading, and the condition then persists with nothing standing behind it.
//! What is pinned here is that doctor answers the same question from present
//! state at any later moment, and answers it from this checkout alone: no
//! source workspace is consulted, nothing is regenerated to compare against,
//! and no record of what used to be true is kept.
//!
//! Driven through the shipped binary, and the `--json` assertions parse the
//! envelope rather than grepping it — an agent branching on `kind` is the
//! consumer this exists for, and a text-only assertion would be green with the
//! typed surface dropping the finding entirely.

use std::path::{Path, PathBuf};

mod common;

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {args:?} in {} failed", dir.display());
}

fn git_init_with_commit(dir: &Path) {
    git(&["init", "--initial-branch=main"], dir);
    git(&["config", "user.email", "test@test.com"], dir);
    git(&["config", "user.name", "Test"], dir);
    git(&["add", "-A"], dir);
    git(&["commit", "-m", "init"], dir);
}

fn rwv(args: &[&str], cwd: &Path) -> (bool, String) {
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

fn doctor_json(ws: &Path) -> serde_json::Value {
    let output = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(ws)
        .output()
        .expect("rwv should run");
    serde_json::from_slice(&output.stdout).expect("`--json` must emit parseable JSON")
}

/// The advisories doctor raises, as an agent reads them.
fn advisories(ws: &Path) -> Vec<serde_json::Value> {
    doctor_json(ws)["advisories"]
        .as_array()
        .expect("`advisories` must be present, empty rather than absent")
        .clone()
}

const LEDGER: &str = ".rwv-owned-digests";
const LOCK: &str = "version = 4\n";

/// Two `rwv.lock` bodies that both parse. The staleness axis is about bytes
/// changing under a generation, so an unparseable lock would test doctor's
/// manifest gate instead — that gate reports first and returns.
const EMPTY_LOCK: &str = "{\n  \"repositories\": {}\n}\n";
const MOVED_LOCK: &str = "{\n  \"repositories\": {}\n}\n\n";

/// A weave whose `Cargo.lock` is attested as a generation, with the inputs that
/// generation read recorded beside it.
///
/// Written through the library's own stamp rather than as a literal, so the
/// fixture cannot drift from the shape production writes — a hand-written
/// ledger would keep parsing after the writer changed and quietly stop testing
/// anything.
fn weave(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(ws.join("github")).unwrap();

    std::fs::write(project_dir.join("rwv.toml"), "[repositories]\n").unwrap();
    std::fs::write(project_dir.join("rwv.lock"), EMPTY_LOCK).unwrap();
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

    std::fs::write(project_dir.join("Cargo.lock"), LOCK).unwrap();
    let project = repoweave::manifest::ProjectName::new("app").unwrap();
    repoweave::integrations::merge::stamp_owned_generation(
        &project_dir,
        "Cargo.lock",
        LOCK.as_bytes(),
        repoweave::integrations::merge::generation_inputs(&project_dir, &project),
    )
    .unwrap();
    ws
}

/// Nothing has moved, so nothing is reported. The control for every assertion
/// below: a check that fires unconditionally would pass all of them.
#[test]
fn a_generation_whose_inputs_have_not_moved_is_silent() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());

    let (_, report) = rwv(&["doctor"], &ws);
    assert!(
        !report.contains("may no longer match"),
        "a current generation must not be reported:\n{report}"
    );
    assert!(
        advisories(&ws).is_empty(),
        "and the typed surface must agree"
    );
}

/// The measured condition: the lock and its record agree with each other while
/// both are stale against the checkout's own inputs. Nothing reads as corrupt,
/// which is why no drift check ever saw it.
#[test]
fn an_input_that_moved_makes_the_generation_stale_on_both_surfaces() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    let project_dir = ws.join("projects/app");

    // The lock and its digest still agree — this is not drift.
    std::fs::write(project_dir.join("rwv.lock"), MOVED_LOCK).unwrap();

    let (_, report) = rwv(&["doctor"], &ws);
    assert!(
        report.contains("projects/app/Cargo.lock may no longer match this checkout"),
        "doctor must name the generated file:\n{report}"
    );
    assert!(
        report.contains("projects/app/rwv.lock"),
        "and the input that moved:\n{report}"
    );
    assert!(
        report.contains("rwv materialize"),
        "and a remedy runnable where it printed:\n{report}"
    );

    let found = advisories(&ws);
    assert_eq!(
        found.len(),
        1,
        "one advisory, one stale generation: {found:?}"
    );
    assert_eq!(found[0]["kind"], "derived_state_stale");
    assert_eq!(found[0]["remedy"], "rwv materialize");
    assert_eq!(
        found[0]["inputs"],
        serde_json::json!(["projects/app/rwv.lock"]),
        "the advisory carries paths, never prose to grep"
    );
}

/// The ratified reading of a ledger written before inputs were attested: stale.
///
/// It is the honest answer rather than a lenient one. rwv accepted those bytes
/// without recording what produced them, so it cannot claim they still follow
/// from anything — and the operator can clear it, which is what keeps an honest
/// answer from being a permanent warning.
#[test]
fn an_entry_with_no_recorded_inputs_reads_stale_and_is_clearable() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    let project_dir = ws.join("projects/app");

    // The pre-amendment spelling: a bare digest, no inputs.
    repoweave::integrations::merge::stamp_owned_digest(&project_dir, "Cargo.lock", LOCK.as_bytes())
        .unwrap();

    let (_, report) = rwv(&["doctor"], &ws);
    assert!(
        report.contains("accepted without a record of what produced it"),
        "the reason must say what is unknown, not merely that something is:\n{report}"
    );
    let found = advisories(&ws);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        found[0]["inputs"],
        serde_json::json!([]),
        "no input is known to have moved, so none is claimed"
    );

    let (ok, materialized) = rwv(&["materialize"], &ws);
    assert!(ok, "the named remedy must run:\n{materialized}");
    assert!(
        advisories(&ws).is_empty(),
        "the first generation rewrites the entry in the attested shape, so the \
         condition heals itself"
    );
}

/// An input that did not exist when the generation ran and exists now has moved
/// the derivation just as much as one whose bytes changed. Recording only what
/// was readable would make an appearing input invisible.
#[test]
fn an_input_that_appeared_since_the_generation_counts_as_moved() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    let project_dir = ws.join("projects/app");

    std::fs::remove_file(project_dir.join("rwv.lock")).unwrap();
    let project = repoweave::manifest::ProjectName::new("app").unwrap();
    repoweave::integrations::merge::stamp_owned_generation(
        &project_dir,
        "Cargo.lock",
        LOCK.as_bytes(),
        repoweave::integrations::merge::generation_inputs(&project_dir, &project),
    )
    .unwrap();
    assert!(
        advisories(&ws).is_empty(),
        "precondition: a generation over the inputs that exist is current"
    );

    std::fs::write(project_dir.join("rwv.lock"), EMPTY_LOCK).unwrap();

    let found = advisories(&ws);
    assert_eq!(
        found.len(),
        1,
        "an appearing input is a moved input: {found:?}"
    );
    assert_eq!(
        found[0]["inputs"],
        serde_json::json!(["projects/app/rwv.lock"])
    );
}

/// The ratified prohibition: the ledger gains inputs without a version bump.
///
/// A version field would be a compatibility promise on a machine-local file
/// that regenerates on the next materialize, and the pre-amendment shape is
/// already handled by reading it as stale. To make this fail, add a version key
/// to what `write_owned_digests` serializes; the entries here are written by the
/// production writer, so nothing else can.
#[test]
fn the_ledger_carries_no_version_field() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(ws.join("projects/app").join(LEDGER)).unwrap(),
    )
    .expect("the ledger is JSON");

    let entries = ledger.as_object().expect("a flat map of file to entry");
    assert_eq!(
        entries.keys().collect::<Vec<_>>(),
        vec!["Cargo.lock"],
        "every top-level key names a generated file; a version key would be the \
         one that does not: {ledger:#}"
    );
    let entry = entries["Cargo.lock"]
        .as_object()
        .expect("an attested entry is an object");
    assert_eq!(
        entry.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["digest", "inputs"],
        "the entry carries what it attests and nothing else: {ledger:#}"
    );
    assert!(
        entry["inputs"].as_object().is_some_and(|inputs| inputs
            .values()
            .all(|digest| digest.as_str().is_some_and(|d| d.starts_with("sha256:")))),
        "inputs map workspace-relative paths to digests: {ledger:#}"
    );
}

/// A weave whose attested `Cargo.lock` has a real producer: one Rust member and
/// cargo-workspace enabled, so the hook that regenerates and re-attests is
/// reachable.
///
/// The cargo-free fixture above cannot carry this claim. With nothing producing
/// the lock, materialize drops the attestation instead of re-earning it, which
/// is correct and is not the composition being tested.
fn weave_with_a_producer(root: &Path) -> PathBuf {
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
    std::fs::write(project_dir.join("rwv.lock"), EMPTY_LOCK).unwrap();
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

    let ctx = repoweave::workspace::WorkspaceContext::resolve(&ws, None).unwrap();
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
    repoweave::integrations::merge::stamp_owned_generation(
        &project_dir,
        "Cargo.lock",
        LOCK.as_bytes(),
        repoweave::integrations::merge::generation_inputs(&project_dir, &project),
    )
    .unwrap();
    ws
}

/// A2 and A3 compose rather than deadlock. A stale entry on a drifted file
/// still refuses, because drift is the more specific condition and its consents
/// are the ones that describe the loss — and taking either consent clears both,
/// because the generation that follows attests its inputs.
#[test]
fn a_stale_entry_on_a_drifted_file_is_not_a_deadlock() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let tmp = common::tempdir().unwrap();
    let ws = weave_with_a_producer(tmp.path());
    let project_dir = ws.join("projects/app");

    std::fs::write(project_dir.join("rwv.lock"), MOVED_LOCK).unwrap();
    std::fs::write(project_dir.join("Cargo.lock"), "version = 3\n").unwrap();

    let (ok, refused) = rwv(&["materialize"], &ws);
    assert!(!ok, "drift still refuses:\n{refused}");
    assert!(
        refused.contains("--adopt-drifted") && refused.contains("--regenerate-drifted"),
        "and still names both consents:\n{refused}"
    );

    let (ok, adopted) = rwv(&["materialize", "--adopt-drifted"], &ws);
    assert!(ok, "a consent must get past both conditions:\n{adopted}");
    assert!(
        advisories(&ws).is_empty(),
        "and the staleness must be gone with it — a flag that clears one \
         condition into the other is the deadlock this asserts against"
    );
}
