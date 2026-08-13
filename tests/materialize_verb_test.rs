//! `rwv materialize` — activation's install half, with no claim on selection.
//!
//! The verb exists because activation conflates two operations that have
//! different scopes. Selection needs a primary and can only ever name one
//! project; materialization is meaningful wherever the project identity is
//! already fixed. These tests pin the seam: the verb runs where `rwv activate`
//! is refused, it leaves selection state alone in both checkout kinds, and it
//! refuses — naming the verb that would fix it — where there is no project to
//! materialize.
//!
//! Driven through the shipped binary: the whole claim is about which verb is
//! valid in which checkout, which is a property of dispatch and workspace
//! resolution rather than of any one function.

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

struct Fixture {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
    ww: PathBuf,
}

impl Fixture {
    fn rwv(&self, args: &[&str], cwd: &Path) -> (bool, String) {
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
}

/// Write a two-version directory source under `source_dir` and the
/// `.cargo/config.toml` at `weave_root` that replaces crates.io with it.
///
/// A directory source is the cheapest thing real cargo will resolve a semver
/// range against without a network, and two versions is what makes a resolve
/// something that can be observed to move: the newest matching one is what a
/// fresh resolve picks, so a lock holding the older one is a pin with somewhere
/// to go.
fn write_local_crate_source(source_dir: &Path, weave_root: &Path, versions: &[&str]) {
    for version in versions {
        let pkg = source_dir.join(format!("pinnable-{version}"));
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        std::fs::write(
            pkg.join("Cargo.toml"),
            format!(
                "[package]\nname = \"pinnable\"\nversion = \"{version}\"\nedition = \"2021\"\n"
            ),
        )
        .unwrap();
        std::fs::write(pkg.join("src/lib.rs"), "pub fn pinned() {}\n").unwrap();
        std::fs::write(pkg.join(".cargo-checksum.json"), r#"{"files":{}}"#).unwrap();
    }
    std::fs::create_dir_all(weave_root.join(".cargo")).unwrap();
    std::fs::write(
        weave_root.join(".cargo/config.toml"),
        format!(
            "[source.crates-io]\nreplace-with = \"local\"\n\n[source.local]\ndirectory = \"{}\"\n",
            source_dir.display()
        ),
    )
    .unwrap();
}

/// The `version = "x.y.z"` line of the `pinnable` package in a `Cargo.lock`.
fn locked_pinnable_version(lock_text: &str) -> Option<String> {
    let mut lines = lock_text.lines();
    while let Some(line) = lines.next() {
        if line.trim() == r#"name = "pinnable""# {
            return lines
                .next()
                .and_then(|v| v.trim().strip_prefix(r#"version = ""#).map(str::to_string))
                .and_then(|v| v.strip_suffix('"').map(str::to_string));
        }
    }
    None
}

/// A primary weave with one Rust member and a workweave forked off it.
///
/// The project repo gitignores the lock, so the workweave's worktree arrives
/// without one — which is what makes "the hook produced this" observable.
///
/// **Primary's active project is deliberately not the workweave's.** A fixture
/// where the two agree cannot tell "left the pointer alone" from "rewrote the
/// pointer with the value it already held", and the second is a selection this
/// verb is not allowed to make.
///
/// The member depends on a package the local source offers twice, so the lock
/// this fixture produces holds a resolve that a re-resolve would move. Content
/// that cannot move cannot tell apart the two exits out of drift.
fn fixture() -> Fixture {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let ws = root.join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    write_local_crate_source(&root.join("crate-source"), &ws, &["0.1.0", "0.1.1"]);

    let server = ws.join("github/acme/server");
    std::fs::create_dir_all(server.join("src")).unwrap();
    std::fs::write(
        server.join("Cargo.toml"),
        "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\npinnable = \"0.1\"\n",
    )
    .unwrap();
    std::fs::write(server.join("src/lib.rs"), "").unwrap();
    git_init_with_commit(&server);

    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/server\"]\ntype = \"git\"\nurl = \"https://github.com/acme/server.git\"\nversion = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();
    std::fs::write(project_dir.join(".gitignore"), "/Cargo.lock\n").unwrap();
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

    // Author the managed files without materializing: the lock the tests look
    // for cannot be left over from the fixture's own setup.
    let ctx = repoweave::workspace::WorkspaceContext::resolve(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        "app",
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("intent activation should succeed");
    assert!(
        !project_dir.join("Cargo.lock").exists(),
        "fixture: the setup must not leave a lock behind"
    );
    git(&["add", "-A"], &project_dir);
    git(&["commit", "-m", "activate"], &project_dir);

    // A second project, and it is the one primary selects: the pointer's value
    // is now something a stray selection would visibly change.
    let other_dir = ws.join("projects/other");
    std::fs::create_dir_all(&other_dir).unwrap();
    std::fs::write(other_dir.join("rwv.toml"), "[repositories]\n").unwrap();
    git_init_with_commit(&other_dir);
    std::fs::write(ws.join(".rwv-active"), "other\n").unwrap();

    let weaveroot = root.join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();
    let out = common::rwv()
        .args(["workweave", "app", "create", "agent-1"])
        .current_dir(&ws)
        .output()
        .expect("rwv should run");
    assert!(
        out.status.success(),
        "fixture: workweave create failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let ww = weaveroot.join("app--agent-1");
    write_local_crate_source(&root.join("crate-source"), &ww, &["0.1.0", "0.1.1"]);

    Fixture { _tmp: tmp, ws, ww }
}

/// The seam, stated as one test: the verb runs exactly where the verb it was
/// split out of is refused.
#[test]
fn materialize_runs_in_a_workweave_where_activate_is_refused() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let f = fixture();

    let (activate_ok, activate_report) = f.rwv(&["activate", "app"], &f.ww);
    assert!(
        !activate_ok,
        "precondition: `rwv activate` is refused in a workweave:\n{activate_report}"
    );

    let lock = f.ww.join("projects/app/Cargo.lock");
    assert!(
        !lock.exists(),
        "precondition: the workweave starts without a lock"
    );

    let (ok, report) = f.rwv(&["materialize"], &f.ww);
    assert!(
        ok,
        "`rwv materialize` should succeed in a workweave:\n{report}"
    );
    assert!(
        lock.is_file(),
        "`rwv materialize` should have run the hook that produces {}:\n{report}",
        lock.display()
    );
}

/// Selection is the operation this verb does not perform. A workweave root has
/// no `.rwv-active` at all and must not acquire one; the primary's must not
/// change while a workweave materializes.
#[test]
fn materialize_leaves_selection_state_untouched() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let f = fixture();
    let primary_pointer = f.ws.join(".rwv-active");
    let before = std::fs::read(&primary_pointer).unwrap();

    let (ok, report) = f.rwv(&["materialize"], &f.ww);
    assert!(ok, "materialize should succeed:\n{report}");
    assert!(
        !f.ww.join(".rwv-active").exists(),
        "a workweave root must not acquire a selection pointer"
    );
    assert_eq!(
        std::fs::read(&primary_pointer).unwrap(),
        before,
        "materializing a workweave must not touch primary's selection"
    );

    let (ok, report) = f.rwv(&["materialize"], &f.ws);
    assert!(ok, "materialize should succeed at primary:\n{report}");
    assert_eq!(
        std::fs::read(&primary_pointer).unwrap(),
        before,
        "materializing at primary must not rewrite the selection pointer"
    );
}

/// With no project presented there is nothing to materialize, and the refusal
/// names the verb that gives the checkout one.
#[test]
fn materialize_without_an_active_project_names_activate() {
    let f = fixture();
    std::fs::remove_file(f.ws.join(".rwv-active")).unwrap();

    let (ok, report) = f.rwv(&["materialize"], &f.ws);
    assert!(
        !ok,
        "materialize must refuse when no project is presented:\n{report}"
    );
    assert!(
        report.contains("rwv activate"),
        "the refusal must name the verb that selects a project:\n{report}"
    );
}

/// The verb takes no project name. Accepting one would make it a selection
/// verb wearing a materialize label — the exact conflation it was split out
/// of.
#[test]
fn materialize_takes_no_project_argument() {
    let f = fixture();
    let (ok, report) = f.rwv(&["materialize", "app"], &f.ws);
    assert!(!ok, "materialize must reject a project argument:\n{report}");
}

// ---------------------------------------------------------------------------
// Arriving at drift in an attested generated file
// ---------------------------------------------------------------------------
//
// rwv records a digest when it accepts a generation; a workweave is born
// holding its source's record verbatim, so it can arrive already holding
// content that record does not describe. These drive the shipped binary
// because the claim is about what an operator standing in a workweave can read
// and then run — a message that is merely correct in a unit test is still a
// dead end if the verb it names is refused where it prints.
//
// The discriminator is the resolve, not the bytes: `cargo fetch` re-serializes
// a lock it honours, so a probe that watches for a hand-written line reports
// destruction where the pin in fact survived.

/// Materialize once so the lock exists and is attested, then pin it back to the
/// older version the source offers and leave that unattested.
///
/// This is the shape the operator produces by running the ecosystem tool in the
/// seat: content rwv never accepted, holding a resolve that is *not* what a
/// re-resolve would pick. Returns the lock path.
fn materialized_then_pinned_back(f: &Fixture) -> PathBuf {
    let (ok, report) = f.rwv(&["materialize"], &f.ww);
    assert!(ok, "fixture: first materialize should succeed:\n{report}");

    let lock = f.ww.join("projects/app/Cargo.lock");
    let generated = std::fs::read_to_string(&lock).expect("fixture: the hook writes the lock");
    assert_eq!(
        locked_pinnable_version(&generated).as_deref(),
        Some("0.1.1"),
        "fixture: a first resolve should pick the newest matching version"
    );
    let pinned = generated.replace(r#"version = "0.1.1""#, r#"version = "0.1.0""#);
    assert_ne!(
        pinned, generated,
        "fixture: the downgrade must change the lock"
    );
    std::fs::write(&lock, &pinned).unwrap();
    lock
}

/// The defect this pins: the finding used to name `rwv activate`, which is
/// refused in a workweave, and `rwv doctor --fix`, which never touches a
/// finding it is not permitted to fix. Both halves are asserted through the
/// binary — that the named verb arrives, and that running it clears what named
/// it.
#[test]
fn doctor_in_a_workweave_names_a_remedy_that_runs_there() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let f = fixture();
    materialized_then_pinned_back(&f);

    let (_, doctor) = f.rwv(&["doctor"], &f.ww);
    assert!(
        doctor.contains("differs from the last rwv-accepted generation"),
        "doctor must report content rwv never accepted:\n{doctor}"
    );
    assert!(
        doctor.contains("rwv materialize --adopt-drifted")
            && doctor.contains("rwv materialize --regenerate-drifted"),
        "the finding must name both exits, spelled as they are invoked:\n{doctor}"
    );

    let (ok, report) = f.rwv(&["materialize", "--adopt-drifted"], &f.ww);
    assert!(
        ok,
        "the remedy the finding named must run in the checkout it printed in:\n{report}"
    );

    let (_, after) = f.rwv(&["doctor"], &f.ww);
    assert!(
        !after.contains("differs from the last rwv-accepted generation"),
        "running the named remedy must clear the finding that named it:\n{after}"
    );
}

/// Without a consent flag the verb stops rather than choosing. The hook it
/// would otherwise run re-stamps whatever it produced, so proceeding here is
/// adoption wearing no name.
#[test]
fn materialize_refuses_on_content_it_never_accepted() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let f = fixture();
    let lock = materialized_then_pinned_back(&f);

    let (ok, report) = f.rwv(&["materialize"], &f.ww);
    assert!(
        !ok,
        "materialize must refuse on arriving at drift:\n{report}"
    );
    assert!(
        report.contains("--adopt-drifted") && report.contains("--regenerate-drifted"),
        "the refusal must name both exits:\n{report}"
    );
    assert!(
        report.contains(&lock.display().to_string()),
        "the refusal must list what it would act on:\n{report}"
    );
    assert_eq!(
        locked_pinnable_version(&std::fs::read_to_string(&lock).unwrap()).as_deref(),
        Some("0.1.0"),
        "refusing must leave the content alone"
    );
}

/// The two consents are the two losses, and they are opposite: adopting keeps
/// the resolve and moves the record onto it, regenerating keeps the record's
/// authority and throws the resolve away.
///
/// Each arm is the other's control. A pin that survives proves nothing unless
/// the same fixture can be shown moving it, and the regenerate arm is that
/// demonstration.
#[test]
fn the_two_consents_move_opposite_things() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }

