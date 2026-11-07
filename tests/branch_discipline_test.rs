//! Tests for branch-discipline checks (`rwv doctor`).
//!
//! Enforces the I3 invariant from `docs/explanation/joints/clone-topology.md`
//! (every workweave repo checkout sits on its owned
//! `<project>--<workweave>` ephemeral branch; canonicals sit on a
//! non-ephemeral branch) plus the safe/live doctrine from
//! `docs/explanation/joints/shared-refs-drift.md` applied to refs in (c).
//! The ownership rules the tests below name — R1, R2 — are stated in
//! `docs/internals/branch-model.md`.
//!
//! Three checks:
//!
//!   (a) workweave-branch — `shared-branch`, `foreign-ephemeral`, `detached`
//!   (b) the canonical-store arms —
//!       `canonical-holds-live-workweave-ref`, `canonical-holds-leaked-ref`,
//!       `canonical-detached`
//!   (c) stale-ephemeral-branches — `safe` (auto-fixable) / `live` (never) /
//!       `unowned` (never — rwv holds no receipt)
//!
//! Healthy fixtures (workweave on its own ephemeral branch, canonical on
//! `main`, ephemeral branch whose workweave still exists) must stay clean.
//!
//! **Ownership is by record.** Everything in (b), and the safe/live half of
//! (c), keys on an ownership receipt, not on the branch's name. So most
//! fixtures below record a receipt explicitly — see
//! [`record_receipt`] for how a receipt for `<p>--<a>/<b>` is adopted rather
//! than minted. A fixture that skips the receipt is asserting the *other*
//! half: a branch that merely looks like rwv's is the operator's, and
//! `--fix` must leave it alone.
//!
//! Fixture rationale: branch-discipline operates on real git repos, so the
//! workspaces here include actual git checkouts (not just directory shells
//! like the tree-integrity tests).

use repoweave::git::git_vcs;
use repoweave::manifest::{ProjectName, WorkweaveId, WorkweaveName};
use repoweave::vcs::{EphemeralRefName, LegacyEphemeralRefName, RawRefName};
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
    let primary_str =
        repoweave::path_spelling::resolve(primary).unwrap_or_else(|_| primary.to_path_buf());
    let parent_str =
        repoweave::path_spelling::resolve(parent).unwrap_or_else(|_| parent.to_path_buf());
    let content = common::workweave_marker(&primary_str, project, &parent_str);
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

/// Record an ownership receipt for the branch that
/// `(project, workweave)` names, in `store`.
///
/// This is how rwv's own create path claims a ref, and it is the
/// *only* thing that makes a ref rwv's to destroy — so a fixture that wants
/// doctor to treat a branch as rwv's has to call this.
///
/// **Why some callers pass a `workweave` with a `/` in it.**
/// A receipt for a branch spelled `<project>--<a>/<b>` — the shape the (c)
/// scanner still discovers, until the flat-name cutover lands — cannot be
/// [`EphemeralRefName::mint`]ed: a workweave name may not contain `/`. Such a
/// branch is instead *adopted*, the same route the migration itself takes —
/// `workweave` is split at the first `/` into the live workweave (`<a>`,
/// whose own flat name is the namespace) and the pre-flat branch's remainder
/// (`<b>`), and [`LegacyEphemeralRefName::claim`] recognizes the branch as
/// sitting under that namespace.
///
/// The receipt is recorded at the branch's current tip, and the branch must
/// already exist: recording against an absent ref would produce the dangling
/// state, which is a different fixture.
fn record_receipt(primary: &Path, project: &str, workweave: &str, store: &Path) {
    make_project(primary, project);
    let project = ProjectName::new(project).unwrap();
    let mut registry = RefRegistry::for_project(primary, &project);

    match workweave.split_once('/') {
        None => {
            let name = EphemeralRefName::mint(
                &WorkweaveId {
                    project: project.clone(),
                    name: WorkweaveName::new(workweave).unwrap(),
                }
                .flat(),
            );
            let tip = git_vcs()
                .resolve_local_branch_tip(store, &name.to_raw())
                .expect("store is readable")
                .unwrap_or_else(|| {
                    panic!("branch `{name}` must exist before recording a receipt for it")
                });
            registry
                .record_created(store, name, tip)
                .expect("receipt should record");
        }
        Some((live_workweave, _remainder)) => {
            let flat = EphemeralRefName::mint(
                &WorkweaveId {
                    project: project.clone(),
                    name: WorkweaveName::new(live_workweave).unwrap(),
                }
                .flat(),
            );
            let observed = RawRefName::new(format!("{}--{workweave}", project.as_str()));
            let legacy = LegacyEphemeralRefName::claim(&flat, &observed)
                .expect("fixture: `workweave` must be `<live>/<remainder>` shaped");
            let tip = git_vcs()
                .resolve_local_branch_tip(store, &observed)
                .expect("store is readable")
                .unwrap_or_else(|| {
                    panic!("branch `{observed}` must exist before recording a receipt for it")
                });
            registry
                .adopt_legacy(store, legacy, tip)
                .expect("receipt should record");
        }
    }
}

/// Record an ownership receipt for a ref that does **not** exist — the
/// dangling-receipt state that is the benign crash residue.
fn record_dangling_receipt(primary: &Path, project: &str, workweave: &str, store: &Path) {
    make_project(primary, project);
    let project = ProjectName::new(project).unwrap();
    let mut registry = RefRegistry::for_project(primary, &project);
    let name = EphemeralRefName::mint(
        &WorkweaveId {
            project: project.clone(),
            name: WorkweaveName::new(workweave).unwrap(),
        }
        .flat(),
    );
    // Any resolvable revision works as the recorded tip: the receipt names a
    // ref that is not there, so nothing ever compares against it.
    let head = git_vcs().head_revision(store).expect("store has a HEAD");
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
/// `<project>--<workweave>` ephemeral branch. Doctor should not
/// report any branch-discipline finding for this directory.
#[test]
fn healthy_workweave_ephemeral_branch_is_clean() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    let ww_dir = workweaves_dir(&ws).join("myproj--feat-a");
    write_marker(&ww_dir, &ws, "myproj", &ws);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    worktree_add(&canonical, &ww_checkout, "myproj--feat-a");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("workweave checkout is on")
            && !stdout.contains("detached-HEAD")
            && !stdout.contains("foreign-ephemeral"),
        "healthy workweave on its ephemeral branch should be clean; got:\n{stdout}"
    );
}

// ===========================================================================
// (b) the canonical-store arms
// ===========================================================================

/// Healthy canonical: checked out on a non-ephemeral branch (`main`).
/// No branch-discipline finding expected.
#[test]
fn healthy_canonical_on_main_is_clean() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("canonical store is checked out on"),
        "canonical on main should not fire an attachment arm; got:\n{stdout}"
    );
}

/// A canonical sitting on
/// a **hand-made** `<a>--<b>/<c>` branch is on an operator branch, not on one
/// of rwv's. Name shape is not ownership (R2), so doctor leaves it alone.
///
/// The shipped scan reported this as `ephemeral-at-primary` purely because
/// the name parsed. Non-vacuity: the companion test below builds the same
/// fixture *with* a receipt and asserts the finding does fire, so a scan that
/// simply stopped looking at canonicals cannot make both pass.
#[test]
fn handmade_lookalike_at_canonical_is_not_reported() {
    let tmp = common::tempdir().unwrap();
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
        "a hand-made lookalike is operator state and must not fire an \
         attachment finding; got:\n{stdout}"
    );
}

