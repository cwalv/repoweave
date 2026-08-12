//! `.rwv-active` and `.rwv-workweave` are mutually exclusive.
//!
//! The two files name the same fact — which project a tree belongs to — and
//! occupy ONE tier of the project-resolution chain:
//!
//!     --project > -w prefix > (.rwv-active | .rwv-workweave)
//!
//! A primary root carries the pointer; a workweave root carries the marker;
//! never both. This file pins the three halves of that:
//!
//!   1. **Production.** No rwv verb writes a pointer into a workweave root,
//!      and `rwv activate` — the verb whose whole job is writing one —
//!      refuses to run there at all. Both write paths are covered: the
//!      explicit one `create_workweave` used to have, and the one inside
//!      `activate_at`'s Context mode that `activate_workweave` reaches. The
//!      second is the reason the first is not enough on its own.
//!   2. **Consumption.** Removing the pointer from a workweave root must not
//!      silently disable surfacing. `activate_at`'s intent-mode gate asks
//!      "is the target already the project this root presents?", and until
//!      this change it asked `.rwv-active` — which in a workweave answers
//!      `None` once the pointer is gone, so every intent verb (`add`,
//!      `remove`, `update`) would early-return before `surface_symlinks`.
//!      The gate now reads whichever file governs the root.
//!   3. **Enforcement.** `rwv doctor` reports a root carrying both, and
//!      `--fix` clears exactly the arm that external evidence proves is
//!      redundant — no more.
//!
//! Point 3's asymmetry is the load-bearing part. "The workweave's pointer is
//! the stray" presumes we know the tree is a workweave, and the marker's
//! presence is the only witness of that: primary roots and workweave roots
//! are structurally identical (both hold `projects/` and registry dirs). So
//! the discriminator has to be evidence the tree does not contain — the
//! primary-side `.rwv-workweave-index`, which names every workweave `rwv
//! workweave create` made. A tree that index does not name gets reported and
//! left alone, because deleting the wrong one of two files destroys operator
//! state (the marker carries `primary` and `parent` values that exist
//! nowhere else).

use std::path::{Path, PathBuf};
use std::process;

mod common;

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {args:?} in {} failed", dir.display());
}

fn git_out(args: &[&str], dir: &Path) -> String {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should be available");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed",
        dir.display()
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
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

/// A workspace with one project and one owned repo, at `{tmp}/ws`.
///
/// Workweaves land in `{tmp}/.workweaves/` (the default container is a
/// sibling of the weave root).
///
/// The member repo carries no `origin` — the manifest URL names its own
/// directory — and the project directory is a plain directory, not a git
/// repo. `rwv update`'s fetch has nothing to resolve against, and `rwv
/// doctor --fix`'s merge-driver plant on the project repo errors out on a
/// directory that is not one. A test that needs either verb wants
/// `make_workspace_with_remote_and_project_repo` instead.
fn make_workspace(tmp: &Path, project: &str) -> PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories.\"github/org/repo\"]\ntype = \"git\"\nurl = \"file://{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            repo_path.display()
        ),
    )
    .unwrap();

    ws
}

/// A workspace like [`make_workspace`], but built so the full verb surface
/// can run against it.
///
/// The member repo is a clone of a separate upstream rather than a
/// self-contained directory, so it carries a real `origin` with something to
/// fetch. The project directory is itself a committed git repo, so
/// `create_workweave` forks a worktree of it rather than copying it, which is
/// what lets `rwv doctor --fix` plant its merge driver instead of erroring on
/// a directory that is not a repository at all.
///
/// Returns `(workspace_root, upstream_repo)`. The upstream is returned
/// separately from `workspace_root/github/org/repo` (the clone) so a caller
/// can push new commits there for `rwv update` to fetch — pushing into the
/// clone itself would just move its own checked-out branch, not exercise a
/// fetch.
fn make_workspace_with_remote_and_project_repo(tmp: &Path, project: &str) -> (PathBuf, PathBuf) {
    let upstream = tmp.join("upstream/github/org/repo");
    init_repo_with_commit(&upstream);

    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    std::fs::create_dir_all(repo_path.parent().unwrap()).unwrap();
    git(
        &[
            "clone",
            &upstream.display().to_string(),
            &repo_path.display().to_string(),
        ],
        tmp,
    );
    git(&["config", "user.email", "test@test.com"], &repo_path);
    git(&["config", "user.name", "Test"], &repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories.\"github/org/repo\"]\ntype = \"git\"\nurl = \"file://{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            upstream.display()
        ),
    )
    .unwrap();
    git(&["init", "--initial-branch=main"], &project_dir);
    git(&["config", "user.email", "test@test.com"], &project_dir);
    git(&["config", "user.name", "Test"], &project_dir);
    git(&["add", "-A"], &project_dir);
    git(&["commit", "-m", "add manifest"], &project_dir);

    (ws, upstream)
}

