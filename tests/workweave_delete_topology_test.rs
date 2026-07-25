//! Tier-0 topology behaviour for `workweave delete`.
//!
//! These tests pin the bug-fix from fo-hycb06.6: under the clone-topology
//! joint, `workweave delete` (and the delete step of `sync-to --retire`)
//! MUST resolve each per-repo worktree's actual canonical store on disk
//! rather than assuming `<ws_root>/<repo_path>` is the parent. When the
//! resolved parent reveals a topology violation that delete cannot
//! safely handle (the checkout IS a canonical store with foreign
//! dependents), the verb refuses with a named precondition pointing at
//! `rwv doctor`.
//!
//! All fixtures here are SYNTHETIC — the verb is destructive and we
//! never exercise it against the live weave.

use repoweave::manifest::{ProjectName, WorkweaveName};
use repoweave::vcs::Vcs;
use repoweave::workweave::delete_workweave;
use std::path::{Path, PathBuf};

mod common;

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {args:?} in {} failed", dir.display());
}

fn git_capture(args: &[&str], dir: &Path) -> String {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should be available");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README"), "init").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

/// Build a primary workspace plus a manifest-repo clone. Returns
/// `(ws_root, manifest_repo_path)`.
fn make_workspace(tmp: &Path, project: &str) -> (PathBuf, PathBuf) {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);
    // Mark workspace root.
    std::fs::create_dir_all(ws.join("github")).unwrap();

    // Project repo so `workweave delete` finds something to delete in
    // `projects/<project>/`.
    let project_dir = ws.join("projects").join(project);
    init_repo_with_commit(&project_dir);

    let manifest = format!(
        r#"repositories:
  github/org/repo:
    type: git
    url: file://{repo}
    version: main
    role: owned
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), &manifest).unwrap();
    git(&["add", "rwv.yaml"], &project_dir);
    git(&["commit", "-m", "add manifest"], &project_dir);

    (ws, repo_path)
}

/// Build a workweave at `ww_dir` whose per-repo checkout under
/// `ws_root/<repo_path>` is added as a worktree of `canonical_repo`.
/// Mirrors what `create_workweave` does but lets the test choose the
/// canonical store the worktree is linked into (so we can construct
/// inverted-topology fixtures).
fn add_workweave_checkout(canonical_repo: &Path, ww_dir: &Path, rel_repo_path: &str, branch: &str) {
    let dest = ww_dir.join(rel_repo_path);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let dest_str = dest.to_str().unwrap();
    git(
        &["worktree", "add", "-b", branch, dest_str, "main"],
        canonical_repo,
    );
}

fn write_marker(ww_dir: &Path, primary: &Path, project: &str) {
    let marker = format!(
        "primary: {}\nproject: {}\nparent: {}\n",
        primary.display(),
        project,
        primary.display(),
    );
    std::fs::write(ww_dir.join(".rwv-workweave"), marker).unwrap();
    // Register in the primary-side `.rwv-workweave-index` so the
    // registry-backed delete path can find this hand-crafted fixture.
    // Real `rwv workweave create` writes both the marker AND the index
    // entry; the fixtures here don't go through that entry point.
    let name = ww_dir
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.rsplit_once("--"))
        .map(|(_, n)| n.to_string())
        .expect("workweave dir name must be `<project>--<name>`");
    repoweave::workweave_index::record_workweave(
        primary,
        &repoweave::manifest::ProjectName::new(project),
        &name,
        ww_dir.to_path_buf(),
    )
    .expect("record_workweave should succeed for test fixture");
}

/// Sanity test: under correct topology, `resolve_canonical_store`
/// resolves a workweave checkout's store path back to the canonical clone
/// it links into (via `.parent()`).
#[test]
fn canonical_store_resolves_to_linked_clone_under_correct_topology() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, repo) = make_workspace(tmp.path(), "web-app");
    let ww_root = tmp.path().join(".workweaves").join("web-app--probe");
    add_workweave_checkout(&repo, &ww_root, "github/org/repo", "web-app--probe/main");

    let checkout = ww_root.join("github/org/repo");
    let store_path = repoweave::git::GitVcs
        .resolve_canonical_store(&checkout)
        .expect("resolve_canonical_store returned None");
    // `.parent()` strips the trailing `.git` component to get the clone directory.
    let resolved = store_path
        .parent()
        .expect("store path should have a parent")
        .canonicalize()
        .unwrap();
    let expected = repo.canonicalize().unwrap();
    assert_eq!(
        resolved,
        expected,
        "checkout should resolve to its canonical clone (the linked-in repo at \
         {})",
        expected.display()
    );
    let _ = ws; // silence unused
}

