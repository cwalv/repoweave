//! A repo's lock relation reaches an operator twice — as the bracketed word on
//! a `rwv status` row and as `relation` in `rwv status --json` — and a `jq`
//! filter is written from the first and run against the second. The two must
//! be one spelling, kebab-case like every other wire token.

use repoweave::status::LockRelation;
use serde_json::Value;
use std::path::Path;

mod common;

const DEAD_SHA: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

fn commit_file(repo: &Path, name: &str) -> String {
    std::fs::create_dir_all(repo).unwrap();
    if !repo.join(".git").exists() {
        common::git_in(repo, &["init", "--initial-branch=main"]);
    }
    std::fs::write(repo.join(name), name).unwrap();
    common::git_in(repo, &["add", "."]);
    common::git_in(repo, &["commit", "-m", name]);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

fn enum_tokens(schema: &Value, out: &mut Vec<String>) {
    match schema {
        Value::Object(map) => {
            for (key, value) in map {
                match (key.as_str(), value) {
                    ("enum", Value::Array(tokens)) => {
                        out.extend(tokens.iter().filter_map(Value::as_str).map(str::to_owned));
                    }
                    _ => enum_tokens(value, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| enum_tokens(item, out)),
        _ => {}
    }
}

#[test]
fn every_variant_displays_as_the_token_it_serializes_to() {
    let schema = serde_json::to_value(schemars::schema_for!(LockRelation)).unwrap();
    let mut tokens = Vec::new();
    enum_tokens(&schema, &mut tokens);
    assert!(
        tokens.len() > 1,
        "the schema lists the variants as enum members: {schema:#}"
    );

    for token in tokens {
        let relation: LockRelation = serde_json::from_value(Value::String(token.clone()))
            .unwrap_or_else(|e| panic!("`{token}` is in the schema but does not parse: {e}"));
        assert_eq!(
            relation.to_string(),
            token,
            "text and --json spell {relation:?} differently"
        );
    }
}

#[test]
fn status_text_and_json_spell_every_reachable_relation_alike() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");

    let ok = ws.join("github/org/ok");
    let ok_sha = commit_file(&ok, "one");

    let no_lock = ws.join("github/org/no-lock");
    commit_file(&no_lock, "one");

    let ahead = ws.join("github/org/ahead");
    let ahead_lock = commit_file(&ahead, "one");
    commit_file(&ahead, "two");

    let behind = ws.join("github/org/behind");
    commit_file(&behind, "one");
    let behind_lock = commit_file(&behind, "two");
    common::git_in(&behind, &["reset", "--hard", "HEAD~1"]);

    let diverged = ws.join("github/org/diverged");
    commit_file(&diverged, "one");
    common::git_in(&diverged, &["checkout", "-b", "other"]);
    let diverged_lock = commit_file(&diverged, "other");
    common::git_in(&diverged, &["checkout", "main"]);
    commit_file(&diverged, "main");

    let unreachable = ws.join("github/org/unreachable");
    commit_file(&unreachable, "one");

    let missing = ws.join("github/org/missing");

    let arms: [(&str, &Path, Option<&str>); 7] = [
        ("ok", &ok, Some(&ok_sha)),
        ("no-lock", &no_lock, None),
        ("ahead", &ahead, Some(&ahead_lock)),
        ("behind", &behind, Some(&behind_lock)),
        ("diverged", &diverged, Some(&diverged_lock)),
        ("unreachable", &unreachable, Some(DEAD_SHA)),
        ("missing", &missing, Some(DEAD_SHA)),
    ];

    let project_dir = ws.join("projects/alpha");
    std::fs::create_dir_all(&project_dir).unwrap();
    let mut manifest = String::new();
    let mut locked = Vec::new();
    for (token, repo, lock) in &arms {
        let path = format!("github/org/{token}");
        let url = common::file_url(repo);
        manifest.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
        if let Some(sha) = lock {
            locked.push((path, url, sha.to_string()));
        }
    }
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    let lock_rows: Vec<(&str, &str, &str)> = locked
        .iter()
        .map(|(path, url, sha)| (path.as_str(), url.as_str(), sha.as_str()))
        .collect();
    common::fixture_lock(&project_dir, &lock_rows);
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

    for (token, _, _) in arms {
        let path = format!("github/org/{token}");
        let row = text
            .lines()
            .find(|line| line.starts_with(&path))
            .unwrap_or_else(|| panic!("no row for {path}:\n{text}"));
        let bracketed = row
            .rsplit_once('[')
            .and_then(|(_, tail)| tail.strip_suffix(']'))
            .unwrap_or_else(|| panic!("row does not end in a bracketed relation: {row:?}"));
        let wire = repos
            .iter()
            .find(|r| r["path"] == path)
            .unwrap_or_else(|| panic!("no record for {path}:\n{json:#}"))["relation"]
            .as_str()
            .expect("relation is a string");

        assert_eq!(wire, token, "{path}: --json relation");
        assert_eq!(bracketed, token, "{path}: text relation");
    }
}
