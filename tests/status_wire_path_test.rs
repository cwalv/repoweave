//! `rwv status --json` hands each repo's `absolute_path` to whatever consumes
//! it — `rwv each`, a script — in the wire spelling, byte for byte, on every
//! platform. The structural checks in `wire_path_spelling_test.rs` see the
//! mint at its call sites; this one sees the bytes a consumer receives, and is
//! the only such observation on Windows, where the two spellings differ.

use serde_json::Value;

mod common;

#[test]
fn status_json_absolute_path_arrives_in_the_wire_spelling() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let repo = ws.join("github/org/app");
    std::fs::create_dir_all(&repo).unwrap();
    common::git_in(&repo, &["init", "--initial-branch=main"]);
    std::fs::write(repo.join("README"), "init").unwrap();
    common::git_in(&repo, &["add", "."]);
    common::git_in(&repo, &["commit", "-m", "initial"]);
    let project_dir = ws.join("projects/alpha");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories.\"github/org/app\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            common::file_url(&repo)
        ),
    )
    .unwrap();
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
    let absolute_path = parsed["repos"][0]["absolute_path"]
        .as_str()
        .expect("absolute_path");

    assert!(
        !absolute_path.contains('\\'),
        "absolute_path must be forward-separated: {absolute_path:?}"
    );
    assert_eq!(
        absolute_path,
        repoweave::path_spelling::wire_path(&repo),
        "absolute_path must be the wire spelling verbatim"
    );
    assert!(std::path::Path::new(absolute_path).is_absolute());
}