fn create_workweave(ws: &Path, project: &str, name: &str) -> PathBuf {
    rwv()
        .args(["workweave", project, "create", name])
        .current_dir(ws)
        .assert()
        .success();
    ws.parent()
        .unwrap()
        .join(".workweaves")
        .join(format!("{project}--{name}"))
}

fn has_pointer(root: &Path) -> bool {
    root.join(".rwv-active").exists()
}

fn has_marker(root: &Path) -> bool {
    root.join(".rwv-workweave").exists()
}

/// A hand-written marker whose `primary:` field a real `create_workweave`
/// would never leave dangling — the shape needed to construct a
/// `MarkerDefect::DanglingPrimary` fixture without an actual moved workspace.
fn write_marker(dir: &Path, primary: &Path, project: &str, parent: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(".rwv-workweave"),
        format!(
            "{{\"primary\":\"{}\",\"project\":\"{project}\",\"parent\":\"{}\"}}",
            primary.display(),
            parent.display()
        ),
    )
    .unwrap();
}

/// A marker written before `parent:` became required — `MarkerDefect::Legacy`.
fn write_legacy_marker(dir: &Path, primary: &Path, project: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(".rwv-workweave"),
        format!("primary: {}\nproject: {project}\n", primary.display()),
    )
    .unwrap();
}

/// A marker file that fails to parse — `MarkerDefect::Unreadable`.
fn write_unreadable_marker(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(".rwv-workweave"), "primary: [unclosed\n").unwrap();
}

// ---------------------------------------------------------------------------
// 1. Production — nothing writes a pointer into a workweave root
// ---------------------------------------------------------------------------

/// `rwv workweave <project> create <name>` leaves the marker and no pointer.
#[test]
fn workweave_create_writes_the_marker_and_no_pointer() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "demo");
    let ww = create_workweave(&ws, "demo", "w1");

    assert!(
        has_marker(&ww),
        "workweave root must carry the `.rwv-workweave` marker"
    );
    assert!(
        !has_pointer(&ww),
        "workweave root must NOT carry `.rwv-active`: the marker already names \
         the project, and the two files are mutually exclusive"
    );
}

/// The pointer write inside `activate_at`'s Context mode is the second write
/// path, reached from `create_workweave` via `activate_workweave`. Deleting
/// the explicit `set_active_project` call from `create_workweave` does not
/// close it, so a test that only ran `create` and stopped would pass with
/// that path still live in a build that reintroduced it.
///
/// Re-running the surfacing pass over an existing workweave root drives that
/// path on its own. `rwv add` inside the workweave is a verb that reaches
/// activation with the workweave dir as its root.
#[test]
fn activation_inside_a_workweave_does_not_write_a_pointer() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "demo");
    let ww = create_workweave(&ws, "demo", "w1");

    let second = ws.join("github/org/repo2");
    init_repo_with_commit(&second);
    rwv()
        .args([
            "add",
            &format!("file://{}", second.display()),
            "--role",
            "owned",
        ])
        .current_dir(&ww)
        .assert()
        .success();

    assert!(
        !has_pointer(&ww),
        "an intent verb run inside a workweave must not leave a `.rwv-active` \
         behind: selection is primary-only"
    );
}

