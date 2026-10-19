//! `rwv status --json` hands each repo's `absolute_path` to whatever consumes
//! it — `rwv each`, a script — in the wire spelling, byte for byte, on every
//! platform. The structural checks in `wire_path_spelling_test.rs` see the
//! mint at its call sites; this one sees the bytes a consumer receives, and is
//! the only such observation on Windows, where the two spellings differ.
//!
//! Status builds a record on three separate arms — a present checkout, an
//! absent one, and a present one whose locked revision is not in its object
//! store — and each mints `absolute_path` on its own, so each is observed.

use serde_json::Value;

mod common;

fn commit_one(repo: &std::path::Path) -> String {
    std::fs::create_dir_all(repo).unwrap();
    common::git_in(repo, &["init", "--initial-branch=main"]);
    std::fs::write(repo.join("README"), "init").unwrap();
    common::git_in(repo, &["add", "."]);
    common::git_in(repo, &["commit", "-m", "initial"]);
    common::git_in(repo, &["rev-parse", "HEAD"])
        .trim()
        .to_owned()
}

#[test]
fn status_json_absolute_path_arrives_in_the_wire_spelling_on_every_arm() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");

    let present = ws.join("github/org/present");
    let present_sha = commit_one(&present);
    let unreachable = ws.join("github/org/unreachable");
    commit_one(&unreachable);
    let missing = ws.join("github/org/missing");

    let present_url = common::file_url(&present);
    let unreachable_url = common::file_url(&unreachable);
    let missing_url = common::file_url(tmp.path().join("nowhere"));

    let project_dir = ws.join("projects/alpha");
    std::fs::create_dir_all(&project_dir).unwrap();
    let manifest: String = [
        ("github/org/present", &present_url),
        ("github/org/unreachable", &unreachable_url),
        ("github/org/missing", &missing_url),
    ]
    .iter()
    .map(|(path, url)| {
        format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        )
    })
    .collect();
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    common::fixture_lock(
        &project_dir,
        &[
            ("github/org/present", &present_url, &present_sha),
            (
                "github/org/unreachable",
                &unreachable_url,
                "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            ),
        ],
    );
    std::fs::write(ws.join(".rwv-active"), "alpha\n").unwrap();

    let output = common::rwv()
        .args(["status", "--json"])
        .current_dir(&ws)
        .output()
        .expect("rwv status --json");
    assert!(
        output.status.success(),
        "status --json failed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    let repos = parsed["repos"].as_array().expect("repos array");

    let arms = [
        ("github/org/present", "ok", &present),
        ("github/org/missing", "missing", &missing),
        ("github/org/unreachable", "unreachable", &unreachable),
    ];
    assert_eq!(
        repos.len(),
        arms.len(),
        "one record per manifest repo: {parsed:#}"
    );
    for (path, relation, on_disk) in arms {
        let record = repos
            .iter()
            .find(|r| r["path"] == path)
            .unwrap_or_else(|| panic!("no record for {path}: {parsed:#}"));
        assert_eq!(record["relation"], relation, "{path} reached the wrong arm");
        let absolute_path = record["absolute_path"].as_str().expect("absolute_path");
        assert!(
            !absolute_path.contains('\\'),
            "{relation}: absolute_path must be forward-separated: {absolute_path:?}"
        );
        assert_eq!(
            absolute_path,
            repoweave::path_spelling::wire_path(on_disk),
            "{relation}: absolute_path must be the wire spelling verbatim"
        );
        assert!(std::path::Path::new(absolute_path).is_absolute());
    }
}
