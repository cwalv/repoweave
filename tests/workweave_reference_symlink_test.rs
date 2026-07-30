//! Foundational tests for reference-repo symlink materialization.
//!
//! A `role: reference` repo is materialized as a SYMLINK to the canonical
//! weave-root clone (`<primary_root>/<repo_path>`) instead of cutting a
//! `git worktree`, unless `--worktree-references` restores the legacy
//! behavior. These tests pin:
//!
//!   - create: reference = symlink (not a worktree), `.git` resolves to the
//!     canonical store; owned repos stay worktrees on ephemeral branches.
//!   - `--worktree-references`: reference repos become worktrees again.
//!   - delete: the symlink is unlinked and the canonical clone is
//!     BYTE-FOR-BYTE unchanged (HEAD, refs, working tree, dirty state); no
//!     ephemeral branch is deleted in the canonical.
//!   - delete with a DIRTY canonical: still succeeds, canonical untouched.
//!   - nested workweave: the child's reference symlink targets PRIMARY's
//!     canonical, not the parent workweave's symlink.
//!   - two workweaves sharing a reference: both resolve to the same
//!     canonical; deleting one leaves the other's symlink valid.
//!   - idempotent reuse with a symlinked reference returns clean.

use std::path::{Path, PathBuf};

use repoweave::manifest::{ProjectName, WorkweaveName};
use repoweave::workweave::{classify_checkout, create_workweave, delete_workweave, CheckoutKind};

mod common;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

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

fn git_stdout(args: &[&str], dir: &Path) -> String {
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
    String::from_utf8(out.stdout).expect("git output is UTF-8")
}

/// Init a repo at `path` with one commit on `main` containing `file`.
fn init_repo_with_commit(path: &Path, file: &str, contents: &str) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join(file), contents).unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

const OWNED_REPO: &str = "github/org/owned";
const REF_REPO: &str = "github/org/reference";

/// Build a workspace with one `owned` repo and one `reference` repo.
///
/// Layout:
///   {tmp}/ws/github/                    -- registry marker
///   {tmp}/ws/github/org/owned/          -- owned repo (canonical clone)
///   {tmp}/ws/github/org/reference/      -- reference repo (canonical clone)
///   {tmp}/ws/projects/{project}/rwv.yaml
///
/// Returns the workspace root.
fn make_workspace_with_reference(tmp: &Path, project: &str) -> PathBuf {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();

    let owned = ws.join(OWNED_REPO);
    init_repo_with_commit(&owned, "OWNED", "owned-init");

    let reference = ws.join(REF_REPO);
    init_repo_with_commit(&reference, "REF", "reference-init");

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let manifest = format!(
        r#"repositories:
  {owned_path}:
    type: git
    url: file://{owned}
    version: main
    role: owned
  {ref_path}:
    type: git
    url: file://{reference}
    version: main
    role: reference
"#,
        owned_path = OWNED_REPO,
        ref_path = REF_REPO,
        owned = owned.display(),
        reference = reference.display(),
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    ws
}

/// Capture a byte-for-byte fingerprint of the canonical reference clone:
/// HEAD sha, the full ref list, the working-tree file content, and the
/// porcelain status (dirty state). Used to assert "canonical untouched".
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
        head: git_stdout(&["rev-parse", "HEAD"], canonical),
        refs: git_stdout(&["show-ref"], canonical),
        worktree_file: std::fs::read_to_string(canonical.join("REF")).unwrap_or_default(),
        status: git_stdout(&["status", "--porcelain"], canonical),
        current_branch: git_stdout(&["symbolic-ref", "--short", "HEAD"], canonical),
    }
}

// ---------------------------------------------------------------------------
// create: reference = symlink, owned = worktree
// ---------------------------------------------------------------------------

