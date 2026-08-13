//! Disablement, the third moment ownership is withdrawn.
//!
//! Deleting a workweave and switching projects both end an integration's claim
//! on a checkout. So does turning it off in `rwv.toml` — and that one left
//! everything it authored on disk with no verb reporting it, which is the
//! defect these pin.
//!
//! Two claims run through every test here, and they pull in opposite
//! directions. Content rwv authored must be reported and removable, or the
//! operator is left with orphans. Content rwv did **not** author must never be
//! named, because the remedy deletes and a finding that over-reports is a
//! finding that proposes to delete someone else's file. Both directions are
//! seeded, because a scan that reports everything passes the first half alone.
//!
//! Driven through the shipped binary: what is being pinned is which verb acts
//! and which only speaks.

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

/// The lock content stands in for a real `cargo generate-lockfile` run: these
/// tests are about who may remove the file, not about what resolved it, and a
/// real cargo run would make every one of them need cargo on PATH.
const LOCK: &str = "version = 4\n";

/// A primary weave with one Rust member, `gita` and `static-files` enabled, and
/// every integration's content authored and attested.
///
/// `notes.md` is the operator's own file, declared to `static-files` so it is
/// surfaced. That integration authors nothing — it points at files the operator
/// committed — so it is the seed for the over-reporting direction.
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

    std::fs::write(project_dir.join("notes.md"), "operator content\n").unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/lib\"]\n\
         type = \"git\"\n\
         url = \"https://github.com/acme/lib.git\"\n\
         version = \"main\"\n\
         role = \"owned\"\n\
         \n[integrations.gita]\n\
         enabled = true\n\
         \n[integrations.static-files]\n\
         enabled = true\n\
         files = [\"notes.md\"]\n",
    )
    .unwrap();
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
    .expect("intent activation should author the managed files");

    // The lock and its attestation, as the hook would have left them.
    std::fs::write(project_dir.join("Cargo.lock"), LOCK).unwrap();
    repoweave::integrations::merge::stamp_owned_digest(&project_dir, "Cargo.lock", LOCK.as_bytes())
        .unwrap();

    for file in ["Cargo.toml", "Cargo.lock", "gita/repos.csv", "notes.md"] {
        assert!(
            project_dir.join(file).is_file(),
            "fixture: {file} should have been authored"
        );
    }
    ws
}

/// Turn `integration` off in the project manifest.
fn disable(ws: &Path, integration: &str) {
    let manifest = ws.join("projects/app/rwv.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    let section = format!("[integrations.{integration}]\nenabled = true");
    let disabled = format!("[integrations.{integration}]\nenabled = false");
    let updated = if text.contains(&section) {
        text.replace(&section, &disabled)
    } else {
        format!("{text}\n{disabled}\n")
    };
    assert_ne!(updated, text, "fixture: {integration} was not disabled");
    std::fs::write(&manifest, updated).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Both artifact classes are reported, and the verb named is one that removes
/// them. Before this, all four artifacts survived and every verb was silent.
#[test]
fn disabling_an_integration_reports_both_artifact_classes() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    disable(&ws, "cargo-workspace");
    disable(&ws, "gita");

    let (_, report) = rwv(&["doctor"], &ws);
    assert!(
        report.contains("cargo-workspace is disabled") && report.contains("gita is disabled"),
        "every disabled integration holding content must be named:\n{report}"
    );
    assert!(
        report.contains("Cargo.toml (managed region)"),
        "a marked region inside a file the operator co-owns is one class:\n{report}"
    );
    assert!(
        report.contains("Cargo.lock (generated file)")
            && report.contains("repos.csv (generated file)"),
        "a file rwv wrote whole is the other:\n{report}"
    );
    assert!(
        report.contains("surfaced at the weave root as"),
        "the weave-root symlinks are part of what outlived the integration:\n{report}"
    );
    assert!(
        report.contains("rwv materialize"),
        "the finding must name the verb that clears it:\n{report}"
    );
}

/// The ratified prohibition, as a test.
///
/// Doctor reports this class and never acts on it, and there is no `--fix` arm
/// to reach. Disabling an integration is one character in `rwv.toml`, so a
/// `--fix` that deleted what it authored would put a typo one keystroke from
/// data loss. To make this fail, give the finding a repair arm in
/// `collect_doctor_issues` — nothing else in the tree can.
#[test]
fn doctor_fix_reports_the_finding_and_strips_nothing() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    let project_dir = ws.join("projects/app");
    disable(&ws, "cargo-workspace");
    disable(&ws, "gita");

    let before: Vec<(PathBuf, String)> = ["Cargo.toml", "Cargo.lock", "gita/repos.csv"]
        .iter()
        .map(|f| (project_dir.join(f), read(&project_dir.join(f))))
        .collect();

    let (_, report) = rwv(&["doctor", "--fix"], &ws);
    assert!(
        report.contains("cargo-workspace is disabled") && report.contains("gita is disabled"),
        "--fix must still report what it refuses to touch:\n{report}"
    );
    for (path, content) in &before {
        assert_eq!(
            &read(path),
            content,
            "`doctor --fix` must not touch {}",
            path.display()
        );
    }
    assert!(
        ws.join("Cargo.toml").exists() && ws.join("Cargo.lock").exists(),
        "`doctor --fix` must not unsurface either"
    );
}

