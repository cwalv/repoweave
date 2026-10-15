//! A lock entry that names a tag or branch instead of a commit id. Every
//! writer records the commit id; a verb that reads an existing entry pinned
//! by name resolves it, says so, and the next write records the commit.

use std::path::{Path, PathBuf};
use std::process::Output;

mod common;

const PROJECT: &str = "app";
const DEP: &str = "local/team/dep";
const EXTRA: &str = "local/team/extra";

fn run(dir: &Path, args: &[&str]) -> Output {
    common::rwv().args(args).current_dir(dir).output().unwrap()
}

fn succeed(dir: &Path, args: &[&str]) -> (String, String) {
    let out = run(dir, args);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "rwv {args:?} in {} failed:\nstdout:\n{stdout}\nstderr:\n{stderr}",
        dir.display()
    );
    (stdout, stderr)
}

/// A bare remote with one commit on `main`, tagged `tag`. Returns the commit.
fn tagged_remote(bare: &Path, tag: &str) -> String {
    let parent = bare.parent().unwrap();
    common::git_in(
        parent,
        &["init", "--bare", "-b", "main", &bare.to_string_lossy()],
    );
    let work = parent.join(format!(
        "{}-work",
        bare.file_stem().unwrap().to_string_lossy()
    ));
    common::git_in(
        parent,
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
    );
    std::fs::write(work.join("README"), "init\n").unwrap();
    common::git_in(&work, &["add", "."]);
    common::git_in(&work, &["commit", "-m", "initial"]);
    common::git_in(&work, &["tag", "-a", tag, "-m", "release"]);
    common::git_in(&work, &["push", "origin", "main", "--tags"]);
    common::git_in(&work, &["rev-parse", "HEAD"])
}

fn manifest_toml(repos: &[(&str, &str)]) -> String {
    repos
        .iter()
        .map(|(path, url)| {
            format!(
                "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
            )
        })
        .collect()
}

fn version_in(lock_path: &Path, repo: &str) -> String {
    let lock = repoweave::manifest::LockFile::from_path(lock_path).unwrap();
    lock.get_entry(&repoweave::manifest::RepoPath::new(repo).unwrap())
        .unwrap_or_else(|| panic!("{} has no entry for {repo}", lock_path.display()))
        .version
        .as_str()
        .to_owned()
}

fn names_the_pin(stderr: &str, entry: &str, version: &str) -> bool {
    stderr.contains(&format!(
        "{entry}: lock pins `{version}`, a name rather than a commit id"
    )) && stderr.contains("`rwv lock` records the commit id")
}

struct Published {
    _tmp: tempfile::TempDir,
    workspace: PathBuf,
    source: String,
    dep_sha: String,
    extra_sha: String,
}

/// A published project whose committed lock pins `dep` by its tag. With
/// `with_extra`, the manifest also names a second repo the lock does not
/// cover, whose HEAD is tagged too.
fn published(with_extra: bool) -> Published {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let dep_bare = tmp.path().join("dep.git");
    let dep_sha = tagged_remote(&dep_bare, "v1.0.0");
    let extra_bare = tmp.path().join("extra.git");
    let extra_sha = tagged_remote(&extra_bare, "v2.0.0");

    let project_bare = tmp.path().join("project.git");
    common::git_in(
        tmp.path(),
        &[
            "init",
            "--bare",
            "-b",
            "main",
            &project_bare.to_string_lossy(),
        ],
    );
    let work = tmp.path().join("project-work");
    common::git_in(
        tmp.path(),
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
    );
    let dep_url = common::file_url(&dep_bare);
    let extra_url = common::file_url(&extra_bare);
    let mut repos = vec![(DEP, dep_url.as_str())];
    if with_extra {
        repos.push((EXTRA, extra_url.as_str()));
    }
    std::fs::write(work.join("rwv.toml"), manifest_toml(&repos)).unwrap();
    common::fixture_lock(&work, &[(DEP, &dep_url, "v1.0.0")]);
    common::git_in(&work, &["add", "."]);
    common::git_in(&work, &["commit", "-m", "manifest+lock"]);
    common::git_in(&work, &["push", "origin", "main"]);

    Published {
        _tmp: tmp,
        workspace,
        source: common::file_url(&project_bare),
        dep_sha,
        extra_sha,
    }
}