#[test]
fn create_materializes_reference_as_symlink_and_owned_as_worktree() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_reference(tmp.path(), "proj");

    let ww = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("feat").unwrap(),
        false,
        false,
        false, // default: symlink references
        None,
    )
    .expect("create_workweave should succeed");

    // Reference repo is a SYMLINK, not a worktree.
    let ref_checkout = ww.join(REF_REPO);
    assert!(
        ref_checkout.is_symlink(),
        "reference repo should be a symlink at {}",
        ref_checkout.display()
    );
    assert_eq!(
        classify_checkout(&ref_checkout),
        CheckoutKind::ReferenceAlias,
        "classify_checkout must report ReferenceAlias for the reference symlink"
    );

    // The symlink target is PRIMARY's canonical clone.
    let target = std::fs::read_link(&ref_checkout).expect("read_link on reference symlink");
    assert_eq!(
        target,
        ws.join(REF_REPO),
        "reference symlink must point at <primary_root>/<repo_path>"
    );

    // Its `.git` resolves to the canonical store (it IS the canonical store,
    // viewed through the symlink) — the repo's git-common-dir is the
    // canonical's own .git, NOT a linked-worktree gitdir.
    let common_dir = git_stdout(&["rev-parse", "--git-common-dir"], &ref_checkout);
    let canonical_git = ws.join(REF_REPO).join(".git");
    let resolved = Path::new(common_dir.trim());
    let resolved_abs = if resolved.is_absolute() {
        resolved.to_path_buf()
    } else {
        ref_checkout.join(resolved)
    };
    assert_eq!(
        resolved_abs.canonicalize().unwrap(),
        canonical_git.canonicalize().unwrap(),
        "reference symlink's git-common-dir must be the canonical store"
    );

    // Owned repo is a real worktree (.git is a FILE) on an ephemeral branch.
    let owned_checkout = ww.join(OWNED_REPO);
    assert!(
        !owned_checkout.is_symlink(),
        "owned repo must not be a symlink"
    );
    assert_eq!(
        classify_checkout(&owned_checkout),
        CheckoutKind::Worktree,
        "owned repo classifies as Worktree"
    );
    let dot_git = owned_checkout.join(".git");
    assert!(
        dot_git.is_file(),
        "owned repo .git should be a FILE (worktree gitlink)"
    );
    let branch = git_stdout(&["symbolic-ref", "--short", "HEAD"], &owned_checkout);
    assert_eq!(
        branch.trim(),
        "proj--feat",
        "owned worktree should be on the flat ephemeral branch (§3.5)"
    );

    // The canonical reference clone gained NO ephemeral branch.
    let refs = git_stdout(&["branch", "--list"], &ws.join(REF_REPO));
    assert!(
        !refs.contains("proj--feat"),
        "canonical reference must not get an ephemeral branch, got: {refs}"
    );
}

// ---------------------------------------------------------------------------
// --worktree-references escape hatch: reference becomes a worktree
// ---------------------------------------------------------------------------

#[test]
fn worktree_references_flag_cuts_a_worktree_for_reference_repos() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_reference(tmp.path(), "proj");

    let ww = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("feat").unwrap(),
        false,
        false,
        true, // escape hatch: worktree references
        None,
    )
    .expect("create_workweave should succeed");

    let ref_checkout = ww.join(REF_REPO);
    assert!(
        !ref_checkout.is_symlink(),
        "with --worktree-references the reference repo must be a real worktree, not a symlink"
    );
    assert_eq!(
        classify_checkout(&ref_checkout),
        CheckoutKind::Worktree,
        "worktree'd reference classifies as Worktree (escape hatch composes via CheckoutKind)"
    );
    let dot_git = ref_checkout.join(".git");
    assert!(
        dot_git.is_file(),
        "worktree'd reference .git should be a FILE (worktree gitlink)"
    );
    // On its own ephemeral branch (old behavior intact).
    let branch = git_stdout(&["symbolic-ref", "--short", "HEAD"], &ref_checkout);
    assert_eq!(
        branch.trim(),
        "proj--feat",
        "worktree'd reference should be on the flat ephemeral branch (§3.5)"
    );
}

// ---------------------------------------------------------------------------
// delete: symlink unlinked; canonical BYTE-FOR-BYTE unchanged
// ---------------------------------------------------------------------------

