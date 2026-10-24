//! Each repair the `orphaned-clone` finding names, run to a report with
//! nothing on it, beside the two it steers away from.
//!
//! An orphan sits under a registry segment some manifest or built-in names,
//! so unlike `unknown-segment-clone` it can already sit where its URL places
//! it; that is the one spelling where adopting it in place is the whole
//! repair.

use std::path::{Path, PathBuf};

mod common;

fn bare_weave(tmp: &tempfile::TempDir) -> PathBuf {
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    common::rwv()
        .args(["init", "demo"])
        .current_dir(&ws)
        .assert()
        .success();
    ws
}

/// A bare remote at `origins/acme/x.git`, whose URL places a clone at
/// `local/acme/x`.
fn origin(tmp: &tempfile::TempDir) -> String {
    let origin = common::under(tmp.path(), "origins/acme/x.git");
    std::fs::create_dir_all(origin.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&origin);
    common::file_url(&origin)
}

/// `(kind, path)` for every violation `rwv doctor --all --json` reports.
fn doctor_findings(dir: &Path) -> Vec<(String, String)> {
    let out = common::rwv()
        .args(["doctor", "--all", "--json"])
        .current_dir(dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json must parse ({e}):\n{stdout}"));
    report["violations"]
        .as_array()
        .expect("the report carries a violations array")
        .iter()
        .map(|v| {
            (
                v["kind"].as_str().unwrap_or_default().to_owned(),
                v["path"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// The line `rwv doctor --all` prints for the orphan at `path`.
fn orphan_advice(dir: &Path, path: &str) -> String {
    let out = common::rwv()
        .args(["doctor", "--all"])
        .current_dir(dir)
        .output()
        .unwrap();
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    report
        .lines()
        .find(|l| l.contains(&format!("orphaned clone: {path} ")))
        .unwrap_or_else(|| panic!("no orphaned-clone line for {path}:\n{report}"))
        .to_owned()
}

fn assert_advises_adopt_at_the_placement(advice: &str) {
    assert!(
        advice.contains(
            "move it to the path its remote's URL places it at under the primary weave root"
        ) && advice.contains("run `rwv add --adopt` with that path"),
        "the advice must name the move to the clone's placement and `rwv add --adopt` there: \
         {advice}"
    );
    assert!(
        !advice.contains("rwv add <url>"),
        "`rwv add <url>` makes a second clone of an orphan off its placement: {advice}"
    );
}

fn adopt(dir: &Path, path: &str) {
    common::rwv()
        .args(["add", "--adopt", path])
        .current_dir(dir)
        .assert()
        .success();
}

#[test]
fn adopting_an_orphan_already_at_its_placement_clears_the_finding() {
    let tmp = common::tempdir().unwrap();
    let ws = bare_weave(&tmp);
    let url = origin(&tmp);
    common::git_in(&ws, &["clone", "--quiet", &url, "local/acme/x"]);
    assert_eq!(
        doctor_findings(&ws),
        vec![("orphaned-clone".to_owned(), "local/acme/x".to_owned())]
    );
    assert_advises_adopt_at_the_placement(&orphan_advice(&ws, "local/acme/x"));

    adopt(&ws, "local/acme/x");

    assert_eq!(doctor_findings(&ws), Vec::new());
}

#[test]
fn moving_an_orphan_to_its_placement_and_adopting_it_there_clears_the_finding() {
    let tmp = common::tempdir().unwrap();
    let ws = bare_weave(&tmp);
    let url = origin(&tmp);
    common::git_in(&ws, &["clone", "--quiet", &url, "local/acme/stray"]);
    assert_advises_adopt_at_the_placement(&orphan_advice(&ws, "local/acme/stray"));

    std::fs::rename(
        common::under(&ws, "local/acme/stray"),
        common::under(&ws, "local/acme/x"),
    )
    .unwrap();
    adopt(&ws, "local/acme/x");

    assert_eq!(doctor_findings(&ws), Vec::new());
}

/// Adopted where its URL does not place it, an `owned` orphan is registered
/// there and its entry then disagrees with its own URL.
#[test]
fn adopting_an_orphan_where_its_url_does_not_place_it_reports_placement_disagreement() {
    let tmp = common::tempdir().unwrap();
    let ws = bare_weave(&tmp);
    let url = origin(&tmp);
    common::git_in(&ws, &["clone", "--quiet", &url, "local/acme/stray"]);

    adopt(&ws, "local/acme/stray");

    assert_eq!(
        doctor_findings(&ws),
        vec![("provenance".to_owned(), "local/acme/stray".to_owned())]
    );
}

#[test]
fn adding_its_url_while_it_sits_off_its_placement_makes_a_second_clone() {
    let tmp = common::tempdir().unwrap();
    let ws = bare_weave(&tmp);
    let url = origin(&tmp);
    common::git_in(&ws, &["clone", "--quiet", &url, "local/acme/stray"]);

    common::rwv()
        .args(["add", &url])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(common::under(&ws, "local/acme/x").join(".git").is_dir());
    assert_eq!(
        doctor_findings(&ws),
        vec![("orphaned-clone".to_owned(), "local/acme/stray".to_owned())]
    );
}

#[test]
fn removing_the_orphan_clears_the_finding() {
    let tmp = common::tempdir().unwrap();
    let ws = bare_weave(&tmp);
    let url = origin(&tmp);
    common::git_in(&ws, &["clone", "--quiet", &url, "local/acme/stray"]);
    assert!(orphan_advice(&ws, "local/acme/stray").contains("or remove the directory manually"));

    std::fs::remove_dir_all(common::under(&ws, "local/acme/stray")).unwrap();

    assert_eq!(doctor_findings(&ws), Vec::new());
}

/// In a workweave the orphan is moved into the primary weave, where the
/// canonical clone of every member lives, and adopted from the workweave,
/// which then holds a worktree of it.
#[test]
fn a_workweave_orphan_moved_to_its_placement_in_the_primary_and_adopted_clears_the_finding() {
    let tmp = common::tempdir().unwrap();
    let ws = bare_weave(&tmp);
    common::git_in(ws.join("projects").join("demo"), &["add", "."]);
    common::git_in(
        ws.join("projects").join("demo"),
        &["commit", "--quiet", "-m", "init"],
    );
    common::rwv()
        .args(["workweave", "demo", "create", "feat"])
        .current_dir(&ws)
        .assert()
        .success();
    let ww = tmp.path().join(".workweaves").join("demo--feat");
    let url = origin(&tmp);
    common::git_in(&ww, &["clone", "--quiet", &url, "local/acme/stray"]);
    assert_advises_adopt_at_the_placement(&orphan_advice(&ww, "local/acme/stray"));

    std::fs::create_dir_all(common::under(&ws, "local/acme")).unwrap();
    std::fs::rename(
        common::under(&ww, "local/acme/stray"),
        common::under(&ws, "local/acme/x"),
    )
    .unwrap();
    adopt(&ww, "local/acme/x");

    assert_eq!(doctor_findings(&ww), Vec::new());
    assert!(
        common::under(&ww, "local/acme/x").join(".git").is_file(),
        "the workweave holds a worktree of the adopted clone"
    );
}
