//! A `reference` member is a worktree like every other member, and flows
//! through every verb that walks a workweave's members the way an `owned` one
//! does.
//!
//!   - create: every member is a worktree of the canonical clone on the
//!     workweave's ephemeral branch; no member slot is a symlink.
//!   - nested create: the child's worktree links into the canonical store, not
//!     into the parent workweave.
//!   - delete: the canonical clone keeps everything but the workweave's own
//!     branch, even while it is dirty.
//!   - sync, sync-to, `--retire` and abort move and restore a reference
//!     member's worktree like an owned one, and never write into the canonical
//!     clone's own branch except by landing.

use assert_cmd::Command as AssertCommand;
use std::path::{Path, PathBuf};

mod common;

const OWNED_PATH: &str = "github/org/owned";
const REF_PATH: &str = "github/org/reference";
const PROJECT: &str = "app";

fn rwv() -> AssertCommand {
    common::rwv()
}

fn init_repo(path: &Path, file: &str, contents: &str) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    common::git_in(path, &["config", "user.email", "test@test.com"]);
    common::git_in(path, &["config", "user.name", "Test"]);
    std::fs::write(path.join(file), contents).unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

fn commit_file(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    common::git_in(repo, &["add", filename]);
    common::git_in(repo, &["commit", "-m", msg]);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

/// Everything about the canonical clone a workweave could disturb: its HEAD,
/// every ref, its working tree and its status.
#[derive(Debug, PartialEq, Eq)]
struct CanonicalFingerprint {
    head: String,
    refs: String,
    worktree_file: String,
    status: String,
    current_branch: String,
}

fn fingerprint(canonical: &Path) -> CanonicalFingerprint {
    CanonicalFingerprint {
        head: common::git_in(canonical, &["rev-parse", "HEAD"]),
        refs: common::git_in(
            canonical,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        ),
        worktree_file: std::fs::read_to_string(canonical.join("REF")).unwrap_or_default(),
        status: common::git_in(canonical, &["status", "--porcelain"]),
        current_branch: common::git_in(canonical, &["symbolic-ref", "--short", "HEAD"]),
    }
}

struct Primary {
    root: PathBuf,
    owned_canonical: PathBuf,
    ref_canonical: PathBuf,
}

/// A primary weave whose committed manifest and lock name one `owned` and one
/// `reference` member.
fn make_primary(parent: &Path) -> Primary {
    let ws = parent.join("primary");
    std::fs::create_dir_all(ws.join("github/org")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();

    let owned = ws.join(OWNED_PATH);
    let owned_sha = init_repo(&owned, "OWNED", "owned-init\n");
    let reference = ws.join(REF_PATH);
    let ref_sha = init_repo(&reference, "REF", "reference-init\n");

    let project_dir = ws.join("projects").join(PROJECT);
    init_repo(&project_dir, "PLACEHOLDER", "p\n");
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    let manifest = format!(
        "[repositories.\"{OWNED_PATH}\"]\ntype = \"git\"\nurl = \"file://{owned}\"\nversion = \"main\"\nrole = \"owned\"\n\n[repositories.\"{REF_PATH}\"]\ntype = \"git\"\nurl = \"file://{reference}\"\nversion = \"main\"\nrole = \"reference\"\n",
        owned = common::url_path(&owned),
        reference = common::url_path(&reference),
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    let owned_url = common::file_url(&owned);
    let reference_url = common::file_url(&reference);
    common::fixture_lock(
        &project_dir,
        &[
            (OWNED_PATH, owned_url.as_str(), owned_sha.as_str()),
            (REF_PATH, reference_url.as_str(), ref_sha.as_str()),
        ],
    );
    common::git_in(
        &project_dir,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&project_dir, &["commit", "-m", "lock: initial"]);
    std::fs::write(ws.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    Primary {
        root: ws,
        owned_canonical: owned,
        ref_canonical: reference,
    }
}

struct Workweave {
    root: PathBuf,
    owned_checkout: PathBuf,
    ref_checkout: PathBuf,
}

fn workweave_at(root: PathBuf) -> Workweave {
    Workweave {
        owned_checkout: root.join(OWNED_PATH),
        ref_checkout: root.join(REF_PATH),
        root,
    }
}

fn create_workweave(primary: &Primary, name: &str) -> Workweave {
    rwv()
        .args(["workweave", PROJECT, "create", name])
        .current_dir(&primary.root)
        .assert()
        .success();
    workweave_at(
        primary
            .root
            .parent()
            .unwrap()
            .join(".workweaves")
            .join(format!("{PROJECT}--{name}")),
    )
}

fn rwv_lock_commit(workspace_root: &Path) {
    rwv()
        .args(["lock", "--commit"])
        .current_dir(workspace_root)
        .assert()
        .success();
}

/// The store `checkout`'s objects and refs live in.
fn store_of(checkout: &Path) -> PathBuf {
    let common_dir = common::git_in(checkout, &["rev-parse", "--git-common-dir"]);
    std::fs::canonicalize(checkout.join(common_dir)).unwrap()
}

/// What every member of a workweave is: a linked worktree of the canonical
/// clone, on the workweave's ephemeral branch.
fn assert_member_worktree(checkout: &Path, canonical: &Path, branch: &str) {
    assert!(
        !checkout.is_symlink(),
        "{} is a symlink; no member is ever one",
        checkout.display()
    );
    assert!(
        checkout.join(".git").is_file(),
        "{} must be a linked worktree (`.git` a file)",
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
        branch,
        "{} must sit on the workweave's ephemeral branch",
        checkout.display()
    );
}

#[test]
fn create_gives_every_member_a_worktree_on_the_ephemeral_branch() {
    let tmp = common::tempdir().unwrap();
    let primary = make_primary(tmp.path());

    let ww = create_workweave(&primary, "feat");

    assert_member_worktree(&ww.owned_checkout, &primary.owned_canonical, "app--feat");
    assert_member_worktree(&ww.ref_checkout, &primary.ref_canonical, "app--feat");
}

#[test]
fn a_nested_workweave_links_its_reference_worktree_into_the_canonical_store() {
    let tmp = common::tempdir().unwrap();
    let primary = make_primary(tmp.path());
    let parent = create_workweave(&primary, "parent");

    rwv()
        .args([
            "workweave",
            PROJECT,
            "create",
            "child",
            "--from",
            &parent.root.to_string_lossy(),
        ])
        .current_dir(&primary.root)
        .assert()
        .success();
    let child = workweave_at(parent.root.parent().unwrap().join("app--child"));

    assert_member_worktree(&child.ref_checkout, &primary.ref_canonical, "app--child");
    assert_eq!(
        common::git_in(&child.ref_checkout, &["rev-parse", "HEAD"]),
        common::git_in(&parent.ref_checkout, &["rev-parse", "HEAD"]),
        "the child forks the parent's reference worktree at its HEAD"
    );
}

#[test]
fn deleting_a_workweave_leaves_a_dirty_canonical_clone_untouched() {
    let tmp = common::tempdir().unwrap();
    let primary = make_primary(tmp.path());
    let before_create = fingerprint(&primary.ref_canonical);

    let ww = create_workweave(&primary, "feat");
    std::fs::write(primary.ref_canonical.join("DIRTY"), "uncommitted").unwrap();
    std::fs::write(primary.ref_canonical.join("REF"), "locally-edited").unwrap();
    let dirty = fingerprint(&primary.ref_canonical);

    rwv()
        .args(["workweave", PROJECT, "delete", "feat"])
        .current_dir(&primary.root)
        .assert()
        .success();

    assert!(!ww.root.exists(), "the workweave directory must be gone");
    let after = fingerprint(&primary.ref_canonical);
    assert_eq!(
        after.refs, before_create.refs,
        "the workweave's ephemeral branch goes with it, and no other ref moves"
    );
    assert_eq!(
        CanonicalFingerprint {
            refs: dirty.refs.clone(),
            ..after
        },
        dirty,
        "delete must leave the canonical clone's HEAD, working tree and dirt as it \
         found them"
    );
}

#[test]
fn two_workweaves_hold_separate_reference_worktrees() {
    let tmp = common::tempdir().unwrap();
    let primary = make_primary(tmp.path());
    let wa = create_workweave(&primary, "wa");
    let wb = create_workweave(&primary, "wb");
    commit_file(&wb.ref_checkout, "B", "b\n", "wb: reference commit");

    rwv()
        .args(["workweave", PROJECT, "delete", "wa"])
        .current_dir(&primary.root)
        .assert()
        .success();

    assert!(!wa.root.exists(), "wa must be gone");
    assert_member_worktree(&wb.ref_checkout, &primary.ref_canonical, "app--wb");
    assert!(
        wb.ref_checkout.join("B").is_file(),
        "wb's reference worktree keeps its own commit after wa is deleted"
    );
}

#[test]
fn sync_brings_a_reference_advance_into_a_workweave() {
    let tmp = common::tempdir().unwrap();
    let primary = make_primary(tmp.path());
    let ww = create_workweave(&primary, "feat");

    let advanced = commit_file(&primary.ref_canonical, "NEXT", "n\n", "primary: advance");
    rwv_lock_commit(&primary.root);

    rwv()
        .args(["sync", "primary"])
        .current_dir(&ww.root)
        .assert()
        .success();

    assert_member_worktree(&ww.ref_checkout, &primary.ref_canonical, "app--feat");
    assert_eq!(
        common::git_in(&ww.ref_checkout, &["rev-parse", "HEAD"]),
        advanced,
        "sync must fast-forward the workweave's reference worktree to the source's pin"
    );
}

#[test]
fn sync_to_lands_a_reference_commit_like_an_owned_one() {
    let tmp = common::tempdir().unwrap();
    let primary = make_primary(tmp.path());
    let ww = create_workweave(&primary, "feat");

    let ref_sha = commit_file(&ww.ref_checkout, "R", "r\n", "ww: reference");
    let owned_sha = commit_file(&ww.owned_checkout, "O", "o\n", "ww: owned");
    rwv_lock_commit(&ww.root);

    rwv()
        .args(["sync-to", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .success();

    assert_eq!(
        common::git_in(&primary.owned_canonical, &["rev-parse", "main"]),
        owned_sha
    );
    assert_eq!(
        common::git_in(&primary.ref_canonical, &["rev-parse", "main"]),
        ref_sha,
        "a reference member's commit lands upward exactly like an owned one's"
    );
}

#[test]
fn sync_to_retire_with_a_reference_member_retires_the_workweave() {
    let tmp = common::tempdir().unwrap();
    let primary = make_primary(tmp.path());
    let before = fingerprint(&primary.ref_canonical);
    let ww = create_workweave(&primary, "feat");

    let owned_sha = commit_file(&ww.owned_checkout, "O", "o\n", "ww: owned");
    rwv_lock_commit(&ww.root);

    rwv()
        .args(["sync-to", &primary.root.to_string_lossy(), "--retire"])
        .current_dir(&ww.root)
        .assert()
        .success();

    assert!(!ww.root.exists(), "--retire must delete the workweave");
    assert_eq!(
        common::git_in(&primary.owned_canonical, &["rev-parse", "main"]),
        owned_sha
    );
    let after = fingerprint(&primary.ref_canonical);
    assert_eq!(
        before.refs, after.refs,
        "the retired workweave's reference branch and every savepoint must be gone"
    );
    assert_eq!(
        before, after,
        "the reference canonical clone is otherwise untouched"
    );
}

#[test]
fn abort_restores_a_reference_member_like_any_other() {
    let tmp = common::tempdir().unwrap();
    let primary = make_primary(tmp.path());
    let ww = create_workweave(&primary, "feat");

    let op_id = "1700000000000000000";
    let start = common::git_in(&ww.ref_checkout, &["rev-parse", "HEAD"]);
    common::git_in(
        &ww.ref_checkout,
        &["update-ref", &format!("refs/rwv/pre-op/{op_id}"), &start],
    );
    let advanced = commit_file(&ww.ref_checkout, "R", "r\n", "op: advance");
    let record = format!(
        "{{\"id\": \"{op_id}\", \"verb\": \"sync\", \"strategy\": \"rebase\", \"project\": \"{PROJECT}\", \
         \"source\": \"{root}\", \"target\": \"{root}\", \"retire\": false, \"phase\": \"replay\", \
         \"advanced_tips\": {{\"{REF_PATH}\": \"{advanced}\"}}, \"converged_tips\": {{}}, \
         \"overrides\": [], \"started_at\": \"2026-05-27T10:00:00Z\"}}",
        root = common::json_escaped(&ww.root),
    );
    std::fs::write(ww.root.join(".rwv-op"), record).unwrap();
    let canonical_main = common::git_in(&primary.ref_canonical, &["rev-parse", "main"]);

    rwv()
        .args(["abort"])
        .current_dir(&ww.root)
        .assert()
        .success();

    assert_eq!(
        common::git_in(&ww.ref_checkout, &["rev-parse", "HEAD"]),
        start,
        "abort must restore the reference worktree to its savepoint"
    );
    assert_eq!(
        common::git_in(&primary.ref_canonical, &["rev-parse", "main"]),
        canonical_main,
        "restoring the workweave's branch never moves the canonical clone's own"
    );
}
