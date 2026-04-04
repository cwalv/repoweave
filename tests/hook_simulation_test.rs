//! Integration tests for the Claude Code hook scripts.
//!
//! These tests simulate what Claude Code does: pipe JSON to the shell scripts
//! at `examples/hooks/` and verify the outcomes. Each test is CI-safe — it
//! checks for `jq` at runtime and skips gracefully if it is not available.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// ============================================================================
// Shared helpers (mirrors workweave_test.rs)
// ============================================================================

/// Run a git command in `dir`, panicking on failure.
fn git(args: &[&str], dir: &Path) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {:?} in {} failed", args, dir.display());
}

/// Initialise a normal git repo at `path` with one commit on `main`.
fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README"), "init").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

/// Create a minimal workspace with one project and one repo, and write
/// `.rwv-active` so the hook can read the active project.
///
/// Layout:
///   {tmp}/ws/                          -- workspace root
///   {tmp}/ws/github/                   -- registry marker
///   {tmp}/ws/projects/{project}/       -- project dir with rwv.yaml
///   {tmp}/ws/github/org/repo/          -- git repo
///   {tmp}/ws/.rwv-active               -- active project file
///
/// Returns the workspace root path.
fn make_workspace(tmp: &Path, project: &str) -> PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let manifest = format!(
        r#"repositories:
  github/org/repo:
    type: git
    url: file://{repo}
    version: main
    role: primary
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    // Write .rwv-active so the hook script can find the project name.
    std::fs::write(ws.join(".rwv-active"), format!("{}\n", project)).unwrap();

    ws
}

/// Return the path to a hook script relative to the manifest dir.
fn hook_path(script_name: &str) -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .join("examples/hooks")
        .join(script_name)
}

/// Return the directory that contains the built `rwv` binary.
///
/// `assert_cmd` builds the binary on demand; we just need the directory so
/// the shell scripts can find `rwv` on PATH. We rely on the fact that
/// `cargo test` compiles the binary before running tests, and that the binary
/// lands in `target/{debug,release}/rwv`.
fn rwv_bin_dir() -> PathBuf {
    // CARGO_BIN_EXE_rwv is set by Cargo when building integration tests.
    let exe = env!("CARGO_BIN_EXE_rwv");
    PathBuf::from(exe)
        .parent()
        .expect("rwv binary should have a parent directory")
        .to_path_buf()
}

