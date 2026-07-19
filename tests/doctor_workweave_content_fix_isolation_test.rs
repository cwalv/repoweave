//! Regression test: `rwv doctor --fix` invoked from INSIDE a workweave must
//! not mutate any file under the PRIMARY weave's project directory.
//!
//! Before the fix, the content-fix path in `run_check` called
//! `activate::activate_intent(project, workspace_dir)` where `workspace_dir`
//! is the workweave dir. Inside `activate_intent`, `WorkspaceContext::resolve`
//! walked back up to the primary root and `activate_at` regenerated the
//! project's managed files at `primary/projects/<project>/`. Any fixable
//! managed-file drift finding in a workweave silently rewrote primary state.
//!
//! The surfacing-fix path already special-cased the workweave by re-running
//! the surfacing primitive against the workweave dir directly. The content-fix
//! path now follows the same principle: when doctor runs inside a workweave,
//! it dispatches to `activate_workweave_intent` (which authors into the
//! workweave's own `projects/<project>/` and skips install hooks) instead of
//! `activate_intent`.
//!
//! The test:
//!   1. Builds a scratch primary weave with the `go-work` integration active
//!      (two go modules → managed `go.work`).
//!   2. Activates the primary — writes the primary's canonical `go.work` and
//!      surfaces symlinks.
//!   3. Creates a workweave off it. The workweave gets its own project
//!      worktree at `<ww>/projects/<project>/` with its own `go.work`.
//!   4. Injects DRIFT into primary's `go.work` (removes the protocol entry),
//!      then snapshots every file under `primary/projects/<project>/`.
//!   5. Injects DRIFT into the workweave's `go.work` (removes the server
//!      entry). The framework's `verify()` reports safe_to_fix=true drift
//!      for the workweave.
//!   6. Runs `rwv doctor --fix` with cwd inside the workweave.
//!   7. Asserts:
//!      - Every file under `primary/projects/<project>/` is byte-identical
//!        to the pre-doctor snapshot — including the injected primary drift.
//!        Primary heals on its own doctor invocation, never as a side effect
//!        of a workweave-scoped fix.
//!      - The workweave's `go.work` has the server entry re-added: the fix
//!        landed where it belonged (option (a) — repair-in-workweave).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{self, Command};

mod common;

/// Return early (skip) if `go` is not on PATH. `go-work` activate() calls out
/// to the go binary in the primary path; without it, the fixture can't be
/// built. verify() is deterministic without go, but activate() (which runs
/// during the fix) is not.
macro_rules! require_go {
    () => {
        if Command::new("which")
            .arg("go")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            // go is available, continue
        } else {
            eprintln!("skipping test: `go` not found on PATH");
            return;
        }
    };
}

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(
        status.success(),
        "git {:?} in {} failed",
        args,
        dir.display()
    );
}

fn git_init_with_commit(dir: &Path) {
    git(&["init", "--initial-branch=main"], dir);
    git(&["config", "user.email", "test@test.com"], dir);
    git(&["config", "user.name", "Test"], dir);
    git(&["add", "-A"], dir);
    git(&["commit", "-m", "init"], dir);
}

/// A single filesystem entry captured for byte-level comparison.
///
/// - `File(bytes)`: a real file's contents.
/// - `Symlink(target)`: a symlink's raw target string (never followed).
/// - `Dir`: a directory (existence + kind; contents captured as separate
///   entries in the map).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Snap {
    File(Vec<u8>),
    Symlink(PathBuf),
    Dir,
}

/// Recursively snapshot every entry under `root`, keyed by path relative to
/// `root`. Symlinks are captured by target (not followed) so that the diff
/// catches "primary's `go.work` was replaced by a real file" as well as
/// content changes. The `.git/` subtree is excluded to keep the snapshot
/// deterministic — the fix path never claims to leave `.git/index` bytes
/// alone (a stray `git status` mtime touch would otherwise flake the test)
/// and `.git/` is not part of the primary weave's project surface we care
/// about isolating.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Snap> {
    let mut out = BTreeMap::new();
    snapshot_walk(root, root, &mut out);
    out
}

fn snapshot_walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Snap>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // `.git/` is excluded — see `snapshot` doc.
        if path.file_name().map(|n| n == ".git").unwrap_or(false) {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap().to_path_buf();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&path).unwrap_or_default();
            out.insert(rel, Snap::Symlink(target));
        } else if meta.is_dir() {
            out.insert(rel, Snap::Dir);
            snapshot_walk(root, &path, out);
        } else if meta.is_file() {
            let bytes = std::fs::read(&path).unwrap_or_default();
            out.insert(rel, Snap::File(bytes));
        }
    }
}

