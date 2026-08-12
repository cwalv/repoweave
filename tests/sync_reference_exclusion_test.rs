//! E2E coverage for the sync reference-exclusion.
//!
//! A `role: reference` repo is materialized as a SYMLINK
//! (`CheckoutKind::ReferenceAlias`) aliasing the single canonical weave-root
//! clone shared by every workweave. Sync's per-repo phase machine
//! (savepoint → replay → advance-target → abort, plus materialize/prune) would
//! otherwise operate THROUGH the symlink onto that shared store — writing
//! `refs/rwv/pre-op/*` into it, rebasing/ff'ing its branch, `reset --hard`-ing
//! it on abort. The fix excludes `ReferenceAlias` checkouts at a single
//! chokepoint (`checkout_is_syncable`), so the canonical store is unreachable
//! from every mutating phase BY CONSTRUCTION.
//!
//! These tests pin the BEHAVIORAL contract (not the implementation):
//!
//!   - `sync` is a no-op for a symlinked reference: the canonical's HEAD, refs
//!     (NO `refs/rwv/pre-op/*` written), working tree, and dirty state are
//!     BYTE-FOR-BYTE unchanged, while the owned repo still syncs.
//!   - `sync-to` is likewise a no-op for the reference; the owned repo advances.
//!   - two workweaves each running `sync-to` that share one reference: neither
//!     writes op refs into the shared canonical; no collision.
//!   - `abort` after a `sync-to` never resets the canonical reference clone.
//!   - `sync-to --retire` with a symlinked reference present: retire's
//!     merged/dirty checks ignore the reference; the symlink is unlinked and
//!     the canonical survives byte-for-byte.
//!   - REGRESSION (alias-keyed, not role-keyed): a `--worktree-references`
//!     reference repo is a real worktree and participates in sync NORMALLY.

use assert_cmd::Command as AssertCommand;
use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Git + rwv helpers
// ---------------------------------------------------------------------------

fn git(args: &[&str], dir: &Path) {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git command failed to start");
    assert!(
        out.status.success(),
        "git {:?} in {} failed:\n{}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_out(args: &[&str], dir: &Path) -> String {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git command failed to start");
    assert!(
        out.status.success(),
        "git {:?} in {} failed:\n{}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn rwv() -> AssertCommand {
    common::rwv()
}

/// Init a git repo at `path` with one commit on `main`. Returns HEAD SHA.
fn init_repo(path: &Path, file: &str, contents: &str) -> String {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "-b", "main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join(file), contents).unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
    git_out(&["rev-parse", "HEAD"], path)
}

/// Stage and commit `filename` (relative to `repo`). Returns new HEAD SHA.
fn commit_file(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    let path = repo.join(filename);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
    git(&["add", filename], repo);
    git(&["commit", "-m", msg], repo);
    git_out(&["rev-parse", "HEAD"], repo)
}

const OWNED_PATH: &str = "github/org/owned";
const REF_PATH: &str = "github/org/reference";
const PROJECT: &str = "app";

/// Capture a byte-for-byte fingerprint of the canonical reference clone:
/// HEAD sha, the FULL ref list (including any `refs/rwv/pre-op/*`), the
/// working-tree file, porcelain status (dirty state), and the current branch.
/// Equality of this struct is the "canonical untouched" assertion.
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
        head: git_out(&["rev-parse", "HEAD"], canonical),
        // `for-each-ref` over the full namespace catches a stray
        // `refs/rwv/pre-op/*` that a savepoint would write into the canonical.
        refs: git_out(
            &["for-each-ref", "--format=%(refname) %(objectname)"],
            canonical,
        ),
        worktree_file: std::fs::read_to_string(canonical.join("REF")).unwrap_or_default(),
        status: git_out(&["status", "--porcelain"], canonical),
        current_branch: git_out(&["symbolic-ref", "--short", "HEAD"], canonical),
    }
}

/// Assert NO `refs/rwv/pre-op/*` savepoint refs exist in `repo` — the
/// load-bearing "no op-ref was written into the shared canonical" check.
fn assert_no_pre_op_refs(repo: &Path, ctx: &str) {
    let refs = git_out(
        &["for-each-ref", "--format=%(refname)", "refs/rwv/pre-op/"],
        repo,
    );
    assert!(
        refs.is_empty(),
        "{ctx}: canonical reference clone must have NO refs/rwv/pre-op/* refs, found:\n{refs}"
    );
}

// ---------------------------------------------------------------------------
// Primary workspace fixture: one owned repo + one reference repo.
// ---------------------------------------------------------------------------

struct Primary {
    root: PathBuf,
    owned_canonical: PathBuf,
    ref_canonical: PathBuf,
}

