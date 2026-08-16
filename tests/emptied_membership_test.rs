//! The managed region of a hybrid ecosystem file is a function of the current
//! manifest, not of the membership history that produced it.
//!
//! When the last Rust or Go member leaves `rwv.toml`, the integration has
//! nothing to contribute — and the region it authored while it did must go with
//! it. Before these tests the integrations fell silent in that state instead,
//! and the departed member's path sat in `Cargo.toml` / `go.work` indefinitely.
//!
//! The verb split is deliberate and pinned here in both directions: the intent
//! verbs and `rwv doctor --fix` author, `rwv activate` reports and leaves the
//! file alone.

use std::path::Path;

mod common;

const CARGO_MEMBER: &str = "github/acme/rustlib";
const GO_MEMBER: &str = "github/acme/golib";

/// A weave with one Rust member, one Go member, and `demo` active.
fn setup_weave(root: &Path) {
    std::fs::create_dir_all(root.join(CARGO_MEMBER).join("src")).unwrap();
    std::fs::create_dir_all(root.join(GO_MEMBER)).unwrap();
    std::fs::create_dir_all(root.join("projects/demo")).unwrap();

    std::fs::write(
        root.join(CARGO_MEMBER).join("Cargo.toml"),
        "[package]\nname = \"rustlib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join(CARGO_MEMBER).join("src/lib.rs"),
        "pub fn f() {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join(GO_MEMBER).join("go.mod"),
        "module example.com/golib\n\ngo 1.20\n",
    )
    .unwrap();
    std::fs::write(root.join(GO_MEMBER).join("lib.go"), "package golib\n").unwrap();

    write_manifest(root, &[CARGO_MEMBER, GO_MEMBER]);
    std::fs::write(root.join(".rwv-active"), "demo\n").unwrap();

    for repo in [CARGO_MEMBER, GO_MEMBER, "projects/demo"] {
        let dir = root.join(repo);
        common::git_in(&dir, &["init", "-q", "-b", "main"]);
        common::git_in(&dir, &["add", "-A"]);
        common::git_in(
            &dir,
            &[
                "-c",
                "user.email=test@test.com",
                "-c",
                "user.name=Test",
                "commit",
                "-qm",
                "initial",
            ],
        );
    }
}