/// Check if `jq` is available; return false if the test should be skipped.
fn jq_available() -> bool {
    Command::new("which")
        .arg("jq")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Pipe `json` to `script_path` as stdin, with the given env vars added to
/// PATH (prepended). `process_cwd` sets the working directory of the bash
/// process — important because `rwv workweave` resolves the workspace from
/// the process's current directory, not from the JSON `cwd` field.
/// Returns `(exit_status, stdout, stderr)`.
fn run_hook(
    script_path: &Path,
    json: &str,
    envs: &[(&str, &str)],
    extra_path: Option<&Path>,
    process_cwd: Option<&Path>,
) -> (std::process::ExitStatus, String, String) {
    let mut path_val = std::env::var("PATH").unwrap_or_default();
    if let Some(extra) = extra_path {
        path_val = format!("{}:{}", extra.display(), path_val);
    }

    let mut cmd = Command::new("bash");
    cmd.arg(script_path)
        .env("PATH", &path_val)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(cwd) = process_cwd {
        cmd.current_dir(cwd);
    }

    for (k, v) in envs {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().expect("bash should be available");

    // Write JSON to stdin.
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin should be piped");
        stdin.write_all(json.as_bytes()).expect("write to stdin");
    }

    let output = child.wait_with_output().expect("wait on hook process");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (output.status, stdout, stderr)
}

// ============================================================================
// Tests
// ============================================================================

/// 1. Create hook produces a workweave with the expected structure.
#[test]
fn hook_create_produces_workweave() {
    if !jq_available() {
        eprintln!("skipping hook_create_produces_workweave: jq not found");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let json = format!(
        r#"{{"cwd": "{cwd}", "branch_name": "test-feature", "session_id": "sess-123", "hook_event_name": "WorktreeCreate"}}"#,
        cwd = ws.display()
    );

    let script = hook_path("rwv-workweave-create.sh");
    let (status, stdout, stderr) = run_hook(
        &script,
        &json,
        &[("WEAVEROOT", weaveroot.to_str().unwrap())],
        Some(&rwv_bin_dir()),
        Some(&ws),
    );

    assert!(
        status.success(),
        "create hook should exit 0, stderr: {stderr}"
    );

    // stdout should be exactly one non-empty line.
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "create hook should print exactly one line, got: {:?}",
        lines
    );

    let ww_path = PathBuf::from(lines[0].trim());
    assert!(
        ww_path.is_absolute(),
        "create hook output should be an absolute path, got: {}",
        ww_path.display()
    );
    assert!(
        ww_path.exists(),
        "workweave directory should exist at {}",
        ww_path.display()
    );
    assert!(
        ww_path.join(".rwv-workweave").exists(),
        ".rwv-workweave marker should exist in {}",
        ww_path.display()
    );
    assert!(
        ww_path.join(".rwv-active").exists(),
        ".rwv-active should exist in {}",
        ww_path.display()
    );
    assert!(
        ww_path.join("github").exists(),
        "github/ directory should exist in {}",
        ww_path.display()
    );
}

/// 2. When branch_name is "null", the workweave name uses the session_id.
#[test]
fn hook_create_null_branch_uses_session_id() {
    if !jq_available() {
        eprintln!("skipping hook_create_null_branch_uses_session_id: jq not found");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let json = format!(
        r#"{{"cwd": "{cwd}", "branch_name": "null", "session_id": "abc-123", "hook_event_name": "WorktreeCreate"}}"#,
        cwd = ws.display()
    );

    let script = hook_path("rwv-workweave-create.sh");
    let (status, stdout, stderr) = run_hook(
        &script,
        &json,
        &[("WEAVEROOT", weaveroot.to_str().unwrap())],
        Some(&rwv_bin_dir()),
        Some(&ws),
    );

    assert!(
        status.success(),
        "create hook should exit 0, stderr: {stderr}"
    );

    let ww_path = stdout.trim().to_string();
    assert!(
        ww_path.contains("abc-123"),
        "workweave name should contain session_id 'abc-123', got: {ww_path}"
    );
    assert!(
        !ww_path.contains("null"),
        "workweave name should not contain literal 'null', got: {ww_path}"
    );
}

/// 3. When both branch_name and session_id are "null", the name starts with "ww-".
#[test]
fn hook_create_null_branch_null_session_uses_timestamp() {
    if !jq_available() {
        eprintln!("skipping hook_create_null_branch_null_session_uses_timestamp: jq not found");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let json = format!(
        r#"{{"cwd": "{cwd}", "branch_name": "null", "session_id": "null", "hook_event_name": "WorktreeCreate"}}"#,
        cwd = ws.display()
    );

    let script = hook_path("rwv-workweave-create.sh");
    let (status, stdout, stderr) = run_hook(
        &script,
        &json,
        &[("WEAVEROOT", weaveroot.to_str().unwrap())],
        Some(&rwv_bin_dir()),
        Some(&ws),
    );

    assert!(
        status.success(),
        "create hook should exit 0, stderr: {stderr}"
    );

    let ww_path = stdout.trim().to_string();
    let dir_name = PathBuf::from(&ww_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    // The workweave dir is named {primary}--{name}; strip the prefix.
    let name_part = dir_name
        .split("--")
        .nth(1)
        .unwrap_or(&dir_name)
        .to_string();
    assert!(
        name_part.starts_with("ww-"),
        "workweave name should start with 'ww-' when both branch and session are null, got: {name_part}"
    );
}

/// 4. Hook works when cwd is a subdirectory of the workspace.
#[test]
fn hook_create_from_subdirectory() {
    if !jq_available() {
        eprintln!("skipping hook_create_from_subdirectory: jq not found");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Use a subdirectory (the repo path) as cwd.
    let subdir = ws.join("github/org/repo");
    let json = format!(
        r#"{{"cwd": "{cwd}", "branch_name": "from-subdir", "session_id": "sess-456", "hook_event_name": "WorktreeCreate"}}"#,
        cwd = subdir.display()
    );

    let script = hook_path("rwv-workweave-create.sh");
    let (status, stdout, stderr) = run_hook(
        &script,
        &json,
        &[("WEAVEROOT", weaveroot.to_str().unwrap())],
        Some(&rwv_bin_dir()),
        Some(&ws),
    );

    assert!(
        status.success(),
        "create hook from subdirectory should exit 0, stderr: {stderr}"
    );

    let ww_path = PathBuf::from(stdout.trim());
    assert!(
        ww_path.exists(),
        "workweave directory should exist even when cwd is a subdirectory, path: {}",
        ww_path.display()
    );
}

/// 5. Remove hook deletes the workweave directory.
#[test]
fn hook_remove_cleans_up() {
    if !jq_available() {
        eprintln!("skipping hook_remove_cleans_up: jq not found");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // First create a workweave via the create hook.
    let create_json = format!(
        r#"{{"cwd": "{cwd}", "branch_name": "to-remove", "session_id": "sess-del", "hook_event_name": "WorktreeCreate"}}"#,
        cwd = ws.display()
    );

    let create_script = hook_path("rwv-workweave-create.sh");
    let (create_status, create_stdout, create_stderr) = run_hook(
        &create_script,
        &create_json,
        &[("WEAVEROOT", weaveroot.to_str().unwrap())],
        Some(&rwv_bin_dir()),
        Some(&ws),
    );

    assert!(
        create_status.success(),
        "create hook should succeed before remove test, stderr: {create_stderr}"
    );

    let ww_path = PathBuf::from(create_stdout.trim());
    assert!(ww_path.exists(), "workweave should exist before removal");

    // Now remove it via the remove hook.
    let remove_json = format!(
        r#"{{"worktree_path": "{path}", "hook_event_name": "WorktreeRemove"}}"#,
        path = ww_path.display()
    );

    let remove_script = hook_path("rwv-workweave-remove.sh");
    let (remove_status, _remove_stdout, remove_stderr) = run_hook(
        &remove_script,
        &remove_json,
        &[("WEAVEROOT", weaveroot.to_str().unwrap())],
        Some(&rwv_bin_dir()),
        Some(&ws),
    );

    assert!(
        remove_status.success(),
        "remove hook should exit 0, stderr: {remove_stderr}"
    );
    assert!(
        !ww_path.exists(),
        "workweave directory should be removed after hook, path: {}",
        ww_path.display()
    );
}

/// 6. Remove hook exits 0 when the target has no .rwv-workweave marker.
#[test]
fn hook_remove_missing_marker_exits_zero() {
    if !jq_available() {
        eprintln!("skipping hook_remove_missing_marker_exits_zero: jq not found");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    // Create a random directory with no .rwv-workweave marker.
    let random_dir = tmp.path().join("not-a-workweave");
    std::fs::create_dir_all(&random_dir).unwrap();

    let json = format!(
        r#"{{"worktree_path": "{path}", "hook_event_name": "WorktreeRemove"}}"#,
        path = random_dir.display()
    );

    let script = hook_path("rwv-workweave-remove.sh");
    let (status, _stdout, stderr) = run_hook(
        &script,
        &json,
        &[],
        Some(&rwv_bin_dir()),
        None,
    );

    assert!(
        status.success(),
        "remove hook should exit 0 even without .rwv-workweave marker, stderr: {stderr}"
    );
}
