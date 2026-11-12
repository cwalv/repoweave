//! `rwv doctor` reports a member's pin under the `pin` kind, one sub-kind per
//! state, and `rwv explain pin` serves the kind's one entry.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::common;

use common::git_in;

const REPO: &str = "github/acme/server";
const URL: &str = "https://github.com/acme/server.git";

struct Weave {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    project: PathBuf,
}

fn commit(repo: &Path, file: &str) -> String {
    std::fs::write(repo.join(file), file).unwrap();
    git_in(repo, &["add", file]);
    git_in(repo, &["commit", "-qm", file]);
    git_in(repo, &["rev-parse", "HEAD"])
}

fn weave() -> (Weave, String) {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().join("ws");
    let repo = root.join(REPO);
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    git_in(&repo, &["init", "-q", "-b", "main"]);
    let first = commit(&repo, "first");
    let project = root.join("projects/my-app");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        format!(
            "[repositories.\"{REPO}\"]\ntype = \"git\"\nurl = \"{URL}\"\nbranch = \"main\"\nrole = \"owned\"\n"
        ),
    )
    .unwrap();
    common::set_up_lock_driver(&project);
    (
        Weave {
            _tmp: tmp,
            root,
            repo,
            project,
        },
        first,
    )
}

fn pin_at(w: &Weave, commit: &str) {
    common::fixture_lock(&w.project, &[(REPO, URL, commit)]);
}

fn doctor(w: &Weave, args: &[&str]) -> std::process::Output {
    common::rwv()
        .arg("doctor")
        .args(args)
        .current_dir(&w.root)
        .output()
        .expect("rwv runs")
}

fn pin_finding(w: &Weave) -> Value {
    pin_finding_exiting(w, 1)
}

fn pin_finding_exiting(w: &Weave, status: i32) -> Value {
    let out = doctor(w, &["--json"]);
    assert_eq!(
        out.status.code(),
        Some(status),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).expect("doctor --json prints JSON");
    let pins: Vec<Value> = doc["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["kind"] == "pin")
        .cloned()
        .collect();
    let [pin] = &pins[..] else {
        panic!("exactly one pin finding expected, got {pins:?} in {doc}");
    };
    assert_eq!(pin["path"], REPO);
    assert_eq!(pin["project"], "my-app");
    pin.clone()
}

fn text_names(w: &Weave, sub_kind: &str) {
    text_names_as(w, "error", sub_kind);
}

fn text_names_as(w: &Weave, severity: &str, sub_kind: &str) {
    let out = doctor(w, &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!("[{severity}: pin/{sub_kind}]")),
        "doctor's text opens the line with the severity, kind and sub-kind:\n{text}"
    );
}

#[test]
fn a_lock_with_no_entry_for_a_member_is_pin_none() {
    let (w, _) = weave();
    common::fixture_lock(&w.project, &[]);
    let pin = pin_finding(&w);
    assert_eq!(pin["sub_kind"], "none");
    assert!(pin.get("locked").is_none() && pin.get("actual").is_none());
    text_names(&w, "none");
}

