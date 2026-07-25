//! Tests for branch-discipline checks (`rwv doctor`, fo-hycb06.2).
//!
//! Enforces the I3 invariant from `docs/explanation/joints/clone-topology.md`
//! (every workweave repo checkout sits on its owned
//! `<project>--<workweave>/<segment>` ephemeral branch; canonicals sit on a
//! non-ephemeral branch) plus the safe/live doctrine from
//! `docs/explanation/joints/shared-refs-drift.md` applied to refs in (c).
//!
//! Three checks:
//!
//!   (a) workweave-branch — `shared-branch`, `foreign-ephemeral`, `detached`
//!   (b) the `branch-model.md` §7.2 canonical-store arms —
//!       `canonical-holds-live-workweave-ref`, `canonical-holds-leaked-ref`,
//!       `canonical-detached`
//!   (c) stale-ephemeral-branches — `safe` (auto-fixable) / `live` (never) /
//!       `unowned` (never — rwv holds no receipt)
//!
//! Healthy fixtures (workweave on its own ephemeral branch, canonical on
//! `main`, ephemeral branch whose workweave still exists) must stay clean.
//!
//! **Ownership is by record.** Everything in (b), and the safe/live half of
//! (c), keys on an ownership receipt (`branch-model.md` R2), not on the
//! branch's name. So most fixtures below record a receipt explicitly — see
//! [`record_receipt`] for why a receipt for `<p>--<a>/<b>` is minted from
//! the workweave name `<a>/<b>`. A fixture that skips the receipt is
//! asserting the *other* half: a branch that merely looks like rwv's is the
//! operator's, and `--fix` must leave it alone.
//!
//! Fixture rationale: branch-discipline operates on real git repos, so the
//! workspaces here include actual git checkouts (not just directory shells
//! like the tree-integrity tests).

use repoweave::git::GitVcs;
use repoweave::manifest::{ProjectName, WorkweaveName};
use repoweave::vcs::{EphemeralRefName, Vcs};
use repoweave::workweave_index::RefRegistry;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

// ---------------------------------------------------------------------------
// Helpers: build a primary + workweave with real git checkouts on demand.
// ---------------------------------------------------------------------------

/// Create a minimal primary workspace with a `github/` registry dir and a
/// `projects/` directory. Returns the workspace root.
fn make_primary(parent: &Path) -> PathBuf {
    let ws = parent.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    ws
}

/// Return the `.workweaves/` parent directory for `ws_root`.
fn workweaves_dir(ws_root: &Path) -> PathBuf {
    ws_root
        .parent()
        .expect("ws_root has a parent")
        .join(".workweaves")
}

/// Write a well-formed `.rwv-workweave` marker file into `ww_dir`.
fn write_marker(ww_dir: &Path, primary: &Path, project: &str, parent: &Path) {
    std::fs::create_dir_all(ww_dir).unwrap();
    let primary_str = primary
        .canonicalize()
        .unwrap_or_else(|_| primary.to_path_buf());
    let parent_str = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    let content = format!(
        "primary: {}\nproject: {}\nparent: {}\n",
        primary_str.display(),
        project,
        parent_str.display()
    );
    std::fs::write(ww_dir.join(".rwv-workweave"), content).unwrap();
}

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn git() -> Command {
    common::git()
}

/// Run a git subcommand in `cwd` and assert success. Strip inherited `GIT_*`
/// env (see `tests/common/mod.rs` for context).
fn git_in(cwd: &Path, args: &[&str]) {
    let out = git()
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} in {} failed to spawn: {e}", cwd.display()));
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Initialize a git repo at `path` with a single commit on `main`. Returns
/// the path so the caller can chain.
fn init_repo_with_commit(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).unwrap();
    git_in(path, &["init", "--initial-branch=main", "-q"]);
    git_in(path, &["config", "user.email", "test@test"]);
    git_in(path, &["config", "user.name", "Test"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git_in(path, &["add", "README.md"]);
    git_in(path, &["commit", "-q", "-m", "init"]);
    path.to_path_buf()
}

/// Add a worktree from canonical `repo` at `worktree_path`, on a new branch
/// `branch_name` starting from the canonical's current HEAD. Returns the
/// worktree path.
fn worktree_add(repo: &Path, worktree_path: &Path, branch_name: &str) -> PathBuf {
    git_in(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            branch_name,
            worktree_path.to_str().unwrap(),
        ],
    );
    worktree_path.to_path_buf()
}

/// Add a worktree from canonical `repo` at `worktree_path`, on the *existing*
/// branch `branch_name` (no `-b`). Used to fixture shared-branch and
/// foreign-ephemeral cases.
fn worktree_add_existing(repo: &Path, worktree_path: &Path, branch_name: &str) {
    git_in(
        repo,
        &[
            "worktree",
            "add",
            worktree_path.to_str().unwrap(),
            branch_name,
        ],
    );
}

/// Add a detached worktree from canonical `repo` at `worktree_path` pointing
/// at HEAD. Used to fixture the detached case.
fn worktree_add_detached(repo: &Path, worktree_path: &Path) {
    git_in(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            worktree_path.to_str().unwrap(),
            "HEAD",
        ],
    );
}

/// Append a commit on the currently checked-out branch in `repo`.
fn add_commit(repo: &Path, fname: &str, msg: &str) {
    std::fs::write(repo.join(fname), format!("{msg}\n")).unwrap();
    git_in(repo, &["add", fname]);
    git_in(repo, &["commit", "-q", "-m", msg]);
}

