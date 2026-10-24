//! `rwv doctor`'s text report names each finding by the token `rwv explain`
//! takes for it: `[<severity>: <token>]`, where the token is the finding's
//! `kind`, or `<kind>/<sub-kind>` for a finding that carries a sub-kind.
//!
//! Driven rather than matched: a token is only worth printing if `rwv explain`
//! serves its entry, so each one is read off the report and handed back.

use std::path::{Path, PathBuf};

mod common;

use common::doctor_corpus::{case_token, corpus, issue_corpus, issue_kind_token};

/// The token `[<severity>: <token>]` opens `line` with, if it opens with one.
fn printed_token(line: &str) -> Option<&str> {
    let bracket = line.strip_prefix('[')?.split(']').next()?;
    let (severity, token) = bracket.split_once(": ")?;
    ["warning", "error"].contains(&severity).then_some(token)
}

/// The token an entry's heading names.
fn heading_token(entry: &str) -> &str {
    entry
        .lines()
        .find(|line| line.starts_with('#'))
        .and_then(|line| line.trim_start_matches('#').trim().strip_prefix('`'))
        .and_then(|rest| rest.strip_suffix('`'))
        .unwrap_or_else(|| panic!("an entry opens with its token's heading:\n{entry}"))
}

/// Whether `entry` is the one `token` names: headed by its kind, or by its
/// sub-kind.
fn names(entry: &str, token: &str) -> bool {
    let heading = heading_token(entry);
    token.split('/').any(|part| part == heading)
}

fn git(dir: &Path, args: &[&str]) {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-b", "main"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "initial"]);
}

/// A weave holding a finding with no sub-kind (an orphaned clone), one with a
/// sub-kind (a project with no replay exclusion), and an integration issue (a
/// declared static file that is not there).
fn weave_with_findings(parent: &Path) -> PathBuf {
    let root = parent.join("ws");
    init_repo(&root.join("github/acme/server"));
    init_repo(&root.join("github/acme/stray-clone"));
    let project = root.join("projects/my-app");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        "[repositories.\"github/acme/server\"]\ntype = \"git\"\n\
         url = \"https://github.com/acme/server.git\"\nversion = \"main\"\nrole = \"owned\"\n\n\
         [integrations.static-files]\nenabled = true\nfiles = [\"turbo.json\"]\n",
    )
    .unwrap();
    root
}

#[test]
fn every_token_the_text_report_prints_is_one_explain_serves() {
    let tmp = common::tempdir().unwrap();
    let root = weave_with_findings(tmp.path());

    let out = common::rwv()
        .arg("doctor")
        .current_dir(&root)
        .output()
        .expect("rwv runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let tokens: Vec<&str> = stdout.lines().filter_map(printed_token).collect();
    for expected in [
        "orphaned-clone",
        "missing-replay-exclusion/absent",
        "managed-file-missing",
    ] {
        assert!(
            tokens.contains(&expected),
            "the report must name `{expected}`:\n{stdout}"
        );
    }
    let unnamed: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with('[') && printed_token(line).is_none())
        .collect();
    assert!(unnamed.is_empty(), "lines with no token: {unnamed:#?}");

    for token in tokens {
        let explained = common::rwv()
            .args(["explain", token])
            .output()
            .expect("rwv runs");
        let page = String::from_utf8_lossy(&explained.stdout).into_owned();
        assert!(
            explained.status.success(),
            "`rwv explain {token}`, printed by doctor, refused:\n{}",
            String::from_utf8_lossy(&explained.stderr)
        );
        let entry = common::strip_version_banner(&page);
        assert!(
            names(entry, token),
            "`rwv explain {token}` served another entry:\n{entry}"
        );
    }
}

/// Over one sample of every finding: the token the text report derives is the
/// wire's `kind` and `sub_kind`, and `rwv explain` serves an entry for it —
/// the sub-kind two kinds share included, which only its qualified spelling
/// reaches.
#[test]
fn every_finding_prints_a_token_explain_serves() {
    let mut unserved = Vec::new();
    let mut tokens = Vec::new();
    for violation in corpus() {
        let expected = case_token(&violation);
        let token = repoweave::check::ViolationOutput::from_violation(
            violation,
            Path::new("/ws"),
            &Default::default(),
        )
        .token();
        assert_eq!(token, expected, "the token is the wire's kind and sub_kind");
        tokens.push(token);
    }
    for issue in issue_corpus() {
        let token = issue.kind.tag();
        assert_eq!(token, issue_kind_token(&issue.kind));
        tokens.push(token);
    }
    assert!(
        tokens.len() > 100
            && tokens.iter().any(|t| t == "index-drift/safe-to-fix")
            && tokens.iter().any(|t| t == "working-tree-drift/safe-to-fix"),
        "the corpus walk read {} findings: it has stopped reading the corpus",
        tokens.len()
    );
    for token in &tokens {
        match repoweave::explain::entry_for_token(token) {
            Some(entry) if names(entry, token) => {}
            Some(entry) => unserved.push(format!("`{token}` serves `{}`", heading_token(entry))),
            None => unserved.push(format!("`{token}` serves nothing")),
        }
    }
    assert!(unserved.is_empty(), "{unserved:#?}");
}