#[test]
fn a_frozen_fetch_resolves_a_tag_pinned_entry_and_warns() {
    let p = published(false);
    let (_, stderr) = succeed(&p.workspace, &["fetch", &p.source, "--frozen"]);

    assert!(
        names_the_pin(&stderr, DEP, "v1.0.0"),
        "fetch must say the entry is pinned by name and name the repair:\n{stderr}"
    );
    assert_eq!(
        common::git_in(common::under(&p.workspace, DEP), &["rev-parse", "HEAD"]),
        p.dep_sha,
        "the entry resolves to the commit the tag names"
    );
}

/// The additive write pins both the repo it adds and the existing entry
/// pinned by name by commit id, though a tag names each HEAD.
#[test]
fn an_additive_fetch_writes_every_entry_as_a_commit_id() {
    let p = published(true);
    let (_, stderr) = succeed(&p.workspace, &["fetch", &p.source]);
    assert!(
        names_the_pin(&stderr, DEP, "v1.0.0"),
        "fetch must say the entry is pinned by name:\n{stderr}"
    );

    let lock_path = p
        .workspace
        .join("projects")
        .join("project")
        .join("rwv.lock");
    assert_eq!(
        version_in(&lock_path, EXTRA),
        p.extra_sha,
        "the added entry must be the commit id, not the tag naming it"
    );
    assert_eq!(
        version_in(&lock_path, DEP),
        p.dep_sha,
        "the existing entry pinned by name must be written as its commit id"
    );
}

/// A sync reads both sides' locks and proceeds on an entry pinned by name;
/// its relock records the commit, on the landing seat and on the target it
/// advances.
#[test]
fn a_landing_warns_on_a_tag_pinned_entry_and_its_relock_records_the_commit() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(root.join("projects")).unwrap();
    let dep_bare = tmp.path().join("dep.git");
    let dep_sha = tagged_remote(&dep_bare, "v1.0.0");
    common::git_in(
        tmp.path(),
        &[
            "clone",
            &dep_bare.to_string_lossy(),
            &common::under(&root, DEP).to_string_lossy(),
        ],
    );

    let project = root.join("projects").join(PROJECT);
    std::fs::create_dir_all(&project).unwrap();
    common::git_in(&project, &["init", "-b", "main"]);
    std::fs::write(project.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
    std::fs::write(project.join(".gitignore"), "*.code-workspace\ngita/\n").unwrap();
    let dep_url = common::file_url(&dep_bare);
    std::fs::write(project.join("rwv.toml"), manifest_toml(&[(DEP, &dep_url)])).unwrap();
    common::fixture_lock(&project, &[(DEP, &dep_url, "v1.0.0")]);
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-m", "initial"]);
    std::fs::write(root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    succeed(&root, &["workweave", PROJECT, "create", "s1"]);
    let seat = tmp
        .path()
        .join(".workweaves")
        .join(format!("{PROJECT}--s1"));

    let (_, stderr) = succeed(&seat, &["sync-to"]);
    assert!(
        names_the_pin(&stderr, &format!("ws/{DEP}"), "v1.0.0"),
        "the landing must name the target's entry pinned by name:\n{stderr}"
    );
    assert!(
        names_the_pin(&stderr, &format!("s1/{DEP}"), "v1.0.0"),
        "the landing must name the seat's entry pinned by name:\n{stderr}"
    );
    for workspace in [&seat, &root] {
        let lock_path = workspace.join("projects").join(PROJECT).join("rwv.lock");
        assert_eq!(
            version_in(&lock_path, DEP),
            dep_sha,
            "the relock must record the commit id in {}",
            workspace.display()
        );
    }
}

fn json(dir: &Path, args: &[&str]) -> serde_json::Value {
    let (stdout, _) = succeed(dir, args);
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("rwv {args:?}: {e}\n{stdout}"))
}