/// Build a primary workspace whose committed manifest+lock list an owned repo
/// (role: owned) and a reference repo (role: reference).
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
    // `rwv init` writes this so sync's native rebase keeps source's rwv.lock.
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();

    let manifest = format!(
        "[repositories.\"{owned_path}\"]\ntype = \"git\"\nurl = \"file://{owned}\"\nversion = \"main\"\nrole = \"owned\"\n\n[repositories.\"{ref_path}\"]\ntype = \"git\"\nurl = \"file://{reference}\"\nversion = \"main\"\nrole = \"reference\"\n",
        owned_path = OWNED_PATH,
        owned = owned.display(),
        ref_path = REF_PATH,
        reference = reference.display(),
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    // Round-trips through the real parser + `lock::write_lock`: a
    // hand-formatted string that differs only in whitespace from what
    // `rwv lock` itself would emit still diffs against a real relock.
    let owned_url = format!("file://{}", owned.display());
    let reference_url = format!("file://{}", reference.display());
    let raw_lock = format!(
        "{{\"repositories\": {{{owned_path:?}: {{\"type\": \"git\", \"url\": {owned_url:?}, \"version\": {owned_sha:?}}}, {ref_path:?}: {{\"type\": \"git\", \"url\": {reference_url:?}, \"version\": {ref_sha:?}}}}}}}",
        owned_path = OWNED_PATH,
        ref_path = REF_PATH,
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();

    git(
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
        &project_dir,
    );
    git(&["commit", "-m", "lock: initial"], &project_dir);
    std::fs::write(ws.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    let _ = project_dir;
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

/// Create a workweave via `rwv workweave <project> create <name>`.
///
/// `worktree_references = false` symlinks the reference repo (the default,
/// alias path); `true` exercises the `--worktree-references` escape hatch.
fn create_workweave(
    primary: &Primary,
    weaveroot: &Path,
    name: &str,
    worktree_references: bool,
) -> Workweave {
    let mut args = vec![
        "workweave".to_string(),
        PROJECT.to_string(),
        "create".to_string(),
        name.to_string(),
    ];
    if worktree_references {
        args.push("--worktree-references".to_string());
    }
    rwv()
        .args(&args)
        .current_dir(&primary.root)
        .assert()
        .success();

    let root = weaveroot.join(format!("{PROJECT}--{name}"));
    Workweave {
        owned_checkout: root.join(OWNED_PATH),
        ref_checkout: root.join(REF_PATH),
        root,
    }
}

/// Run `rwv lock --commit` from a workspace root.
fn rwv_lock_commit(workspace_root: &Path) {
    rwv()
        .args(["lock", "--commit"])
        .current_dir(workspace_root)
        .assert()
        .success();
}

/// Plant an owner record (`.rwv-op`) at `workspace` so `rwv abort` enters its
/// per-repo restore loop. Mirrors the JSON shape `abort_hardening_test.rs`
/// plants by hand. `source`/`target` both point at `workspace` (plain-sync
/// shape), so abort restores only this workspace's repos.
fn plant_owner_record(workspace: &Path, op_id: &str, phase: &str) {
    let json = format!(
        "{{\"id\": \"{op_id}\", \"verb\": \"sync\", \"strategy\": \"rebase\", \
         \"source\": \"{root}\", \"target\": \"{root}\", \"retire\": false, \"phase\": \"{phase}\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \"overrides\": [], \
         \"started_at\": \"2026-05-27T10:00:00Z\"}}",
        root = workspace.display(),
    );
    std::fs::write(workspace.join(".rwv-op"), &json).unwrap();
}

/// Create a `refs/rwv/pre-op/<op-id>` savepoint pointing at `sha` in `repo`.
fn plant_savepoint(repo: &Path, op_id: &str, sha: &str) {
    git(
        &["update-ref", &format!("refs/rwv/pre-op/{op_id}"), sha],
        repo,
    );
}

// ---------------------------------------------------------------------------
// sync (pull) is a no-op for a symlinked reference; owned still syncs.
// ---------------------------------------------------------------------------

#[test]
fn sync_is_a_no_op_for_a_symlinked_reference_and_leaves_canonical_untouched() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let primary = make_primary(tmp.path());
    let ww = create_workweave(&primary, &weaveroot, "feat", false);

    // The reference is a symlink; the owned repo is a real worktree.
    assert!(
        ww.ref_checkout.is_symlink(),
        "reference repo must be a symlink in the workweave"
    );
    assert!(
        !ww.owned_checkout.is_symlink(),
        "owned repo must be a real worktree"
    );

    // Workweave advances the OWNED repo and locks (so sync has real work to do
    // on the owned side — proving the no-op is reference-specific).
    let owned_sha = commit_file(
        &ww.owned_checkout,
        "foo.txt",
        "ww foo\n",
        "ww: advance owned",
    );
    rwv_lock_commit(&ww.root);

    // Fingerprint the canonical reference clone BEFORE the sync.
    let before = fingerprint(&primary.ref_canonical);
    assert_no_pre_op_refs(&primary.ref_canonical, "before sync");

    // From primary: sync the workweave (default ff). This pulls the owned
    // advance and MUST NOT touch the reference's canonical store.
    rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();

    // Owned repo advanced on primary (proves sync ran for non-reference repos).
    let primary_owned_head = git_out(&["rev-parse", "main"], &primary.owned_canonical);
    assert_eq!(
        primary_owned_head, owned_sha,
        "owned repo must advance on primary after sync"
    );

    // Canonical reference clone is BYTE-FOR-BYTE unchanged, and no savepoint
    // ref was written into it.
    let after = fingerprint(&primary.ref_canonical);
    assert_eq!(
        before, after,
        "canonical reference clone must be byte-for-byte unchanged after sync"
    );
    assert_no_pre_op_refs(&primary.ref_canonical, "after sync");
}

// ---------------------------------------------------------------------------
// sync-to (push) is a no-op for a symlinked reference; owned still advances.
// ---------------------------------------------------------------------------

#[test]
fn sync_to_is_a_no_op_for_a_symlinked_reference_and_leaves_canonical_untouched() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let primary = make_primary(tmp.path());
    let ww = create_workweave(&primary, &weaveroot, "feat", false);
    assert!(ww.ref_checkout.is_symlink());

    // Workweave advances the owned repo + locks.
    let owned_sha = commit_file(
        &ww.owned_checkout,
        "foo.txt",
        "ww foo\n",
        "ww: advance owned",
    );
    rwv_lock_commit(&ww.root);

    let before = fingerprint(&primary.ref_canonical);
    assert_no_pre_op_refs(&primary.ref_canonical, "before sync-to");

    // From the workweave: sync-to primary. advance-target ff's primary's owned
    // repo; the reference must be excluded (no ff of the shared canonical).
    rwv()
        .args([
            "sync-to",
            &primary.root.to_string_lossy(),
            "--strategy=rebase",
        ])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Owned advanced on primary.
    let primary_owned_head = git_out(&["rev-parse", "main"], &primary.owned_canonical);
    assert_eq!(
        primary_owned_head, owned_sha,
        "owned repo must advance on primary after sync-to"
    );

    // Reference canonical untouched; no pre-op refs.
    let after = fingerprint(&primary.ref_canonical);
    assert_eq!(
        before, after,
        "canonical reference clone must be byte-for-byte unchanged after sync-to"
    );
    assert_no_pre_op_refs(&primary.ref_canonical, "after sync-to");
}

// ---------------------------------------------------------------------------
// Two workweaves each running sync-to share one reference: no op-ref collision.
// ---------------------------------------------------------------------------

#[test]
fn two_workweaves_sync_to_share_a_reference_without_colliding_on_op_refs() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let primary = make_primary(tmp.path());
    let wa = create_workweave(&primary, &weaveroot, "wa", false);
    let wb = create_workweave(&primary, &weaveroot, "wb", false);

    // Both share the same canonical reference store.
    assert_eq!(
        wa.ref_checkout.canonicalize().unwrap(),
        primary.ref_canonical.canonicalize().unwrap(),
    );
    assert_eq!(
        wb.ref_checkout.canonicalize().unwrap(),
        primary.ref_canonical.canonicalize().unwrap(),
    );

    let before = fingerprint(&primary.ref_canonical);

    // Each workweave advances the owned repo on an independent file + locks,
    // then sync-to's to primary.
    commit_file(&wa.owned_checkout, "a.txt", "a\n", "wa: advance");
    rwv_lock_commit(&wa.root);
    rwv()
        .args([
            "sync-to",
            &primary.root.to_string_lossy(),
            "--strategy=rebase",
        ])
        .current_dir(&wa.root)
        .assert()
        .success();

    // wb syncs FROM primary first to pick up wa's owned advance (so its later
    // sync-to is a clean ff), then advances + sync-to's.
    rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&wb.root)
        .assert()
        .success();
    commit_file(&wb.owned_checkout, "b.txt", "b\n", "wb: advance");
    rwv_lock_commit(&wb.root);
    rwv()
        .args([
            "sync-to",
            &primary.root.to_string_lossy(),
            "--strategy=rebase",
        ])
        .current_dir(&wb.root)
        .assert()
        .success();

    // Neither workweave's sync nor sync-to wrote an op ref into the shared
    // canonical reference store, and it is byte-for-byte unchanged.
    assert_no_pre_op_refs(&primary.ref_canonical, "after both workweaves sync-to");
    let after = fingerprint(&primary.ref_canonical);
    assert_eq!(
        before, after,
        "shared canonical reference must be untouched after two workweaves sync-to"
    );
}