/// The canonical is attached to a ref rwv **recorded** for a
/// workweave that is gone — a leak.
///
/// `--fix` cannot reclaim it while this store's own HEAD is on it (git
/// refuses to delete a branch a worktree uses), so the finding names the
/// `git switch` that frees it and the ref survives the run.
#[test]
fn canonical_holding_recorded_ref_of_deleted_workweave_is_reported() {
    let tmp = common::tempdir().unwrap();
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

/// A canonical store attached to the ref recorded for `myproj`'s workweave
/// `feat-a`, whose directory is still on disk. Returns the primary.
fn canonical_on_a_live_workweaves_ref(tmp: &Path) -> PathBuf {
    let ws = make_primary(tmp);
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    create_branch(&canonical, "myproj--feat-a", "main");
    record_receipt(&ws, "myproj", "feat-a", &canonical);
    git_in(&canonical, &["checkout", "myproj--feat-a", "-q"]);

    // The workweave directory is there — git could not have produced this
    // topology, so a directory was moved or copied.
    let ww_dir = workweaves_dir(&ws).join("myproj--feat-a");
    write_marker(&ww_dir, &ws, "myproj", &ws);
    ws
}

/// The canonical is attached to a ref recorded for a workweave
/// that is **still on disk** — an I3 disjointness violation that only a
/// moved or copied directory can produce. Report-only.
#[test]
fn canonical_holding_recorded_ref_of_live_workweave_is_reported() {
    let tmp = common::tempdir().unwrap();
    let ws = canonical_on_a_live_workweaves_ref(tmp.path());

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
    assert!(
        stdout.contains("recorded for workweave `myproj--feat-a`"),
        "the workweave is named by its identity, project included; got:\n{stdout}"
    );
}

#[test]
fn canonical_holding_a_live_workweaves_ref_names_it_qualified_in_json() {
    let tmp = common::tempdir().unwrap();
    let ws = canonical_on_a_live_workweaves_ref(tmp.path());

    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let finding = json["violations"]
        .as_array()
        .expect("violations is an array")
        .iter()
        .find_map(|v| v["sub_kind"].get("canonical-holds-live-workweave-ref"))
        .unwrap_or_else(|| panic!("no canonical-holds-live-workweave-ref finding in {json}"));
    assert_eq!(
        finding["workweave"],
        serde_json::json!("myproj--feat-a"),
        "{finding}"
    );
}

// ===========================================================================
// The Detached arm — at the canonical, and at the project repo
// ===========================================================================

/// An attached project repo produces no finding — the fixture above minus
/// the detach, so the assertion there cannot be passing on a scan that
/// reports every project repo unconditionally.
#[test]
fn attached_project_repo_is_clean() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    write_project_manifest(&ws, "myproj", "github/acme/repo");
    set_active_project(&ws, "myproj");

    let project_repo = ws.join("projects").join("myproj");
    init_repo_with_commit(&project_repo);
    git_in(&project_repo, &["add", "rwv.toml"]);
    git_in(&project_repo, &["commit", "-q", "-m", "manifest"]);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("detached-HEAD state"),
        "an attached project repo must be clean; got:\n{stdout}"
    );
}

// ===========================================================================
// Dangling ownership receipts
// ===========================================================================

/// A receipt whose ref never appeared is the benign residue of a crash
/// between the receipt write and the ref creation. Doctor reports it;
/// `--fix` retracts it.
#[test]
fn dangling_receipt_is_reported_and_retracted() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    write_project_manifest(&ws, "myproj", "github/acme/repo");
    set_active_project(&ws, "myproj");

    record_dangling_receipt(&ws, "myproj", "never-born", &canonical);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("dangling-ref-receipt"),
        "doctor should report the dangling receipt; got:\n{stdout}"
    );
    // The receipt's ref name is per-item detail, carried by `--json`.
    let json = doctor_json_compact(&ws, false);
    assert!(
        json.contains("myproj--never-born"),
        "the record must name the dangling receipt's ref; got:\n{json}"
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
    let tmp = common::tempdir().unwrap();
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
    // The count line carries no names, so probe for the class token: a
    // scoped run must not even count project-b's finding.
    assert!(
        !scoped_stdout.contains("dangling-ref-receipt"),
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

    // --all sees it, and --all --fix retracts it. The text line is the
    // per-class count; the ref name is in the `--json` record.
    let all = rwv()
        .args(["doctor", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&all.stdout).contains("dangling-ref-receipt"),
        "--all must report project-b's dangling receipt"
    );
    assert!(
        doctor_json_compact(&ws, true).contains("project-b--ghost"),
        "--all --json must name project-b's dangling receipt"
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // Stale ephemeral branch pointing at the same commit as `main` — its
    // tip is trivially an ancestor of `main`'s tip.
    create_branch(&canonical, "myproj--dead", "main");
    record_receipt(&ws, "myproj", "dead", &canonical);

    // Advance main so it strictly dominates the stale branch (still
    // trivially safe — stale branch tip is_ancestor of main tip).
    add_commit(&canonical, "f2.txt", "second");

    // No workweave directory `.workweaves/myproj--dead/` exists.

    // First doctor run: report the safe-class violation.
    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("stale-ephemeral-branch-safe"),
        "doctor should report safe-class leaked ephemeral branch; got:\n{stdout}"
    );
    // The branch name is per-item detail, carried by `--json`.
    assert!(
        doctor_json_compact(&ws, false).contains("myproj--dead"),
        "the record should name the offending branch"
    );

    // Branch still exists pre-fix.
    let pre_fix = git()
        .args(["branch", "--list", "myproj--dead"])
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
        fix_stdout.contains("[fixed]") && fix_stdout.contains("myproj--dead"),
        "--fix should announce the delete; got:\n{fix_stdout}"
    );

    // Branch gone post-fix.
    let post_fix = git()
        .args(["branch", "--list", "myproj--dead"])
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
        !again_stdout.contains("myproj--dead"),
        "second --fix run should be a no-op for the deleted branch; got:\n{again_stdout}"
    );
}

/// **A ref a live checkout is on is not a leak, whatever the record says.**
///
/// The safe-class fixture above plus one thing: a worktree sitting on the
/// branch, placed where neither of rwv's own liveness sources can see it —
/// outside every container the walk knows (a `--dir` seat) and absent from
/// the workweave index (what a lost, hand-edited or restored index leaves
/// behind, which is the state the reconciliation pass exists for). Both
/// sources therefore answer "no workweave mints this", and the receipt plus
/// the ancestry then read as safe-class: a deleted workweave's leftover.
///
/// Driven against the binary this fixture was written for, doctor said
/// exactly that and `--fix` ran `git branch -D` on the branch the seat had
/// checked out. Git refused, and rwv reported git's refusal as a raw
/// `[error]` — so the only thing standing between a live seat and a deleted
/// branch was a guard rwv does not own. Both halves are asserted below: the
/// finding is not raised, and the delete is not attempted.
#[test]
fn a_live_checkouts_branch_is_not_classified_stale() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // The seat: a workweave directory no container holds, on its own
    // ephemeral branch.
    let seat = tmp.path().join("elsewhere").join("unrelated-name");
    write_marker(&seat, &ws, "myproj", &ws);
    let seat_checkout = seat.join("github").join("acme").join("repo");
    std::fs::create_dir_all(seat_checkout.parent().unwrap()).unwrap();
    worktree_add(&canonical, &seat_checkout, "myproj--seat");
    record_receipt(&ws, "myproj", "seat", &canonical);

    // Advance the canonical past the seat's tip: what would otherwise earn
    // the `Merged` warrant and put this in the auto-fixable class.
    add_commit(&canonical, "f2.txt", "second");

    // Non-vacuity: without the receipt this branch is the operator's and
    // every assertion below would hold for the wrong reason.
    assert!(
        receipt_recorded(&ws, "myproj", "myproj--seat"),
        "fixture: the receipt is what makes this ref rwv's to destroy"
    );

    let sub_kinds = branch_discipline_sub_kinds(&ws);
    assert!(
        !sub_kinds
            .iter()
            .any(|k| k.starts_with("stale-ephemeral-branch")),
        "a branch a live checkout holds is not stale in any class: {sub_kinds:?}"
    );

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("stale-ephemeral-branch"),
        "and the text surface must not say so either; got:\n{stdout}"
    );

    let fix_out = rwv()
        .args(["doctor", "--fix", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let fix_stdout = String::from_utf8_lossy(&fix_out.stdout);
    assert!(
        branch_exists(&canonical, "myproj--seat"),
        "the live seat's branch must survive `doctor --fix`; doctor said:\n{fix_stdout}"
    );
    assert!(
        !fix_stdout.contains("failed to delete safe-class stale ephemeral branch"),
        "and rwv must not have attempted the delete: surviving because git \
         refused is the defect, not the fix; got:\n{fix_stdout}"
    );
}

/// The negative control for the test above, and the reason the guard reads
/// git's worktree table for *live* checkouts rather than for registrations.
///
/// Same seat, its directory removed the way an operator removes one — by
/// hand, without `rwv workweave delete`. The registration outlives the
/// directory (that is what `stale-worktree-registration` reports and what
/// `worktree prune` drops), and git refuses to delete a branch it holds just
/// as firmly. A guard that keyed on the registration would therefore be
/// green here too — and would have silently emptied the class this whole
/// section is about.
#[test]
fn a_dead_seats_branch_is_still_classified_stale() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    let seat = tmp.path().join("elsewhere").join("unrelated-name");
    write_marker(&seat, &ws, "myproj", &ws);
    let seat_checkout = seat.join("github").join("acme").join("repo");
    std::fs::create_dir_all(seat_checkout.parent().unwrap()).unwrap();
    worktree_add(&canonical, &seat_checkout, "myproj--seat");
    record_receipt(&ws, "myproj", "seat", &canonical);
    add_commit(&canonical, "f2.txt", "second");

    std::fs::remove_dir_all(&seat).unwrap();

    let sub_kinds = branch_discipline_sub_kinds(&ws);
    assert!(
        sub_kinds.contains(&"stale-ephemeral-branch-safe".to_string()),
        "a receipted branch whose seat is gone is still the safe class, \
         registration or no registration: {sub_kinds:?}"
    );
}

