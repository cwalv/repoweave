//! Integration tests verifying doc claims about Unix composition recipes.
//!
//! These tests shell out to actually run the documented pipelines (jq +
//! xargs over `rwv status --json`) so a typo in the docs surfaces here.
//! Requires `jq` and `xargs` on PATH (standard on CI runners).

use std::path::Path;
use std::process;

mod common;

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

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README"), "init").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

/// Run a shell pipeline in the given working directory. The `rwv` binary
/// is exposed as the literal name `rwv` via a PATH prefix that points at
/// the cargo-built binary's directory. This lets the recipe be written
/// exactly as it appears in the how-to.
fn run_recipe(recipe: &str, cwd: &Path) -> std::process::Output {
    let rwv_path = assert_cmd::cargo::cargo_bin("rwv");
    let bin_dir = rwv_path.parent().expect("rwv binary has no parent dir");
    let path_env = std::env::var("PATH").unwrap_or_default();
    let augmented_path = format!("{}:{}", bin_dir.display(), path_env);

    std::process::Command::new("bash")
        .args(["-c", recipe])
        .current_dir(cwd)
        .env("PATH", augmented_path)
        .output()
        .expect("bash pipeline failed to spawn")
}

fn ensure_tool(name: &str) -> bool {
    which::which(name).is_ok()
}

fn setup_two_repo_workspace() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let repo_primary = ws.join("github/org/primary");
    let repo_fork = ws.join("github/org/fork");
    init_repo_with_commit(&repo_primary);
    init_repo_with_commit(&repo_fork);

    let project_dir = ws.join("projects/my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    let manifest = format!(
        r#"[repositories."github/org/primary"]
type = "git"
url = "file://{r1}"
version = "main"
role = "owned"

[repositories."github/org/fork"]
type = "git"
url = "file://{r2}"
version = "main"
role = "fork"
"#,
        r1 = common::url_path(&repo_primary),
        r2 = common::url_path(&repo_fork)
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    // Active project marker — required so `rwv status` resolves to my-app
    // without relying on CWD-inside-projects/ inference.
    std::fs::write(ws.join(".rwv-active"), "my-app\n").unwrap();

    let ws_path = ws.clone();
    (tmp, ws_path, project_dir)
}

#[test]
fn recipe_checkout_branch_across_forks_via_jq_xargs() {
    if !ensure_tool("jq") || !ensure_tool("xargs") {
        eprintln!("skipping: jq or xargs not on PATH");
        return;
    }

    let (_tmp, ws, _project_dir) = setup_two_repo_workspace();

    // Verbatim from docs/how-to/run-a-command-across-repos.md, recipe #2.
    let recipe = r#"rwv status --json | jq -r '.repos[] | select(.role == "fork") | .absolute_path' | xargs -I {} git -C {} checkout -b feat/doc-claim"#;
    let output = run_recipe(recipe, &ws);
    assert!(
        output.status.success(),
        "recipe failed: stderr={}, stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );

    // The fork should have the new branch; the primary should not.
    let fork_branch = ws.join("github/org/fork/.git/refs/heads/feat/doc-claim");
    let primary_branch = ws.join("github/org/primary/.git/refs/heads/feat/doc-claim");
    assert!(
        fork_branch.exists(),
        "fork should have feat/doc-claim branch"
    );
    assert!(
        !primary_branch.exists(),
        "primary should NOT have feat/doc-claim branch (filtered out)"
    );
}

#[test]
fn recipe_filter_by_role_owned_via_jq() {
    // Recipe #1's filter expression, isolated. Doesn't run `git pull`
    // (that requires a network remote); just verifies the role filter
    // selects the right repos.
    if !ensure_tool("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }

    let (_tmp, ws, _project_dir) = setup_two_repo_workspace();

    // Verbatim from docs/how-to/run-a-command-across-repos.md, recipe #1.
    // The role rename shipped `owned` as the canonical wire spelling;
    // this filter pins that the status JSON role field is `owned`.
    let recipe =
        r#"rwv status --json | jq -r '.repos[] | select(.role == "owned") | .absolute_path'"#;
    let output = run_recipe(recipe, &ws);
    assert!(
        output.status.success(),
        "jq filter failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let paths = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = paths.lines().collect();
    assert_eq!(lines.len(), 1, "expected one owned repo, got: {paths:?}");
    assert!(common::path_ends_with(lines[0], "github/org/primary"));
    assert!(std::path::Path::new(lines[0]).is_absolute());
}
