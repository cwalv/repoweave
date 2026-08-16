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

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
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

/// The `kind`s doctor raises against `repo`, sorted.
fn violation_kinds_for(ws: &Path, repo: &str) -> Vec<String> {
    let mut kinds: Vec<String> = doctor_json(ws)["violations"]
        .as_array()
        .expect("`violations` must be present, empty rather than absent")
        .iter()
        .filter(|v| v["path"].as_str() == Some(repo))
        .map(|v| {
            v["kind"]
                .as_str()
                .expect("a violation carries a kind")
                .to_owned()
        })
        .collect();
    kinds.sort();
    kinds
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
    repoweave::owned_state::stamp_owned_generation(
        &project_dir,
        "Cargo.lock",
        LOCK.as_bytes(),
        repoweave::owned_state::generation_inputs(&project_dir, &project, &ws),
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
    repoweave::owned_state::stamp_owned_digest(&project_dir, "Cargo.lock", LOCK.as_bytes())
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
    repoweave::owned_state::stamp_owned_generation(
        &project_dir,
        "Cargo.lock",
        LOCK.as_bytes(),
        repoweave::owned_state::generation_inputs(&project_dir, &project, &ws),
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
        repoweave::owned_state::generation_inputs(&project_dir, &project, &ws),
    )
    .unwrap();
    ws
}

/// A weave whose owned member depends, via a `path =` dependency, on a
/// directory that is not itself a member: not registry-shaped
/// (`<registry>/<owner>/<repo>`), so `rwv.lock` pins nothing for it and
/// `scan_repos_on_disk` never walks it.
fn weave_with_a_path_dep(root: &Path) -> (PathBuf, PathBuf) {
    let ws = root.join("ws");
    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let outside = root.join("outside-helper");
    std::fs::create_dir_all(outside.join("src")).unwrap();
    std::fs::write(
        outside.join("Cargo.toml"),
        "[package]\nname = \"helper\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(outside.join("src/lib.rs"), "").unwrap();

    let member = ws.join("github/acme/lib");
    std::fs::create_dir_all(member.join("src")).unwrap();
    std::fs::write(
        member.join("Cargo.toml"),
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\nhelper = { path = \"../../../../outside-helper\" }\n",
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

    let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        "app",
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("intent activation should author the workspace manifest");

    (ws, outside)
}

/// A `path =` dependency into a directory that is not itself a member has no
/// `rwv.lock` entry to pin its commit, and the directory is not
/// registry-shaped, so no scan walks it either. The join point that closes
/// the silence is a digest of the target `Cargo.toml` itself: the
/// generation reads it, so the ledger records it, so doctor answers from it.
///
/// Driven through the shipped binary end-to-end on the SANCTIONED route
/// (`rwv materialize`, the one the operator is told to run), asserting the
/// signal fires between the edit and the next materialize — where before
/// this fix the whole route was permanently silent.
#[test]
fn a_path_dependency_into_a_non_member_directory_fires_the_staleness_axis() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let tmp = common::tempdir().unwrap();
    let (ws, outside) = weave_with_a_path_dep(tmp.path());
    let project_dir = ws.join("projects/app");

    let (ok, first) = rwv(&["materialize"], &ws);
    assert!(
        ok,
        "first materialize should resolve the path dep for real:\n{first}"
    );

    let lock_text = std::fs::read_to_string(project_dir.join("Cargo.lock")).unwrap();
    assert!(
        lock_text.contains("name = \"helper\"") && lock_text.contains("version = \"0.1.0\""),
        "the generated lock must carry the path dep's real resolve:\n{lock_text}"
    );
    assert!(
        advisories(&ws).is_empty(),
        "precondition: the generation is current"
    );

    // Edit the non-member directory. No commit: it carries no git repo at
    // all, and it is not registry-shaped, so nothing in a weave scans it.
    let helper_manifest = outside.join("Cargo.toml");
    let declared = std::fs::read_to_string(&helper_manifest).unwrap();
    std::fs::write(&helper_manifest, declared.replace("0.1.0", "0.2.0")).unwrap();

    // The join point: the outside `Cargo.toml` is an attested input, so
    // moving its bytes moves the derivation, and doctor names it as such
    // WITHOUT anything else having to rewrite `Cargo.lock` first. Before
    // this axis widened, this call was silent — and stayed silent through
    // the second materialize below, absorbing the edit permanently.
    let found = advisories(&ws);
    assert_eq!(
        found.len(),
        1,
        "the outside manifest's edit is a moved input on the same axis: {found:?}"
    );
    assert_eq!(found[0]["kind"], "derived_state_stale");
    assert_eq!(found[0]["remedy"], "rwv materialize");
    let inputs = found[0]["inputs"]
        .as_array()
        .expect("advisory carries a paths array");
    assert!(
        inputs
            .iter()
            .any(|v| v.as_str() == Some("../outside-helper/Cargo.toml")),
        "the moved-inputs list names the outside manifest, spelled relative \
         to the workspace root so an agent that never sees this repo can \
         still resolve it: {inputs:?}"
    );

    // And the sanctioned exit clears it: the same materialize that
    // regenerates `Cargo.lock` also re-hashes the outside manifest and
    // re-stamps, so the advisory that fired above is what stands between
    // silence and coverage — not something the operator has to keep
    // remembering.
    let (ok, materialized) = rwv(&["materialize"], &ws);
    assert!(ok, "{materialized}");
    let lock_text = std::fs::read_to_string(project_dir.join("Cargo.lock")).unwrap();
    assert!(
        lock_text.contains("version = \"0.2.0\""),
        "the edit reached Cargo.lock: {lock_text}"
    );
    assert!(
        advisories(&ws).is_empty(),
        "and the re-stamp cleared the staleness: {:?}",
        advisories(&ws)
    );
}

/// The one route that does catch it: something other than `rwv materialize`
/// touching `Cargo.lock` first. Confirms the recorded-digest DRIFT axis is
/// live for this file even though nothing upstream of it is watching the
/// path dependency.
#[test]
fn a_bare_cargo_run_after_the_edit_is_caught_by_drift() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let tmp = common::tempdir().unwrap();
    let (ws, outside) = weave_with_a_path_dep(tmp.path());

    let (ok, first) = rwv(&["materialize"], &ws);
    assert!(ok, "{first}");

    let helper_manifest = outside.join("Cargo.toml");
    let declared = std::fs::read_to_string(&helper_manifest).unwrap();
    std::fs::write(&helper_manifest, declared.replace("0.1.0", "0.2.0")).unwrap();

    // Out-of-band: an operator building the workspace directly instead of
    // going through rwv.
    let status = std::process::Command::new("cargo")
        .arg("fetch")
        .current_dir(&ws)
        .status()
        .unwrap();
    assert!(status.success(), "cargo fetch should resolve the bump");

    let (_, report) = rwv(&["doctor"], &ws);
    assert!(
        report.contains("generated file has drift"),
        "the reactive axis is what catches it once materialize is bypassed:\n{report}"
    );
}

/// The single-producer assumption `generation_inputs`'s docstring rests on:
/// `activate_hook` runs `cargo fetch`, then immediately reads the bytes it
/// produced and calls `generation_inputs` to attest them, with nothing
/// between the two steps that re-reads what `cargo` actually saw. A second
/// producer writing to a tracked input in that window stamps a digest of
/// ITS edit against bytes `cargo` resolved from a different, earlier one.
///
/// Reconstructed rather than raced: landing inside a live process's window
/// between one subprocess call and the next few lines of Rust is not a
/// fixture any test here can hit deterministically without instrumenting the
/// shipped binary. What follows runs `activate_hook`'s own two halves
/// directly — the same `cargo fetch` subprocess call the bypass-route
/// fixture above already drives, then the same
/// `generation_inputs`/`stamp_owned_generation` pair `weave_with_a_producer`
/// already drives — with a second edit inserted between them. Every step is
/// the production call `activate_hook` makes; only the interleaving is
/// authored, standing in for what a second producer's timing would put in
/// that window for real.
#[test]
fn a_second_producer_between_regenerate_and_stamp_defeats_the_join_point() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let tmp = common::tempdir().unwrap();
    let (ws, outside) = weave_with_a_path_dep(tmp.path());
    let project_dir = ws.join("projects/app");
    let helper_manifest = outside.join("Cargo.toml");

    let (ok, first) = rwv(&["materialize"], &ws);
    assert!(ok, "{first}");
    assert!(
        advisories(&ws).is_empty(),
        "precondition: the generation is current"
    );

    // Producer A's regenerate half: the same bare `cargo fetch` the
    // bypass-route fixture above runs, resolving against 0.2.0.
    let declared = std::fs::read_to_string(&helper_manifest).unwrap();
    std::fs::write(&helper_manifest, declared.replace("0.1.0", "0.2.0")).unwrap();
    let status = std::process::Command::new("cargo")
        .arg("fetch")
        .current_dir(&ws)
        .status()
        .unwrap();
    assert!(status.success(), "cargo fetch should resolve the bump");
    let lock_after_regen = std::fs::read_to_string(project_dir.join("Cargo.lock")).unwrap();
    assert!(
        lock_after_regen.contains("version = \"0.2.0\""),
        "producer A's regenerate resolved against 0.2.0:\n{lock_after_regen}"
    );

    // A second producer's write, landing in the window `activate_hook`
    // leaves open between that regenerate and its own stamp.
    let after_regen = std::fs::read_to_string(&helper_manifest).unwrap();
    std::fs::write(&helper_manifest, after_regen.replace("0.2.0", "0.3.0")).unwrap();

    // Producer A's stamp half: exactly what `activate_hook` runs right after
    // the subprocess call returns — read the bytes cargo just produced, and
    // attest them against inputs read now.
    let lock_bytes = std::fs::read(project_dir.join("Cargo.lock")).unwrap();
    let project = repoweave::manifest::ProjectName::new("app").unwrap();
    repoweave::owned_state::stamp_owned_generation(
        &project_dir,
        "Cargo.lock",
        &lock_bytes,
        repoweave::owned_state::generation_inputs(&project_dir, &project, &ws),
    )
    .unwrap();

    // The ledger now attests 0.2.0-produced bytes against 0.3.0 inputs.
    // Doctor re-hashes current disk (0.3.0) against the recorded digest
    // (also 0.3.0, since the stamp above just read it) and finds nothing
    // moved — silent, even though the accepted bytes were never regenerated
    // from 0.3.0 at all.
    assert!(
        advisories(&ws).is_empty(),
        "MEASURED: the second producer's edit is absorbed into the stamp \
         with no signal, reopening the pre-qiza shape for any producer that \
         races the sanctioned one instead of merely preceding it: {:?}",
        advisories(&ws)
    );

    // Control: the silence above is a false negative, not a coincidence — a
    // real regenerate against the inputs the ledger just vouched for
    // produces DIFFERENT bytes from the ones it attested.
    let (ok, materialized) = rwv(&["materialize"], &ws);
    assert!(ok, "{materialized}");
    let lock_final = std::fs::read_to_string(project_dir.join("Cargo.lock")).unwrap();
    assert!(
        lock_final.contains("version = \"0.3.0\""),
        "the attested generation was already stale against 0.3.0 when \
         doctor called it current:\n{lock_final}"
    );
    assert_ne!(
        lock_after_regen, lock_final,
        "control: the attested bytes and a real regenerate against the same \
         claimed inputs differ, which is what makes the earlier silence \
         wrong rather than merely permissive"
    );
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

/// A weave whose owned member's `build.rs` reads a source file via
/// `include!` that sits outside the member's own directory and is not
/// itself a member: no git repo tracks it and it is not registry-shaped, so
/// nothing in a weave scans it — the same non-member shape
/// `weave_with_a_path_dep` builds for a manifest, but for a source file a
/// build script reads instead.
fn weave_with_a_build_script_producer(root: &Path) -> (PathBuf, PathBuf) {
    let ws = root.join("ws");
    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let shared = ws.join("github/acme/shared");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(
        shared.join("version.rs"),
        "pub const BUILD_INPUT: &str = \"v1\";\n",
    )
    .unwrap();

    let member = ws.join("github/acme/lib");
    std::fs::create_dir_all(member.join("src")).unwrap();
    std::fs::write(
        member.join("Cargo.toml"),
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\nbuild = \"build.rs\"\n",
    )
    .unwrap();
    std::fs::write(member.join("src/lib.rs"), "").unwrap();
    std::fs::write(
        member.join("build.rs"),
        "include!(\"../shared/version.rs\");\nfn main() { let _ = BUILD_INPUT; }\n",
    )
    .unwrap();
    git_init_with_commit(&member);

    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/lib\"]\ntype = \"git\"\nurl = \"https://github.com/acme/lib.git\"\nversion = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();
    std::fs::write(project_dir.join("rwv.lock"), EMPTY_LOCK).unwrap();
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

    (ws, shared.join("version.rs"))
}

/// MEASURED: a member's `build.rs` can `include!` a source file that sits
/// outside the member entirely, with no git repo and no manifest anywhere to
/// pin it — the qiza mechanism's shape, one input class over. Unlike a
/// `path =` dependency target, `cargo generate-lockfile` / `cargo fetch`
/// never execute a build script during resolution: nothing about the lock
/// depends on what a build script reads, so there is no channel back into
/// `Cargo.lock`'s bytes for this edit to travel through, silently or
/// otherwise. Driven through the shipped binary on the sanctioned route.
#[test]
fn a_build_script_include_target_cannot_move_cargo_lock() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let tmp = common::tempdir().unwrap();
    let (ws, shared_file) = weave_with_a_build_script_producer(tmp.path());
    let project_dir = ws.join("projects/app");

    let (ok, first) = rwv(&["materialize"], &ws);
    assert!(
        ok,
        "first materialize should resolve the workspace for real:\n{first}"
    );
    let lock_before = std::fs::read(project_dir.join("Cargo.lock")).unwrap();
    assert!(
        advisories(&ws).is_empty(),
        "precondition: the generation is current"
    );

    let declared = std::fs::read_to_string(&shared_file).unwrap();
    std::fs::write(&shared_file, declared.replace("v1", "v2")).unwrap();

    assert!(
        advisories(&ws).is_empty(),
        "the include target is not a recorded input, so editing it leaves \
         the generation current on this axis: {:?}",
        advisories(&ws)
    );

    let (ok, materialized) = rwv(&["materialize"], &ws);
    assert!(ok, "{materialized}");
    let lock_after = std::fs::read(project_dir.join("Cargo.lock")).unwrap();
    assert_eq!(
        lock_before, lock_after,
        "the edit cannot reach Cargo.lock at all: cargo's resolve step does \
         not execute build.rs, so there is nothing here for the inputs map \
         to fail to track"
    );

    // Reachability control: an edit that DOES move the resolve, on the same
    // fixture, through the same two-materialize shape, must still move
    // `Cargo.lock` — otherwise "unchanged" above would be indistinguishable
    // from a fixture where `materialize` is not really re-resolving at all.
    let member_manifest = ws.join("github/acme/lib/Cargo.toml");
    let member_declared = std::fs::read_to_string(&member_manifest).unwrap();
    std::fs::write(&member_manifest, member_declared.replace("0.1.0", "0.2.0")).unwrap();
    let (ok, third) = rwv(&["materialize"], &ws);
    assert!(ok, "{third}");
    let lock_third = std::fs::read(project_dir.join("Cargo.lock")).unwrap();
    assert_ne!(
        lock_after, lock_third,
        "control: a change materialize actually resolves against must move \
         Cargo.lock, or the two assertions above are not measuring anything"
    );
}

/// A member's own `Cargo.toml` is not a recorded input, so editing one in place
/// under a still lock moves nothing on this axis. The boundary is deliberate,
/// and what makes it safe is that no other axis is silent in the states it
/// leaves.
///
/// Measured, walking a member manifest edit from the edit to a clean tree:
/// uncommitted, doctor reports `working-tree-drift` against the member;
/// committed, the member's HEAD leaves the lock behind and doctor reports
/// `stale-lock`; the `rwv lock` that clears that moves a recorded input, and
/// the staleness axis fires with `rwv materialize`. Every route from the edit
/// to a quiet report passes through a moved lock, which is the join point the
/// inputs map is drawn around.
///
/// The two halves are one test on one fixture on purpose. "No advisory after a
/// member edit" is equally true of a staleness check that reports nothing at
/// all, and the drift assertion is what keeps the silence from becoming a hole
/// if the axis that covers it goes away.
#[test]
fn a_member_manifest_is_not_a_recorded_input() {
    let tmp = common::tempdir().unwrap();
    let ws = weave_with_a_producer(tmp.path());
    let project_dir = ws.join("projects/app");
    let member = "github/acme/lib";
    let member_manifest = ws.join(member).join("Cargo.toml");

    let ledger: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project_dir.join(LEDGER)).unwrap())
            .expect("the ledger is JSON");
    assert_eq!(
        ledger["Cargo.lock"]["inputs"]
            .as_object()
            .expect("an attested entry carries an inputs map")
            .keys()
            .collect::<Vec<_>>(),
        vec!["projects/app/rwv.lock", "projects/app/rwv.toml"],
        "the map is the weave's own record of membership and revisions, and a \
         member path appearing here is the decision this pins: {ledger:#}"
    );
    assert!(
        advisories(&ws).is_empty(),
        "precondition: the generation is current before the edit"
    );
    let quiet = violation_kinds_for(&ws, member);
    assert!(
        !quiet.contains(&"working-tree-drift".to_owned()),
        "precondition: the member is clean before the edit; got {quiet:?}"
    );

    let declared = std::fs::read_to_string(&member_manifest).unwrap();
    std::fs::write(&member_manifest, declared.replace("0.1.0", "0.2.0")).unwrap();

    assert!(
        advisories(&ws).is_empty(),
        "a member manifest is not an input, so editing one leaves the \
         generation current on this axis: {:?}",
        advisories(&ws)
    );
    let moved = violation_kinds_for(&ws, member);
    assert!(
        moved.contains(&"working-tree-drift".to_owned()),
        "and the report is not silent about the member — this is the axis that \
         covers the state the staleness map declines to see; got {moved:?}"
    );

    std::fs::write(project_dir.join("rwv.lock"), MOVED_LOCK).unwrap();
    let found = advisories(&ws);
    assert_eq!(
        found.len(),
        1,
        "the lock is an input and it moved: {found:?}"
    );
    assert_eq!(found[0]["kind"], "derived_state_stale");
    assert_eq!(
        found[0]["inputs"],
        serde_json::json!(["projects/app/rwv.lock"]),
        "and the member manifest is not among what moved"
    );
}