fn registered_worktrees(store: &Path) -> String {
    String::from_utf8_lossy(
        &git()
            .args(["worktree", "list", "--porcelain"])
            .current_dir(store)
            .output()
            .unwrap()
            .stdout,
    )
    .into_owned()
}

fn registers(store: &Path, worktree: &Path) -> bool {
    registered_worktrees(store)
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|registered| Path::new(registered) == worktree)
}

/// A seat's checkout in a store no manifest lists, holding a receipted
/// branch the store's tip has passed.
fn seat_in_an_orphaned_store(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let ws = make_primary(tmp);
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    let seat = tmp.join("elsewhere").join("unrelated-name");
    write_marker(&seat, &ws, "myproj", &ws);
    let seat_checkout = seat.join("github").join("acme").join("repo");
    std::fs::create_dir_all(seat_checkout.parent().unwrap()).unwrap();
    worktree_add(&canonical, &seat_checkout, "myproj--seat");
    record_receipt(&ws, "myproj", "seat", &canonical);
    add_commit(&canonical, "f2.txt", "second");
    (ws, canonical, seat_checkout)
}

/// git refuses to delete a branch a registration holds even when the
/// registration's directory is gone, and a store no manifest lists gets no
/// `stale-worktree-registration` finding to prune it. The branch fix prunes
/// it itself, says so, and then deletes.
#[test]
fn a_hand_deleted_seats_branch_in_an_orphaned_store_is_deleted_by_fix() {
    let tmp = common::tempdir().unwrap();
    let (ws, canonical, seat_checkout) = seat_in_an_orphaned_store(tmp.path());
    std::fs::remove_dir_all(seat_checkout.parent().unwrap()).unwrap();
    assert!(
        registered_worktrees(&canonical).contains("prunable"),
        "fixture: the registration outlives its directory"
    );

    let out = rwv()
        .args(["doctor", "--fix", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.lines().any(|line| line
            .starts_with("[fixed] core: pruned the registration of removed worktree ")
            && line.contains("unrelated-name")),
        "{stdout}"
    );
    assert!(!branch_exists(&canonical, "myproj--seat"), "{stdout}");
    assert!(!registers(&canonical, &seat_checkout), "{stdout}");
    assert!(!receipt_recorded(&ws, "myproj", "myproj--seat"), "{stdout}");
}

/// A locked registration is the operator saying its directory will be back:
/// it survives `--fix` with its directory gone, and so does the branch it
/// holds.
#[test]
fn a_locked_registration_and_its_branch_survive_fix() {
    let tmp = common::tempdir().unwrap();
    let (ws, canonical, seat_checkout) = seat_in_an_orphaned_store(tmp.path());
    git_in(
        &canonical,
        &["worktree", "lock", seat_checkout.to_str().unwrap()],
    );
    std::fs::remove_dir_all(seat_checkout.parent().unwrap()).unwrap();

    let out = rwv()
        .args(["doctor", "--fix", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(registers(&canonical, &seat_checkout), "{stdout}");
    assert!(branch_exists(&canonical, "myproj--seat"), "{stdout}");
    assert!(!stdout.contains("pruned the registration"), "{stdout}");
}

/// git calls a registration prunable once its path stops leading back to
/// the store, and that includes a directory standing there again without
/// the link. Whatever is in that directory is not rwv's to orphan: the
/// registration, the directory and the branch all survive `--fix`.
#[test]
fn a_registration_whose_directory_stands_survives_fix() {
    let tmp = common::tempdir().unwrap();
    let (ws, canonical, seat_checkout) = seat_in_an_orphaned_store(tmp.path());
    std::fs::remove_file(seat_checkout.join(".git")).unwrap();
    std::fs::write(seat_checkout.join("work.txt"), "keep\n").unwrap();
    assert!(
        registered_worktrees(&canonical).contains("prunable"),
        "fixture: git calls the registration prunable"
    );

    let out = rwv()
        .args(["doctor", "--fix", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(registers(&canonical, &seat_checkout), "{stdout}");
    assert!(seat_checkout.join("work.txt").exists(), "{stdout}");
    assert!(branch_exists(&canonical, "myproj--seat"), "{stdout}");
    assert!(!stdout.contains("pruned the registration"), "{stdout}");
}

/// The ordinary route to the fixture above — no `--dir`, no hand-edited
/// index — reached by driving `rwv workweave create` and then `rm -rf`ing
/// the seat the way an operator removes one without `rwv workweave delete`.
///
/// `apply_finding_repairs` used to call `fix_stale_ephemeral_branches`
/// before the loop that prunes `stale-worktree-registration`, so the branch
/// delete ran while git's own worktree table still held the (prunable)
/// entry and git refused it — a raw `[error]` on the first `--fix`, self-
/// healing on the second because the prune had landed by then. The first
/// pass must now finish both repairs with nothing left for a second run.
#[test]
fn rm_rf_of_a_container_placed_seat_is_fixed_in_one_pass() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    let project_dir = ws.join("projects").join("myproj");
    init_repo_with_commit(&project_dir);
    write_project_manifest(&ws, "myproj", "github/acme/repo");
    git_in(&project_dir, &["add", "rwv.toml"]);
    git_in(&project_dir, &["commit", "-q", "-m", "add manifest"]);
    common::record_trunk(&project_dir);

    rwv()
        .args(["workweave", "create", "--project", "myproj", "seat"])
        .current_dir(&ws)
        .assert()
        .success();

    let list = git()
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&canonical)
        .output()
        .unwrap();
    let canonical_canon = repoweave::path_spelling::resolve(&canonical).unwrap();
    let seat_checkout = String::from_utf8_lossy(&list.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
        .find(|p| {
            repoweave::path_spelling::resolve(p)
                .map(|c| c != canonical_canon)
                .unwrap_or(true)
        })
        .expect("`workweave create` should have registered a second worktree");

    // Advance main past the seat's tip: what earns the safe-class warrant.
    add_commit(&canonical, "f2.txt", "second");

    let seat_dir = workweaves_dir(&ws).join("myproj--seat");
    assert!(
        repoweave::path_spelling::CanonicalPath::of(&seat_dir)
            .contains(&repoweave::path_spelling::CanonicalPath::of(&seat_checkout)),
        "fixture: `create` should place the seat's checkout under the \
         container path {}; got {}",
        seat_dir.display(),
        seat_checkout.display()
    );
    std::fs::remove_dir_all(&seat_dir).unwrap();

    let fix_out = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let fix_stdout = String::from_utf8_lossy(&fix_out.stdout);

    assert!(
        !fix_stdout.contains("failed to delete safe-class stale ephemeral branch"),
        "the first --fix pass must not attempt the delete before the \
         registration guarding it is pruned; got:\n{fix_stdout}"
    );
    assert!(
        !fix_stdout.contains("[error"),
        "no raw git error should reach the operator on the first pass; \
         got:\n{fix_stdout}"
    );
    assert!(
        !branch_exists(&canonical, "myproj--seat"),
        "the branch must be gone after the FIRST --fix pass, not the \
         second; doctor said:\n{fix_stdout}"
    );
}

/// The same blind spot in the one class discovered by shape rather than by
/// record, which is why the guard sits in both loops.
///
/// A seat that predates the flat-name cutover and never migrated holds a
/// `<project>--<name>/<segment>` branch that no receipt names. Placed where
/// neither of rwv's records can see it, its namespace is not among the live
/// ones either — so the unowned arm reported the branch a live checkout is
/// sitting on as one no workweave claims, and that finding's standing advice
/// is to remove it by hand. Report-only makes rwv not the one deleting it;
/// it does not make the sentence true.
#[test]
fn a_live_checkouts_pre_flat_branch_is_not_reported_unowned() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    make_project(&ws, "myproj");

    let seat = tmp.path().join("elsewhere").join("unrelated-name");
    write_marker(&seat, &ws, "myproj", &ws);
    let seat_checkout = seat.join("github").join("acme").join("repo");
    std::fs::create_dir_all(seat_checkout.parent().unwrap()).unwrap();
    worktree_add(&canonical, &seat_checkout, "myproj--seat/main");

    let sub_kinds = branch_discipline_sub_kinds(&ws);
    assert!(
        !sub_kinds.contains(&"stale-ephemeral-branch-unowned".to_string()),
        "a live checkout's own branch is not an orphan of the cutover: \
         {sub_kinds:?}"
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
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // The project has to exist, or the registry cannot be written to at all
    // and the fixture would prove nothing: a `--fix` that tried to forge a
    // receipt for this branch would fail on the missing directory rather than
    // on the rule under test. (Measured — without this line a mutation that
    // forges receipts and deletes the unowned class leaves this test green.)
    make_project(&ws, "myproj");

    // The operator's own branch. It happens to be spelled the way rwv spells
    // its ephemeral refs; nothing recorded it.
    create_branch(&canonical, "myproj--dead/main", "main");
    add_commit(&canonical, "f2.txt", "second");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("stale-ephemeral-branch-unowned"),
        "doctor should report it as unowned, not as safe class; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("stale-ephemeral-branch-safe"),
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

/// The byte-for-byte pair of `stale_ephemeral_branch_safe_is_reported_and_fixable`:
/// the **flat** name rwv actually mints, same store, same ancestry, same
/// absent workweave directory — minus the receipt.
///
/// This is the fixture that pins the leak scan to the registry. The pre-flat
/// pair above (`handmade_lookalike_branch_survives_doctor_fix`) can be
/// answered by a name-shape rule; this one cannot, because the safe-class
/// fixture it differs from has a *character-identical* branch name. Anything
/// that deletes here deletes the operator's branch.
#[test]
fn flat_lookalike_branch_survives_doctor_fix() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // `record_receipt` mints this as a side effect; the safe-class fixture
    // therefore has it and this one must too, or the two stop being
    // byte-for-byte and a receipt-forging `--fix` would fail here for the
    // wrong reason. See the note in `handmade_lookalike_branch_survives_doctor_fix`.
    make_project(&ws, "myproj");

    // Identical to the safe-class fixture, with `record_receipt` removed.
    create_branch(&canonical, "myproj--dead", "main");
    add_commit(&canonical, "f2.txt", "second");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("safe class"),
        "an unreceipted branch must never be classified safe class; got:\n{stdout}"
    );

    let fix_out = rwv()
        .args(["doctor", "--fix", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(
        branch_exists(&canonical, "myproj--dead"),
        "a flat look-alike with no receipt must survive `doctor --fix`; doctor said:\n{}",
        String::from_utf8_lossy(&fix_out.stdout)
    );
}

/// The unowned class's whole jurisdiction, pinned from one listing.
///
/// Scope: the scan screens the canonical's branch names with
/// `looks_like_a_pre_flat_ref`, so the only spelling that can reach the
/// finding is `<a>--<b>/<c>` — the shape rwv minted before the flat cutover.
/// Everything else is invisible to it by decision, not by accident: the flat
/// `<a>--<b>` with no receipt is an operator branch under R2, and so is any
/// name outside both mint shapes — a segmented name with no `--` left of the
/// `/` (`stray-seat/main`), or no `/` at all (`freeform-stray`). A stray
/// of those spellings is the operator's to census by hand; no doctor class
/// reports it.
///
/// The firing control in the same store is what keeps the five silence
/// assertions from passing against a scan that stopped running. To check the
/// pin is real, weaken either arm of `looks_like_a_pre_flat_ref` — drop the
/// `split_at_weave_separator` requirement, or answer the slashless arm with
/// anything but `false` — and the count assertion reddens.
#[test]
fn unowned_class_fires_only_on_the_pre_flat_mint_shape() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    make_project(&ws, "myproj");

    create_branch(&canonical, "myproj--ghost/main", "main");
    create_branch(&canonical, "myproj--noseat", "main");
    create_branch(&canonical, "stray-seat/main", "main");
    create_branch(&canonical, "freeform-stray", "main");
    create_branch(&canonical, "null/main", "main");
    create_branch(&canonical, "test-worktree/main", "main");

    let json = doctor_json_compact(&ws, false);
    assert!(
        json.contains("stale-ephemeral-branch-unowned") && json.contains("myproj--ghost/main"),
        "the control (pre-flat mint shape, no receipt) must fire as unowned; got:\n{json}"
    );
    assert_eq!(
        json.matches("stale-ephemeral-branch").count(),
        1,
        "exactly one stale-ephemeral finding — the control; got:\n{json}"
    );
    for silent in [
        "myproj--noseat",
        "stray-seat/main",
        "freeform-stray",
        "null/main",
        "test-worktree/main",
    ] {
        assert!(
            !json.contains(silent),
            "`{silent}` is outside the unowned class's jurisdiction and must \
             not surface anywhere in the report; got:\n{json}"
        );
    }
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
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // Create the stale branch and add a unique commit on it (so it carries
    // work not reachable from main).
    git_in(&canonical, &["checkout", "-b", "myproj--dead", "-q"]);
    add_commit(&canonical, "unique.txt", "live work");
    git_in(&canonical, &["checkout", "main", "-q"]);
    record_receipt(&ws, "myproj", "dead", &canonical);

    // Advance main on a divergent path so the live branch's tip is
    // genuinely not an ancestor of main's tip.
    add_commit(&canonical, "mainwork.txt", "main work");

    // No workweave directory `.workweaves/myproj--dead/` exists.

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("stale-ephemeral-branch-live"),
        "doctor should report live-class leaked ephemeral branch; got:\n{stdout}"
    );
    // The branch name is per-item detail, carried by `--json`.
    assert!(
        doctor_json_compact(&ws, false).contains("myproj--dead"),
        "the record should name the offending branch"
    );

    // Branch exists pre-fix.
    let pre_fix = git()
        .args(["branch", "--list", "myproj--dead"])
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
        .args(["branch", "--list", "myproj--dead"])
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
    let tmp = common::tempdir().unwrap();
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
// Project-scope isolation: --fix without --all must NOT delete
// stale ephemeral branches belonging to OTHER projects.
// ===========================================================================

/// Mint `projects/<project>/` as a project: the directory plus the manifest
/// that makes it one.
///
/// The manifest is not decoration. A project is a directory under `projects/`
/// holding an `rwv.toml`, and the enumeration every doctor scan runs on stops
/// at that file — so a bare directory holds a registry nothing reads, which is
/// a state no rwv verb produces.
fn make_project(primary: &Path, project: &str) {
    let dir = primary.join("projects").join(project);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("rwv.toml"), "[repositories]\n").unwrap();
}

/// Write a minimal `rwv.toml` for `project_name` that declares a single repo
/// at `repo_path` (manifest-relative forward-slash string).
fn write_project_manifest(ws: &Path, project_name: &str, repo_path: &str) {
    let project_dir = ws.join("projects").join(project_name);
    std::fs::create_dir_all(&project_dir).unwrap();
    let manifest = format!(
        "[repositories.\"{repo_path}\"]\ntype = \"git\"\nurl = \"https://example.com/{repo_path}.git\"\nbranch = \"main\"\nrole = \"owned\"\n"
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
}

/// Set the active project by writing `.rwv-active` into the workspace root.
fn set_active_project(ws: &Path, project_name: &str) {
    std::fs::write(ws.join(".rwv-active"), format!("{project_name}\n")).unwrap();
}

/// `rwv doctor --fix` without `--all`, with project-a active, must NOT delete
/// a safe-class stale ephemeral branch in a repo owned by project-b.  With
/// `--all`, the deletion must happen normally.
///
/// Regression: before the fix, branch-discipline --fix walked
/// the entire weave regardless of scope, deleting branches across projects.
#[test]
fn fix_stale_ephemeral_branch_scoped_to_active_project() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());

    // Two repos: repo-a (owned by project-a) and repo-b (owned by project-b).
    let repo_a = ws.join("github").join("acme").join("repo-a");
    let repo_b = ws.join("github").join("acme").join("repo-b");
    init_repo_with_commit(&repo_a);
    init_repo_with_commit(&repo_b);

    // Create a stale safe-class ephemeral branch in repo-b for project-b's
    // dead workweave.  Safe-class: rwv holds a receipt for it AND its tip is
    // an ancestor of repo-b's primary tip.
    // Flat, because a receipt naming a pre-flat ref is retracted rather than
    // acted on (`fix_pre_flat_receipts`) — that branch would survive `--all
    // --fix` for a reason that has nothing to do with scope.
    create_branch(&repo_b, "project-b--dead", "main");
    record_receipt(&ws, "project-b", "dead", &repo_b);
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
        !report_stdout.contains("project-b--dead"),
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
        !fix_stdout.contains("project-b--dead"),
        "--fix with project-a active must not touch project-b's branch; got:\n{fix_stdout}"
    );

    // Branch still present after project-scoped --fix.
    let still_there = git()
        .args(["branch", "--list", "project-b--dead"])
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
        all_fix_stdout.contains("[fixed]") && all_fix_stdout.contains("project-b--dead"),
        "--all --fix must delete the branch weave-wide; got:\n{all_fix_stdout}"
    );

    // Branch gone after weave-wide --all --fix.
    let gone = git()
        .args(["branch", "--list", "project-b--dead"])
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
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());

    let repo_a = ws.join("github").join("acme").join("repo-a");
    let repo_b = ws.join("github").join("acme").join("repo-b");
    init_repo_with_commit(&repo_a);
    init_repo_with_commit(&repo_b);

    // Stale safe-class ephemeral branch in repo-b only. Flat, for the reason
    // the text-output twin above spells out.
    create_branch(&repo_b, "project-b--dead", "main");
    record_receipt(&ws, "project-b", "dead", &repo_b);
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
    let tmp = common::tempdir().unwrap();
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
// Symlinked member slots, and reference members as worktrees
//
// Older releases materialized a `reference` member as a symlink to the
// canonical clone, so through the link it sits on the canonical's shared
// non-ephemeral branch (e.g. `main`). `scan_repos_on_disk` discovers it
// because `is_dir()` follows the symlink; the I3 branch-discipline scan would
// mis-read it as a `shared-branch` violation. The scan must not read through
// it: it is unmigrated state, reported as `workweave-drift`.
//
// A `reference` member is a worktree on the workweave's ephemeral branch like
// any other member, and is checked like any other.
// ===========================================================================