/// Diff two snapshots; returns a human-readable list of (path, kind-of-change)
/// entries, empty on byte-identical.
fn diff_snapshots(
    before: &BTreeMap<PathBuf, Snap>,
    after: &BTreeMap<PathBuf, Snap>,
) -> Vec<String> {
    let mut diffs = Vec::new();
    for (path, before_snap) in before {
        match after.get(path) {
            None => diffs.push(format!("REMOVED: {}", path.display())),
            Some(after_snap) if after_snap != before_snap => match (before_snap, after_snap) {
                (Snap::File(a), Snap::File(b)) => diffs.push(format!(
                    "MODIFIED: {} ({} bytes → {} bytes)",
                    path.display(),
                    a.len(),
                    b.len()
                )),
                (Snap::Symlink(a), Snap::Symlink(b)) => diffs.push(format!(
                    "SYMLINK-CHANGED: {} ({} → {})",
                    path.display(),
                    a.display(),
                    b.display()
                )),
                (a, b) => diffs.push(format!(
                    "KIND-CHANGED: {} ({:?} → {:?})",
                    path.display(),
                    a,
                    b
                )),
            },
            _ => {}
        }
    }
    for path in after.keys() {
        if !before.contains_key(path) {
            diffs.push(format!("ADDED: {}", path.display()));
        }
    }
    diffs
}

