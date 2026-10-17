//! An integration whose `[integrations.<name>]` block does not deserialize
//! cannot say what it generates or surfaces, and nothing may read that silence
//! as "it generates nothing".
//!
//! The regression pinned here: an unknown key inside a cargo `members` entry
//! made `rwv materialize` drop the `Cargo.lock` attestation ("[forgot] … rwv no
//! longer vouches for it") and only then refuse, and made `rwv doctor` report
//! the settings error twice and the integration's own weave-root links as
//! undeclared.

use std::path::{Path, PathBuf};

mod common;

const LOCK: &str = "version = 4\n";

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

/// A cargo weave authored and attested, then given an unknown key inside its
/// `members` entry.
fn weave_with_malformed_member_spec(root: &Path) -> PathBuf {
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

    let manifest = "[repositories.\"github/acme/lib\"]\n\
                    type = \"git\"\n\
                    url = \"https://github.com/acme/lib.git\"\n\
                    version = \"main\"\n\
                    role = \"owned\"\n";
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
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
    .expect("intent activation should author Cargo.toml");
    std::fs::write(project_dir.join("Cargo.lock"), LOCK).unwrap();
    repoweave::owned_state::stamp_owned_digest(&project_dir, "Cargo.lock", LOCK.as_bytes())
        .unwrap();

    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "{manifest}\n[integrations.cargo-workspace.members.\"github/acme/lib\"]\n\
             incldue = [\"core\"]\n"
        ),
    )
    .unwrap();
    for link in ["Cargo.toml", "Cargo.lock"] {
        assert!(
            ws.join(link).symlink_metadata().is_ok(),
            "fixture: the weave root must surface {link}"
        );
    }
    ws
}

fn digests(ws: &Path) -> Vec<u8> {
    std::fs::read(ws.join("projects/app/.rwv-owned-digests")).expect("the ledger must exist")
}

#[test]
fn materialize_refuses_over_a_malformed_member_spec_and_changes_nothing() {
    let tmp = common::tempdir().unwrap();
    let ws = weave_with_malformed_member_spec(tmp.path());
    let before = digests(&ws);
    assert!(
        String::from_utf8_lossy(&before).contains("Cargo.lock"),
        "fixture: Cargo.lock must be attested"
    );

    let (ok, out) = rwv(&["materialize"], &ws);
    assert!(!ok, "materialize must refuse:\n{out}");
    assert!(
        out.contains("nothing here was changed") && out.contains("incldue"),
        "the refusal names the field and that nothing moved:\n{out}"
    );
    assert!(
        !out.contains("[forgot]"),
        "nothing may be forgotten:\n{out}"
    );
    assert_eq!(
        digests(&ws),
        before,
        "a refusing materialize must leave the attestations byte-identical"
    );

    let (ok, out) = rwv(&["materialize", "--remove-undeclared-links"], &ws);
    assert!(!ok, "materialize must refuse:\n{out}");
    for link in ["Cargo.toml", "Cargo.lock"] {
        assert!(
            ws.join(link).symlink_metadata().is_ok(),
            "the refusal must not unlink {link}:\n{out}"
        );
    }
    assert_eq!(digests(&ws), before);
}

#[test]
fn doctor_reports_a_malformed_member_spec_once_and_no_undeclared_links() {
    let tmp = common::tempdir().unwrap();
    let ws = weave_with_malformed_member_spec(tmp.path());

    let output = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .output()
        .expect("rwv should run");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("`rwv doctor --json` must emit JSON: {e}"));
    let issues = report["issues"].as_array().expect("`issues` is an array");

    let malformed: Vec<_> = issues
        .iter()
        .filter(|i| i["kind"] == "malformed-settings")
        .collect();
    assert_eq!(
        malformed.len(),
        1,
        "one malformed block is one finding:\n{malformed:#?}"
    );
    assert_eq!(malformed[0]["integration"], "cargo-workspace");

    let undeclared: Vec<_> = issues
        .iter()
        .filter(|i| {
            i["kind"] == "surfacing"
                && i["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("no longer declares"))
        })
        .collect();
    assert!(
        undeclared.is_empty(),
        "the integration's own links are not undeclared while its settings are \
         unreadable:\n{undeclared:#?}"
    );
}
