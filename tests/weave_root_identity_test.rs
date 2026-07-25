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
fn make_workspace(tmp: &Path, project: &str) -> PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.yaml"),
        format!(
            "repositories:\n  github/org/repo:\n    type: git\n    url: file://{}\n    \
             version: main\n    role: owned\n",
            repo_path.display()
        ),
    )
    .unwrap();

    ws
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

// ---------------------------------------------------------------------------
// 1. Production — nothing writes a pointer into a workweave root
// ---------------------------------------------------------------------------

/// `rwv workweave <project> create <name>` leaves the marker and no pointer.
#[test]
fn workweave_create_writes_the_marker_and_no_pointer() {
    let tmp = tempfile::tempdir().unwrap();
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
    let tmp = tempfile::tempdir().unwrap();
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
    let tmp = tempfile::tempdir().unwrap();
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

/// The primary root keeps its pointer — the rule removes the workweave copy,
/// not the selector itself.
#[test]
fn activate_at_primary_still_writes_the_pointer() {
    let tmp = tempfile::tempdir().unwrap();
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
    let tmp = tempfile::tempdir().unwrap();
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

// ---------------------------------------------------------------------------
// 3. Enforcement — doctor reports both, --fix clears only the witnessed arm
// ---------------------------------------------------------------------------

/// A registered workweave that acquired a pointer (what every workweave made
/// by a pre-exclusivity build looks like) is reported, and `--fix` clears the
/// pointer while leaving the marker.
#[test]
fn doctor_fix_clears_a_registered_workweaves_stray_pointer() {
    let tmp = tempfile::tempdir().unwrap();
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
}

/// A tree carrying both files that the registry does not name — what an
/// out-of-band `cp -r` of a workweave produces — is report-only. Its identity
/// is disputed, and deleting either file would be a guess.
#[test]
fn doctor_fix_leaves_an_unregistered_both_files_tree_alone() {
    let tmp = tempfile::tempdir().unwrap();
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

/// The `--json` channel carries the finding with its sub-kind, so a caller can
/// tell the fixable arm from the report-only one without parsing prose.
#[test]
fn doctor_json_carries_the_conflict_and_its_sub_kind() {
    let tmp = tempfile::tempdir().unwrap();
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

/// A clean workspace produces no finding — neither root carries both files.
#[test]
fn clean_workspace_reports_no_conflict() {
    let tmp = tempfile::tempdir().unwrap();
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