/// Create a fresh local branch in `repo` pointing at `start_point` (a SHA
/// or another branch name) without switching to it.
fn create_branch(repo: &Path, name: &str, start_point: &str) {
    git_in(repo, &["branch", name, start_point]);
}

/// Record an ownership receipt (`branch-model.md` §4.2) for the branch that
/// `(project, workweave)` mints, in `store`.
///
/// This is how rwv's own create path claims a ref, and after R2 it is the
/// *only* thing that makes a ref rwv's to destroy — so a fixture that wants
/// doctor to treat a branch as rwv's has to call this.
///
/// **Why the workweave name looks odd in some callers.**
/// [`EphemeralRefName::mint`] is total on `(project, workweave)` and yields
/// `<project>--<workweave>`; there is no other route to a recordable name,
/// because recording an *observed* name would mint the receipt that
/// authorizes destroying it. So a receipt for a branch spelled
/// `<project>--<a>/<b>` — the shape the (c) scanner still discovers, until
/// §7.1's flat-name cutover lands — is minted from the workweave name
/// `<a>/<b>`. `mint` deliberately does not validate its components (Q12
/// leaves the legal grammar for names open), so that is a legal name today.
///
/// The receipt is recorded at the branch's current tip, and the branch must
/// already exist: recording against an absent ref would produce the dangling
/// state, which is a different fixture.
fn record_receipt(primary: &Path, project: &str, workweave: &str, store: &Path) {
    std::fs::create_dir_all(primary.join("projects").join(project)).unwrap();
    let project = ProjectName::new(project);
    let mut registry = RefRegistry::for_project(primary, &project);
    let name = EphemeralRefName::mint(&project, &WorkweaveName::new(workweave));
    let tip = GitVcs
        .resolve_local_branch_tip(store, &name.to_raw())
        .expect("store is readable")
        .unwrap_or_else(|| panic!("branch `{name}` must exist before recording a receipt for it"));
    registry
        .record_created(store, name, tip)
        .expect("receipt should record");
}

/// Record an ownership receipt for a ref that does **not** exist — the
/// dangling-receipt state §4.2 calls the benign crash residue.
fn record_dangling_receipt(primary: &Path, project: &str, workweave: &str, store: &Path) {
    std::fs::create_dir_all(primary.join("projects").join(project)).unwrap();
    let project = ProjectName::new(project);
    let mut registry = RefRegistry::for_project(primary, &project);
    let name = EphemeralRefName::mint(&project, &WorkweaveName::new(workweave));
    // Any resolvable revision works as the recorded tip: the receipt names a
    // ref that is not there, so nothing ever compares against it.
    let head = GitVcs.head_revision(store).expect("store has a HEAD");
    registry
        .record_created(store, name, head)
        .expect("receipt should record");
}

/// Whether `project`'s registry holds a receipt named `ref_name`.
///
/// Reads the `receipts` array specifically rather than substring-matching
/// the index file: the index also records each workweave's absolute
/// **path**, which ends in `<project>--<workweave>` — so a substring test
/// for a receipt name passes on the placement entry alone, and an assertion
/// that a receipt survived would hold with every receipt retracted.
fn receipt_recorded(primary: &Path, project: &str, ref_name: &str) -> bool {
    let path = primary
        .join("projects")
        .join(project)
        .join(".rwv-workweave-index");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return false;
    };
    let index: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("index at {} is not JSON: {e}", path.display()));
    index["receipts"]
        .as_array()
        .map(|rs| rs.iter().any(|r| r["name"] == ref_name))
        .unwrap_or(false)
}

/// Whether a local branch exists in `repo`.
fn branch_exists(repo: &Path, name: &str) -> bool {
    !String::from_utf8_lossy(
        &git()
            .args(["branch", "--list", name])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .is_empty()
}

// ===========================================================================
// (a) workweave-branch
// ===========================================================================

/// Healthy workweave: each repo checkout sits on its
/// `<project>--<workweave>/<segment>` ephemeral branch. Doctor should not
/// report any branch-discipline finding for this directory.
#[test]
fn healthy_workweave_ephemeral_branch_is_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    let ww_dir = workweaves_dir(&ws).join("myproj--feat-a");
    write_marker(&ww_dir, &ws, "myproj", &ws);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    worktree_add(&canonical, &ww_checkout, "myproj--feat-a/main");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("workweave checkout is on")
            && !stdout.contains("detached-HEAD")
            && !stdout.contains("foreign-ephemeral"),
        "healthy workweave on its ephemeral branch should be clean; got:\n{stdout}"
    );
}

/// shared-branch sub-kind: workweave repo checkout on `main` (the canonical's
/// tracking branch). This is the bare-main-in-workweave case from the
/// acceptance criteria — must flag from creation, before any commit lands.
#[test]
fn shared_branch_main_in_workweave_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // Move the canonical off `main` so the workweave can check it out.
    git_in(&canonical, &["checkout", "-b", "rwv-primary-tip", "-q"]);

    let ww_dir = workweaves_dir(&ws).join("myproj--feat-a");
    write_marker(&ww_dir, &ws, "myproj", &ws);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    // The spec's bare-main case: workweave checkout sits on `main`, no
    // commits beyond the canonical's first commit.
    worktree_add_existing(&canonical, &ww_checkout, "main");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("shared-branch")
            || stdout.contains("workweave checkout is on shared-branch"),
        "doctor should report shared-branch sub-kind for bare-main-in-workweave; got:\n{stdout}"
    );
    assert!(
        stdout.contains("main"),
        "report should name the offending branch (main); got:\n{stdout}"
    );
}