/// Under inverted topology — the workweave checkout links to a DIFFERENT
/// canonical store than `<ws_root>/<repo_path>` — `delete_workweave`
/// must run `git worktree remove` in the RESOLVED parent (so the actual
/// canonical store's registration is cleaned up) and leave no stale
/// entry behind. The disconnected primary slot stays untouched.
#[test]
fn delete_uses_resolved_parent_under_inverted_topology() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, primary_slot) = make_workspace(tmp.path(), "web-app");

    // Fabricate the "real" canonical store living OUTSIDE the primary
    // slot — this is the inverted-topology hazard. Clone from the
    // primary slot so the SHAs line up (the new clone has the same
    // initial commit), then make `<ws>/.workweaves/...` and add a
    // worktree FROM this disconnected canonical store.
    let real_canonical = tmp.path().join("real-canonical/github/org/repo");
    std::fs::create_dir_all(real_canonical.parent().unwrap()).unwrap();
    let primary_slot_str = primary_slot.to_str().unwrap();
    let real_canonical_str = real_canonical.to_str().unwrap();
    let status = common::git()
        .args(["clone", primary_slot_str, real_canonical_str])
        .status()
        .expect("git clone");
    assert!(status.success(), "clone failed");
    // Re-configure user so commits don't fail if needed downstream.
    git(&["config", "user.email", "test@test.com"], &real_canonical);
    git(&["config", "user.name", "Test"], &real_canonical);

    let weaveroot = tmp.path().join(".workweaves");
    let ww_dir = weaveroot.join("web-app--ww");
    // Per-repo checkout linked into the REAL canonical store, not the
    // primary slot.
    add_workweave_checkout(
        &real_canonical,
        &ww_dir,
        "github/org/repo",
        "web-app--ww/main",
    );
    // Project worktree is a normal one — linked into primary's project.
    let project_dir = ws.join("projects/web-app");
    add_workweave_checkout(
        &project_dir,
        &ww_dir,
        "projects/web-app",
        "web-app--ww/proj",
    );
    write_marker(&ww_dir, &ws, "web-app");
    // Active project marker (delete_workweave resolves the dir through
    // the primary-side registry).
    std::fs::write(ww_dir.join(".rwv-active"), "web-app\n").unwrap();

    // Confirm the inverted topology: the worktree in the workweave is
    // registered in `real_canonical`, not in `primary_slot`.
    let real_worktrees = git_capture(&["worktree", "list", "--porcelain"], &real_canonical);
    assert!(
        real_worktrees.contains("web-app--ww"),
        "real canonical should know about the workweave worktree:\n{real_worktrees}"
    );
    let primary_worktrees = git_capture(&["worktree", "list", "--porcelain"], &primary_slot);
    assert!(
        !primary_worktrees.contains("web-app--ww"),
        "primary slot should NOT know about the workweave worktree:\n{primary_worktrees}"
    );

    // Now: invoke delete via the production code. Under the bug, this
    // would run `worktree remove` in `primary_slot` (the wrong DAG) and
    // leave a stale registration in `real_canonical`. Under the fix it
    // resolves to `real_canonical` and the registration is cleaned up.
    let result = delete_workweave(
        &ws,
        &ProjectName::new("web-app"),
        &WorkweaveName::new("ww"),
        true, // discard_uncommitted: we're testing topology, not the dirty gate
        repoweave::cli::consent::DiscardUnmergedConsent::from_flag(true), // the unmerged waiver, as the CLI mints it
    );
    assert!(
        result.is_ok(),
        "delete should succeed via resolved parent; got: {result:?}"
    );
    assert!(
        !ww_dir.exists(),
        "workweave dir should be gone after delete"
    );

    // The real canonical store's worktree list must no longer include
    // the workweave checkout — `worktree remove` ran in the right repo.
    let real_after = git_capture(&["worktree", "list", "--porcelain"], &real_canonical);
    assert!(
        !real_after.contains("web-app--ww/github/org/repo"),
        "real canonical should have no stale workweave registration after delete:\n{real_after}"
    );
}

