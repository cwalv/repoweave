//! `rwv doctor --locked` and the pipeline's `stale-lock` violation both compare
//! a repo's tip against its `rwv.lock` entry, and neither reads the other. What
//! keeps them from drifting apart is measured here.
//!
//! Two properties, and the split between them is the point:
//!
//! - Where both surfaces enumerate the same lock entry, they must report the
//!   same two revisions in the same spelling. Existing coverage pins each
//!   surface against a hand-written expectation in a SHA-form fixture, where
//!   `ResolvedRevisionId`'s canonical and display forms coincide — so it cannot
//!   see a surface that starts rendering the other one. These use a tag-form
//!   lock, where the two forms differ.
//!
//! - `--locked` walks the raw lock, so a lock entry whose repo is absent from
//!   disk is one of its findings. The pipeline walks the resolved lock, which
//!   `LockFile::resolve_versions` builds by dropping exactly those entries, and
//!   `find_violations` is pure — it cannot re-read disk to recover them. The
//!   divergence is structural, not a spelling difference, and the second test
//!   pins the whole finding set on that fixture so either side changing shows up
//!   here.

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

const DRIFTED: &str = "github/acme/drifted";
const CLEAN: &str = "github/acme/clean";
const SERVER: &str = "github/acme/server";
const ABSENT: &str = "github/acme/absent-reference";
const TAG: &str = "v1.0.0";

fn rwv_cmd() -> Command {
    let mut cmd = common::rwv();
    cmd.current_dir(std::env::temp_dir());
    cmd
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git failed to start");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    git_in(path, &["init", "-b", "main"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git_in(path, &["add", "."]);
    git_in(path, &["commit", "-m", "initial"]);
    git_in(path, &["rev-parse", "HEAD"])
}

fn commit_more(path: &Path) -> String {
    std::fs::write(path.join("extra.txt"), "more\n").unwrap();
    git_in(path, &["add", "."]);
    git_in(path, &["commit", "-m", "second"]);
    git_in(path, &["rev-parse", "HEAD"])
}