/// `rwv activate` — the verb whose entire job is writing the pointer —
/// refuses inside a workweave rather than writing one or silently
/// retargeting primary. The exclusivity rule depends on there being no verb
/// that puts a pointer under a marker.
#[test]
fn activate_refuses_inside_a_workweave() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "demo");
    let ww = create_workweave(&ws, "demo", "w1");

    let out = rwv()
        .args(["activate", "demo"])
        .current_dir(&ww)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("has no effect in a workweave"),
        "expected a refusal naming the workweave; got: {stderr}"
    );
    assert!(
        !has_pointer(&ww),
        "the refusal must not leave a pointer behind"
    );
}

/// The same refusal via `-w` addressing, which reaches the workweave without
/// standing in it. The guard is the absence of a selection witness on the
/// resolved context, so it cannot depend on how the context was addressed —
/// but that is an argument, and this is the observation.
#[test]
fn activate_refuses_a_workweave_addressed_by_flag() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "demo");
    let ww = create_workweave(&ws, "demo", "w1");

    let out = rwv()
        .args(["-w", "demo--w1", "activate", "demo"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("has no effect in a workweave"),
        "expected a refusal naming the workweave; got: {stderr}"
    );
    assert!(
        !has_pointer(&ww),
        "the refusal must not leave a pointer behind"
    );
}

/// The primary root keeps its pointer — the rule removes the workweave copy,
/// not the selector itself.
#[test]
fn activate_at_primary_still_writes_the_pointer() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "demo");

    rwv()
        .args(["activate", "demo"])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        has_pointer(&ws),
        "primary root must still carry `.rwv-active` after `rwv activate`"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join(".rwv-active"))
            .unwrap()
            .trim(),
        "demo"
    );
    assert!(
        !has_marker(&ws),
        "primary root must not carry a `.rwv-workweave` marker"
    );
}

// ---------------------------------------------------------------------------
// 2. Consumption — removing the pointer must not disable surfacing
// ---------------------------------------------------------------------------

/// Regression guard for the reader half.
///
/// `activate_at` gates its surfacing step on "is the target already the
/// project this root presents?". Answering that from `.rwv-active` is wrong
/// in a workweave, which no longer has one: the gate would see `None`, take
/// the early return, and skip `surface_symlinks` — leaving the ecosystem
/// workspace file unsurfaced at the workweave root with no error anywhere.
///
/// Asserts the surfacing symlink is present after an intent verb, which is
/// the observable the early return would remove.
#[test]
fn intent_verbs_still_surface_inside_a_pointerless_workweave() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "demo");
    let ww = create_workweave(&ws, "demo", "w1");

    // Clear the surfacing so the assertion below can only pass if the verb
    // re-created it.
    let surfaced = ww.join("demo.code-workspace");
    let _ = std::fs::remove_file(&surfaced);

    let second = ws.join("github/org/repo2");
    init_repo_with_commit(&second);
    rwv()
        .args([
            "add",
            &format!("file://{}", second.display()),
            "--role",
            "owned",
        ])
        .current_dir(&ww)
        .assert()
        .success();

    assert!(
        surfaced.symlink_metadata().is_ok(),
        "`rwv add` inside a workweave must still surface \
         `demo.code-workspace` at the workweave root; a missing symlink here \
         means the intent-mode gate read `.rwv-active` (absent in a \
         workweave) instead of the file that governs this root"
    );
}

/// `rwv update` reaches the same gate by a different route than `rwv add` —
/// through `update_for_project`, which also hands the surfacing set the
/// container kind off its own resolved checkout. Both halves are observable
/// here: a wrong gate answer removes the symlink, a wrong kind changes what
/// the integration set declares.
///
/// Pushes a real commit to the upstream and checks it landed in the
/// workweave's checkout: a self-referential `origin` would let `rwv update`
/// exit 0 on a fetch that resolved nothing new, which would pass this test
/// even if the run never advanced anything at all.
#[test]
fn update_still_surfaces_inside_a_pointerless_workweave() {
    let tmp = common::tempdir().unwrap();
    let (ws, upstream) = make_workspace_with_remote_and_project_repo(tmp.path(), "demo");

    let ww = create_workweave(&ws, "demo", "w1");

    std::fs::write(upstream.join("NEW"), "new content\n").unwrap();
    git(&["add", "NEW"], &upstream);
    git(&["commit", "-m", "new upstream commit"], &upstream);
    let upstream_head = git_out(&["rev-parse", "HEAD"], &upstream);

    let surfaced = ww.join("demo.code-workspace");
    let _ = std::fs::remove_file(&surfaced);

    rwv().args(["update"]).current_dir(&ww).assert().success();

    let clone = ww.join("github/org/repo");
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &clone),
        upstream_head,
        "`rwv update` must fast-forward the workweave's checkout to the \
         upstream tip"
    );
    assert!(
        clone.join("NEW").exists(),
        "the fetched commit's content must be checked out, not just referenced"
    );
    assert!(
        !has_pointer(&ww),
        "`rwv update` inside a workweave must not leave a `.rwv-active` behind"
    );
    assert!(
        surfaced.symlink_metadata().is_ok(),
        "`rwv update` inside a workweave must still surface \
         `demo.code-workspace` at the workweave root"
    );
}