/// When a workweave checkout is itself a canonical store that OTHER
/// worktrees link into (the catastrophic case the joint flags as
/// fo-a0spgj hazard 2), delete must refuse with a named precondition
/// pointing at `rwv doctor`. The refusal is NOT bypassable with the discard
/// waivers.
#[test]
fn delete_refuses_when_checkout_hosts_foreign_worktrees_even_with_waivers() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, primary_slot) = make_workspace(tmp.path(), "web-app");

    // The workweave's per-repo checkout will BE the canonical store. To
    // arrange this synthetically, build a standalone clone INSIDE the
    // workweave directory at the manifest-repo slot, then add a
    // "foreign" worktree from that clone living somewhere ELSE on disk.
    let weaveroot = tmp.path().join(".workweaves");
    let ww_dir = weaveroot.join("web-app--bad");
    let ww_repo_slot = ww_dir.join("github/org/repo");
    std::fs::create_dir_all(ww_repo_slot.parent().unwrap()).unwrap();
    let status = common::git()
        .args([
            "clone",
            primary_slot.to_str().unwrap(),
            ww_repo_slot.to_str().unwrap(),
        ])
        .status()
        .expect("git clone");
    assert!(status.success(), "clone failed");
    git(&["config", "user.email", "test@test.com"], &ww_repo_slot);
    git(&["config", "user.name", "Test"], &ww_repo_slot);

    // Add a foreign worktree linked into ww_repo_slot (outside the
    // workweave dir). Deleting the workweave would orphan this.
    let foreign = tmp.path().join("foreign-checkout");
    git(
        &[
            "worktree",
            "add",
            "-b",
            "stranger",
            foreign.to_str().unwrap(),
            "main",
        ],
        &ww_repo_slot,
    );

    // Project worktree: a normal linked workspace under primary.
    let project_dir = ws.join("projects/web-app");
    add_workweave_checkout(
        &project_dir,
        &ww_dir,
        "projects/web-app",
        "web-app--bad/proj",
    );
    write_marker(&ww_dir, &ws, "web-app");
    std::fs::write(ww_dir.join(".rwv-active"), "web-app\n").unwrap();

    // Even with both waivers, delete must refuse.
    let result = delete_workweave(
        &ws,
        &ProjectName::new("web-app"),
        &WorkweaveName::new("bad"),
        true,
        repoweave::cli::consent::DiscardUnmergedConsent::from_flag(true),
    );
    let err = result.expect_err("delete should refuse when a checkout hosts foreign worktrees");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("inverted clone topology")
            && msg.contains("no-canonical-store-with-foreign-dependents")
            && msg.contains("rwv doctor"),
        "refusal must name the precondition and point at `rwv doctor`. got:\n{msg}"
    );
    // Workweave dir must STILL EXIST — the refusal must not have
    // partially destroyed it.
    assert!(
        ww_dir.exists(),
        "workweave dir should be preserved by a refusing delete"
    );
    // The foreign worktree must still exist and be a registered worktree.
    assert!(
        foreign.exists(),
        "foreign worktree must not be orphaned by the refusing delete"
    );
    let registrations = git_capture(&["worktree", "list", "--porcelain"], &ww_repo_slot);
    assert!(
        registrations.contains("foreign-checkout"),
        "foreign worktree registration must remain intact:\n{registrations}"
    );
}

/// Companion sanity: when a checkout is a canonical store but has NO
/// foreign dependents (only itself), delete proceeds normally — the
/// precondition is "foreign dependents", not "is a canonical store".
#[test]
fn delete_proceeds_when_canonical_checkout_has_no_foreign_dependents() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, primary_slot) = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    let ww_dir = weaveroot.join("web-app--lone");
    let ww_repo_slot = ww_dir.join("github/org/repo");
    std::fs::create_dir_all(ww_repo_slot.parent().unwrap()).unwrap();
    let status = common::git()
        .args([
            "clone",
            primary_slot.to_str().unwrap(),
            ww_repo_slot.to_str().unwrap(),
        ])
        .status()
        .expect("git clone");
    assert!(status.success(), "clone failed");
    git(&["config", "user.email", "test@test.com"], &ww_repo_slot);
    git(&["config", "user.name", "Test"], &ww_repo_slot);
    // No foreign worktree.

    let project_dir = ws.join("projects/web-app");
    add_workweave_checkout(
        &project_dir,
        &ww_dir,
        "projects/web-app",
        "web-app--lone/proj",
    );
    write_marker(&ww_dir, &ws, "web-app");
    std::fs::write(ww_dir.join(".rwv-active"), "web-app\n").unwrap();

    let result = delete_workweave(
        &ws,
        &ProjectName::new("web-app"),
        &WorkweaveName::new("lone"),
        true,
        repoweave::cli::consent::DiscardUnmergedConsent::from_flag(true),
    );
    assert!(
        result.is_ok(),
        "delete should succeed when canonical checkout has no foreign dependents; got: {result:?}"
    );
    assert!(!ww_dir.exists(), "workweave dir should be removed");
}

