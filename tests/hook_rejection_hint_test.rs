//! A hook that refuses a worktree add must be named as such to the operator.
//!
//! The hint used to be driven by looking for "hook" in the failure text. git
//! puts it there only by accident: git's own output never mentions a hook, so
//! the word arrives from whatever the hook printed, or from the destination
//! path. Both fixtures here are built so that accident cannot happen — nothing
//! in the workspace, the project, or the workweave name contains the word — and
//! the hint is still required to appear.

use std::path::Path;

mod common;

fn git(args: &[&str], dir: &Path) {
    let out = common::git().args(args).current_dir(dir).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "-b", "main"], path);
    git(&["config", "user.email", "t@t.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

fn write_manifest(project_dir: &Path, repo_path: &Path) {
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories.\"github/org/repo\"]\ntype = \"git\"\n\
             url = \"file://{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            repo_path.display()
        ),
    )
    .unwrap();
}

/// A `post-checkout` hook that refuses and prints nothing, so the word "hook"
/// appears nowhere in what git hands back.
///
/// A caller must assert that the create FAILED before it asserts anything
/// about the operator text. That assertion is what proves the hook fired.
fn plant_silent_refusing_hook(repo: &Path) {
    let hooks = repo.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("post-checkout");
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    // Nothing to set on Windows: git's own `access()` there masks off X_OK, so
    // a hook file that exists is one git runs, and git resolves the shebang
    // itself rather than leaving it to the OS.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// Fail loudly if a fixture smuggles the word in by way of a path, which would
/// let a text matcher pass this suite for the wrong reason.
fn assert_word_absent_from_paths(paths: &[&Path]) {
    for p in paths {
        assert!(
            !p.to_string_lossy().contains("hook"),
            "fixture path {} contains the word this suite must not supply",
            p.display()
        );
    }
}

#[test]
fn a_silent_hook_refusing_a_manifest_repo_is_still_named_to_the_operator() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join("myproject");
    std::fs::create_dir_all(&project_dir).unwrap();
    write_manifest(&project_dir, &repo_path);
    std::fs::create_dir_all(tmp.path().join(".workweaves")).unwrap();
    assert_word_absent_from_paths(&[tmp.path(), &ws, &repo_path]);

    plant_silent_refusing_hook(&repo_path);

    let out = common::rwv()
        .args(["workweave", "myproject", "create", "wt"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        !out.status.success(),
        "the hook refused, so create must fail; without that this test asserts \
         nothing:\n{text}"
    );
    assert!(
        text.contains("a git hook in this repo rejected the worktree creation"),
        "operator was not told a hook refused:\n{text}"
    );
}

#[test]
fn a_silent_hook_refusing_the_project_repo_is_still_named_to_the_operator() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join("myproject");
    init_repo_with_commit(&project_dir);
    write_manifest(&project_dir, &repo_path);
    git(&["add", "rwv.toml"], &project_dir);
    git(&["commit", "-m", "add manifest"], &project_dir);
    std::fs::create_dir_all(tmp.path().join(".workweaves")).unwrap();
    assert_word_absent_from_paths(&[tmp.path(), &ws, &project_dir]);

    // Only the project repo refuses; the manifest repo must get far enough for
    // the project-worktree birth to be reached at all.
    plant_silent_refusing_hook(&project_dir);

    let out = common::rwv()
        .args(["workweave", "myproject", "create", "wt"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        !out.status.success(),
        "the hook refused, so create must fail; without that this test asserts \
         nothing:\n{text}"
    );
    assert!(
        text.contains("a git hook in the project repo rejected the worktree creation"),
        "operator was not told a hook refused:\n{text}"
    );
}