// ---------------------------------------------------------------------------
// abort after a sync-to never resets the canonical reference clone.
// ---------------------------------------------------------------------------

#[test]
fn abort_does_not_reset_the_canonical_reference_even_with_a_planted_savepoint() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let primary = make_primary(tmp.path());
    let ww = create_workweave(&primary, &weaveroot, "feat", false);
    assert!(ww.ref_checkout.is_symlink());

    let op_id = "1700000000000000000";

    // Adversarial worst case: a savepoint exists in the canonical reference
    // clone (planted at its current tip), exactly as a non-excluded savepoint
    // phase would have created. If abort wrongly operated on the reference
    // (through the workweave's symlink), `abort_one_repo` would — BEFORE any
    // HEAD-verification — write a durable `refs/rwv/pre-abort/<op-id>` ref into
    // the shared canonical store (a cross-workweave ref write), and could then
    // `reset --hard` it. The exclusion makes `abort_one_repo` unreachable for
    // the reference, so neither happens.
    let ref_tip = git_out(&["rev-parse", "main"], &primary.ref_canonical);
    plant_savepoint(&primary.ref_canonical, op_id, &ref_tip);

    // Sanity: the canonical has the planted pre-op savepoint but NO pre-abort
    // ref yet.
    let pre_abort = git_out(
        &["for-each-ref", "--format=%(refname)", "refs/rwv/pre-abort/"],
        &primary.ref_canonical,
    );
    assert!(pre_abort.is_empty(), "precondition: no pre-abort ref yet");

    // Plant an in-progress op in the WORKWEAVE so `rwv abort` enters its
    // per-repo restore loop, which iterates the manifest (owned + reference).
    // `replay` is a valid recorded phase (abort restores regardless of phase).
    plant_owner_record(&ww.root, op_id, "replay");

    // Run abort from the workweave. abort's CWD-manifest restore loop visits
    // both repos; the reference (a symlink → ReferenceAlias) must be SKIPPED
    // by the chokepoint, so `abort_one_repo` is never invoked for it.
    let _ = rwv().args(["abort"]).current_dir(&ww.root).assert();

    // LOAD-BEARING: abort wrote NO `refs/rwv/pre-abort/*` ref into the shared
    // canonical store. (Without the exclusion, `abort_one_repo`'s Rail-1
    // pre-abort ref would be written here, BEFORE the HEAD-verification rail
    // even runs — so this assertion isolates the chokepoint, not the verifier.)
    let pre_abort_after = git_out(
        &["for-each-ref", "--format=%(refname)", "refs/rwv/pre-abort/"],
        &primary.ref_canonical,
    );
    assert!(
        pre_abort_after.is_empty(),
        "abort must NOT write a refs/rwv/pre-abort/* ref into the shared canonical reference; \
         found:\n{pre_abort_after}"
    );

    // And the canonical reference's `main` is unchanged.
    let ref_after = git_out(&["rev-parse", "main"], &primary.ref_canonical);
    assert_eq!(
        ref_after, ref_tip,
        "abort must not move the shared canonical reference's branch"
    );
}