/// A symlinked member slot must NOT fire a branch-discipline finding, even
/// though it resolves (through the symlink) to the canonical store on its
/// shared `main` branch. It is the canonical viewed through a link, not a
/// workweave checkout that wandered onto a shared branch.
#[test]
fn symlinked_reference_does_not_fire_shared_branch() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    // The canonical sits on `main` — exactly the branch a shared-branch
    // finding flags when a *worktree* checkout sits on it.

    let ww_dir = workweaves_dir(&ws).join("myproj--ref");
    write_marker(&ww_dir, &ws, "myproj", &ws);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    // The old reference-member shape: a symlink at the workweave checkout
    // pointing at the canonical clone (which is on `main`).
    repoweave::symlink::create(
        &canonical,
        &ww_checkout,
        repoweave::symlink::LinkTarget::Directory,
    )
    .unwrap();
    assert!(ww_checkout.is_symlink(), "fixture must be a symlink");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("shared-branch")
            && !stdout.contains("workweave checkout is on")
            && !stdout.contains("detached-HEAD")
            && !stdout.contains("foreign-ephemeral"),
        "a symlinked member slot must not fire any branch-discipline finding \
         (through the link it is the canonical on its `main`); got:\n{stdout}"
    );
}

/// A `reference` member is a *real worktree* on its own ephemeral branch, and
/// flows through the normal I2/I3 checks unchanged: healthy on its ephemeral
/// branch → clean, and (the adversarial half, below) if it wanders onto
/// `main`, it must STILL fire `shared-branch`. The skip keys on the slot
/// being a symlink, never on role.
#[test]
fn worktree_reference_on_ephemeral_branch_flows_through_normally() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    let ww_dir = workweaves_dir(&ws).join("myproj--wtref");
    write_marker(&ww_dir, &ws, "myproj", &ws);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    // A reference member: a real worktree on the workweave's ephemeral branch.
    worktree_add(&canonical, &ww_checkout, "myproj--wtref");
    assert!(
        !ww_checkout.is_symlink(),
        "a reference member must be a real worktree, not a symlink"
    );

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("workweave checkout is on")
            && !stdout.contains("detached-HEAD")
            && !stdout.contains("pre-flat"),
        "a worktree'd reference on its ephemeral branch should be clean; got:\n{stdout}"
    );
}