/// `doctor --fix`'s SURFACING repair is a third route to the same question,
/// and the one with no `rwv.toml` edit behind it: it re-runs the surfacing
/// primitive against the weave it scanned, deciding what to expect from what
/// that root presents. A workweave carries no pointer, so a repair that asked
/// for one would find the root presents nothing, expect no symlinks, and
/// report a clean tree over a broken one.
///
/// The sibling content-repair arm is pinned by
/// `doctor_workweave_content_fix_isolation_test`, which asserts the
/// regenerated file lands in the workweave's own project dir.
#[test]
fn doctor_fix_repairs_surfacing_inside_a_pointerless_workweave() {
    let tmp = common::tempdir().unwrap();
    // The project dir has to be a git repo before the fork, so the workweave
    // gets a worktree of it rather than a plain directory — `doctor --fix`
    // plants a merge driver in the project repo and errors out on anything
    // else, which would mask the arm under test.
    let (ws, _upstream) = make_workspace_with_remote_and_project_repo(tmp.path(), "demo");

    let ww = create_workweave(&ws, "demo", "w1");

    // Creation surfaces only files that already exist in the project dir, so
    // the first repair is what authors the ecosystem file and links it. The
    // second is the arm under test.
    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ww)
        .assert()
        .success();

    let surfaced = ww.join("demo.code-workspace");
    assert!(
        surfaced.symlink_metadata().is_ok(),
        "`rwv doctor --fix` should have authored and surfaced \
         `demo.code-workspace` in the workweave"
    );
    std::fs::remove_file(&surfaced).unwrap();

    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ww)
        .assert()
        .success();

    assert!(
        surfaced.symlink_metadata().is_ok(),
        "`rwv doctor --fix` inside a workweave must re-surface \
         `demo.code-workspace`; a still-missing symlink means the repair \
         read `.rwv-active` to decide what this root presents"
    );
    assert!(
        !has_pointer(&ww),
        "the repair must not author a pointer into the workweave root"
    );
}

// ---------------------------------------------------------------------------
// 3. Enforcement — doctor reports both, --fix clears only the witnessed arm
// ---------------------------------------------------------------------------

/// A registered workweave that acquired a pointer (what every workweave made
/// by a pre-exclusivity build looks like) is reported, and `--fix` clears the
/// pointer while leaving the marker.
#[test]
fn doctor_fix_clears_a_registered_workweaves_stray_pointer() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "demo");
    let ww = create_workweave(&ws, "demo", "w1");

    // Recreate the pre-exclusivity on-disk state.
    std::fs::write(ww.join(".rwv-active"), "demo\n").unwrap();

    let out = rwv().arg("doctor").current_dir(&ws).output().unwrap();
    let report =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        report.contains("mutually exclusive") && report.contains("w1"),
        "doctor must report the conflict and name the workweave; got: {report}"
    );

    let out = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let fixed =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        fixed.contains("[fixed]") && fixed.contains(".rwv-active"),
        "--fix must report what it deleted; got: {fixed}"
    );

    assert!(
        !has_pointer(&ww),
        "--fix must delete the redundant pointer at a registered workweave"
    );
    assert!(
        has_marker(&ww),
        "--fix must leave the marker: it carries `primary` and `parent` \
         values that exist nowhere else"
    );

    // Idempotency: the pointer is gone, so a second run finds nothing left
    // to fix — `--fix` deleting an absent file is not itself an error, but
    // the finding it repairs must not resurface.
    let out = rwv().arg("doctor").current_dir(&ws).output().unwrap();
    let report =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        !report.contains("mutually exclusive"),
        "a second doctor run after --fix must report the root clean; got: {report}"
    );
}