/// foreign-ephemeral sub-kind: workweave checkout on `<project>--<other>/...`,
/// naming a different workweave's branch.
#[test]
fn foreign_ephemeral_branch_in_workweave_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    let ww_dir = workweaves_dir(&ws).join("myproj--feat-a");
    write_marker(&ww_dir, &ws, "myproj", &ws);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    // Check out on a foreign workweave's ephemeral branch.
    worktree_add(&canonical, &ww_checkout, "myproj--feat-b/main");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("foreign-ephemeral") || stdout.contains("names a different workweave"),
        "doctor should report foreign-ephemeral sub-kind; got:\n{stdout}"
    );
    assert!(
        stdout.contains("myproj--feat-b/main"),
        "report should name the offending branch; got:\n{stdout}"
    );
}

/// detached sub-kind: workweave checkout in detached-HEAD state.
#[test]
fn detached_head_in_workweave_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    let ww_dir = workweaves_dir(&ws).join("myproj--feat-a");
    write_marker(&ww_dir, &ws, "myproj", &ws);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    worktree_add_detached(&canonical, &ww_checkout);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("detached-HEAD") || stdout.contains("detached"),
        "doctor should report detached-HEAD sub-kind; got:\n{stdout}"
    );
}

// ===========================================================================
// (b) the §7.2 canonical-store arms
// ===========================================================================

/// Healthy canonical: checked out on a non-ephemeral branch (`main`).
/// No branch-discipline finding expected.
#[test]
fn healthy_canonical_on_main_is_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("canonical store is checked out on"),
        "canonical on main should not fire a §7.2 attachment arm; got:\n{stdout}"
    );
}

/// §7.2 arm 1, and the [S] scenario this bead inverts: a canonical sitting on
/// a **hand-made** `<a>--<b>/<c>` branch is on an operator branch, not on one
/// of rwv's. Name shape is not ownership (R2), so doctor leaves it alone.
///
/// The shipped scan reported this as `ephemeral-at-primary` purely because
/// the name parsed. Non-vacuity: the companion test below builds the same
/// fixture *with* a receipt and asserts the finding does fire, so a scan that
/// simply stopped looking at canonicals cannot make both pass.
#[test]
fn handmade_lookalike_at_canonical_is_not_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // Switch the canonical onto an ephemeral-*shaped* branch. No receipt:
    // rwv never created this ref.
    git_in(&canonical, &["checkout", "-b", "myproj--feat-a/main", "-q"]);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("canonical store is checked out on"),
        "a hand-made lookalike is operator state (§7.2 arm 1) and must not fire an \
         attachment finding; got:\n{stdout}"
    );
}

/// §7.2 arm 3: the canonical is attached to a ref rwv **recorded** for a
/// workweave that is gone — a leak.
///
/// `--fix` cannot reclaim it while this store's own HEAD is on it (git
/// refuses to delete a branch a worktree uses), so the finding names the
/// `git switch` that frees it and the ref survives the run.
#[test]
fn canonical_holding_recorded_ref_of_deleted_workweave_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // A ref rwv created for workweave `feat-a`, recorded, and then left
    // behind when the workweave directory went away.
    create_branch(&canonical, "myproj--feat-a", "main");
    record_receipt(&ws, "myproj", "feat-a", &canonical);
    git_in(&canonical, &["checkout", "myproj--feat-a", "-q"]);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("canonical store is checked out on `myproj--feat-a`"),
        "doctor should report the leaked recorded ref; got:\n{stdout}"
    );
    assert!(
        stdout.contains("whose workweave is gone"),
        "the report should say the workweave is gone (arm 3, not arm 2); got:\n{stdout}"
    );

    // `--fix` must not destroy the ref the store is standing on.
    let _ = rwv()
        .args(["doctor", "--fix", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(
        branch_exists(&canonical, "myproj--feat-a"),
        "the leaked ref must survive --fix while the store's HEAD is on it"
    );
}

/// §7.2 arm 2: the canonical is attached to a ref recorded for a workweave
/// that is **still on disk** — an I3 disjointness violation that only a
/// moved or copied directory can produce. Report-only.
#[test]
fn canonical_holding_recorded_ref_of_live_workweave_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    create_branch(&canonical, "myproj--feat-a", "main");
    record_receipt(&ws, "myproj", "feat-a", &canonical);
    git_in(&canonical, &["checkout", "myproj--feat-a", "-q"]);

    // The workweave directory is there — git could not have produced this
    // topology, so a directory was moved or copied.
    let ww_dir = workweaves_dir(&ws).join("myproj--feat-a");
    write_marker(&ww_dir, &ws, "myproj", &ws);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("canonical store is checked out on `myproj--feat-a`"),
        "doctor should report the live-workweave attachment; got:\n{stdout}"
    );
    assert!(
        stdout.contains("which is still on disk"),
        "the report should distinguish arm 2 from arm 3; got:\n{stdout}"
    );
}

// ===========================================================================
// §7.2's Detached arm — at the canonical, and at the project repo (§5.1)
// ===========================================================================