// ===========================================================================
// The migration pass and the flat-name cutover
// ===========================================================================
//
// The cutover is atomic by construction: `EphemeralRefName::mint` produces
// the flat name, the scanner asks `AttachedRef::is_minted` for the healthy
// case, and the delete path globs `is_this_workweaves_namespace` — all three
// derive from the same mint. There is no build in which one has moved and
// another has not, because nothing spells the shape independently any more.
// `healthy_workweave_ephemeral_branch_is_clean` (flat) and
// `unmigrated_ephemeral_branch_is_reported_and_renamed` (pre-flat) are the
// two ends of that: the first fails if the scanner still wants a segment,
// the second fails if it no longer recognizes one.

/// Read the `receipts` array's recorded tip for `ref_name`, if present.
fn receipt_created_at(primary: &Path, project: &str, ref_name: &str) -> Option<String> {
    let path = primary
        .join("projects")
        .join(project)
        .join(".rwv-workweave-index");
    let raw = std::fs::read_to_string(&path).ok()?;
    let index: serde_json::Value = serde_json::from_str(&raw).ok()?;
    index["receipts"].as_array()?.iter().find_map(|r| {
        (r["name"] == ref_name).then(|| r["created_at"].as_str().unwrap_or_default().to_owned())
    })
}

/// Record the primary's index for `project` so a hand-built workweave is
/// recognized as placed (what `rwv workweave create` writes alongside the
/// marker).
fn record_placement(primary: &Path, project: &str, workweave: &str, dir: &Path) {
    make_project(primary, project);
    repoweave::workweave_index::record_workweave(
        primary,
        &ProjectName::new(project).unwrap(),
        &repoweave::manifest::WorkweaveName::new(workweave).unwrap(),
        repoweave::path_spelling::CanonicalPath::of(dir),
    )
    .expect("placement recorded");
}

/// The head SHA of `rev` in `repo`.
fn rev(repo: &Path, r: &str) -> String {
    String::from_utf8_lossy(
        &git()
            .args(["rev-parse", r])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_owned()
}

/// The flat ref exists with no receipt. Reported, and `--fix`
/// adopts it at its observed tip.
///
/// This is the state a build that minted flat names before receipts existed
/// leaves behind, and — because `record_created` is a no-op on an existing
/// key — it is also what makes the whole pass re-runnable.
#[test]
fn unrecorded_flat_ref_is_reported_and_adopted() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    let ww_dir = workweaves_dir(&ws).join("myproj--feat-a");
    write_marker(&ww_dir, &ws, "myproj", &ws);
    record_placement(&ws, "myproj", "feat-a", &ww_dir);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    worktree_add(&canonical, &ww_checkout, "myproj--feat-a");
    add_commit(&ww_checkout, "work.txt", "operator work");
    let tip = rev(&ww_checkout, "HEAD");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("rwv holds no ownership receipt for it"),
        "doctor should report the unowned flat ref; got:\n{stdout}"
    );

    let fix = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let fix_stdout = String::from_utf8_lossy(&fix.stdout);
    assert!(
        fix_stdout.contains("adopted `myproj--feat-a`"),
        "--fix should announce the adoption; got:\n{fix_stdout}"
    );
    assert_eq!(
        receipt_created_at(&ws, "myproj", "myproj--feat-a").as_deref(),
        Some(tip.as_str()),
        "the receipt must record the OBSERVED tip"
    );

    // Idempotent, and the recorded tip does not drift when the branch moves.
    add_commit(&ww_checkout, "more.txt", "more work");
    let _ = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert_eq!(
        receipt_created_at(&ws, "myproj", "myproj--feat-a").as_deref(),
        Some(tip.as_str()),
        "a re-run must not re-stamp created_at onto a moved tip — that would \
         forge an Unmoved warrant over the operator's commits"
    );
}