#[test]
fn doctor_fix_from_workweave_leaves_primary_project_dir_byte_identical() {
    require_go!();

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // ------------------------------------------------------------------
    // 1. Build the primary weave: two go modules + a project manifest.
    // ------------------------------------------------------------------
    let ws = root.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();

    let protocol_dir = ws.join("github/chatly/protocol");
    std::fs::create_dir_all(&protocol_dir).unwrap();
    std::fs::write(
        protocol_dir.join("go.mod"),
        "module github.com/chatly/protocol\n\ngo 1.21\n",
    )
    .unwrap();
    std::fs::write(
        protocol_dir.join("protocol.go"),
        "package protocol\nfunc Greeting() string { return \"hi\" }\n",
    )
    .unwrap();
    git_init_with_commit(&protocol_dir);

    let server_dir = ws.join("github/chatly/server");
    std::fs::create_dir_all(&server_dir).unwrap();
    std::fs::write(
        server_dir.join("go.mod"),
        "module github.com/chatly/server\n\ngo 1.21\n\nrequire github.com/chatly/protocol v0.0.0\n",
    )
    .unwrap();
    std::fs::write(
        server_dir.join("server.go"),
        "package server\nimport \"github.com/chatly/protocol\"\nfunc M() string { return protocol.Greeting() }\n",
    )
    .unwrap();
    git_init_with_commit(&server_dir);

    // Project directory is itself a git repo so create_workweave can add it
    // as a worktree (matches make_workspace_with_project_repo in
    // workweave_test.rs).
    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    let rwv_yaml = "\
repositories:\n  \
  github/chatly/protocol:\n    \
    type: git\n    \
    url: https://github.com/chatly/protocol.git\n    \
    version: main\n    \
    role: owned\n  \
  github/chatly/server:\n    \
    type: git\n    \
    url: https://github.com/chatly/server.git\n    \
    version: main\n    \
    role: owned\n";
    std::fs::write(project_dir.join("rwv.yaml"), rwv_yaml).unwrap();
    git_init_with_commit(&project_dir);

    // ------------------------------------------------------------------
    // 2. Activate the primary — authors go.work at projects/web-app/go.work
    //    and surfaces the top-level symlink at ws/go.work.
    // ------------------------------------------------------------------
    repoweave::activate::activate_intent("web-app", &ws)
        .expect("primary activate_intent should succeed");
    assert!(
        project_dir.join("go.work").exists(),
        "primary go.work should live at projects/web-app/go.work"
    );

    // Commit the activate-generated files so create_workweave (which refuses
    // to fork a dirty project worktree) is happy.
    git(&["add", "-A"], &project_dir);
    git(&["commit", "-m", "activate"], &project_dir);

    // ------------------------------------------------------------------
    // 3. Create workweave.
    // ------------------------------------------------------------------
    let weaveroot = root.join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();
    std::env::set_var("RWV_WORKWEAVE_DIR", &weaveroot);

    let ww_project = repoweave::manifest::ProjectName::new("web-app");
    let ww_name = repoweave::manifest::WorkweaveName::new("agent-1");
    let ww_dir = repoweave::workweave::create_workweave(
        &ws,
        &ws,
        &ww_project,
        &ww_name,
        false,
        false,
        false,
    )
    .expect("create_workweave should succeed");

    let ww_project_dir = ww_dir.join("projects/web-app");
    let ww_go_work = ww_project_dir.join("go.work");
    assert!(
        ww_go_work.exists(),
        "workweave should have its own go.work at {}",
        ww_go_work.display()
    );

    // ------------------------------------------------------------------
    // 4. Inject drift into PRIMARY's go.work too — remove the protocol entry.
    //    This is the sharp end of the bug: under the buggy `activate_intent`
    //    path, doctor --fix run from the workweave resolves cwd → primary and
    //    would regenerate primary's `go.work` (adding the protocol entry back)
    //    even though the operator ran doctor from a workweave that has its
    //    own separate drift. Under the fix, primary's drift is left ALONE —
    //    the operator is expected to run doctor at primary to fix primary
    //    state; workweave doctor only touches workweave state.
    //
    //    Committed afterwards so an unrelated pre-check on a dirty primary
    //    doesn't confound the assertion.
    // ------------------------------------------------------------------
    let primary_go_work = project_dir.join("go.work");
    {
        let text = std::fs::read_to_string(&primary_go_work).unwrap();
        let drifted: String = text
            .lines()
            .filter(|l| !l.contains("./github/chatly/protocol"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&primary_go_work, format!("{drifted}\n")).unwrap();
    }
    git(&["add", "go.work"], &project_dir);
    git(&["commit", "-m", "inject primary drift"], &project_dir);

    // Sanity: primary's go.work is now missing the protocol line.
    let primary_go_work_pre = std::fs::read_to_string(&primary_go_work).unwrap();
    assert!(
        !primary_go_work_pre.contains("./github/chatly/protocol"),
        "primary go.work must be missing protocol pre-doctor. got:\n{primary_go_work_pre}"
    );

    // Snapshot AFTER the primary drift is committed — the assertion is that
    // this exact snapshot (drift included) survives doctor --fix run from the
    // workweave.
    let before = snapshot(&project_dir);
    assert!(
        !before.is_empty(),
        "snapshot of primary project dir must be non-empty"
    );

    // ------------------------------------------------------------------
    // 5. Inject fixable drift into the WORKWEAVE's go.work.
    //    Delete the `./github/chatly/server` entry from the workweave's go.work
    //    while leaving the marker in place. verify() then reports safe_to_fix=true
    //    drift (marker present, but on-disk `use` set diverges from expected).
    //    Under --fix, activate() should re-add the entry via `go work use` —
    //    an operation that only writes into the WORKWEAVE's own project dir
    //    once the isolation bug is fixed.
    // ------------------------------------------------------------------
    let ww_content = std::fs::read_to_string(&ww_go_work).expect("read workweave go.work");
    assert!(
        ww_content.contains("./github/chatly/server"),
        "workweave go.work must reference the server module pre-drift; got:\n{ww_content}"
    );
    // Drop the server line entirely — go tolerates a use block with only a
    // subset of the ecosystem's modules; the drift-detector reports it.
    let drifted: String = ww_content
        .lines()
        .filter(|l| !l.contains("./github/chatly/server"))
        .collect::<Vec<_>>()
        .join("\n");
    let drifted = format!("{drifted}\n");
    // Commit the drift so the workweave repo isn't left dirty (some doctor
    // pre-checks bail on dirty state).
    std::fs::write(&ww_go_work, &drifted).unwrap();
    git(
        &["-C", ww_project_dir.to_str().unwrap(), "add", "go.work"],
        &ww_project_dir,
    );
    git(
        &[
            "-C",
            ww_project_dir.to_str().unwrap(),
            "commit",
            "-m",
            "inject drift",
        ],
        &ww_project_dir,
    );

    // Sanity: the server line is gone in the workweave's go.work.
    assert!(
        !std::fs::read_to_string(&ww_go_work)
            .unwrap()
            .contains("./github/chatly/server"),
        "workweave go.work must have the server line removed after drift injection"
    );

    // ------------------------------------------------------------------
    // 6. Run `rwv doctor --fix` with cwd inside the workweave.
    // ------------------------------------------------------------------
    let mut cmd = common::rwv();
    cmd.args(["doctor", "--fix"])
        .current_dir(&ww_dir)
        .env("RWV_WORKWEAVE_DIR", &weaveroot);
    let output = cmd.output().expect("rwv doctor should run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}");

    // ------------------------------------------------------------------
    // 7. Isolation assertion: primary project dir byte-identical.
    //    Primary carries its own uncorrected drift (protocol entry removed
    //    in step 4). The fix contract: doctor --fix run FROM THE WORKWEAVE
    //    must not touch primary at all — primary's own drift is not this
    //    invocation's business.
    // ------------------------------------------------------------------
    let after = snapshot(&project_dir);
    let diffs = diff_snapshots(&before, &after);
    let primary_go_work_after = std::fs::read_to_string(&primary_go_work).unwrap();
    assert!(
        diffs.is_empty(),
        "PRIMARY project dir at {} must be byte-identical after doctor --fix in a workweave, \
         but these entries changed:\n  {}\n\nprimary go.work is now:\n{}\n\ndoctor combined output:\n{}",
        project_dir.display(),
        diffs.join("\n  "),
        primary_go_work_after,
        combined,
    );
    // Additional direct assertion: primary's drift must NOT have been
    // silently "fixed" by a workweave-scoped doctor invocation.
    assert!(
        !primary_go_work_after.contains("./github/chatly/protocol"),
        "primary go.work must still be missing the protocol entry — doctor --fix run \
         from a workweave must not modify primary. got:\n{primary_go_work_after}\n\ndoctor combined output:\n{combined}"
    );

    // Workweave's go.work should have the server line re-added — the fix
    // landed inside the workweave. This confirms option (a) semantics: the
    // workweave's own copy of the managed file is what was regenerated.
    let ww_go_work_after = std::fs::read_to_string(&ww_go_work).unwrap();
    assert!(
        ww_go_work_after.contains("./github/chatly/server"),
        "workweave go.work should have been regenerated (server line re-added). \
         got:\n{ww_go_work_after}\n\ndoctor combined output:\n{combined}"
    );
}