    let adopted = fixture();
    let lock = materialized_then_pinned_back(&adopted);
    let (ok, report) = adopted.rwv(&["materialize", "--adopt-drifted"], &adopted.ww);
    assert!(ok, "--adopt-drifted should succeed:\n{report}");
    assert_eq!(
        locked_pinnable_version(&std::fs::read_to_string(&lock).unwrap()).as_deref(),
        Some("0.1.0"),
        "adopting must attest the resolve that was there, not replace it"
    );

    let regenerated = fixture();
    let lock = materialized_then_pinned_back(&regenerated);
    let (ok, report) = regenerated.rwv(&["materialize", "--regenerate-drifted"], &regenerated.ww);
    assert!(ok, "--regenerate-drifted should succeed:\n{report}");
    assert_eq!(
        locked_pinnable_version(&std::fs::read_to_string(&lock).unwrap()).as_deref(),
        Some("0.1.1"),
        "regenerating must discard what it was consented to discard"
    );

    let (_, doctor) = regenerated.rwv(&["doctor"], &regenerated.ww);
    assert!(
        !doctor.contains("differs from the last rwv-accepted generation"),
        "regeneration must re-attest what it produced:\n{doctor}"
    );
}

/// Both flags at once is not a stricter request, it is two contradictory ones.
/// Refusing at the parse boundary is what keeps a precedence rule from being
/// invented downstream to break the tie.
#[test]
fn the_two_consents_cannot_be_given_together() {
    let f = fixture();
    let (ok, report) = f.rwv(
        &["materialize", "--adopt-drifted", "--regenerate-drifted"],
        &f.ww,
    );
    assert!(
        !ok,
        "contradictory consents must be refused, not ranked:\n{report}"
    );
}