/// The named remedy clears what named it, taking each class by its own cleanup
/// shape: a marked region is stripped out of a file the operator keeps, a file
/// rwv wrote whole is removed, and the attestation goes with it.
#[test]
fn materialize_strips_what_the_finding_named() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    let project_dir = ws.join("projects/app");
    disable(&ws, "cargo-workspace");
    disable(&ws, "gita");

    let (ok, report) = rwv(&["materialize"], &ws);
    assert!(ok, "materialize should succeed:\n{report}");
    assert!(
        report.contains("Cargo.lock") && report.contains("repos.csv"),
        "the operation must name what it removed, for an operator who never ran \
         doctor:\n{report}"
    );

    assert!(
        !project_dir.join("Cargo.lock").exists(),
        "a file rwv wrote whole is removed"
    );
    assert!(
        !project_dir.join("gita/repos.csv").exists()
            && !project_dir.join("gita/groups.csv").exists(),
        "gita's CSVs are removed"
    );
    assert!(
        !ws.join("Cargo.lock").exists() && !ws.join("Cargo.toml").exists(),
        "the weave-root surfacing goes with the content it surfaced"
    );

    let manifest = read(&project_dir.join("Cargo.toml"));
    assert!(
        !manifest.contains("managed by rwv") && !manifest.contains("members"),
        "rwv's region must be gone from the hybrid file:\n{manifest}"
    );
    assert!(
        !read(&project_dir.join(".rwv-owned-digests")).contains("Cargo.lock"),
        "an attestation of a file that no longer exists describes nothing"
    );

    let (_, after) = rwv(&["doctor"], &ws);
    assert!(
        !after.contains("is disabled"),
        "the remedy must clear the finding that named it:\n{after}"
    );
}

/// The over-reporting direction, and the reason the default answer is "I own
/// nothing here".
///
/// `static-files` declares the operator's own committed file so the weave root
/// surfaces it. Naming that file as an artifact of a disabled integration would
/// be a finding whose remedy deletes the operator's content.
#[test]
fn a_disabled_integration_that_authored_nothing_names_nothing() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    disable(&ws, "static-files");

    let (_, report) = rwv(&["doctor"], &ws);
    assert!(
        !report.contains("static-files is disabled"),
        "static-files points at operator content; it authors none:\n{report}"
    );

    let (ok, _) = rwv(&["materialize"], &ws);
    assert!(ok, "materialize should succeed");
    assert_eq!(
        read(&ws.join("projects/app/notes.md")),
        "operator content\n",
        "the operator's own file must survive their disabling the integration \
         that surfaced it"
    );
}

/// The pen test: with no marker, rwv did not write the region, so the file and
/// the lock beside it are not rwv's to name — the same rule `deactivate`
/// already applies before stripping.
#[test]
fn an_unmarked_file_is_not_attributed_to_the_integration() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    let project_dir = ws.join("projects/app");
    disable(&ws, "cargo-workspace");

    let held = "[workspace]\nmembers = [\"github/acme/lib\"]\nresolver = \"2\"\n";
    std::fs::write(project_dir.join("Cargo.toml"), held).unwrap();

    let (_, report) = rwv(&["doctor"], &ws);
    assert!(
        !report.contains("cargo-workspace is disabled"),
        "an unmarked workspace is the operator's; rwv must not offer to delete \
         it:\n{report}"
    );

    let (ok, _) = rwv(&["materialize"], &ws);
    assert!(ok, "materialize should succeed");
    assert_eq!(
        read(&project_dir.join("Cargo.toml")),
        held,
        "a hand-authored workspace must survive"
    );
    assert!(
        project_dir.join("Cargo.lock").exists(),
        "the lock of a workspace rwv did not author is not rwv's to remove"
    );
}

/// The control. Everything enabled is the state this scan must be silent in,
/// and materialize must not strip a thing — without this, a scan that reported
/// every artifact it found would pass every test above.
#[test]
fn nothing_is_reported_or_stripped_while_the_integrations_are_enabled() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    let project_dir = ws.join("projects/app");

    let (_, report) = rwv(&["doctor"], &ws);
    assert!(
        !report.contains("is disabled"),
        "an enabled integration's content is exactly where it belongs:\n{report}"
    );

    let (ok, materialized) = rwv(&["materialize"], &ws);
    assert!(ok, "materialize should succeed:\n{materialized}");
    assert!(
        !materialized.contains("[stripped]"),
        "materialize must strip nothing while every integration is enabled:\n{materialized}"
    );
    for file in ["Cargo.toml", "Cargo.lock", "gita/repos.csv", "notes.md"] {
        assert!(
            project_dir.join(file).is_file(),
            "{file} must survive a materialize with nothing disabled"
        );
    }
}
