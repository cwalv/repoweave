//! `dangling-reference` names the repair its state needs: `rwv fetch` when
//! nothing stands where the entry's URL places it, and a re-key when a clone
//! of that URL stands there — the checkout, moved to its placement ahead of
//! the re-key `placement-disagreement` asks for. Each is followed to a report
//! with nothing on it, beside the `rwv fetch` the second state steers away
//! from.

use std::path::{Path, PathBuf};

mod common;

/// A weave whose project `demo` records a clone of `origins/acme/new.git` at
/// `local/acme/old`, where that URL does not place it, locked and committed.
fn weave_with_misplaced_member(tmp: &tempfile::TempDir) -> (PathBuf, String) {
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    common::rwv()
        .args(["init", "demo"])
        .current_dir(&ws)
        .assert()
        .success();
    let origin = common::under(tmp.path(), "origins/acme/new.git");
    std::fs::create_dir_all(origin.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&origin);
    let url = common::file_url(&origin);
    common::git_in(&ws, &["clone", "--quiet", &url, "local/acme/old"]);
    common::rwv()
        .args(["add", "--adopt", "local/acme/old"])
        .current_dir(&ws)
        .assert()
        .success();
    common::rwv()
        .arg("lock")
        .current_dir(&ws)
        .assert()
        .success();
    (ws, url)
}

/// Every violation `rwv doctor --all --json` reports, as its JSON record.
fn violations(ws: &Path) -> Vec<serde_json::Value> {
    let out = common::rwv()
        .args(["doctor", "--all", "--json"])
        .current_dir(ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json must parse ({e}):\n{stdout}"));
    report["violations"].as_array().unwrap().clone()
}

fn kinds(ws: &Path) -> Vec<(String, String)> {
    violations(ws)
        .iter()
        .map(|v| {
            (
                v["kind"].as_str().unwrap_or_default().to_owned(),
                v["path"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn dangling_record(ws: &Path, path: &str) -> serde_json::Value {
    violations(ws)
        .into_iter()
        .find(|v| v["kind"] == "dangling-reference" && v["path"] == path)
        .unwrap_or_else(|| panic!("no dangling-reference for {path}: {:?}", kinds(ws)))
}

fn dangling_advice(ws: &Path, path: &str) -> String {
    let out = common::rwv()
        .args(["doctor", "--all"])
        .current_dir(ws)
        .output()
        .unwrap();
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    report
        .lines()
        .find(|l| l.contains(&format!("dangling reference in demo: {path} ")))
        .unwrap_or_else(|| panic!("no dangling-reference line for {path}:\n{report}"))
        .to_owned()
}

fn rekey(ws: &Path, from: &str, to: &str) {
    let manifest = ws.join("projects").join("demo").join("rwv.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    let quoted = format!("\"{from}\"");
    assert_eq!(text.matches(&quoted).count(), 1, "one key to re-key:\n{text}");
    std::fs::write(&manifest, text.replace(&quoted, &format!("\"{to}\""))).unwrap();
}

#[test]
fn a_checkout_gone_from_disk_is_fetched_back() {
    let tmp = common::tempdir().unwrap();
    let (ws, url) = weave_with_misplaced_member(&tmp);
    std::fs::rename(
        common::under(&ws, "local/acme/old"),
        common::under(&ws, "local/acme/new"),
    )
    .unwrap();
    rekey(&ws, "local/acme/old", "local/acme/new");
    common::rwv()
        .arg("lock")
        .current_dir(&ws)
        .assert()
        .success();
    assert_eq!(kinds(&ws), Vec::new());
    std::fs::remove_dir_all(common::under(&ws, "local/acme/new")).unwrap();
    assert_eq!(
        dangling_record(&ws, "local/acme/new")["clone_at_placement"],
        serde_json::Value::Null
    );
    let advice = dangling_advice(&ws, "local/acme/new");
    assert!(advice.contains("run `rwv fetch`"), "{advice}");
    assert!(!advice.contains("re-key"), "{advice}");

    common::rwv()
        .arg("fetch")
        .current_dir(&ws)
        .assert()
        .success();

    assert_eq!(kinds(&ws), Vec::new());
    assert_eq!(
        common::git_in(common::under(&ws, "local/acme/new"), &["remote", "get-url", "origin"])
            .trim(),
        url
    );
}

/// The checkout moved to where its URL places it, and the entry not yet
/// re-keyed: the clone there is the entry's, not an orphan, and the repair is
/// the re-key.
#[test]
fn a_checkout_moved_to_its_placement_is_re_keyed_and_locked() {
    let tmp = common::tempdir().unwrap();
    let (ws, _url) = weave_with_misplaced_member(&tmp);
    std::fs::rename(
        common::under(&ws, "local/acme/old"),
        common::under(&ws, "local/acme/new"),
    )
    .unwrap();

    let found = kinds(&ws);
    assert!(
        !found.iter().any(|(k, _)| k == "orphaned-clone"),
        "the moved checkout is its entry's, not an orphan: {found:?}"
    );
    assert_eq!(
        dangling_record(&ws, "local/acme/old")["clone_at_placement"],
        "local/acme/new"
    );
    let advice = dangling_advice(&ws, "local/acme/old");
    assert!(
        advice.contains(
            "re-key the entry in rwv.toml from `local/acme/old` to `local/acme/new` and run \
             `rwv lock`"
        ),
        "{advice}"
    );
    assert!(!advice.contains("run `rwv fetch`"), "{advice}");

    rekey(&ws, "local/acme/old", "local/acme/new");
    common::rwv()
        .arg("lock")
        .current_dir(&ws)
        .assert()
        .success();

    assert_eq!(kinds(&ws), Vec::new());
    assert!(!common::under(&ws, "local/acme/old").exists());
}

/// What the advice steers away from: `rwv fetch` clones the URL a second time
/// at the recorded path, and the moved checkout is then an orphan.
#[test]
fn fetching_while_the_checkout_sits_at_its_placement_makes_a_second_clone() {
    let tmp = common::tempdir().unwrap();
    let (ws, _url) = weave_with_misplaced_member(&tmp);
    std::fs::rename(
        common::under(&ws, "local/acme/old"),
        common::under(&ws, "local/acme/new"),
    )
    .unwrap();

    common::rwv()
        .arg("fetch")
        .current_dir(&ws)
        .assert()
        .success();

    assert!(common::under(&ws, "local/acme/old").join(".git").is_dir());
    let found = kinds(&ws);
    assert!(
        found.contains(&("orphaned-clone".to_owned(), "local/acme/new".to_owned())),
        "{found:?}"
    );
}

/// A clone of some other remote at the placement is not the entry's checkout,
/// so it does not change the repair.
#[test]
fn a_clone_of_another_remote_at_the_placement_leaves_fetch_the_repair() {
    let tmp = common::tempdir().unwrap();
    let (ws, _url) = weave_with_misplaced_member(&tmp);
    std::fs::remove_dir_all(common::under(&ws, "local/acme/old")).unwrap();
    let other = common::under(tmp.path(), "elsewhere/acme/new.git");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&other);
    common::git_in(
        &ws,
        &[
            "clone",
            "--quiet",
            &common::file_url(&other),
            "local/acme/new",
        ],
    );

    assert_eq!(
        dangling_record(&ws, "local/acme/old")["clone_at_placement"],
        serde_json::Value::Null
    );
    assert!(dangling_advice(&ws, "local/acme/old").contains("run `rwv fetch`"));
    assert!(kinds(&ws).contains(&("orphaned-clone".to_owned(), "local/acme/new".to_owned())));
}