/// §7.2 arm 4: a detached canonical store is a finding. The shipped scan
/// read a collapsed `Option` and produced nothing here.
///
/// The fixture detaches at a commit that `main` does not point at, so
/// §7.2's reattach condition (counterpart tip == HEAD) is **false** and the
/// report says so — the honest-but-partial half of §6 item 2.
#[test]
fn detached_canonical_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    write_project_manifest(&ws, "myproj", "github/acme/repo");
    set_active_project(&ws, "myproj");

    let first = String::from_utf8_lossy(
        &git()
            .args(["rev-parse", "HEAD"])
            .current_dir(&canonical)
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    add_commit(&canonical, "second.txt", "second");
    // Detach at the *older* commit: `main` exists but points elsewhere.
    git_in(&canonical, &["checkout", "--detach", &first, "-q"]);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("canonical store is in detached-HEAD state"),
        "doctor should report the detached canonical; got:\n{stdout}"
    );
    assert!(
        stdout.contains("does not exist or points elsewhere"),
        "the report should say the reattach condition is not met; got:\n{stdout}"
    );

    // Even with the consent flag, this one must not be reattached: the
    // counterpart's tip differs from HEAD, so reattaching would move the
    // operator's working state onto a different commit.
    let _ = rwv()
        .args(["doctor", "--fix", "--reattach-checkouts"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let head_after = String::from_utf8_lossy(
        &git()
            .args(["symbolic-ref", "-q", "HEAD"])
            .current_dir(&canonical)
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    assert!(
        head_after.is_empty(),
        "HEAD must still be detached; symbolic-ref reported `{head_after}`"
    );
}

/// §7.2 arm 4's `--fix`: when the tracking counterpart exists and its tip
/// equals HEAD, `--fix --reattach-checkouts` reattaches.
///
/// Non-vacuity is pinned by the pair: the same fixture without the flag must
/// stay detached, so a `--fix` that reattached unconditionally fails the
/// first assertion and one that never reattached fails the second.
#[test]
fn detached_canonical_reattaches_only_with_consent() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    write_project_manifest(&ws, "myproj", "github/acme/repo");
    set_active_project(&ws, "myproj");

    // Detach at exactly `main`'s tip — §7.2's reattach condition holds.
    git_in(&canonical, &["checkout", "--detach", "main", "-q"]);

    let report = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let report_stdout = String::from_utf8_lossy(&report.stdout);
    assert!(
        report_stdout.contains("--reattach-checkouts` will \nreattach it")
            || report_stdout.contains("reattach-checkouts"),
        "the report should name the flag that would repair it; got:\n{report_stdout}"
    );

    // `--fix` WITHOUT the flag: report only, HEAD stays detached.
    let _ = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let still_detached = String::from_utf8_lossy(
        &git()
            .args(["symbolic-ref", "-q", "HEAD"])
            .current_dir(&canonical)
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    assert!(
        still_detached.is_empty(),
        "`--fix` without --reattach-checkouts must not change attachment; \
         symbolic-ref reported `{still_detached}`"
    );

    // WITH the flag: reattached to the counterpart.
    let fixed = rwv()
        .args(["doctor", "--fix", "--reattach-checkouts"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let fixed_stdout = String::from_utf8_lossy(&fixed.stdout);
    let now = String::from_utf8_lossy(
        &git()
            .args(["symbolic-ref", "-q", "HEAD"])
            .current_dir(&canonical)
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    assert_eq!(
        now, "refs/heads/main",
        "`--fix --reattach-checkouts` should reattach to the tracking counterpart; \
         doctor said:\n{fixed_stdout}"
    );
}

/// §5.1: `projects/<project>/` enters the branch-discipline scan.
///
/// Before this, `git checkout --detach` there yielded **zero** findings while
/// the same action on a member was a violation — the scope hole §5.1 closes.
/// The project repo is not a manifest member, so this also pins that the
/// project-scope filter does not silently drop it.
#[test]
fn detached_project_repo_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    write_project_manifest(&ws, "myproj", "github/acme/repo");
    set_active_project(&ws, "myproj");

    // The project repo is a real repo, and it is the thing being detached.
    let project_repo = ws.join("projects").join("myproj");
    init_repo_with_commit(&project_repo);
    git_in(&project_repo, &["add", "rwv.yaml"]);
    git_in(&project_repo, &["commit", "-q", "-m", "manifest"]);
    git_in(&project_repo, &["checkout", "--detach", "HEAD", "-q"]);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("canonical store is in detached-HEAD state"),
        "doctor should report the detached project repo; got:\n{stdout}"
    );
    assert!(
        stdout.contains("projects/myproj"),
        "the finding should name projects/<project>, not a member; got:\n{stdout}"
    );
}

/// An attached project repo produces no finding — the fixture above minus
/// the detach, so the assertion there cannot be passing on a scan that
/// reports every project repo unconditionally.
#[test]
fn attached_project_repo_is_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    write_project_manifest(&ws, "myproj", "github/acme/repo");
    set_active_project(&ws, "myproj");

    let project_repo = ws.join("projects").join("myproj");
    init_repo_with_commit(&project_repo);
    git_in(&project_repo, &["add", "rwv.yaml"]);
    git_in(&project_repo, &["commit", "-q", "-m", "manifest"]);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("detached-HEAD state"),
        "an attached project repo must be clean; got:\n{stdout}"
    );
}

// ===========================================================================
// Dangling ownership receipts (§4.2)
// ===========================================================================

/// A receipt whose ref never appeared is the benign residue of a crash
/// between the receipt write and the ref creation. Doctor reports it;
/// `--fix` retracts it.
#[test]
fn dangling_receipt_is_reported_and_retracted() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    write_project_manifest(&ws, "myproj", "github/acme/repo");
    set_active_project(&ws, "myproj");

    record_dangling_receipt(&ws, "myproj", "never-born", &canonical);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ownership receipt for `myproj--never-born`")
            && stdout.contains("no such ref is there"),
        "doctor should report the dangling receipt; got:\n{stdout}"
    );

    let fixed = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let fixed_stdout = String::from_utf8_lossy(&fixed.stdout);
    assert!(
        fixed_stdout.contains("[fixed]") && fixed_stdout.contains("myproj--never-born"),
        "--fix should announce the retraction; got:\n{fixed_stdout}"
    );

    let again = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let again_stdout = String::from_utf8_lossy(&again.stdout);
    assert!(
        !again_stdout.contains("myproj--never-born"),
        "the receipt should be gone after --fix; got:\n{again_stdout}"
    );
}