#[test]
fn delete_unlinks_symlink_and_leaves_canonical_byte_for_byte_unchanged() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_reference(tmp.path(), "proj");
    let canonical = ws.join(REF_REPO);

    let before = fingerprint(&canonical);

    let ww = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("feat").unwrap(),
        false,
        false,
        false,
        None,
    )
    .expect("create should succeed");
    assert!(ww.join(REF_REPO).is_symlink());

    delete_workweave(
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("feat").unwrap(),
        false,
        None,
    )
    .expect("delete should succeed");

    // Workweave dir (and thus the symlink) is gone.
    assert!(!ww.exists(), "workweave dir should be removed");

    // The canonical clone still exists and is byte-for-byte identical.
    assert!(canonical.exists(), "canonical reference clone must survive");
    let after = fingerprint(&canonical);
    assert_eq!(
        before, after,
        "canonical reference clone must be byte-for-byte unchanged after delete"
    );

    // No ephemeral branch was created or deleted in the canonical.
    let branches = git_stdout(&["branch", "--list"], &canonical);
    assert!(
        !branches.contains("proj--feat"),
        "delete must not have touched any ephemeral branch in the canonical"
    );
}

#[test]
fn delete_succeeds_with_a_dirty_canonical_and_leaves_it_untouched() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_reference(tmp.path(), "proj");
    let canonical = ws.join(REF_REPO);

    let ww = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("feat").unwrap(),
        false,
        false,
        false,
        None,
    )
    .expect("create should succeed");
    assert!(ww.join(REF_REPO).is_symlink());

    // Dirty the canonical reference clone AFTER create. A dirty canonical
    // must not block delete (no per-workweave dirty state for an alias) and
    // must be left untouched.
    std::fs::write(canonical.join("DIRTY"), "uncommitted").unwrap();
    std::fs::write(canonical.join("REF"), "locally-edited").unwrap();
    let before = fingerprint(&canonical);
    assert!(
        before.status.contains("DIRTY") && before.status.contains("REF"),
        "precondition: canonical should be dirty, got status: {:?}",
        before.status
    );

    delete_workweave(
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("feat").unwrap(),
        false, // no waiver: alias dirty state must not be attributed here
        None,
    )
    .expect("delete must succeed even with a dirty canonical");

    assert!(!ww.exists(), "workweave dir should be removed");
    assert!(canonical.exists(), "dirty canonical must survive");
    let after = fingerprint(&canonical);
    assert_eq!(
        before, after,
        "dirty canonical must be byte-for-byte unchanged after delete"
    );
}

// ---------------------------------------------------------------------------
// nested workweave: child symlink targets PRIMARY's canonical
// ---------------------------------------------------------------------------

#[test]
fn nested_workweave_reference_symlink_targets_primary_canonical() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_reference(tmp.path(), "proj");

    // Parent workweave forked from primary.
    let parent = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("parent").unwrap(),
        false,
        false,
        false,
        None,
    )
    .expect("parent create should succeed");
    assert!(parent.join(REF_REPO).is_symlink());

    // Child workweave forked FROM the parent workweave (source_root = parent),
    // but primary_root stays the primary weave.
    let child = create_workweave(
        &ws,     // primary_root
        &parent, // source_root = parent workweave
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("child").unwrap(),
        false,
        false,
        false,
        None,
    )
    .expect("child create should succeed");

    let child_ref = child.join(REF_REPO);
    assert!(child_ref.is_symlink(), "child reference must be a symlink");

    // CRITICAL: the child's symlink must point at PRIMARY's canonical clone,
    // NOT at the parent workweave's own symlink (which would be a
    // symlink->symlink chain that breaks when the parent is deleted).
    let target = std::fs::read_link(&child_ref).expect("read_link child reference");
    assert_eq!(
        target,
        ws.join(REF_REPO),
        "nested child reference symlink must target PRIMARY's canonical, not the parent's symlink"
    );
    assert_ne!(
        target,
        parent.join(REF_REPO),
        "child reference must NOT chain through the parent workweave's symlink"
    );
}

// ---------------------------------------------------------------------------
// two workweaves sharing a reference
// ---------------------------------------------------------------------------