// ===========================================================================
// Ownership receipts naming a pre-flat ref: `--fix` retracts the record and
// leaves the ref alone.
// ===========================================================================

/// A receipt whose name carries a `/` segment is a record rwv cannot have
/// produced: after the flat-name cutover every name it mints is flat. The
/// canonical-store check asks which live
/// workweave mints a recorded name, none mints a segmented one, and so the
/// branch reads as a leak rwv owns — which is the class `--fix` deletes
/// from. **The false record is what manufactures the deletion warrant.**
///
/// So it must be retracted, not acted on. This fixture is the deletion the
/// arm removes: a pre-flat branch at the store's own tip (trivially
/// `Merged`, i.e. safe class) with a receipt already written. Without the
/// retraction `--fix` destroys it.
#[test]
fn fix_retracts_a_pre_flat_receipt_instead_of_deleting_the_branch() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // No workweave `myproj--ghost` on disk: nothing mints this name.
    create_branch(&canonical, "myproj--ghost/main", "main");
    let ghost_tip = rev(&canonical, "myproj--ghost/main");
    record_receipt(&ws, "myproj", "ghost/main", &canonical);
    add_commit(&canonical, "advance.txt", "advance main");

    let fix = rwv()
        .args(["doctor", "--fix", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let fix_stdout = String::from_utf8_lossy(&fix.stdout).into_owned();

    assert!(
        !receipt_recorded(&ws, "myproj", "myproj--ghost/main"),
        "the receipt must be retracted; got:\n{fix_stdout}"
    );
    assert!(
        fix_stdout.contains("[fixed]") && fix_stdout.contains("myproj--ghost/main"),
        "and the retraction must be announced, naming the receipt; got:\n{fix_stdout}"
    );
    assert!(
        branch_exists(&canonical, "myproj--ghost/main"),
        "the branch must survive: retraction drops a record, it does not touch \
         the store; got:\n{fix_stdout}"
    );
    assert_eq!(
        rev(&canonical, "myproj--ghost/main"),
        ghost_tip,
        "and it must not have moved either"
    );

    // What the operator is left holding: an unowned ref. Visible, and under
    // R2 not rwv's to delete — which is the whole reason retraction is safe.
    let after = rwv()
        .args(["doctor", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let after_stdout = String::from_utf8_lossy(&after.stdout).into_owned();
    assert!(
        after_stdout.contains("stale-ephemeral-branch-unowned"),
        "the ref must fall back to the unowned class, not go quiet; got:\n{after_stdout}"
    );
    assert!(
        doctor_json_compact(&ws, true).contains("myproj--ghost/main"),
        "the unowned record must name the ref"
    );

    let index_path = ws
        .join("projects")
        .join("myproj")
        .join(".rwv-workweave-index");
    let after_first = std::fs::read(&index_path).expect("the index survives the run");
    let again = rwv()
        .args(["doctor", "--fix", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let again_stdout = String::from_utf8_lossy(&again.stdout).into_owned();
    assert_eq!(
        std::fs::read(&index_path).expect("the index survives the second run"),
        after_first,
        "a second `--fix` must not write the index again; got:\n{again_stdout}"
    );
    assert!(
        !again_stdout.contains("[fixed]"),
        "and it must have nothing left to fix; got:\n{again_stdout}"
    );
    assert!(
        branch_exists(&canonical, "myproj--ghost/main"),
        "the branch is still not rwv's to delete on the second pass either"
    );
}

/// This used to pin a liveness-guard safety net: [`EphemeralRefName::mint`]
/// did not validate its components, so a workweave *named* `a/b` minted
/// `p--a/b` — a segmented name that was a live workweave's own ref, which
/// retraction had to be taught not to disown (nothing re-adopts a placement
/// recorded by absolute path, outside every container scan).
///
/// That state is no longer reachable to retract-or-keep: `WorkweaveName::new`
/// now refuses a name containing `/` before a workweave can be created at
/// all, so `EphemeralRefName::mint` can no longer be called with one — the
/// guard has nothing left to defend. This pins the refusal at the point
/// that matters instead: the CLI verb itself.
#[test]
fn workweave_create_with_a_slash_in_the_name_is_refused() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let project_dir = ws.join("projects").join("myproj");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("rwv.toml"), "[repositories]\n").unwrap();
    let weaveroot = workweaves_dir(&ws);
    std::fs::create_dir_all(&weaveroot).unwrap();

    let create = rwv()
        .args([
            "workweave",
            "create",
            "--project",
            "myproj",
            "feat-a/nested",
        ])
        .current_dir(&ws)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&create.stderr);
    assert!(
        !create.status.success(),
        "a workweave name containing `/` must be refused, not silently \
         minted into a name that masquerades as a different workweave's \
         pre-flat ref; got:\n{stderr}"
    );
    assert!(
        stderr.contains("not a valid") && stderr.contains('/'),
        "the refusal should be the name-validation error naming the \
         offending character, not some other failure; got:\n{stderr}"
    );
    assert!(
        !weaveroot.join("myproj--feat-a").exists(),
        "no partial workweave directory must be left behind"
    );
}

// ===========================================================================
// Report path vs `--fix` skip guards, for the three auto-fixable kinds
//
// `fix_branch_model_migration` refuses on three pass rules of its own — an
// operation in flight, a workweave name that will not parse, and more than one
// ref under a workweave's namespace. Each is compared here against what the
// *report* path says in the same state, because a report that promises a
// repair the pass will skip is a remedy the operator cannot run.
// ===========================================================================

/// `rwv doctor --json` (with optional `--all`), re-serialized compact so a
/// substring probe for `"field":"value"` pairs is spacing-independent.
///
/// The per-item facts the text report used to spell inline (branch names,
/// receipt refs, op ids) live here since the text report collapsed
/// reclamation/frozen classes to per-class count lines.
fn doctor_json_compact(ws: &Path, all: bool) -> String {
    let mut args = vec!["doctor", "--json"];
    if all {
        args.insert(1, "--all");
    }
    let out = rwv().args(&args).current_dir(ws).output().unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json produced invalid JSON: {e}\noutput: {stdout}"));
    json.to_string()
}

/// The `sub_kind` discriminants doctor reports, sorted. An externally-tagged
/// enum puts the variant name in the sole key of the `sub_kind` object; a unit
/// variant is a bare string.
fn branch_discipline_sub_kinds(ws: &Path) -> Vec<String> {
    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json produced invalid JSON: {e}\noutput: {stdout}"));
    let mut kinds: Vec<String> = json["violations"]
        .as_array()
        .expect("violations is array")
        .iter()
        .filter(|v| v["kind"] == "branch-discipline")
        .map(|v| match &v["sub_kind"] {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Object(o) => o
                .keys()
                .next()
                .expect("a sub_kind object carries its variant name")
                .clone(),
            other => panic!("unexpected sub_kind shape: {other}"),
        })
        .collect();
    kinds.sort();
    kinds
}

/// Every `kind` doctor reports, sorted — used where the finding that names a
/// blocker is not a branch-discipline one.
fn doctor_kinds(ws: &Path) -> Vec<String> {
    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json produced invalid JSON: {e}\noutput: {stdout}"));
    let mut kinds: Vec<String> = json["violations"]
        .as_array()
        .expect("violations is array")
        .iter()
        .map(|v| {
            v["kind"]
                .as_str()
                .expect("a violation has a kind")
                .to_owned()
        })
        .collect();
    kinds.sort();
    kinds
}

// ===========================================================================
// The unregistered-directory name fallback, at the two places it is written
// down
//
// `doctor_scan_container` takes an unregistered directory's name half from the
// basename, and two `--fix` arms persist it: the registry adopt, and the
// branch-model migration's rename. The name half is read against the marker's
// own project, so a directory some *other* name rendered yields nothing rather
// than a plausible answer. Both fixtures below carry a correctly-named
// unregistered sibling as the reachability control: the fallback is what
// registers and migrates it, so the skip beside it is a decision and not a
// scan that stopped running.
// ===========================================================================

/// One project, two unregistered marker-bearing directories under the
/// container: `myproj--good`, which `myproj` renders, and `other--seat`, which
/// it does not. Each holds a project-repo checkout on its own pre-flat ref.
/// Returns `(ws, project_repo)`.
fn unregistered_dirs_one_named_for_another_project(tmp: &Path) -> (PathBuf, PathBuf) {
    let ws = make_primary(tmp);
    let project_repo = ws.join("projects").join("myproj");
    init_repo_with_commit(&project_repo);

    for (dir_name, branch) in [
        ("myproj--good", "myproj--good/project"),
        ("other--seat", "myproj--seat/project"),
    ] {
        let ww_dir = workweaves_dir(&ws).join(dir_name);
        write_marker(&ww_dir, &ws, "myproj", &ws);
        let ww_project = ww_dir.join("projects").join("myproj");
        std::fs::create_dir_all(ww_project.parent().unwrap()).unwrap();
        worktree_add(&project_repo, &ww_project, branch);
    }

    (ws, project_repo)
}

/// The names a project's registry records, sorted — the value the adopt arm
/// writes, read back as itself rather than probed for a substring. The index
/// also stores each workweave's path, which ends in the directory name, so a
/// `contains("seat")` would pass on the path of a directory that was never
/// adopted.
fn recorded_workweave_names(ws: &Path, project: &str) -> Vec<String> {
    repoweave::workweave_index::read(ws, &ProjectName::new(project).unwrap())
        .expect("index is readable")
        .map(|idx| idx.workweaves.into_keys().map(|n| n.to_string()).collect())
        .unwrap_or_default()
}

/// `doctor --fix` adopts an unregistered workweave under the name its own
/// project renders, and adopts nothing at all from a directory that project
/// never rendered.
///
/// The defect: read at the first `--`, `other--seat` offers the name `seat`,
/// and the adopt writes `myproj -> seat -> <container>/other--seat` into the
/// authoritative registry. Every later resolution then trusts it — `-w
/// myproj--seat` resolves to a directory spelled for somebody else, and a real
/// `myproj--seat` created afterwards collides with the entry.
#[test]
fn the_adopt_arm_registers_only_directories_the_marker_project_renders() {
    let tmp = common::tempdir().unwrap();
    let (ws, _project_repo) = unregistered_dirs_one_named_for_another_project(tmp.path());

    let fix = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let fix_stdout = String::from_utf8_lossy(&fix.stdout);

    assert_eq!(
        recorded_workweave_names(&ws, "myproj"),
        vec!["good".to_string()],
        "the adopt must register the directory `myproj` renders and nothing \
         else; got:\n{fix_stdout}"
    );

    let report = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&report.stdout);
    assert!(
        stdout.contains("intended name is not derivable"),
        "and the directory it declined to name must be reported rather than \
         silently skipped; got:\n{stdout}"
    );
}

/// The remaining pair in the matrix, and why it needs no report-side change:
/// the namespace guard cannot reach `unrecorded-ephemeral-branch`, because that
/// finding requires the flat ref to exist and git will not hold
/// `refs/heads/p--w` beside any `refs/heads/p--w/...`.
///
/// Measured against git rather than asserted, in both directions, so the
/// unreachability is a property of the tool and not of a reading of it.
#[test]
fn the_flat_ref_and_a_namespace_ref_cannot_coexist_so_the_skip_cannot_reach_unrecorded() {
    let tmp = common::tempdir().unwrap();
    let repo = init_repo_with_commit(&tmp.path().join("repo"));

    create_branch(&repo, "myproj--feat-a/main", "main");
    let blocked = git()
        .args(["branch", "myproj--feat-a", "main"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        !blocked.status.success(),
        "git must refuse the flat ref while a ref exists under its namespace; \
         it succeeded, which would make the two states co-occur"
    );

    let other = init_repo_with_commit(&tmp.path().join("other"));
    create_branch(&other, "myproj--feat-a", "main");
    let blocked_other = git()
        .args(["branch", "myproj--feat-a/main", "main"])
        .current_dir(&other)
        .output()
        .unwrap();
    assert!(
        !blocked_other.status.success(),
        "and must refuse a namespace ref while the flat ref exists"
    );
}

// ===========================================================================
// Report scope and repair scope read the same source: the marker.
//
// Identity is by record, never by name shape. The project is the marker's;
// the workweave name is the registry's when an entry names the path, and the
// directory basename's name half only for unregistered directories. A
// directory whose basename disagrees with those records is a `misnamed-dir`
// tree-integrity finding, not a shift of identity.
// ===========================================================================

/// Fixture: dir `proja--feat-x`, marker project `projb`, registered as
/// projb/feat-x, checkout on pre-flat `projb--feat-x/main` (the namespace the
/// records mint). Both projects exist. The state one `mv` produces from a
/// healthy projb workweave.
fn divergent_marker_fixture(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let ws = make_primary(tmp);
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    write_project_manifest(&ws, "proja", "github/acme/repo");
    write_project_manifest(&ws, "projb", "github/acme/repo");

    let ww_dir = workweaves_dir(&ws).join("proja--feat-x");
    write_marker(&ww_dir, &ws, "projb", &ws);
    record_placement(&ws, "projb", "feat-x", &ww_dir);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    worktree_add(&canonical, &ww_checkout, "projb--feat-x/main");

    (ws, canonical, ww_checkout)
}

/// The divergence this area was audited for, one level below the audited
/// guard table: the report's project-scope filter used to read the directory
/// basename while the repair's read the marker, so the same finding was
/// shown under one project and repaired under another.
///
/// With the dirname's project active: the finding is OUT of scope on both
/// sides — the report does not show it and `--fix` does not act. Before the
/// fix, the report showed the rename promise here and `--fix` skipped it.
///
/// **Mutation evidence**: revert `branch_discipline_in_scope`'s (a)-arm to
/// take the project from the directory basename's own left half instead of
/// from the marker it carries, and the first assertion reddens (the finding
/// reappears under proja while the repair still skips).
#[test]
fn divergent_marker_report_and_repair_agree_under_dirname_project() {
    let tmp = common::tempdir().unwrap();
    let (ws, canonical, _ck) = divergent_marker_fixture(tmp.path());
    set_active_project(&ws, "proja");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("renames it to `projb--feat-x`"),
        "the branch finding belongs to projb (the marker's project); it must \
         not surface under proja's scope where the repair would skip it; \
         got:\n{stdout}"
    );

    let _ = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(
        !branch_exists(&canonical, "projb--feat-x"),
        "and the repair agrees: nothing of projb's is migrated under proja"
    );
}

/// `--fix --all` must converge on the divergent state. Before the fix, the
/// orphan pass keyed on the DIRNAME's project while registry validation
/// keyed on the marker's, so every run adopted the workweave into the
/// dirname's project and every subsequent run pruned that entry as a
/// project-mismatch and re-adopted it — two `[fixed]` lines per run,
/// forever.
#[test]
fn divergent_marker_fix_converges_instead_of_prune_adopt_looping() {
    let tmp = common::tempdir().unwrap();
    let (ws, _canonical, _ck) = divergent_marker_fixture(tmp.path());
    set_active_project(&ws, "proja");

    let first = rwv()
        .args(["doctor", "--fix", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    assert!(
        !first_stdout.contains("into project `proja`'s registry"),
        "the orphan adopt must follow the marker, never the dirname; \
         got:\n{first_stdout}"
    );

    let second = rwv()
        .args(["doctor", "--fix", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        !second_stdout.contains("pruned stale registry entry `feat-x`")
            && !second_stdout.contains("adopted workweave `feat-x`"),
        "a second `--fix --all` must not keep pruning and re-adopting the \
         same workweave; got:\n{second_stdout}"
    );
}

/// The divergent directory is not silent: the tree-integrity scan reports
/// `misnamed-dir` naming the directory the records expect, and the control
/// half proves the finding clears when the name is restored.
#[test]
fn divergent_marker_dir_is_reported_as_misnamed_with_the_recorded_target() {
    let tmp = common::tempdir().unwrap();
    let (ws, _canonical, _ck) = divergent_marker_fixture(tmp.path());
    set_active_project(&ws, "projb");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("disagrees with its records")
            && stdout.contains("Rename the directory to `projb--feat-x`"),
        "the misnamed dir must be reported with the recorded target name; \
         got:\n{stdout}"
    );

    // Control: restore the recorded name and the finding clears. The
    // registry entry follows the move.
    let old_dir = workweaves_dir(&ws).join("proja--feat-x");
    let new_dir = workweaves_dir(&ws).join("projb--feat-x");
    std::fs::rename(&old_dir, &new_dir).unwrap();
    record_placement(&ws, "projb", "feat-x", &new_dir);

    let after = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let after_stdout = String::from_utf8_lossy(&after.stdout);
    assert!(
        !after_stdout.contains("disagrees with its records"),
        "restoring the recorded name must clear the finding; got:\n{after_stdout}"
    );
}

/// A registered workweave whose directory was renamed to a basename
/// `WorkweaveName::new` rejects (`feat--y` contains `--`) is still reported,
/// with the name the registry records as the one to rename it to.
#[test]
fn unparseable_dirname_with_registry_record_still_scans_and_reports() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    write_project_manifest(&ws, "proja", "github/acme/repo");
    set_active_project(&ws, "proja");

    let ww_dir = workweaves_dir(&ws).join("proja--feat--y");
    write_marker(&ww_dir, &ws, "proja", &ws);
    record_placement(&ws, "proja", "feat-y", &ww_dir);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    git_in(&canonical, &["checkout", "-b", "rwv-primary-tip", "-q"]);
    worktree_add_existing(&canonical, &ww_checkout, "main");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Rename the directory to `proja--feat-y`"),
        "and the rename itself must be reported with the recorded target; \
         got:\n{stdout}"
    );
}

/// The unrecoverable corner: unparseable basename AND no registry entry.
/// The scans cannot derive an identity to validate against, so they skip —
/// and `misnamed-dir` is the one signal left. It must fire, and it must not
/// be accompanied by an orphan-adopt offer (adopting an identity that does
/// not exist would register garbage).
#[test]
fn unparseable_unregistered_dirname_reports_misnamed_not_silence() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    write_project_manifest(&ws, "proja", "github/acme/repo");
    set_active_project(&ws, "proja");

    let ww_dir = workweaves_dir(&ws).join("proja--feat--y");
    write_marker(&ww_dir, &ws, "proja", &ws);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("disagrees with its records")
            && stdout.contains("intended name is not derivable"),
        "the unrecoverable rename must be reported rather than silent; \
         got:\n{stdout}"
    );
    assert!(
        !stdout.contains("not recorded in `.rwv-workweave-index`"),
        "and no orphan adopt may be offered for an identity that cannot be \
         derived; got:\n{stdout}"
    );
}

