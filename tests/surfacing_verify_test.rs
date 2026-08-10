//! E2E tests for the framework-level **Axis-1 surfacing** check wired into
//! `rwv doctor` and its `--fix` re-surfacing primitive.
//!
//! The surfacing check is a SECOND CONSUMER of the same
//! `generated_files() ∪ managed_files()` union that drives symlink CREATION:
//! it asserts that `<weave-dir>/<file>` exists as a symlink resolving to
//! `projects/<project>/<file>`. Any divergence (manual `rm`, interrupted
//! create, a manifest change that adds a file, enabling an integration in an
//! existing workweave) is invisible to the per-integration `verify()` pass —
//! these tests pin that doctor now flags it, and that `--fix` re-surfaces via
//! the step-2 primitive (`surface_symlinks`) rather than re-running
//! `activate_intent` (which would be a project re-selection — illegal in a
//! workweave).

mod common;

use std::path::{Path, PathBuf};

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn git() -> std::process::Command {
    common::git()
}

fn run_git(args: &[&str], dir: &Path) {
    let out = git()
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
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Build a workspace at `parent/ws` with a single project `alpha` whose
/// `static-files` integration declares `.claude`. The `.claude` file is
/// authored in the project dir so it can be surfaced. The project dir is a
/// real git repo so workspace context resolves to a weave root. Returns the
/// workspace root.
fn make_workspace(parent: &Path) -> PathBuf {
    let ws = parent.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();

    let project_dir = ws.join("projects").join("alpha");
    std::fs::create_dir_all(&project_dir).unwrap();

    // static-files declares `.claude`; disable the default integrations that
    // surface files unconditionally so the surfacing union is exactly
    // `.claude` and the doctor output is deterministic.
    let manifest = "[repositories]\n\n[integrations.static-files]\nenabled = true\nfiles = [\".claude\"]\n\n[integrations.vscode-workspace]\nenabled = false\n\n[integrations.go-work]\nenabled = false\n";
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    // Author the declared static file in the project dir.
    std::fs::write(project_dir.join(".claude"), "claude config\n").unwrap();

    run_git(&["init", "-b", "main"], &project_dir);
    run_git(&["add", "."], &project_dir);
    run_git(&["commit", "-m", "init"], &project_dir);

    ws
}

/// Run `rwv <args>` in `ws`, returning combined stdout+stderr regardless of
/// exit status. Doctor's overall exit status is irrelevant to the surfacing
/// contract under test.
fn rwv_output(ws: &Path, args: &[&str]) -> String {
    let assertion = {
        let mut cmd = rwv();
        for a in args {
            cmd.arg(a);
        }
        cmd.current_dir(ws).assert()
    };
    let out = assertion.get_output();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    format!("{stdout}\n{stderr}")
}

/// True iff `<ws>/.claude` is a symlink resolving to `projects/alpha/.claude`.
fn claude_surfaced(ws: &Path) -> bool {
    let link = ws.join(".claude");
    let meta = match link.symlink_metadata() {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !meta.file_type().is_symlink() {
        return false;
    }
    std::fs::read_link(&link)
        .map(|t| t == Path::new("projects/alpha/.claude"))
        .unwrap_or(false)
}

/// After `rwv activate`, the static file is surfaced. Then `rm` the symlink to
/// simulate manual removal / a manifest change that added the file after this
/// weave was created. `rwv doctor` flags the missing surfacing.
#[test]
fn doctor_flags_missing_surfacing_symlink() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());

    // Surface via activate (no install hooks needed).
    rwv_output(&ws, &["activate", "alpha", "--no-materialize"]);
    assert!(
        claude_surfaced(&ws),
        "activate should surface `.claude` as a symlink"
    );

    // Simulate the gap: the surfacing symlink is gone.
    std::fs::remove_file(ws.join(".claude")).unwrap();
    assert!(!ws.join(".claude").exists());

    let out = rwv_output(&ws, &["doctor"]);
    assert!(
        out.contains("surfacing") && out.contains(".claude") && out.contains("not surfaced"),
        "doctor should flag the missing surfacing symlink, got:\n{out}"
    );
}

/// `rwv doctor --fix` re-surfaces the missing symlink via the step-2
/// primitive: the symlink is re-created and a `[fixed]` line is emitted.
#[test]
fn doctor_fix_re_surfaces_missing_symlink() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());

    rwv_output(&ws, &["activate", "alpha", "--no-materialize"]);
    std::fs::remove_file(ws.join(".claude")).unwrap();
    assert!(!claude_surfaced(&ws));

    let out = rwv_output(&ws, &["doctor", "--fix"]);
    assert!(
        out.contains("re-surfaced") && out.contains("alpha"),
        "doctor --fix should report re-surfacing, got:\n{out}"
    );
    assert!(
        claude_surfaced(&ws),
        "doctor --fix should re-create the `.claude` surfacing symlink"
    );

    // Idempotent: a second doctor run finds nothing to flag.
    let out2 = rwv_output(&ws, &["doctor"]);
    assert!(
        !(out2.contains("surfacing") && out2.contains("not surfaced")),
        "after --fix, doctor should report no missing surfacing, got:\n{out2}"
    );
}

/// A clean (just-activated) workspace reports no surfacing findings — the
/// check must not produce false positives when surfacing is intact.
#[test]
fn doctor_clean_when_surfacing_intact() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());

    rwv_output(&ws, &["activate", "alpha", "--no-materialize"]);
    assert!(claude_surfaced(&ws));

    let out = rwv_output(&ws, &["doctor"]);
    assert!(
        !out.contains("not surfaced"),
        "doctor should not flag surfacing when intact, got:\n{out}"
    );
}