/// A workspace whose project directory is the git repo `rwv doctor` expects, so
/// the only violations left are the lock ones under test.
fn make_workspace(parent: &Path) -> (PathBuf, PathBuf) {
    let root = parent.join("ws");
    std::fs::create_dir_all(root.join("github/acme")).unwrap();
    let project_dir = root.join("projects/my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    (root, project_dir)
}

fn seal_project_repo(project_dir: &Path) {
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    git_in(project_dir, &["init", "-b", "main"]);
    git_in(project_dir, &["add", "."]);
    git_in(project_dir, &["commit", "-m", "initial"]);
    git_in(project_dir, &["config", "merge.rwv-ours.driver", "true"]);
}

fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    let mut toml = String::from("[repositories]\n");
    for (repo, role) in repos {
        toml.push_str(&format!(
            "[repositories.{repo:?}]\ntype = \"git\"\nurl = \"https://example.invalid/{repo}.git\"\nversion = \"main\"\nrole = \"{role}\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), toml).unwrap();
}

fn write_lock(project_dir: &Path, repos: &[(&str, &str)]) {
    let entries: Vec<String> = repos
        .iter()
        .map(|(repo, version)| {
            format!(
                "{repo:?}: {{\"type\": \"git\", \"url\": \"https://example.invalid/{repo}.git\", \"version\": {version:?}}}"
            )
        })
        .collect();
    let raw = format!("{{\"repositories\": {{{}}}}}", entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
}

fn run_locked(cwd: &Path) -> (String, bool) {
    let out = rwv_cmd()
        .args(["doctor", "--locked"])
        .current_dir(cwd)
        .output()
        .expect("rwv failed to start");
    (
        String::from_utf8(out.stdout).expect("stdout not utf-8"),
        out.status.success(),
    )
}

fn run_doctor_json(cwd: &Path) -> Value {
    let out = rwv_cmd()
        .args(["doctor", "--json"])
        .current_dir(cwd)
        .output()
        .expect("rwv failed to start");
    let stdout = String::from_utf8(out.stdout).expect("stdout not utf-8");
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{stdout}"))
}

/// The `--locked` line for `repo`, with the leading `<repo>: ` stripped.
fn locked_line(stdout: &str, repo: &str) -> String {
    let prefix = format!("{repo}: ");
    stdout
        .lines()
        .find_map(|l| l.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("`--locked` printed no line for {repo}; got:\n{stdout}"))
        .to_owned()
}

/// The `(tip, lock)` pair out of a `tip <a> ≠ lock <b>` line. Panics rather than
/// returning an `Option`: a parse that quietly yields nothing is how a
/// comparison test passes while comparing nothing.
fn drift_pair(stdout: &str, repo: &str) -> (String, String) {
    let line = locked_line(stdout, repo);
    let rest = line
        .strip_prefix("tip ")
        .unwrap_or_else(|| panic!("`--locked` line for {repo} is not a drift line: {line:?}"));
    let (tip, lock) = rest
        .split_once(" ≠ lock ")
        .unwrap_or_else(|| panic!("`--locked` drift line for {repo} unparseable: {line:?}"));
    assert!(
        !tip.is_empty() && !lock.is_empty(),
        "`--locked` drift line for {repo} carried an empty revision: {line:?}"
    );
    (tip.to_owned(), lock.to_owned())
}

fn violations_for(doc: &Value, repo: &str) -> Vec<Value> {
    doc.get("violations")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("violations missing or not an array: {doc}"))
        .iter()
        .filter(|v| v.get("path").and_then(|p| p.as_str()) == Some(repo))
        .cloned()
        .collect()
}

fn kinds(violations: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = violations
        .iter()
        .map(|v| {
            v.get("kind")
                .and_then(|k| k.as_str())
                .unwrap_or_else(|| panic!("violation without a kind: {v}"))
                .to_owned()
        })
        .collect();
    out.sort();
    out
}

/// Both surfaces name the same two revisions, in the same spelling, for a lock
/// entry both of them enumerate — and neither reports the repo that matches.
///
/// The lock pins a tag, so the locked revision has a canonical form and a
/// distinct display form. A surface that switched to the other one would still
/// satisfy every assertion that checks one surface alone.
#[test]
fn locked_and_stale_lock_agree_on_the_revisions_they_report() {
    let tmp = common::tempdir().unwrap();
    let (root, project_dir) = make_workspace(tmp.path());

    let drifted = root.join(DRIFTED);
    init_repo(&drifted);
    git_in(&drifted, &["tag", TAG]);
    let drifted_tip = commit_more(&drifted);

    let clean_tip = init_repo(&root.join(CLEAN));

    write_manifest(&project_dir, &[(DRIFTED, "owned"), (CLEAN, "owned")]);
    write_lock(&project_dir, &[(DRIFTED, TAG), (CLEAN, &clean_tip)]);
    seal_project_repo(&project_dir);

    let (stdout, ok) = run_locked(&root);
    assert!(
        !ok,
        "`--locked` must exit non-zero on drift; got:\n{stdout}"
    );
    assert_eq!(
        locked_line(&stdout, CLEAN),
        "ok",
        "the matching repo must read `ok`; got:\n{stdout}"
    );
    let (locked_tip, locked_lock) = drift_pair(&stdout, DRIFTED);

    let doc = run_doctor_json(&root);
    assert!(
        violations_for(&doc, CLEAN).is_empty(),
        "the matching repo must raise no violation; got:\n{doc}"
    );
    let stale = violations_for(&doc, DRIFTED);
    assert_eq!(
        kinds(&stale),
        vec!["stale-lock".to_owned()],
        "the drifted repo's only violation must be `stale-lock`; got:\n{doc}"
    );
    let stale = &stale[0];
    let json_actual = stale.get("actual").and_then(|v| v.as_str()).unwrap();
    let json_locked = stale.get("locked").and_then(|v| v.as_str()).unwrap();

    assert_eq!(
        (locked_tip.as_str(), locked_lock.as_str()),
        (json_actual, json_locked),
        "`--locked` and `stale-lock` must name the same tip and the same lock \
         revision; `--locked` said tip={locked_tip} lock={locked_lock}, the \
         pipeline said actual={json_actual} locked={json_locked}"
    );
    assert_eq!(
        (json_actual, json_locked),
        (drifted_tip.as_str(), TAG),
        "both surfaces must report the tip SHA against the lock's tag spelling"
    );
}

/// A lock entry whose repo is absent from disk is `--locked`'s finding and the
/// pipeline's blind spot.
///
/// `resolve_versions` drops the entry before `find_violations` runs, so no
/// `stale-lock` is reachable for it. The role is `reference`, which is exempt
/// from `dangling-reference` because a reference clone is allowed to be absent —
/// that exemption is what leaves the pipeline with nothing correct to say.
///
/// What it says instead is `incomplete-lock`, whose remedy is to add a lock
/// entry that this fixture's `rwv.lock` already has. That is wrong, and the
/// assertion below pins it as measured rather than intended: giving the pipeline
/// a diagnosis for an absent-on-disk lock entry, or narrowing the coverage check
/// to the raw lock, reddens this test and the pin is what should change.
#[test]
fn locked_reports_an_absent_lock_entry_the_pipeline_cannot_see() {
    let tmp = common::tempdir().unwrap();
    let (root, project_dir) = make_workspace(tmp.path());

    let server_tip = init_repo(&root.join(SERVER));

    write_manifest(&project_dir, &[(SERVER, "owned"), (ABSENT, "reference")]);
    write_lock(
        &project_dir,
        &[(SERVER, &server_tip), (ABSENT, &server_tip)],
    );
    seal_project_repo(&project_dir);

    let lock_text = std::fs::read_to_string(project_dir.join("rwv.lock")).unwrap();
    assert!(
        lock_text.contains(ABSENT),
        "fixture is vacuous: the lock must carry an entry for the absent repo"
    );
    assert!(
        !root.join(ABSENT).exists(),
        "fixture is vacuous: the absent repo must not be on disk"
    );

    let (stdout, ok) = run_locked(&root);
    assert!(
        !ok,
        "`--locked` must exit non-zero for a lock entry with no repo on disk; got:\n{stdout}"
    );
    assert_eq!(
        locked_line(&stdout, SERVER),
        "ok",
        "the present repo must read `ok`; got:\n{stdout}"
    );
    let absent_line = locked_line(&stdout, ABSENT);
    assert!(
        absent_line.starts_with("missing on disk"),
        "`--locked` must name the condition for the absent repo; got: {absent_line:?}"
    );
    assert!(
        absent_line.contains("rwv sync"),
        "`--locked` must name the verb that materializes the repo; got: {absent_line:?}"
    );

    let doc = run_doctor_json(&root);
    assert!(
        violations_for(&doc, SERVER).is_empty(),
        "the present, matching repo must raise no violation; got:\n{doc}"
    );
    assert_eq!(
        kinds(&violations_for(&doc, ABSENT)),
        vec!["incomplete-lock".to_owned()],
        "the pipeline has one thing to say about an absent-on-disk lock entry \
         and it contradicts the lock file; got:\n{doc}"
    );
}