/// Two workspaces sharing one workweave container: `--fix` run at workspace A
/// must not touch a conflicted root belonging to workspace B.
///
/// This is a real on-disk shape, not a hypothetical — a single
/// `~/weaveroot/.workweaves/` holding the workweaves of several primaries is
/// the default layout, since the default container is a sibling of the weave
/// root. The scan enumerates that shared container, so B's roots are in front
/// of A's `--fix`, and only the marker-names-this-primary test keeps A off
/// them. B's registry is the authority for B's trees, and A does not read it.
#[test]
fn doctor_fix_ignores_a_conflicted_root_of_another_workspace() {
    let tmp = common::tempdir().unwrap();
    let ws_a = make_workspace(tmp.path(), "alpha");

    // A second primary whose default container is the same directory.
    let ws_b = tmp.path().join("ws-b");
    let repo_b = ws_b.join("github/org/repo");
    init_repo_with_commit(&repo_b);
    let proj_b = ws_b.join("projects/beta");
    std::fs::create_dir_all(&proj_b).unwrap();
    std::fs::write(
        proj_b.join("rwv.toml"),
        format!(
            "[repositories.\"github/org/repo\"]\ntype = \"git\"\nurl = \"file://{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            repo_b.display()
        ),
    )
    .unwrap();
    rwv()
        .args(["workweave", "beta", "create", "b1"])
        .current_dir(&ws_b)
        .assert()
        .success();
    let ww_b = tmp.path().join(".workweaves/beta--b1");
    assert!(ww_b.is_dir(), "both workspaces should share the container");

    // B's workweave acquires a stray pointer.
    std::fs::write(ww_b.join(".rwv-active"), "beta\n").unwrap();

    let out = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws_a)
        .output()
        .unwrap();
    let report =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);

    assert!(
        has_pointer(&ww_b),
        "`--fix` at workspace A must not clear a pointer in a workweave whose \
         marker names workspace B: A's registry has no say over B's trees"
    );
    assert!(has_marker(&ww_b), "and must not touch B's marker either");

    // A's report must EXPLAIN it correctly. Not touching the tree is the
    // easy half — the registry lookup would decline it anyway, since A's
    // index for `beta` does not exist. What the foreign-primary test adds is
    // the reason the operator is given. Without it the finding falls through
    // to the unregistered arm and blames `cp -r`, which is a false statement
    // about a workweave that is simply another workspace's.
    assert!(
        report.contains("which is not this workspace"),
        "A must report B's tree as belonging to another primary; got: {report}"
    );
    assert!(
        !report.contains("copied out-of-band"),
        "A must NOT blame an out-of-band copy for a tree that is another \
         workspace's workweave; got: {report}"
    );

    // Run from B, the same root IS repairable — the classification turns on
    // whose registry vouches for the tree, not on the tree itself.
    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws_b)
        .output()
        .unwrap();
    assert!(
        !has_pointer(&ww_b),
        "`--fix` at workspace B must clear it: B's registry records this \
         directory"
    );
    assert!(has_marker(&ww_b), "the marker survives the fix");
}

/// A tree carrying both files that the registry does not name — what an
/// out-of-band `cp -r` of a workweave produces — is report-only. Its identity
/// is disputed, and deleting either file would be a guess.
#[test]
fn doctor_fix_leaves_an_unregistered_both_files_tree_alone() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "demo");
    let ww = create_workweave(&ws, "demo", "w1");

    // Copy the workweave out-of-band. The copy carries both the marker and
    // (as of this fixture) a pointer; the registry still names only `w1`.
    let copy = ww.parent().unwrap().join("demo--copy");
    copy_dir(&ww, &copy);
    std::fs::write(copy.join(".rwv-active"), "demo\n").unwrap();

    let out = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let report =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        report.contains("demo--copy") && report.contains("copied out-of-band"),
        "doctor must report the copy and name its likely cause; got: {report}"
    );

    assert!(
        has_pointer(&copy),
        "--fix must NOT delete the pointer of a tree the registry does not \
         name: nothing outside the tree says which file is the stray"
    );
    assert!(has_marker(&copy), "--fix must NOT delete the marker either");
}