/// Dangling-receipt findings obey the same project scoping as every other
/// doctor finding: project-a active must not see — or retract — project-b's.
#[test]
fn dangling_receipt_is_scoped_to_active_project() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let repo_a = ws.join("github").join("acme").join("repo-a");
    let repo_b = ws.join("github").join("acme").join("repo-b");
    init_repo_with_commit(&repo_a);
    init_repo_with_commit(&repo_b);
    write_project_manifest(&ws, "project-a", "github/acme/repo-a");
    write_project_manifest(&ws, "project-b", "github/acme/repo-b");
    set_active_project(&ws, "project-a");

    record_dangling_receipt(&ws, "project-b", "ghost", &repo_b);

    let scoped = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let scoped_stdout = String::from_utf8_lossy(&scoped.stdout);
    assert!(
        !scoped_stdout.contains("project-b--ghost"),
        "project-a scope must not report project-b's dangling receipt; got:\n{scoped_stdout}"
    );

    // --fix under project-a scope must leave it recorded.
    let _ = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(
        receipt_recorded(&ws, "project-b", "project-b--ghost"),
        "project-a-scoped --fix must not retract project-b's receipt"
    );

    // --all sees it, and --all --fix retracts it.
    let all = rwv()
        .args(["doctor", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&all.stdout).contains("project-b--ghost"),
        "--all must report project-b's dangling receipt"
    );
    let _ = rwv()
        .args(["doctor", "--all", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(
        !receipt_recorded(&ws, "project-b", "project-b--ghost"),
        "--all --fix must retract it"
    );
}

/// The receipt of a ref that **does** exist must not be retracted — the
/// negative half of the test above, so a `fix_dangling_receipts` that
/// retracted everything cannot pass both.
#[test]
fn live_receipt_is_not_retracted() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    write_project_manifest(&ws, "myproj", "github/acme/repo");
    set_active_project(&ws, "myproj");

    create_branch(&canonical, "myproj--feat-a", "main");
    record_receipt(&ws, "myproj", "feat-a", &canonical);
    // Keep the workweave alive so the ref is not a stale-branch finding
    // either — this test is only about receipt retraction.
    write_marker(
        &workweaves_dir(&ws).join("myproj--feat-a"),
        &ws,
        "myproj",
        &ws,
    );

    let _ = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();

    assert!(
        receipt_recorded(&ws, "myproj", "myproj--feat-a"),
        "a receipt whose ref exists must survive --fix"
    );
}

// ===========================================================================
// (c) stale-ephemeral-branches: safe class
// ===========================================================================

/// Safe-class fixture: rwv holds an ownership receipt for the stale
/// ephemeral branch **and** its tip is an ancestor of the canonical's tip —
/// so a `Merged` warrant can be established and no commits are lost.
/// Doctor should report it; `--fix` should delete it; a follow-up doctor
/// run should be clean (idempotency).
///
/// The receipt is the load-bearing half. `handmade_lookalike_branch_survives_doctor_fix`
/// below is this fixture with the `record_receipt` line removed and the
/// opposite assertion, so neither test can pass on a `--fix` that ignores
/// the registry in either direction.
#[test]
fn stale_ephemeral_branch_safe_is_reported_and_fixable() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // Stale ephemeral branch pointing at the same commit as `main` — its
    // tip is trivially an ancestor of `main`'s tip.
    create_branch(&canonical, "myproj--dead/main", "main");
    record_receipt(&ws, "myproj", "dead/main", &canonical);

    // Advance main so it strictly dominates the stale branch (still
    // trivially safe — stale branch tip is_ancestor of main tip).
    add_commit(&canonical, "f2.txt", "second");

    // No workweave directory `.workweaves/myproj--dead/` exists.

    // First doctor run: report the safe-class violation.
    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("stale ephemeral branch") && stdout.contains("safe class"),
        "doctor should report safe-class stale ephemeral branch; got:\n{stdout}"
    );
    assert!(
        stdout.contains("myproj--dead/main"),
        "report should name the offending branch; got:\n{stdout}"
    );

    // Branch still exists pre-fix.
    let pre_fix = git()
        .args(["branch", "--list", "myproj--dead/main"])
        .current_dir(&canonical)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&pre_fix.stdout).is_empty(),
        "stale branch should exist before --fix"
    );

    // Apply --fix: doctor should delete the safe-class branch.
    let fix_out = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let fix_stdout = String::from_utf8_lossy(&fix_out.stdout);
    assert!(
        fix_stdout.contains("[fixed]") && fix_stdout.contains("myproj--dead/main"),
        "--fix should announce the delete; got:\n{fix_stdout}"
    );

    // Branch gone post-fix.
    let post_fix = git()
        .args(["branch", "--list", "myproj--dead/main"])
        .current_dir(&canonical)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&post_fix.stdout).trim().is_empty(),
        "stale branch should be deleted after --fix"
    );

    // Idempotency: a second --fix run finds nothing to fix and stays clean
    // of the safe-class warning.
    let again = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let again_stdout = String::from_utf8_lossy(&again.stdout);
    assert!(
        !again_stdout.contains("myproj--dead/main"),
        "second --fix run should be a no-op for the deleted branch; got:\n{again_stdout}"
    );
}

