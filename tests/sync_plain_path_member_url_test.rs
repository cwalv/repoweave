//! A member whose `url` in the source's `rwv.toml` is a plain path, pulled by
//! `rwv sync` into a weave without a checkout of it: the clone refuses, the
//! member's line and its `--json` record name `unresolvable-repo-source`, and
//! respelling the `url` as a `file://` URL, as the message says, lets the sync
//! complete.

use std::path::{Path, PathBuf};
use std::process::Output;

mod common;

const PROJECT: &str = "app";

fn run(dir: &Path, args: &[&str]) -> Output {
    common::rwv().args(args).current_dir(dir).output().unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn succeed(dir: &Path, args: &[&str]) -> Output {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "rwv {args:?} in {}: {}",
        dir.display(),
        text(&out)
    );
    out
}

fn bare(tmp: &Path, name: &str) -> PathBuf {
    let path = tmp.join("remotes").join(format!("{name}.git"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&path);
    path
}

fn commit_project(workspace: &Path, message: &str) {
    let project = workspace.join("projects").join(PROJECT);
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-qm", message]);
}

struct Weaves {
    _tmp: tempfile::TempDir,
    source: PathBuf,
    target: PathBuf,
    beta: PathBuf,
    member: String,
}

/// `source` holds `alpha` and then adds `beta`, whose `url` it rewrites as a
/// plain path; `target` was fetched from `source` before `beta` was added.
fn weaves() -> Weaves {
    let tmp = common::tempdir().unwrap();
    let alpha = bare(tmp.path(), "alpha");
    let beta = bare(tmp.path(), "beta");
    let source = tmp.path().join("source");
    std::fs::create_dir_all(source.join("projects")).unwrap();
    succeed(&source, &["init", PROJECT]);
    let project = source.join("projects").join(PROJECT);
    common::git_in(&project, &["config", "user.email", "test@test.com"]);
    common::git_in(&project, &["config", "user.name", "Test"]);
    succeed(&source, &["add", &common::file_url(&alpha)]);
    succeed(&source, &["lock"]);
    commit_project(&source, "alpha");

    let target = tmp.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    succeed(&target, &["fetch", &common::file_url(&project)]);

    succeed(&source, &["add", &common::file_url(&beta)]);
    succeed(&source, &["lock"]);
    commit_project(&source, "beta");
    let member = "local/remotes/beta".to_owned();
    assert!(
        common::under(&source, &member).is_dir(),
        "fixture places beta"
    );
    let manifest = project.join("rwv.toml");
    let body = std::fs::read_to_string(&manifest).unwrap();
    let file_url = common::file_url(&beta);
    assert!(body.contains(&file_url), "{body}");
    std::fs::write(
        &manifest,
        body.replace(
            &format!("\"{file_url}\""),
            &format!("\"{}\"", beta.to_str().unwrap().replace('\\', "\\\\")),
        ),
    )
    .unwrap();
    commit_project(&source, "beta by plain path");
    common::git_in(
        target.join("projects").join(PROJECT),
        &["fetch", "-q", "origin"],
    );

    Weaves {
        _tmp: tmp,
        source,
        target,
        beta,
        member,
    }
}

#[test]
fn sync_names_the_refusal_and_respelling_the_url_completes_it() {
    let w = weaves();
    let source = w.source.to_str().unwrap().to_owned();

    let out = run(&w.target, &["sync", &source]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let failed = stderr
        .lines()
        .position(|l| l.starts_with(&format!("  {}: materialize failed:", w.member)))
        .unwrap_or_else(|| panic!("{}", text(&out)));
    assert_eq!(
        stderr.lines().nth(failed + 1),
        Some("rwv explain unresolvable-repo-source"),
        "{}",
        text(&out)
    );
    assert!(stderr.contains("file://"), "{}", text(&out));
    assert!(!out.status.success());
    succeed(&w.target, &["abort"]);

    let out = run(&w.target, &["sync", &source, "--json"]);
    let envelope: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&out)));
    let members = envelope["members"].as_array().unwrap();
    assert_eq!(members.len(), 1, "{envelope:#}");
    assert_eq!(members[0]["kind"], "materialize-failed", "{envelope:#}");
    assert_eq!(members[0]["path"], w.member.as_str(), "{envelope:#}");
    assert_eq!(
        members[0]["refusal"], "unresolvable-repo-source",
        "{envelope:#}"
    );
    assert_eq!(envelope["complete"], false, "{envelope:#}");
    succeed(&w.target, &["abort"]);

    let manifest = w.source.join("projects").join(PROJECT).join("rwv.toml");
    let body = std::fs::read_to_string(&manifest).unwrap();
    let plain = format!("\"{}\"", w.beta.to_str().unwrap().replace('\\', "\\\\"));
    std::fs::write(
        &manifest,
        body.replace(&plain, &format!("\"{}\"", common::file_url(&w.beta))),
    )
    .unwrap();
    commit_project(&w.source, "beta by file url");
    common::git_in(
        w.target.join("projects").join(PROJECT),
        &["fetch", "-q", "origin"],
    );

    succeed(&w.target, &["sync", &source]);
    assert!(common::under(&w.target, &w.member).join(".git").exists());
}