/// A tree that is itself the registry's home — it holds
/// `projects/<project>/.rwv-workweave-index` — yet also carries a
/// `.rwv-workweave` marker is report-only, the same as an unregistered copy.
/// The registry it holds names entries elsewhere (the real workweave `w1`),
/// never itself, so the reverse lookup this root would need to be
/// `--fix`-eligible comes back empty.
#[test]
fn doctor_fix_leaves_a_registry_holding_root_alone() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "demo");
    create_workweave(&ws, "demo", "w1");

    // `ws` already holds `projects/demo/.rwv-workweave-index` from the create
    // above. Giving it a marker of its own recreates a hand-added-marker (or
    // primary-copied-onto-a-workweave) accident without touching the real
    // registry.
    std::fs::write(
        ws.join(".rwv-workweave"),
        format!(
            "{{\"primary\":\"{}\",\"project\":\"demo\",\"parent\":\"{}\"}}",
            ws.display(),
            ws.display()
        ),
    )
    .unwrap();
    std::fs::write(ws.join(".rwv-active"), "demo\n").unwrap();

    let out = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let report =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        report.contains("mutually exclusive"),
        "doctor must report the conflict on the registry-holding root; got: {report}"
    );

    assert!(
        has_pointer(&ws),
        "--fix must NOT delete the pointer: this root's own registry names \
         `w1` elsewhere, never itself, so the reverse lookup finds no entry"
    );
    assert!(has_marker(&ws), "--fix must NOT delete the marker either");
}

/// Doctor and status are the two verbs exempt from the hard refusal a
/// disputed root gives every other verb, so doctor invoked *from inside* a
/// copied both-present tree must still classify it, rather than erroring
/// before the scan even runs.
#[test]
fn doctor_run_from_inside_the_copy_classifies_its_own_root() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "demo");
    let ww = create_workweave(&ws, "demo", "w1");

    let copy = ww.parent().unwrap().join("demo--copy");
    copy_dir(&ww, &copy);
    std::fs::write(copy.join(".rwv-active"), "demo\n").unwrap();

    let out = rwv().arg("doctor").current_dir(&copy).output().unwrap();
    let report =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        report.contains("mutually exclusive") && report.contains("demo--copy"),
        "doctor run from inside the disputed root itself must still resolve \
         and classify it, not refuse before the scan starts; got: {report}"
    );
}

/// The `--json` channel carries the finding with its sub-kind, so a caller can
/// tell the fixable arm from the report-only one without parsing prose.
#[test]
fn doctor_json_carries_the_conflict_and_its_sub_kind() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "demo");
    let ww = create_workweave(&ws, "demo", "w1");
    std::fs::write(ww.join(".rwv-active"), "demo\n").unwrap();

    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json must emit JSON ({e}); got: {stdout}"));

    let finding = doc["violations"]
        .as_array()
        .expect("violations array")
        .iter()
        .find(|v| v["kind"] == "weave-root-identity-conflict")
        .unwrap_or_else(|| {
            panic!("expected a weave-root-identity-conflict violation; got: {stdout}")
        });

    assert_eq!(finding["pointer_project"], "demo");
    assert_eq!(
        finding["sub_kind"]["registered-workweave"]["workweave_name"], "w1",
        "the fixable arm must name the registry entry that witnesses it"
    );
}

// ---------------------------------------------------------------------------
// 4. A marker that cannot witness itself — its own sub-kind, not a flattened
//    `unwitnessed` string
// ---------------------------------------------------------------------------