/// A name-half rename of a registered workweave (`projb--feat-x` moved to
/// `projb--feat-z`): the checkout on the recorded flat ref stays HEALTHY —
/// identity is the record, so the scan does not derive a new expectation
/// from the new basename and misreport the workweave's own branch as
/// foreign. The rename surfaces as `misnamed-dir` instead.
#[test]
fn name_half_rename_keeps_recorded_branch_healthy_and_reports_misnamed() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);
    write_project_manifest(&ws, "projb", "github/acme/repo");
    set_active_project(&ws, "projb");

    // Healthy flat-ref workweave, registered, receipted — then renamed.
    let ww_dir = workweaves_dir(&ws).join("projb--feat-z");
    write_marker(&ww_dir, &ws, "projb", &ws);
    record_placement(&ws, "projb", "feat-x", &ww_dir);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    worktree_add(&canonical, &ww_checkout, "projb--feat-x");
    record_receipt(&ws, "projb", "feat-x", &canonical);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("a ref rwv recorded for a different workweave")
            && !stdout.contains("workweave checkout is on"),
        "the checkout is on the branch its records own; the rename must not \
         make the scan call it foreign or shared; got:\n{stdout}"
    );
    assert!(
        stdout.contains("Rename the directory to `projb--feat-x`"),
        "the rename is the finding, with the recorded target; got:\n{stdout}"
    );
}