// ===========================================================================
// (c) stale-ephemeral-branches: unowned class — THE headline change
// ===========================================================================

/// **A branch that merely looks like rwv's is not rwv's.**
///
/// Byte-for-byte the safe-class fixture above minus the receipt: same name,
/// same store, same ancestry, same absent workweave directory. The shipped
/// `--fix` deleted it, because the scan classified by name shape and the
/// deletion trusted that classification. Under R2 the registry is asked
/// instead, and this branch survives.
///
/// This is the assertion to break first when checking the change is real: if
/// `fix_stale_ephemeral_branches` reverts to deleting by name shape, this
/// test fails and `stale_ephemeral_branch_safe_is_reported_and_fixable`
/// still passes — which is exactly the pairing that makes neither vacuous.
#[test]
fn handmade_lookalike_branch_survives_doctor_fix() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // The operator's own branch. It happens to be spelled the way rwv spells
    // its ephemeral refs; nothing recorded it.
    create_branch(&canonical, "myproj--dead/main", "main");
    add_commit(&canonical, "f2.txt", "second");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("rwv holds no ownership receipt for it"),
        "doctor should report it as unowned, not as safe class; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("safe class"),
        "an unreceipted branch must never be classified safe class; got:\n{stdout}"
    );

    let fix_out = rwv()
        .args(["doctor", "--fix", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let fix_stdout = String::from_utf8_lossy(&fix_out.stdout);
    assert!(
        branch_exists(&canonical, "myproj--dead/main"),
        "a hand-made lookalike must survive `doctor --fix`; doctor said:\n{fix_stdout}"
    );
}

// ===========================================================================
// (c) stale-ephemeral-branches: live class
// ===========================================================================

/// Live-class fixture: rwv holds a receipt for the stale ephemeral branch,
/// but its tip carries commits not reachable from the canonical's tip, so no
/// `Merged` warrant can be established. Doctor reports it as live-class;
/// `--fix` must NOT delete it.
#[test]
fn stale_ephemeral_branch_live_is_reported_and_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // Create the stale branch and add a unique commit on it (so it carries
    // work not reachable from main).
    git_in(&canonical, &["checkout", "-b", "myproj--dead/main", "-q"]);
    add_commit(&canonical, "unique.txt", "live work");
    git_in(&canonical, &["checkout", "main", "-q"]);
    record_receipt(&ws, "myproj", "dead/main", &canonical);

    // Advance main on a divergent path so the live branch's tip is
    // genuinely not an ancestor of main's tip.
    add_commit(&canonical, "mainwork.txt", "main work");

    // No workweave directory `.workweaves/myproj--dead/` exists.

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("stale ephemeral branch") && stdout.contains("live class"),
        "doctor should report live-class stale ephemeral branch; got:\n{stdout}"
    );
    assert!(
        stdout.contains("myproj--dead/main"),
        "report should name the offending branch; got:\n{stdout}"
    );

    // Branch exists pre-fix.
    let pre_fix = git()
        .args(["branch", "--list", "myproj--dead/main"])
        .current_dir(&canonical)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&pre_fix.stdout).is_empty(),
        "live branch should exist before --fix"
    );

    // Apply --fix. The live branch must survive untouched.
    let _ = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();

    let post_fix = git()
        .args(["branch", "--list", "myproj--dead/main"])
        .current_dir(&canonical)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&post_fix.stdout).is_empty(),
        "live branch must NOT be deleted by --fix; got:\n{}",
        String::from_utf8_lossy(&post_fix.stdout)
    );
}

// ===========================================================================
// (c) Healthy: ephemeral branch whose workweave still exists is not flagged.
// ===========================================================================

/// An ephemeral branch in the canonical whose `<project>--<name>` workweave
/// directory still exists on disk is owned, not stale. Doctor must not
/// flag it.
#[test]
fn ephemeral_branch_with_existing_workweave_is_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // Create the ephemeral branch in the canonical (just as a bare ref).
    create_branch(&canonical, "myproj--feat-a/main", "main");

    // And create the matching workweave directory with a marker.
    let ww_dir = workweaves_dir(&ws).join("myproj--feat-a");
    write_marker(&ww_dir, &ws, "myproj", &ws);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("stale ephemeral branch"),
        "ephemeral branch with existing workweave dir must not be flagged as stale; got:\n{stdout}"
    );
}

// ===========================================================================
// Project-scope isolation (fo-q5pj2e): --fix without --all must NOT delete
// stale ephemeral branches belonging to OTHER projects.
// ===========================================================================