/// Under inverted topology, the merged-check that gates delete must run
/// in the workweave checkout's RESOLVED canonical store — not the
/// `<ws_root>/<repo_path>` slot, which under inverted topology is a
/// disconnected DAG that knows nothing about the workweave's commits.
///
/// Asking `is_ancestor` in the wrong DAG silently lies: it may green-light
/// deletion of work the operator wanted (false-merged) or refuse a
/// genuinely merged retire (false-unmerged). Both failure modes pin to
/// the same fix.
///
/// This test pins the **false-merged** failure mode: a workweave whose
/// canonical store is the disconnected primary slot (the "bad" inverted
/// case) advances both clones to the SAME tip — so the original code
/// silently vouches via SHA equality on `wt_head == c`, missing the fact
/// that the comparison crossed DAGs. The post-fix code refuses to vouch
/// across distinct canonical stores and treats the workweave as having
/// commits not merged into the baseline.
#[test]
fn merged_check_refuses_vouch_across_distinct_canonical_stores() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, primary_slot) = make_workspace(tmp.path(), "web-app");

    // Real canonical = a separate clone (disconnected DAG). After the
    // initial clone both sides have the same SHA at HEAD.
    let real_canonical = tmp.path().join("real-canonical/github/org/repo");
    std::fs::create_dir_all(real_canonical.parent().unwrap()).unwrap();
    let status = common::git()
        .args([
            "clone",
            primary_slot.to_str().unwrap(),
            real_canonical.to_str().unwrap(),
        ])
        .status()
        .expect("git clone");
    assert!(status.success(), "clone failed");
    git(&["config", "user.email", "test@test.com"], &real_canonical);
    git(&["config", "user.name", "Test"], &real_canonical);

    // Now diverge: add a new commit to real_canonical's main so the
    // workweave's tip carries a SHA that ONLY exists in the real-canonical
    // DAG — never reachable from the primary slot's DAG.
    std::fs::write(real_canonical.join("only-on-real"), "x\n").unwrap();
    git(&["add", "only-on-real"], &real_canonical);
    git(&["commit", "-m", "real-canonical unique"], &real_canonical);

    let weaveroot = tmp.path().join(".workweaves");
    let ww_dir = weaveroot.join("web-app--diverged");
    // Worktree linked into the real_canonical DAG — carries the
    // real-canonical-only commit.
    add_workweave_checkout(
        &real_canonical,
        &ww_dir,
        "github/org/repo",
        "web-app--diverged/main",
    );
    let project_dir = ws.join("projects/web-app");
    add_workweave_checkout(
        &project_dir,
        &ww_dir,
        "projects/web-app",
        "web-app--diverged/proj",
    );
    write_marker(&ww_dir, &ws, "web-app");
    std::fs::write(ww_dir.join(".rwv-active"), "web-app\n").unwrap();

    // Without the unmerged waiver, delete should refuse because the workweave carries
    // commits that the baseline (primary slot, disconnected DAG) cannot
    // reach. The pre-fix code asked `is_ancestor` in the WORKWEAVE
    // checkout (real_canonical DAG, which DOES contain both refs) and
    // would have answered "false" (not ancestral — diverged) here as
    // well, so this assertion holds for both pre- and post-fix code.
    // The post-fix code reaches the same conclusion via the cleaner
    // canonical-store-equality path: real_canonical != primary_slot, so
    // the baseline cannot vouch.
    let result = delete_workweave(
        &ws,
        &ProjectName::new("web-app"),
        &WorkweaveName::new("diverged"),
        true, // uncommitted changes are irrelevant here
        None, // exercise the merged-check
    );
    let err = result.expect_err("delete should refuse when workweave carries unmerged work");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("commits not merged"),
        "refusal must cite the unmerged-commits precondition. got:\n{msg}"
    );
    assert!(
        ww_dir.exists(),
        "workweave dir must be preserved by refusal"
    );
}