fn pinned_by_name_findings(dir: &Path) -> Vec<serde_json::Value> {
    let out = run(dir, &["doctor", "--json"]);
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "doctor --json: {e}\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    doc["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["kind"] == "lock-pinned-by-name")
        .cloned()
        .collect()
}

fn dep_status(dir: &Path) -> serde_json::Value {
    let doc = json(dir, &["status", "--json"]);
    doc["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == DEP)
        .unwrap_or_else(|| panic!("status lists no {DEP}:\n{doc}"))
        .clone()
}

#[test]
fn doctor_reports_and_status_marks_a_tag_pinned_entry() {
    let p = published(false);
    succeed(&p.workspace, &["fetch", &p.source, "--frozen"]);

    let findings = pinned_by_name_findings(&p.workspace);
    assert_eq!(findings.len(), 1, "one finding: {findings:?}");
    assert_eq!(findings[0]["path"], DEP);
    assert_eq!(findings[0]["project"], "project");
    assert_eq!(findings[0]["version"], "v1.0.0");

    let (stdout, _) = succeed(&p.workspace, &["doctor"]);
    assert!(
        stdout.contains("lock pins `v1.0.0`")
            && stdout.contains("`rwv lock` records the commit id"),
        "doctor text must name the entry and the fix:\n{stdout}"
    );

    assert_eq!(dep_status(&p.workspace)["lock_pinned_by_name"], true);
    let (stdout, _) = succeed(&p.workspace, &["status"]);
    assert!(
        stdout.contains("pinned by name; run `rwv lock`"),
        "status text must mark the entry:\n{stdout}"
    );
}

#[test]
fn a_commit_id_pinned_entry_raises_no_finding_and_no_marker() {
    let p = published(false);
    succeed(&p.workspace, &["fetch", &p.source, "--frozen"]);
    succeed(&p.workspace, &["lock", "--project", "project"]);

    assert_eq!(
        pinned_by_name_findings(&p.workspace),
        Vec::<serde_json::Value>::new()
    );
    assert_eq!(dep_status(&p.workspace)["lock_pinned_by_name"], false);
    let (stdout, _) = succeed(&p.workspace, &["status"]);
    assert!(!stdout.contains("pinned by name"), "{stdout}");
}

#[test]
fn doctor_locked_warns_beside_a_tag_pinned_entry() {
    let p = published(false);
    succeed(&p.workspace, &["fetch", &p.source, "--frozen"]);
    let (stdout, _) = succeed(&p.workspace, &["doctor", "--locked"]);
    assert!(
        stdout.contains(&format!("warning: {DEP}: lock pins `v1.0.0`")),
        "{stdout}"
    );
}

#[test]
fn a_json_landing_still_warns_on_a_tag_pinned_entry() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(root.join("projects")).unwrap();
    let dep_bare = tmp.path().join("dep.git");
    tagged_remote(&dep_bare, "v1.0.0");
    common::git_in(
        tmp.path(),
        &[
            "clone",
            &dep_bare.to_string_lossy(),
            &common::under(&root, DEP).to_string_lossy(),
        ],
    );
    let project = root.join("projects").join(PROJECT);
    std::fs::create_dir_all(&project).unwrap();
    common::git_in(&project, &["init", "-b", "main"]);
    std::fs::write(project.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
    std::fs::write(project.join(".gitignore"), "*.code-workspace\ngita/\n").unwrap();
    let dep_url = common::file_url(&dep_bare);
    std::fs::write(project.join("rwv.toml"), manifest_toml(&[(DEP, &dep_url)])).unwrap();
    common::fixture_lock(&project, &[(DEP, &dep_url, "v1.0.0")]);
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-m", "initial"]);
    std::fs::write(root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    succeed(&root, &["workweave", PROJECT, "create", "s1"]);
    let seat = tmp
        .path()
        .join(".workweaves")
        .join(format!("{PROJECT}--s1"));

    let (_, stderr) = succeed(&seat, &["sync-to", "--json"]);
    assert!(
        names_the_pin(&stderr, &format!("ws/{DEP}"), "v1.0.0"),
        "sync-to --json must name the entry on stderr:\n{stderr}"
    );
}
