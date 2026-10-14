//! A member a workweave's lock pins, that the workweave has no worktree for —
//! added to its manifest after the workweave was made, created with
//! `rwv add --new` before it had a commit, or arriving by sync.
//!
//!   - doctor reports it as `workweave-drift`, and only as that;
//!   - `rwv doctor --fix` creates it when the canonical clone holds the pinned
//!     revision, and `rwv fetch` when the canonical clone is not on disk;
//!   - afterwards `rwv fetch` and `rwv doctor` agree;
//!   - sync brings a member the source added, at the source's pin, and a
//!     sync-to replay does not park on a member the target lacks.

use assert_cmd::Command as AssertCommand;
use std::path::{Path, PathBuf};

mod common;

const OWNED_PATH: &str = "github/org/owned";
const LATE_PATH: &str = "github/org/late";
const PROJECT: &str = "app";

fn rwv() -> AssertCommand {
    common::rwv()
}

fn init_repo(path: &Path, file: &str) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    common::git_in(path, &["config", "user.email", "test@test.com"]);
    common::git_in(path, &["config", "user.name", "Test"]);
    std::fs::write(path.join(file), format!("{file}\n")).unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

fn manifest_entry(path: &str, url: &Path, role: &str) -> String {
    format!(
        "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"file://{url}\"\nversion = \"main\"\nrole = \"{role}\"\n",
        url = common::url_path(url),
    )
}

