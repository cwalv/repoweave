//! Integration tests verifying doc claims in
//! `docs/how-to/run-a-command-across-repos.md`.
//!
//! A recipe is run as the how-to spells it — split into words, `rwv` found on
//! `PATH` beside the `rwv-each` plugin it dispatches to — so a typo in the
//! doc, or a recipe the plugin no longer accepts, surfaces here on every
//! platform.

use std::path::Path;

mod common;

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "--initial-branch=main"]);
    std::fs::write(path.join("README"), "init").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
}

const HOW_TO: &str = include_str!("../docs/how-to/run-a-command-across-repos.md");

/// Run a recipe that the how-to contains verbatim, word for word, with no
/// shell: `rwv each` passes arguments as values, which is the property that
/// lets this run unchanged on Windows.
fn run_recipe(recipe: &str, cwd: &Path) -> std::process::Output {
    assert!(
        HOW_TO.contains(recipe),
        "the how-to no longer contains {recipe:?}"
    );
    let words: Vec<&str> = recipe.split_whitespace().collect();
    assert_eq!(words[0], "rwv");
    let bins = std::path::PathBuf::from(env!("CARGO_BIN_EXE_rwv"))
        .parent()
        .unwrap()
        .to_path_buf();
    let rest = std::env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(bins).chain(std::env::split_paths(&rest))).unwrap();
    common::rwv_std()
        .args(&words[1..])
        .current_dir(cwd)
        .env("PATH", path)
        .output()
        .expect("rwv runs")
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
fn recipe_checkout_branch_across_forks_via_rwv_each() {
    let (_tmp, ws, _project_dir) = setup_two_repo_workspace();

    let output = run_recipe(
        "rwv each --role fork -- git checkout -b feat/my-big-change",
        &ws,
    );
    assert!(
        output.status.success(),
        "recipe failed: stderr={}, stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );

    let on_branch = |repo: &str| {
        common::git()
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/heads/feat/my-big-change",
            ])
            .current_dir(ws.join(repo))
            .output()
            .unwrap()
            .status
            .success()
    };
    assert!(on_branch("github/org/fork"), "the fork gets the branch");
    assert!(
        !on_branch("github/org/primary"),
        "the owned repo is not a fork"
    );
}