/// Move the rwv-managed `members` list off what rwv derives, so `verify()` has
/// a finding `--fix` is allowed to repair sitting beside one it is not.
///
/// Returns the manifest path and the authored text, so a later assertion can
/// compare against what rwv produces rather than against a literal — which
/// member list is correct depends on the checkout, and hard-coding one turns a
/// fixture difference into a failure about the wrong thing.
fn move_the_managed_members_list(f: &Fixture) -> (PathBuf, String) {
    let manifest = f.ww.join("projects/app/Cargo.toml");
    let authored = std::fs::read_to_string(&manifest).expect("fixture: the project has a manifest");
    let members = authored
        .lines()
        .find(|l| l.trim_start().starts_with("members = "))
        .unwrap_or_else(|| panic!("fixture: no managed members list to move: {authored}"));
    let moved = if members.contains("github/acme/server") {
        "members = []"
    } else {
        r#"members = ["github/acme/server"]"#
    };
    let broken = authored.replace(members, moved);
    assert_ne!(
        broken, authored,
        "fixture: the edit must actually move the list: {authored}"
    );
    std::fs::write(&manifest, &broken).unwrap();
    (manifest, authored)
}

/// `--fix` regenerates a project's whole managed set as soon as ANY verify
/// finding is fixable, and regeneration is one of the two exits out of drift.
/// Entered for a different finding it is that exit taken with nobody's consent:
/// the operator's resolve is discarded and they never typed the flag that says
/// so.
///
/// A2's refusal does not reach here. It runs in activation's materialize mode,
/// and `--fix` re-enters through the intent mode, which reports arrived drift
/// and proceeds. The per-finding `safe_to_fix` is no defence either — it keeps
/// `--fix` off the drift finding and says nothing about a repair entered for a
/// neighbour.
///
/// The second half is the control. "The pin did not move" is equally true of a
/// doctor that regenerates nothing, so the same fixture is shown moving it once
/// the drift is settled and the same finding is re-broken.
#[test]
fn doctor_fix_withholds_regeneration_while_drift_is_unsettled() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let f = fixture();
    let lock = materialized_then_pinned_back(&f);
    let (manifest, authored) = move_the_managed_members_list(&f);

    let (_, report) = f.rwv(&["doctor", "--fix"], &f.ww);
    assert!(
        report.contains("withheld the regeneration"),
        "`--fix` must say it declined, not pass over it in silence:\n{report}"
    );
    assert!(
        report.contains("--adopt-drifted") && report.contains("--regenerate-drifted"),
        "and name the two exits, so the operator can take the one they mean:\n{report}"
    );
    assert_eq!(
        locked_pinnable_version(&std::fs::read_to_string(&lock).unwrap()).as_deref(),
        Some("0.1.0"),
        "fixture premise, not the pin: cargo honours a lock that satisfies its \
         constraints, so these bytes survive a regeneration and cannot tell the \
         two exits apart. The attestation is what moves — the next assertion is \
         the one under test"
    );
    let (_, still) = f.rwv(&["doctor"], &f.ww);
    assert!(
        still.contains("differs from the last rwv-accepted generation"),
        "and the drift must still be UNACCEPTED afterwards. Cargo honours a \
         lock that satisfies its constraints, so the bytes survive a \
         regeneration either way — what a re-entered activation moves is the \
         attestation, and moving it is `--adopt-drifted` with nobody's \
         consent:\n{still}"
    );
    assert_ne!(
        std::fs::read_to_string(&manifest).unwrap(),
        authored,
        "and the repair is withheld rather than half-applied"
    );

    let control = fixture();
    let (ok, materialized) = control.rwv(&["materialize"], &control.ww);
    assert!(
        ok,
        "control fixture: materialize should succeed:\n{materialized}"
    );
    let (manifest, authored) = move_the_managed_members_list(&control);

    let (_, after) = control.rwv(&["doctor", "--fix"], &control.ww);
    assert!(
        after.contains("regenerated integration content"),
        "the same finding with no drift beside it must be repaired:\n{after}"
    );
    assert_eq!(
        std::fs::read_to_string(&manifest).unwrap(),
        authored,
        "and the managed region must be back to what rwv derives:\n{after}"
    );
}