fn write_manifest(root: &Path, members: &[&str]) {
    let mut manifest_toml = String::from("[repositories]\n");
    for member in members {
        manifest_toml.push_str(&format!(
            "[repositories.\"{member}\"]\ntype = \"git\"\nurl = \"https://example.com/{member}.git\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    // vscode-workspace detects every repo rather than a manifest file, so it
    // reports its own drift over a zero-repo project and would supply a second
    // fixable finding. `doctor --fix` gates the repair on the finding set being
    // non-empty, not on which integration spoke, so leaving it enabled lets an
    // unrelated finding drive the repair these tests attribute to the strip.
    manifest_toml.push_str("\n[integrations.vscode-workspace]\nenabled = false\n");
    std::fs::write(root.join("projects/demo/rwv.toml"), manifest_toml).unwrap();
}

/// The repair-driving findings `rwv doctor` reports, as `integration/kind`
/// pairs, so a test can state which findings its `--fix` had available.
fn fixable_findings(root: &Path) -> Vec<String> {
    let out = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let doc: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("doctor --json must emit JSON");
    let mut found: Vec<String> = doc["issues"]
        .as_array()
        .expect("doctor --json must carry an issues array")
        .iter()
        .filter(|i| i["safe_to_fix"].as_bool().unwrap_or(false))
        .map(|i| {
            format!(
                "{}/{}",
                i["integration"].as_str().unwrap_or("?"),
                i["kind"].as_str().unwrap_or("?")
            )
        })
        .collect();
    found.sort();
    found
}

/// Author the managed regions the way an intent verb would, and assert the
/// fixture really produced the content the tests below check the removal of.
///
/// Without this the negative assertions would all pass over an empty weave.
fn author_and_assert_seeded(root: &Path) -> (String, String) {
    let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(root, None).unwrap();
    repoweave::activate::activate_intent("demo", &ctx).expect("authoring pass should succeed");

    let cargo_toml = read(root, "Cargo.toml");
    let go_work = read(root, "go.work");
    assert!(
        cargo_toml.contains(CARGO_MEMBER),
        "fixture must seed the Cargo member the test then removes, got:\n{cargo_toml}"
    );
    assert!(
        go_work.contains(GO_MEMBER),
        "fixture must seed the Go member the test then removes, got:\n{go_work}"
    );
    (cargo_toml, go_work)
}

fn read(root: &Path, file: &str) -> String {
    std::fs::read_to_string(root.join("projects/demo").join(file)).unwrap_or_default()
}

fn assert_region_gone(root: &Path, file: &str, member: &str, marker: &str) {
    let text = read(root, file);
    assert!(
        !text.contains(member),
        "{file} still names the removed member {member}:\n{text}"
    );
    assert!(
        !text.contains(marker),
        "{file} still claims an rwv-managed region with nothing to manage:\n{text}"
    );
}

#[test]
fn remove_of_the_last_member_strips_the_region_it_authored() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    setup_weave(root);
    author_and_assert_seeded(root);

    for member in [CARGO_MEMBER, GO_MEMBER] {
        common::rwv()
            .args(["remove", member])
            .current_dir(root)
            .assert()
            .success();
    }

    // Both surfaces are seeded and both must react: a single-surface check
    // would go green on a fix that only reached one integration.
    assert_region_gone(root, "Cargo.toml", CARGO_MEMBER, "managed by rwv");
    assert_region_gone(root, "go.work", GO_MEMBER, "managed by repoweave");
}

/// `doctor --fix` reaches the authoring path only because `verify()` reported
/// the orphaned region: the repair is gated on the fixable-finding set being
/// non-empty, not on which integration filled it. So the finding set is
/// asserted before `--fix` runs — otherwise any unrelated fixable finding would
/// drive the repair and this test would still pass with `verify()` silent,
/// crediting the strip for a repair something else triggered.
#[test]
fn doctor_fix_strips_a_region_no_membership_justifies() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    setup_weave(root);
    author_and_assert_seeded(root);

    // Empty the manifest without an intent verb, so the stale region is the
    // only thing that can drive the repair.
    write_manifest(root, &[]);

    assert_eq!(
        fixable_findings(root),
        vec![
            "cargo-workspace/managed-file-drift",
            "go-work/managed-file-drift"
        ],
        "the orphaned regions must be the ONLY findings available to drive \
         --fix; another integration's fixable finding would make this test \
         pass with verify() silent"
    );

    common::rwv()
        .args(["doctor", "--fix"])
        .current_dir(root)
        .assert()
        .success();

    assert_region_gone(root, "Cargo.toml", CARGO_MEMBER, "managed by rwv");
    assert_region_gone(root, "go.work", GO_MEMBER, "managed by repoweave");
}

/// `rwv activate` is a context verb: it surfaces and verifies, and never
/// authors a managed region. Deleting either half of this test is how the
/// asymmetry gets "tidied up" — the warning is what activate contributes, and
/// the untouched bytes are what it withholds.
#[test]
fn activate_reports_the_stale_region_without_authoring_it() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    setup_weave(root);
    let (cargo_before, go_before) = author_and_assert_seeded(root);

    write_manifest(root, &[]);

    let output = common::rwv()
        .args(["activate", "demo", "--no-materialize"])
        .current_dir(root)
        .output()
        .expect("rwv activate should run");
    let stderr = String::from_utf8_lossy(&output.stderr);

    for integration in ["cargo-workspace", "go-work"] {
        assert!(
            stderr.contains(integration) && stderr.contains("drift"),
            "activate must report {integration} drift over a region no \
             membership justifies, got:\n{stderr}"
        );
    }

    assert_eq!(
        read(root, "Cargo.toml"),
        cargo_before,
        "activate must not author Cargo.toml — reporting is its whole job here"
    );
    assert_eq!(
        read(root, "go.work"),
        go_before,
        "activate must not author go.work — reporting is its whole job here"
    );
}

/// The strip is marker-gated, and this is the data-loss case that gate exists
/// for: a hand-written `[workspace]` with no marker, over a manifest whose
/// membership just emptied.
///
/// `go.work` is along for the ride as the proof that the authoring pass really
/// ran. Without it an ungated strip would pass this test, because the run that
/// was supposed to destroy the file would never have started.
#[test]
fn a_hand_owned_region_survives_an_emptied_membership() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    setup_weave(root);
    author_and_assert_seeded(root);

    let hand_written = "[workspace]\nmembers = [\"github/acme/rustlib\"]\n\n\
                        [profile.release]\nlto = true\n";
    std::fs::write(root.join("projects/demo/Cargo.toml"), hand_written).unwrap();

    for member in [CARGO_MEMBER, GO_MEMBER] {
        common::rwv()
            .args(["remove", member])
            .current_dir(root)
            .assert()
            .success();
    }

    assert_region_gone(root, "go.work", GO_MEMBER, "managed by repoweave");
    assert_eq!(
        read(root, "Cargo.toml"),
        hand_written,
        "rwv must not strip a region it never marked"
    );
}