#[test]
fn a_pin_no_ref_here_reaches_is_pin_unreachable() {
    let (w, _) = weave();
    pin_at(&w, "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    let pin = pin_finding(&w);
    assert_eq!(pin["sub_kind"], "unreachable");
    text_names(&w, "unreachable");
}

#[test]
fn a_checkout_at_an_ancestor_of_its_pin_is_pin_behind() {
    let (w, first) = weave();
    let second = commit(&w.repo, "second");
    pin_at(&w, &second);
    git_in(&w.repo, &["reset", "-q", "--hard", &first]);
    let pin = pin_finding(&w);
    assert_eq!(pin["sub_kind"], "behind");
    assert_eq!(pin["locked"], second.as_str());
    assert_eq!(pin["actual"], first.as_str());
    text_names(&w, "behind");
}

fn ahead_of_its_pin_on_a_remote_branch() -> (Weave, String, String) {
    let (w, first) = weave();
    let bare = w.root.join("origin.git");
    git_in(
        &w.root,
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    git_in(
        &w.repo,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
    pin_at(&w, &first);
    let second = commit(&w.repo, "second");
    git_in(&w.repo, &["push", "-q", "origin", "main"]);
    (w, first, second)
}

#[test]
fn commits_beyond_the_pin_that_a_remote_branch_holds_are_pin_ahead() {
    let (w, first, second) = ahead_of_its_pin_on_a_remote_branch();
    let pin = pin_finding_exiting(&w, 0);
    assert_eq!(pin["sub_kind"], "ahead");
    assert_eq!(pin["locked"], first.as_str());
    assert_eq!(pin["actual"], second.as_str());
    text_names_as(&w, "warning", "ahead");
}

#[test]
fn a_workspace_whose_only_finding_is_pin_ahead_exits_as_it_does_for_warnings() {
    let (w, _, _) = ahead_of_its_pin_on_a_remote_branch();
    for args in [&[][..], &["--json"][..]] {
        let out = doctor(&w, args);
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(0), "{args:?}:\n{text}");
        assert!(!text.contains("[error:") && !text.contains("\"severity\": \"error\""));
    }
    let text = String::from_utf8_lossy(&doctor(&w, &[]).stdout).into_owned();
    assert!(
        text.contains("[warning: pin/ahead]"),
        "the finding is shown as a warning:\n{text}"
    );
}

#[test]
fn commits_beyond_the_pin_that_nothing_else_holds_are_pin_unpinned_and_listed() {
    let (w, first) = weave();
    pin_at(&w, &first);
    commit(&w.repo, "second");
    let pin = pin_finding(&w);
    assert_eq!(pin["sub_kind"], "unpinned");
    let listed = pin["listed"].as_array().unwrap();
    assert!(
        listed.len() == 1 && listed[0].as_str().unwrap().ends_with("second"),
        "{pin}"
    );
    text_names(&w, "unpinned");
}

#[test]
fn a_checkout_and_a_pin_that_each_hold_what_the_other_lacks_are_pin_diverged() {
    let (w, first) = weave();
    let second = commit(&w.repo, "second");
    pin_at(&w, &second);
    git_in(&w.repo, &["switch", "-q", "-c", "side", &first]);
    commit(&w.repo, "third");
    let pin = pin_finding(&w);
    assert_eq!(pin["sub_kind"], "diverged");
    text_names(&w, "diverged");
}

#[test]
fn a_checkout_at_its_pin_raises_no_pin_finding() {
    let (w, first) = weave();
    pin_at(&w, &first);
    let out = doctor(&w, &["--json"]);
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        doc["violations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["kind"] != "pin"),
        "{doc}"
    );
}

fn page(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs/reference")
            .join(name),
    )
    .unwrap()
}

fn explain(token: &str) -> String {
    let out = common::rwv()
        .args(["explain", token])
        .output()
        .expect("rwv runs");
    assert!(out.status.success(), "rwv explain {token} failed");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    common::strip_version_banner(&stdout)
        .trim_end_matches('\n')
        .to_owned()
}

#[test]
fn rwv_explain_pin_serves_the_one_entry_on_the_findings_page() {
    let findings = page("doctor-findings.md");
    let refusals = page("refusals.md");
    let heading = "### `pin`";
    assert_eq!(findings.lines().filter(|l| *l == heading).count(), 1);
    assert_eq!(refusals.lines().filter(|l| *l == heading).count(), 0);
    assert!(
        refusals.contains("(./doctor-findings.md#pin)"),
        "the refusals page links to the entry"
    );

    let entry = explain("pin");
    assert!(entry.starts_with(heading), "{entry}");
    assert!(findings.contains(&entry));
    for sub_kind in [
        "none",
        "unreachable",
        "behind",
        "ahead",
        "unpinned",
        "diverged",
    ] {
        let severity = if sub_kind == "ahead" {
            "Warning"
        } else {
            "Error"
        };
        assert!(
            entry.contains(&format!(
                "\n#### `{sub_kind}`\n\n**{severity}. Report-only.**"
            )),
            "the entry opens `{sub_kind}` with the register's marks:\n{entry}"
        );
        let own = explain(&format!("pin/{sub_kind}"));
        assert!(own.starts_with(&format!("#### `{sub_kind}`")), "{own}");
        assert!(entry.contains(&own));
    }
}

#[test]
fn the_retired_doctor_tokens_are_not_findings_and_name_no_entry_there() {
    let findings = page("doctor-findings.md");
    let (w, _) = weave();
    for token in ["stale-lock", "incomplete-lock", "unresolvable-lock-entry"] {
        let out = doctor(&w, &["--kind", token]);
        assert!(
            !out.status.success()
                && String::from_utf8_lossy(&out.stderr)
                    .contains("rwv explain unknown-finding-kind"),
            "`{token}` is not a finding kind: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !findings.contains(&format!("### `{token}`")),
            "the findings page holds no entry for `{token}`"
        );
    }
    assert!(!repoweave::check::KindFilter::valid_kinds().contains(&"stale-lock".to_owned()));
    assert!(repoweave::check::KindFilter::valid_kinds().contains(&"pin".to_owned()));
}

#[test]
fn the_landing_refusals_that_named_a_stale_lock_are_retired_for_pin_states() {
    let refusals = page("refusals.md");
    for token in [
        "stale-lock",
        "unresolvable-lock-entry",
        "target-lock-behind",
    ] {
        assert!(
            !refusals.contains(&format!("### `{token}`")),
            "the refusals page holds no entry for `{token}`"
        );
        let out = common::rwv()
            .args(["explain", token])
            .output()
            .expect("rwv runs");
        assert!(
            !out.status.success(),
            "`{token}` names no condition any more"
        );
    }
}