/// Each `MarkerDefect` produces its own finding, both in the text report and
/// under its own `--json` tag. A check that only asserted "a finding was
/// produced" would pass on the flattening this sub-kind replaces, where an
/// unreadable marker, a legacy marker, and a dangling `primary:` all reported
/// the same generic detail string.
#[test]
fn doctor_reports_a_distinct_marker_unverifiable_finding_per_defect() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "demo");
    let ww = create_workweave(&ws, "demo", "w1");
    let container = ww.parent().unwrap();

    let gone = tmp.path().join("nowhere");
    let dangling = container.join("demo--dangling");
    write_marker(&dangling, &gone, "demo", &gone);
    std::fs::write(dangling.join(".rwv-active"), "demo\n").unwrap();

    let legacy = container.join("demo--legacy");
    write_legacy_marker(&legacy, &ws, "demo");
    std::fs::write(legacy.join(".rwv-active"), "demo\n").unwrap();

    let unreadable = container.join("demo--unreadable");
    write_unreadable_marker(&unreadable);
    std::fs::write(unreadable.join(".rwv-active"), "demo\n").unwrap();

    let out = rwv().arg("doctor").current_dir(&ws).output().unwrap();
    let report =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    let line_for = |needle: &str| -> &str {
        report
            .lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("expected a line naming `{needle}`; got: {report}"))
    };
    let dangling_line = line_for("demo--dangling");
    let legacy_line = line_for("demo--legacy");
    let unreadable_line = line_for("demo--unreadable");

    assert!(
        dangling_line.contains("is not a repoweave workspace root"),
        "got: {dangling_line}"
    );
    assert!(
        legacy_line.contains("missing the required `parent:` field"),
        "got: {legacy_line}"
    );
    assert!(
        unreadable_line.contains("failed to parse"),
        "got: {unreadable_line}"
    );
    assert_ne!(
        dangling_line, legacy_line,
        "each defect names its own cause"
    );
    assert_ne!(
        legacy_line, unreadable_line,
        "each defect names its own cause"
    );
    assert_ne!(
        dangling_line, unreadable_line,
        "each defect names its own cause"
    );

    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json must emit JSON ({e}); got: {stdout}"));
    let defect_tag = |root: &Path| -> String {
        let canonical = root.canonicalize().unwrap();
        let finding = doc["violations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| {
                v["kind"] == "weave-root-identity-conflict"
                    && v["root"] == canonical.to_string_lossy().as_ref()
            })
            .unwrap_or_else(|| panic!("expected a finding for {}; got: {stdout}", root.display()));
        match &finding["sub_kind"]["marker-unverifiable"]["defect"] {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Object(o) => o.keys().next().unwrap().clone(),
            other => panic!("unexpected defect shape: {other:?}"),
        }
    };
    assert_eq!(defect_tag(&dangling), "dangling-primary");
    assert_eq!(defect_tag(&legacy), "legacy");
    assert_eq!(defect_tag(&unreadable), "unreadable");
}

/// `MarkerUnverifiable` is never auto-fixable: a marker that cannot witness
/// its own claim cannot witness which of the two files is the stray, so
/// `--fix` must leave both alone until the marker itself is repaired. Pinned
/// directly rather than left to the prohibition's comment on
/// `fix_disposition`.
#[test]
fn doctor_fix_never_touches_a_marker_unverifiable_root() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "demo");
    let ww = create_workweave(&ws, "demo", "w1");
    let container = ww.parent().unwrap();

    let gone = tmp.path().join("nowhere");
    let dangling = container.join("demo--dangling");
    write_marker(&dangling, &gone, "demo", &gone);
    std::fs::write(dangling.join(".rwv-active"), "demo\n").unwrap();

    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();

    assert!(
        has_pointer(&dangling),
        "--fix must not delete the pointer at a root whose marker cannot be \
         witnessed"
    );
    assert!(
        has_marker(&dangling),
        "--fix must not touch the marker either — repairing it is a separate step"
    );
}

/// A clean workspace produces no finding — neither root carries both files.
#[test]
fn clean_workspace_reports_no_conflict() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "demo");
    rwv()
        .args(["activate", "demo"])
        .current_dir(&ws)
        .assert()
        .success();
    create_workweave(&ws, "demo", "w1");

    let out = rwv().arg("doctor").current_dir(&ws).output().unwrap();
    let report =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        !report.contains("mutually exclusive"),
        "a workspace whose primary carries only the pointer and whose \
         workweave carries only the marker must produce no conflict; got: \
         {report}"
    );
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let meta = src.symlink_metadata().unwrap();
        if meta.file_type().is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(&src).unwrap(), &dst).unwrap();
        } else if meta.is_dir() {
            copy_dir(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}