// ---------------------------------------------------------------------------
// sync-to --retire end-to-end with a symlinked reference present.
// ---------------------------------------------------------------------------

#[test]
fn sync_to_retire_with_a_symlinked_reference_unlinks_it_and_leaves_canonical_intact() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let primary = make_primary(tmp.path());
    let ww = create_workweave(&primary, &weaveroot, "feat", false);
    assert!(ww.ref_checkout.is_symlink());

    // Workweave advances owned + locks so retire's merged-check has converged
    // owned tips to compare; the reference must be ignored by that check.
    let owned_sha = commit_file(
        &ww.owned_checkout,
        "foo.txt",
        "ww foo\n",
        "ww: advance owned",
    );
    rwv_lock_commit(&ww.root);

    let before = fingerprint(&primary.ref_canonical);

    // sync-to --retire: lands owned upward, then deletes the workweave. The
    // retire merged/dirty checks must ignore the reference (whose canonical
    // could even be dirty), and the symlink is unlinked (delegated to .1's
    // delete behavior), never touching the canonical store.
    rwv()
        .args([
            "sync-to",
            &primary.root.to_string_lossy(),
            "--strategy=rebase",
            "--retire",
        ])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Workweave is gone (retired), and with it the reference symlink.
    assert!(!ww.root.exists(), "workweave must be deleted by --retire");

    // Owned landed upward.
    let primary_owned_head = git_out(&["rev-parse", "main"], &primary.owned_canonical);
    assert_eq!(
        primary_owned_head, owned_sha,
        "owned repo must land on primary after sync-to --retire"
    );

    // The canonical reference clone survives byte-for-byte and carries no op
    // refs.
    assert!(
        primary.ref_canonical.exists(),
        "canonical reference clone must survive retire"
    );
    let after = fingerprint(&primary.ref_canonical);
    assert_eq!(
        before, after,
        "canonical reference clone must be byte-for-byte unchanged after retire"
    );
    assert_no_pre_op_refs(&primary.ref_canonical, "after retire");
}