#[test]
fn two_workweaves_share_one_canonical_and_deleting_one_keeps_the_other_valid() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_reference(tmp.path(), "proj");
    let canonical = ws.join(REF_REPO).canonicalize().unwrap();

    let wa = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("wa").unwrap(),
        false,
        false,
        false,
        None,
    )
    .expect("create wa");
    let wb = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("wb").unwrap(),
        false,
        false,
        false,
        None,
    )
    .expect("create wb");

    // Both resolve to the same canonical store.
    assert_eq!(
        wa.join(REF_REPO).canonicalize().unwrap(),
        canonical,
        "wa reference resolves to the canonical"
    );
    assert_eq!(
        wb.join(REF_REPO).canonicalize().unwrap(),
        canonical,
        "wb reference resolves to the canonical"
    );

    // Delete wa; wb's symlink must remain valid and resolve to the canonical.
    delete_workweave(
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("wa").unwrap(),
        false,
        None,
    )
    .expect("delete wa");

    assert!(!wa.exists(), "wa should be gone");
    assert!(
        wb.join(REF_REPO).is_symlink(),
        "wb reference symlink must still exist"
    );
    assert_eq!(
        wb.join(REF_REPO).canonicalize().unwrap(),
        canonical,
        "wb reference must still resolve to the canonical after wa is deleted"
    );
    // And the canonical itself is intact.
    assert!(canonical.exists(), "canonical must survive wa's delete");
}

// ---------------------------------------------------------------------------
// add-from-workweave decision: a workweave created AFTER an
// `add --role reference` symlinks the new reference repo.
//
// Per the design decision, `rwv add <url> --role reference` clones to the
// weave root and updates the manifest; existing workweaves are forked
// snapshots and do NOT retroactively materialize the repo. The symlink
// happens only at `create`. This test simulates the post-add state (manifest
// gains a reference entry, the canonical clone is present at the weave root)
// and asserts a workweave created afterward symlinks it.
// ---------------------------------------------------------------------------

#[test]
fn workweave_created_after_add_reference_symlinks_the_new_repo() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();

    // Start with a manifest holding ONLY the owned repo.
    let owned = ws.join(OWNED_REPO);
    init_repo_with_commit(&owned, "OWNED", "owned-init");

    let project_dir = ws.join("projects").join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();
    let manifest_owned_only = format!(
        r#"repositories:
  {owned_path}:
    type: git
    url: file://{owned}
    version: main
    role: owned
"#,
        owned_path = OWNED_REPO,
        owned = owned.display(),
    );
    std::fs::write(project_dir.join("rwv.yaml"), &manifest_owned_only).unwrap();

    // Simulate `rwv add <url> --role reference`: clone to the weave root and
    // append a reference entry to the manifest. No workweave exists yet.
    let reference = ws.join(REF_REPO);
    init_repo_with_commit(&reference, "REF", "reference-init");
    let manifest_with_ref = format!(
        "{manifest_owned_only}  {ref_path}:\n    type: git\n    url: file://{reference}\n    version: main\n    role: reference\n",
        ref_path = REF_REPO,
        reference = reference.display(),
    );
    std::fs::write(project_dir.join("rwv.yaml"), &manifest_with_ref).unwrap();

    // NOW create a workweave — the reference added before create must be
    // materialized as a symlink.
    let ww = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("after-add").unwrap(),
        false,
        false,
        false,
        None,
    )
    .expect("create after add should succeed");

    let ref_checkout = ww.join(REF_REPO);
    assert!(
        ref_checkout.is_symlink(),
        "a reference repo added before create must be symlinked, not worktree'd"
    );
    assert_eq!(
        std::fs::read_link(&ref_checkout).unwrap(),
        ws.join(REF_REPO),
        "the post-add reference symlink targets the canonical at the weave root"
    );
}

// ---------------------------------------------------------------------------
// idempotent reuse with a symlinked reference returns clean
// ---------------------------------------------------------------------------

#[test]
fn idempotent_reuse_with_symlinked_reference_returns_clean() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_reference(tmp.path(), "proj");
    let canonical = ws.join(REF_REPO);

    let first = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("feat").unwrap(),
        false,
        false,
        false,
        None,
    )
    .expect("first create");
    assert!(first.join(REF_REPO).is_symlink());

    // Dirty the canonical: an alias has no per-workweave dirty state, so this
    // must NOT make reuse refuse.
    std::fs::write(canonical.join("DIRTY"), "x").unwrap();

    // Re-invoke create without --replace-existing: the idempotent reuse path must
    // validate the marker and return the existing dir without error.
    let second = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("feat").unwrap(),
        false, // not replace_existing
        false,
        false,
        None,
    )
    .expect("idempotent reuse must return clean even with a dirty canonical reference");

    assert_eq!(first, second, "reuse must return the same workweave dir");
    assert!(
        second.join(REF_REPO).is_symlink(),
        "reference must still be a symlink after reuse"
    );
}