/// Write a minimal `rwv.yaml` for `project_name` that declares a single repo
/// at `repo_path` (manifest-relative forward-slash string).
fn write_project_manifest(ws: &Path, project_name: &str, repo_path: &str) {
    let project_dir = ws.join("projects").join(project_name);
    std::fs::create_dir_all(&project_dir).unwrap();
    let manifest = format!(
        "repositories:\n  {repo_path}:\n    type: git\n    url: https://example.com/{repo_path}.git\n    version: main\n    role: owned\n"
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();
}

/// Set the active project by writing `.rwv-active` into the workspace root.
fn set_active_project(ws: &Path, project_name: &str) {
    std::fs::write(ws.join(".rwv-active"), format!("{project_name}\n")).unwrap();
}

/// `rwv doctor --fix` without `--all`, with project-a active, must NOT delete
/// a safe-class stale ephemeral branch in a repo owned by project-b.  With
/// `--all`, the deletion must happen normally.
///
/// Regression for fo-q5pj2e: before the fix, branch-discipline --fix walked
/// the entire weave regardless of scope, deleting branches across projects.
#[test]
fn fix_stale_ephemeral_branch_scoped_to_active_project() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());

    // Two repos: repo-a (owned by project-a) and repo-b (owned by project-b).
    let repo_a = ws.join("github").join("acme").join("repo-a");
    let repo_b = ws.join("github").join("acme").join("repo-b");
    init_repo_with_commit(&repo_a);
    init_repo_with_commit(&repo_b);

    // Create a stale safe-class ephemeral branch in repo-b for project-b's
    // dead workweave.  Safe-class: rwv holds a receipt for it AND its tip is
    // an ancestor of repo-b's primary tip.
    create_branch(&repo_b, "project-b--dead/main", "main");
    record_receipt(&ws, "project-b", "dead/main", &repo_b);
    add_commit(&repo_b, "advance.txt", "advance main");
    // repo-b's main now strictly dominates the stale branch tip → safe class.

    // Project manifests: project-a owns repo-a, project-b owns repo-b.
    write_project_manifest(&ws, "project-a", "github/acme/repo-a");
    write_project_manifest(&ws, "project-b", "github/acme/repo-b");

    // Activate project-a.
    set_active_project(&ws, "project-a");

    // Doctor (no --fix): should NOT report the project-b branch at all under
    // project-a scope (it belongs to a different project).
    let report = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let report_stdout = String::from_utf8_lossy(&report.stdout).into_owned();
    assert!(
        !report_stdout.contains("project-b--dead/main"),
        "doctor (no --fix) with project-a active must not report project-b's branch; got:\n{report_stdout}"
    );

    // --fix with project-a active: must NOT delete the branch.
    let fix_out = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let fix_stdout = String::from_utf8_lossy(&fix_out.stdout).into_owned();
    assert!(
        !fix_stdout.contains("project-b--dead/main"),
        "--fix with project-a active must not touch project-b's branch; got:\n{fix_stdout}"
    );

    // Branch still present after project-scoped --fix.
    let still_there = git()
        .args(["branch", "--list", "project-b--dead/main"])
        .current_dir(&repo_b)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&still_there.stdout)
            .trim()
            .is_empty(),
        "project-b's stale branch must survive a project-a-scoped --fix"
    );

    // --all --fix: now the deletion should happen.
    let all_fix_out = rwv()
        .args(["doctor", "--all", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let all_fix_stdout = String::from_utf8_lossy(&all_fix_out.stdout).into_owned();
    assert!(
        all_fix_stdout.contains("[fixed]") && all_fix_stdout.contains("project-b--dead/main"),
        "--all --fix must delete the branch weave-wide; got:\n{all_fix_stdout}"
    );

    // Branch gone after weave-wide --all --fix.
    let gone = git()
        .args(["branch", "--list", "project-b--dead/main"])
        .current_dir(&repo_b)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&gone.stdout).trim().is_empty(),
        "project-b's stale branch must be deleted after --all --fix"
    );
}

/// `rwv doctor --json` without `--all`, with project-a active, must NOT include
/// branch-discipline findings for project-b's canonical repos.  Mirrors the
/// text-output scope check above but exercises the JSON/collect path.
#[test]
fn json_branch_discipline_scoped_to_active_project() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());

    let repo_a = ws.join("github").join("acme").join("repo-a");
    let repo_b = ws.join("github").join("acme").join("repo-b");
    init_repo_with_commit(&repo_a);
    init_repo_with_commit(&repo_b);

    // Stale safe-class ephemeral branch in repo-b only.
    create_branch(&repo_b, "project-b--dead/main", "main");
    record_receipt(&ws, "project-b", "dead/main", &repo_b);
    add_commit(&repo_b, "advance.txt", "advance main");

    write_project_manifest(&ws, "project-a", "github/acme/repo-a");
    write_project_manifest(&ws, "project-b", "github/acme/repo-b");
    set_active_project(&ws, "project-a");

    // --json without --all: no project-b finding.
    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json invalid JSON: {e}\noutput: {stdout}"));

    let violations = json["violations"].as_array().expect("violations is array");
    let has_project_b_bd = violations
        .iter()
        .any(|v| v["kind"] == "branch-discipline" && v.to_string().contains("project-b--dead"));
    assert!(
        !has_project_b_bd,
        "doctor --json (project-a active) must not include project-b branch-discipline finding; violations: {violations:?}"
    );

    // --all --json: project-b finding IS included.
    let out_all = rwv()
        .args(["doctor", "--all", "--json"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stdout_all = String::from_utf8(out_all.stdout).unwrap();
    let json_all: serde_json::Value = serde_json::from_str(&stdout_all)
        .unwrap_or_else(|e| panic!("doctor --all --json invalid JSON: {e}\noutput: {stdout_all}"));
    let violations_all = json_all["violations"]
        .as_array()
        .expect("violations is array");
    let has_project_b_bd_all = violations_all
        .iter()
        .any(|v| v["kind"] == "branch-discipline" && v.to_string().contains("project-b--dead"));
    assert!(
        has_project_b_bd_all,
        "doctor --all --json must include project-b branch-discipline finding; violations: {violations_all:?}"
    );
}

// ===========================================================================
// JSON output exposes the branch-discipline kind.
// ===========================================================================

#[test]
fn json_output_includes_branch_discipline_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // Synthesize the simplest violation: a stale ephemeral-shaped branch
    // whose workweave is gone. Unowned class — the JSON channel has to carry
    // a `sub_kind` for it just like any other.
    create_branch(&canonical, "myproj--feat-a/main", "main");

    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json produced invalid JSON: {e}\noutput: {stdout}"));

    let violations = json["violations"].as_array().expect("violations is array");
    let found = violations.iter().any(|v| v["kind"] == "branch-discipline");
    assert!(
        found,
        "doctor --json must include a branch-discipline violation; violations: {violations:?}"
    );
}

