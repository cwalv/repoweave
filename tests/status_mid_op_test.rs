//! A repo stopped in an operation reaches an operator twice — as the second
//! bracketed word on a `rwv status` row and as `mid_op` in `rwv status --json`
//! — and `docs/how-to/write-a-plugin.md` has a script branch on the second.
//! Both carry the same `mid-<operation>` token, and a repo in no operation has
//! a null `mid_op` and no second word.

use serde_json::Value;
use std::path::Path;

mod common;

fn git_expecting_a_stop(repo: &Path, args: &[&str]) {
    let out = common::git()
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git runs");
    assert!(
        !out.status.success(),
        "git {args:?} was meant to stop on a conflict:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

fn commit_line(repo: &Path, line: &str) {
    std::fs::write(repo.join("f"), line).unwrap();
    common::git_in(repo, &["add", "."]);
    common::git_in(repo, &["commit", "-m", line]);
}

fn repo_with_diverged_branches(repo: &Path) {
    std::fs::create_dir_all(repo).unwrap();
    common::git_in(repo, &["init", "--initial-branch=main"]);
    commit_line(repo, "base");
    common::git_in(repo, &["checkout", "-b", "other"]);
    commit_line(repo, "other");
    common::git_in(repo, &["checkout", "main"]);
    commit_line(repo, "main");
}

#[test]
fn status_text_and_json_spell_every_mid_operation_alike() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");

    let arms = ["rebase", "merge", "cherry-pick", "bisect", "clean"];
    let mut manifest = String::new();
    for arm in arms {
        let repo = ws.join("github/org").join(arm);
        repo_with_diverged_branches(&repo);
        match arm {
            "rebase" => {
                common::git_in(&repo, &["checkout", "other"]);
                git_expecting_a_stop(&repo, &["rebase", "main"]);
            }
            "merge" => git_expecting_a_stop(&repo, &["merge", "other"]),
            "cherry-pick" => git_expecting_a_stop(&repo, &["cherry-pick", "other"]),
            "bisect" => {
                common::git_in(&repo, &["bisect", "start"]);
            }
            _ => {}
        }
        manifest.push_str(&format!(
            "[repositories.\"github/org/{arm}\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            common::file_url(&repo)
        ));
    }
    let project_dir = ws.join("projects/alpha");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    std::fs::write(ws.join(".rwv-active"), "alpha\n").unwrap();

    let text = common::rwv()
        .args(["status"])
        .current_dir(&ws)
        .output()
        .expect("rwv status");
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    let text = String::from_utf8(text.stdout).unwrap();

    let json = common::rwv()
        .args(["status", "--json"])
        .current_dir(&ws)
        .output()
        .expect("rwv status --json");
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    let json: Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    let repos = json["repos"].as_array().expect("repos array");

    for arm in arms {
        let path = format!("github/org/{arm}");
        let row = text
            .lines()
            .find(|line| line.starts_with(&path))
            .unwrap_or_else(|| panic!("no row for {path}:\n{text}"));
        let record = repos
            .iter()
            .find(|r| r["path"] == path)
            .unwrap_or_else(|| panic!("no record for {path}:\n{json:#}"));

        if arm == "clean" {
            assert!(record["mid_op"].is_null(), "{path}: {record:#}");
            assert!(!row.contains("[mid-"), "{path}: {row:?}");
            continue;
        }
        let token = format!("mid-{arm}");
        assert_eq!(record["mid_op"], token.as_str(), "{path}: --json mid_op");
        assert!(
            row.ends_with(&format!("  [{token}]")),
            "{path}: text row {row:?}"
        );
    }
}
