//! An `[integrations.<name>]` table's `enabled` takes `true` or `false`, and a
//! value of any other type fails the manifest load rather than reading as
//! absent.
//!
//! The regression pinned here: `enabled = "false"` on `cargo-workspace` was
//! read as no `enabled` key at all, so the integration the operator had
//! switched off kept running, with no finding and no refusal.
//!
//! Driven through `rwv doctor --json`, the route the manifest reference names
//! for a manifest that does not parse. Both halves are the pin: the kind is
//! what a consumer branches on, and every malformed manifest produces it, so
//! the message is what shows the refusal is about this key. The control is the
//! same manifest with a boolean, which must load.

use std::path::{Path, PathBuf};

mod common;

fn weave_with_integration_table(root: &Path, table: &str) -> PathBuf {
    let ws = root.join("ws");
    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!("[repositories]\n\n[integrations.cargo-workspace]\n{table}\n"),
    )
    .unwrap();
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();
    ws
}

fn doctor_violations(ws: &Path) -> Vec<serde_json::Value> {
    let output = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json must parse ({e}):\n{stdout}"));
    report["violations"]
        .as_array()
        .expect("report carries violations")
        .clone()
}

#[test]
fn an_enabled_that_is_not_true_or_false_fails_the_load_naming_the_key() {
    let tmp = common::tempdir().unwrap();
    let ws = weave_with_integration_table(tmp.path(), "enabled = \"false\"");

    let unparseable: Vec<_> = doctor_violations(&ws)
        .into_iter()
        .filter(|v| v["kind"] == "unparseable-project")
        .collect();

    assert_eq!(
        unparseable.len(),
        1,
        "a manifest whose `enabled` is a string must not load: {unparseable:?}"
    );
    assert_eq!(unparseable[0]["project"], "app");
    let message = unparseable[0]["message"]
        .as_str()
        .expect("the violation carries a message");
    assert!(
        message.contains("`enabled` in `[integrations.cargo-workspace]`")
            && message.contains("expected `true` or `false`"),
        "the message is the operator's route to the key and what it takes, got: {message}"
    );
}

#[test]
fn an_enabled_that_is_a_boolean_loads() {
    for value in ["true", "false"] {
        let tmp = common::tempdir().unwrap();
        let ws = weave_with_integration_table(tmp.path(), &format!("enabled = {value}"));

        let kinds: Vec<_> = doctor_violations(&ws)
            .into_iter()
            .map(|v| v["kind"].as_str().unwrap_or_default().to_string())
            .collect();

        assert!(
            !kinds.iter().any(|k| k == "unparseable-project"),
            "`enabled = {value}` must load, got findings: {kinds:?}"
        );
    }
}