// ---------------------------------------------------------------------------
// REGRESSION: --worktree-references reference participates in sync NORMALLY.
//
// This is the load-bearing guard that the exclusion is keyed on ALIAS-NESS
// (`CheckoutKind::ReferenceAlias`), NOT on `role == Reference`. A reference
// repo created with `--worktree-references` is a real worktree on its own
// ephemeral branch; sync only ever moves THAT branch (never the canonical's
// shared `main`), so it is safe and MUST still sync.
// ---------------------------------------------------------------------------

#[test]
fn worktree_references_reference_syncs_normally() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let primary = make_primary(tmp.path());
    // Escape hatch: cut a real worktree for the reference repo.
    let ww = create_workweave(&primary, &weaveroot, "feat", true);

    // With --worktree-references the reference is a REAL worktree, not a symlink.
    assert!(
        !ww.ref_checkout.is_symlink(),
        "with --worktree-references the reference must be a real worktree"
    );
    assert!(
        ww.ref_checkout.join(".git").is_file(),
        "worktree'd reference .git must be a worktree gitlink FILE"
    );
    // It is on its OWN ephemeral branch in the workweave (legacy behavior),
    // flat with no segmented third component.
    let ww_ref_branch = git_out(&["symbolic-ref", "--short", "HEAD"], &ww.ref_checkout);
    assert_eq!(
        ww_ref_branch,
        format!("{PROJECT}--feat"),
        "worktree'd reference must be on its own ephemeral branch in the workweave"
    );

    // Advance the worktree'd reference on its ephemeral branch + lock. Because
    // it is a real worktree, this is a genuine per-workweave commit that sync
    // must propagate (unlike a symlink alias, which has no per-workweave state).
    let ref_sha = commit_file(
        &ww.ref_checkout,
        "newref.txt",
        "from-ww\n",
        "ww: advance reference",
    );
    let owned_sha = commit_file(
        &ww.owned_checkout,
        "foo.txt",
        "ww foo\n",
        "ww: advance owned",
    );
    rwv_lock_commit(&ww.root);

    // The canonical reference's `main` BEFORE sync-to (on the primary side, the
    // reference checkout IS the canonical, checked out on `main`).
    let canonical_ref_before = git_out(&["rev-parse", "main"], &primary.ref_canonical);
    assert_ne!(
        canonical_ref_before, ref_sha,
        "precondition: the canonical reference must not yet have the workweave's commit"
    );

    // sync-to: the worktree'd reference must advance NORMALLY (advance-target
    // ff's the TARGET's reference checkout to the workweave's tip), exactly
    // like the owned repo. This is the proof the exclusion is alias-keyed
    // (`CheckoutKind::ReferenceAlias`), NOT role-keyed: a worktree'd reference
    // is a `Worktree`, so it is NOT excluded and syncs.
    rwv()
        .args([
            "sync-to",
            &primary.root.to_string_lossy(),
            "--strategy=rebase",
        ])
        .current_dir(&ww.root)
        .assert()
        .success();

    // The owned repo advanced (sanity).
    assert_eq!(
        git_out(&["rev-parse", "main"], &primary.owned_canonical),
        owned_sha,
        "owned repo must advance on primary"
    );

    // The reference advanced on the primary (target) side to the workweave's
    // new reference commit — proving the worktree'd reference SYNCED. Had the
    // exclusion been role-keyed, this branch would NOT have moved.
    let canonical_ref_after = git_out(&["rev-parse", "main"], &primary.ref_canonical);
    assert_eq!(
        canonical_ref_after, ref_sha,
        "worktree'd reference must advance on sync-to (alias-keyed exclusion, not role-keyed): \
         it is a Worktree, so it syncs exactly like an owned repo"
    );
}