/// The sub-kinds of the findings that advise reattaching a detached
/// workweave checkout.
const DETACHED_SUB_KINDS: [&str; 3] = [
    "detached",
    "blocked-detached-namespace",
    "detached-off-ephemeral-branch",
];

fn detached_sub_kinds(ws: &Path) -> Vec<String> {
    branch_discipline_sub_kinds(ws)
        .into_iter()
        .filter(|k| DETACHED_SUB_KINDS.contains(&k.as_str()))
        .collect()
}

/// A `sync-to` parked on a member's rebase conflict leaves that checkout
/// detached mid-rebase. git refuses a branch switch there, and the op's own
/// exits are `--continue` and `rwv abort`, so doctor names no switch, from
/// either workspace, and reports the op instead.
#[test]
fn a_sync_to_parked_mid_rebase_draws_no_switch_advice() {
    const LIB: &str = "local/owner/lib";
    let tmp = common::tempdir().unwrap();
    let upstream = tmp.path().join("upstream/owner/lib");
    init_repo_with_commit(&upstream);

    let primary = tmp.path().join("ws");
    let primary_lib = common::under(&primary, LIB);
    let primary_project = primary.join("projects").join("app");
    git_in(
        tmp.path(),
        &[
            "clone",
            "-q",
            &common::file_url(&upstream),
            primary_lib.to_str().unwrap(),
        ],
    );
    std::fs::create_dir_all(&primary_project).unwrap();
    git_in(&primary_project, &["init", "-q", "-b", "main"]);
    std::fs::write(
        primary_project.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    std::fs::write(primary_project.join(".gitignore"), "*.code-workspace\n").unwrap();
    std::fs::write(
        primary_project.join("rwv.toml"),
        format!(
            "[repositories.\"{LIB}\"]\ntype = \"git\"\nurl = \"{}\"\nbranch = \"main\"\nrole = \"owned\"\n",
            common::file_url(&upstream)
        ),
    )
    .unwrap();
    std::fs::write(primary.join(".rwv-active"), "app\n").unwrap();
    common::record_trunk(&primary_project);
    rwv().arg("lock").current_dir(&primary).assert().success();
    git_in(&primary_project, &["add", "-A"]);
    git_in(&primary_project, &["commit", "-q", "-m", "initial"]);
    rwv()
        .args(["workweave", "create", "--project", "app", "s1"])
        .current_dir(&primary)
        .assert()
        .success();
    let seat = tmp.path().join(".workweaves").join("app--s1");
    let seat_lib = common::under(&seat, LIB);

    for (repo, text) in [(&seat_lib, "seat\n"), (&primary_lib, "primary\n")] {
        std::fs::write(repo.join("c.txt"), text).unwrap();
        git_in(repo, &["add", "c.txt"]);
        git_in(repo, &["commit", "-q", "-m", text.trim()]);
    }
    rwv().arg("lock").current_dir(&primary).assert().success();
    git_in(&primary_project, &["add", "rwv.lock"]);
    git_in(&primary_project, &["commit", "-q", "-m", "relock"]);

    let parked = rwv().arg("sync-to").current_dir(&seat).output().unwrap();
    assert!(
        !parked.status.success() && seat.join(".rwv-op").exists(),
        "the member conflict must park sync-to:\n{}",
        String::from_utf8_lossy(&parked.stderr)
    );
    let rebase_dir = git()
        .args(["rev-parse", "--git-path", "rebase-merge"])
        .current_dir(&seat_lib)
        .output()
        .unwrap();
    let rebase_dir = seat_lib.join(String::from_utf8_lossy(&rebase_dir.stdout).trim());
    assert!(rebase_dir.is_dir(), "the seat's member is mid-rebase");
    let attached = git()
        .args(["symbolic-ref", "-q", "HEAD"])
        .current_dir(&seat_lib)
        .status()
        .unwrap();
    assert!(!attached.success(), "and detached");

    for from in [&primary, &seat] {
        let report = rwv().arg("doctor").current_dir(from).output().unwrap();
        let report = String::from_utf8_lossy(&report.stdout);
        assert!(
            !report.contains("git switch"),
            "doctor in {} names a switch git refuses mid-rebase:\n{report}",
            from.display()
        );
        assert_eq!(detached_sub_kinds(from), Vec::<String>::new());
    }
    assert!(doctor_kinds(&seat).contains(&"stale-op-state".to_owned()));
}