/// Append `path` to the manifest and lock in `project_dir` and commit both.
fn add_to_project(project_dir: &Path, path: &str, url: &Path, role: &str, pin: &str) {
    let manifest_path = project_dir.join("rwv.toml");
    let manifest = std::fs::read_to_string(&manifest_path).unwrap();
    std::fs::write(
        &manifest_path,
        format!("{manifest}\n{}", manifest_entry(path, url, role)),
    )
    .unwrap();
    let lock: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project_dir.join("rwv.lock")).unwrap())
            .unwrap();
    let mut rows: Vec<(String, String, String)> = lock["repositories"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(p, e)| {
            (
                p.clone(),
                e["url"].as_str().unwrap().to_owned(),
                e["version"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    rows.push((path.to_owned(), common::file_url(url), pin.to_owned()));
    let rows: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|(p, u, v)| (p.as_str(), u.as_str(), v.as_str()))
        .collect();
    common::fixture_lock(project_dir, &rows);
    common::git_in(project_dir, &["add", "rwv.toml", "rwv.lock"]);
    common::git_in(project_dir, &["commit", "-m", &format!("add {path}")]);
}

struct Weave {
    tmp: PathBuf,
    primary: PathBuf,
}

impl Weave {
    fn project_dir(&self) -> PathBuf {
        self.primary.join("projects").join(PROJECT)
    }

    fn workweave(&self, name: &str) -> PathBuf {
        self.tmp
            .join(".workweaves")
            .join(format!("{PROJECT}--{name}"))
    }

    fn create(&self, name: &str, from: Option<&Path>) -> PathBuf {
        let mut cmd = rwv();
        cmd.args(["workweave", PROJECT, "create", name]);
        if let Some(from) = from {
            cmd.args(["--from", &from.to_string_lossy()]);
        }
        cmd.current_dir(&self.primary).assert().success();
        self.workweave(name)
    }
}

/// A primary weave with one owned member, committed and locked.
fn make_weave(tmp: &Path) -> Weave {
    let primary = tmp.join("primary");
    std::fs::create_dir_all(primary.join("github/org")).unwrap();
    let owned = primary.join(OWNED_PATH);
    let owned_pin = init_repo(&owned, "OWNED");
    let project_dir = primary.join("projects").join(PROJECT);
    init_repo(&project_dir, "PLACEHOLDER");
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        manifest_entry(OWNED_PATH, &owned, "owned"),
    )
    .unwrap();
    common::fixture_lock(
        &project_dir,
        &[(OWNED_PATH, common::file_url(&owned).as_str(), &owned_pin)],
    );
    common::git_in(
        &project_dir,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&project_dir, &["commit", "-m", "lock: initial"]);
    std::fs::write(primary.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    Weave {
        tmp: tmp.to_path_buf(),
        primary,
    }
}

fn store_of(checkout: &Path) -> PathBuf {
    let common_dir = common::git_in(checkout, &["rev-parse", "--git-common-dir"]);
    std::fs::canonicalize(checkout.join(common_dir)).unwrap()
}

fn assert_worktree_at(checkout: &Path, canonical: &Path, branch: &str, pin: &str) {
    assert!(
        checkout.join(".git").is_file(),
        "{} must be a linked worktree",
        checkout.display()
    );
    assert_eq!(
        store_of(checkout),
        std::fs::canonicalize(canonical.join(".git")).unwrap(),
        "{} must link into the canonical clone's store",
        checkout.display()
    );
    assert_eq!(
        common::git_in(checkout, &["symbolic-ref", "--short", "HEAD"]),
        branch
    );
    assert_eq!(common::git_in(checkout, &["rev-parse", "HEAD"]), pin);
}

/// The findings `rwv doctor --json` reports about `path` that speak to whether
/// the workspace has its checkout, as `kind` or `kind/sub_kind`.
fn findings_about(workspace: &Path, path: &str) -> Vec<String> {
    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(workspace)
        .output()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "doctor --json must print JSON ({e}); stdout:\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    doc["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["path"] == path)
        .filter(|v| {
            [
                "workweave-drift",
                "dangling-reference",
                "stale-lock",
                "unresolvable-lock-entry",
                "head-unreadable",
            ]
            .contains(&v["kind"].as_str().unwrap())
        })
        .map(|v| match v["sub_kind"].as_str() {
            Some(sub) => format!("{}/{sub}", v["kind"].as_str().unwrap()),
            None => v["kind"].as_str().unwrap().to_owned(),
        })
        .collect()
}

fn run(cmd: &[&str], dir: &Path) -> std::process::Output {
    rwv().args(cmd).current_dir(dir).output().unwrap()
}

fn describe(out: &std::process::Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_late_added_member_of_each_role_is_created_by_doctor_fix() {
    for role in ["owned", "dependency", "reference"] {
        let tmp = common::tempdir().unwrap();
        let weave = make_weave(tmp.path());
        let seat = weave.create("seat", None);

        let canonical = weave.primary.join(LATE_PATH);
        let pin = init_repo(&canonical, "LATE");
        add_to_project(&weave.project_dir(), LATE_PATH, &canonical, role, &pin);
        add_to_project(
            &seat.join("projects").join(PROJECT),
            LATE_PATH,
            &canonical,
            role,
            &pin,
        );

        assert_eq!(
            findings_about(&seat, LATE_PATH),
            vec!["workweave-drift/missing".to_owned()],
            "role {role}: doctor reports the gap once, as the finding whose repair \
             applies — never also as a dangling reference whose remedy is fetch"
        );
        let report = run(&["doctor"], &seat);
        assert_eq!(
            report.status.code(),
            Some(1),
            "role {role}: {}",
            describe(&report)
        );

        let fixed = run(&["doctor", "--fix"], &seat);
        assert!(fixed.status.success(), "role {role}: {}", describe(&fixed));
        assert_worktree_at(&seat.join(LATE_PATH), &canonical, "app--seat", &pin);

        let fetched = run(&["fetch"], &seat);
        assert!(
            fetched.status.success(),
            "role {role}: {}",
            describe(&fetched)
        );
        assert_eq!(
            findings_about(&seat, LATE_PATH),
            Vec::<String>::new(),
            "role {role}: fetch and doctor agree"
        );
        assert_worktree_at(&seat.join(LATE_PATH), &canonical, "app--seat", &pin);
    }
}

#[test]
fn a_late_added_member_whose_canonical_clone_is_not_on_disk_is_created_by_fetch() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let seat = weave.create("seat", None);

    let source = tmp.path().join("source-late");
    let pin = init_repo(&source, "LATE");
    let upstream = tmp.path().join("upstream-late.git");
    common::git_in(
        tmp.path(),
        &[
            "clone",
            "--bare",
            &source.to_string_lossy(),
            &upstream.to_string_lossy(),
        ],
    );
    add_to_project(
        &seat.join("projects").join(PROJECT),
        LATE_PATH,
        &upstream,
        "reference",
        &pin,
    );

    assert_eq!(
        findings_about(&seat, LATE_PATH),
        vec!["workweave-drift/missing-unfetched".to_owned()]
    );
    let fixed = run(&["doctor", "--fix"], &seat);
    assert_eq!(fixed.status.code(), Some(1), "{}", describe(&fixed));
    let canonical = weave.primary.join(LATE_PATH);
    assert!(
        !canonical.exists() && !seat.join(LATE_PATH).exists(),
        "doctor --fix never clones; that is fetch's, behind the network"
    );

    let fetched = run(&["fetch"], &seat);
    assert!(fetched.status.success(), "{}", describe(&fetched));
    assert_worktree_at(&seat.join(LATE_PATH), &canonical, "app--seat", &pin);
    assert_eq!(
        findings_about(&seat, LATE_PATH),
        Vec::<String>::new(),
        "fetch and doctor agree"
    );
}

#[test]
fn a_reference_member_arriving_by_sync_is_a_worktree_at_the_pinned_revision() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let seat = weave.create("seat", None);

    let canonical = weave.primary.join(LATE_PATH);
    init_repo(&canonical, "LATE");
    let pin = common::git_in(&canonical, &["rev-parse", "HEAD"]);
    add_to_project(
        &weave.project_dir(),
        LATE_PATH,
        &canonical,
        "reference",
        &pin,
    );
    std::fs::write(canonical.join("AHEAD"), "a\n").unwrap();
    common::git_in(&canonical, &["add", "AHEAD"]);
    common::git_in(&canonical, &["commit", "-m", "ahead of the pin"]);
    common::git_in(&canonical, &["checkout", "-q", &pin]);

    let synced = run(&["sync", "primary"], &seat);
    assert!(synced.status.success(), "{}", describe(&synced));

    assert_worktree_at(&seat.join(LATE_PATH), &canonical, "app--seat", &pin);
    let lock =
        std::fs::read_to_string(seat.join("projects").join(PROJECT).join("rwv.lock")).unwrap();
    assert!(
        lock.contains(&pin),
        "the workweave's lock pins the member where the source's does; lock:\n{lock}"
    );
}