// ===========================================================================
// Reference-alias carve-out (fo-5mhtf3.2)
//
// A `reference` repo is materialized as a symlink to the canonical clone, so
// it sits on the canonical's shared non-ephemeral branch (e.g. `main`) by
// design — it has no per-workweave ephemeral branch. `scan_repos_on_disk`
// discovers it because `is_dir()` follows the symlink; the I3 branch-
// discipline scan would mis-read it as a `shared-branch` violation. The scan
// must skip the symlink (a `CheckoutKind::ReferenceAlias`).
//
// The escape hatch must still flow through normally: a `reference` repo
// created with `--worktree-references` is a real worktree on its own
// ephemeral branch (a `CheckoutKind::Worktree`), checked like any other.
// ===========================================================================

/// A symlinked reference checkout must NOT fire a branch-discipline finding,
/// even though it resolves (through the symlink) to the canonical store on its
/// shared `main` branch. It is the canonical viewed through a link, not a
/// workweave checkout that wandered onto a shared branch.
#[cfg(unix)]
#[test]
fn symlinked_reference_does_not_fire_shared_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    // The canonical sits on `main` — exactly the branch a shared-branch
    // finding flags when a *worktree* checkout sits on it.

    let ww_dir = workweaves_dir(&ws).join("myproj--ref");
    write_marker(&ww_dir, &ws, "myproj", &ws);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    // Reference-repo materialization: a symlink at the workweave checkout
    // pointing at the canonical clone (which is on `main`).
    std::os::unix::fs::symlink(&canonical, &ww_checkout).unwrap();
    assert!(ww_checkout.is_symlink(), "fixture must be a symlink");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("shared-branch")
            && !stdout.contains("workweave checkout is on")
            && !stdout.contains("detached-HEAD")
            && !stdout.contains("foreign-ephemeral"),
        "a symlinked reference checkout must not fire any branch-discipline \
         finding (it shares the canonical's `main` by design); got:\n{stdout}"
    );
}

/// THE ESCAPE-HATCH TEST: a `reference` repo materialized via
/// `--worktree-references` is a *real worktree* on its own ephemeral branch —
/// a `CheckoutKind::Worktree`, NOT a `ReferenceAlias`. It must flow through
/// the normal I2/I3 checks unchanged: healthy on its ephemeral branch → clean,
/// and (the adversarial half) if it wanders onto `main`, it must STILL fire
/// `shared-branch`. The carve-out keys on alias-ness, never on role, so it
/// must not skip a worktree'd reference.
#[cfg(unix)]
#[test]
fn worktree_reference_on_ephemeral_branch_flows_through_normally() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    let ww_dir = workweaves_dir(&ws).join("myproj--wtref");
    write_marker(&ww_dir, &ws, "myproj", &ws);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    // The escape hatch: a real worktree on the workweave's ephemeral branch.
    worktree_add(&canonical, &ww_checkout, "myproj--wtref/main");
    assert!(
        !ww_checkout.is_symlink(),
        "a --worktree-references reference must be a real worktree, not a symlink"
    );

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("workweave checkout is on")
            && !stdout.contains("detached-HEAD")
            && !stdout.contains("foreign-ephemeral"),
        "a worktree'd reference on its ephemeral branch should be clean; got:\n{stdout}"
    );
}

/// The adversarial complement: a worktree'd reference that has wandered onto
/// the shared `main` branch must STILL fire `shared-branch`. The carve-out
/// keys on `CheckoutKind` (alias-ness), never on `role`, so a real worktree —
/// even of a reference repo — flows through the I3 check and is caught.
#[cfg(unix)]
#[test]
fn worktree_reference_on_shared_branch_still_fires() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // Move the canonical off `main` so the worktree can check `main` out.
    git_in(&canonical, &["checkout", "-b", "rwv-primary-tip", "-q"]);

    let ww_dir = workweaves_dir(&ws).join("myproj--wtref");
    write_marker(&ww_dir, &ws, "myproj", &ws);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    // A real worktree (not a symlink) sitting on the shared `main`.
    worktree_add_existing(&canonical, &ww_checkout, "main");
    assert!(!ww_checkout.is_symlink(), "fixture must be a real worktree");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("shared-branch")
            || stdout.contains("workweave checkout is on shared-branch"),
        "a worktree'd reference on the shared `main` must still fire \
         shared-branch (carve-out keys on alias-ness, not role); got:\n{stdout}"
    );
}