#[test]
fn a_sync_to_does_not_park_on_a_member_its_target_lacks() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let parent = weave.create("parent", None);
    let child = weave.create("child", Some(&parent));

    let canonical = weave.primary.join(LATE_PATH);
    let pin = init_repo(&canonical, "LATE");
    add_to_project(
        &parent.join("projects").join(PROJECT),
        LATE_PATH,
        &canonical,
        "reference",
        &pin,
    );
    assert!(
        !parent.join(LATE_PATH).exists(),
        "fixture: the target lacks it"
    );

    let landed = run(&["sync-to"], &child);
    assert!(
        landed.status.success(),
        "sync-to must land rather than park on a member the target lacks; {}",
        describe(&landed)
    );
    assert!(
        !child.join(".rwv-op").exists() && !parent.join(".rwv-op-lease").exists(),
        "no op may be left parked"
    );
    assert_worktree_at(&child.join(LATE_PATH), &canonical, "app--child", &pin);
    assert_worktree_at(&parent.join(LATE_PATH), &canonical, "app--parent", &pin);
    assert_eq!(
        findings_about(&parent, LATE_PATH),
        Vec::<String>::new(),
        "the landing leaves the target holding every member its lock now pins"
    );
}

#[test]
fn a_sync_to_retire_lands_into_a_target_that_lacked_a_member() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let parent = weave.create("parent", None);
    let child = weave.create("child", Some(&parent));

    let canonical = weave.primary.join(LATE_PATH);
    let pin = init_repo(&canonical, "LATE");
    add_to_project(
        &parent.join("projects").join(PROJECT),
        LATE_PATH,
        &canonical,
        "reference",
        &pin,
    );

    let landed = run(&["sync-to", "--retire"], &child);
    assert!(
        landed.status.success(),
        "retire's merged-check must find the target holding the member; {}",
        describe(&landed)
    );
    assert!(!child.exists(), "--retire deletes the workweave");
    assert_worktree_at(&parent.join(LATE_PATH), &canonical, "app--parent", &pin);
}

#[test]
fn a_member_added_with_new_from_a_workweave_is_created_after_its_first_commit() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let seat = weave.create("seat", None);
    let lab = tmp.path().join("lab");
    std::fs::create_dir_all(&lab).unwrap();

    let added = run(
        &[
            "add",
            "local",
            "--new",
            "--param",
            &format!("root={}", lab.display()),
            "--param",
            "owner=acme",
            "--param",
            "repo=fresh",
        ],
        &seat,
    );
    assert!(added.status.success(), "{}", describe(&added));
    let stderr = String::from_utf8_lossy(&added.stderr);
    assert!(
        stderr.contains("`rwv lock` then `rwv doctor --fix`"),
        "the skip names the repair that will work; {}",
        describe(&added)
    );

    let member = "local/acme/fresh";
    let canonical = weave.primary.join(member);
    assert!(
        !seat.join(member).exists(),
        "fixture: no commit, so no worktree yet"
    );
    common::git_in(&canonical, &["config", "user.email", "test@test.com"]);
    common::git_in(&canonical, &["config", "user.name", "Test"]);
    std::fs::write(canonical.join("README"), "fresh\n").unwrap();
    common::git_in(&canonical, &["add", "README"]);
    common::git_in(&canonical, &["commit", "-m", "first"]);
    let pin = common::git_in(&canonical, &["rev-parse", "HEAD"]);

    let locked = run(&["lock"], &seat);
    assert!(locked.status.success(), "{}", describe(&locked));
    assert_eq!(
        findings_about(&seat, member),
        vec!["workweave-drift/missing".to_owned()]
    );

    let fixed = run(&["doctor", "--fix"], &seat);
    assert!(fixed.status.success(), "{}", describe(&fixed));
    assert_worktree_at(&seat.join(member), &canonical, "app--seat", &pin);

    let fetched = run(&["fetch"], &seat);
    assert!(fetched.status.success(), "{}", describe(&fetched));
    assert_eq!(
        findings_about(&seat, member),
        Vec::<String>::new(),
        "fetch and doctor agree"
    );
}
