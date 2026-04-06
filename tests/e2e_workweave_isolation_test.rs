//! E2E integration test for workweave isolation with ecosystem tools.
//!
//! Verifies that a workweave gets its own `go.work`, can build independently,
//! is isolated from the primary weave, and is cleanly removed on deletion.
//!
//! Requires `go` on PATH — the test skips gracefully if it is not available.

use std::process::Command;

/// Return early (skip) if `go` is not on PATH.
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

/// Run a git command in `dir`, panicking on failure.
fn git(args: &[&str], dir: &std::path::Path) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(
        status.success(),
        "git {:?} in {} failed",
        args,
        dir.display()
    );
}

/// Init a git repo with a user identity and one commit.
fn git_init_with_commit(dir: &std::path::Path) {
    git(&["init", "--initial-branch=main"], dir);
    git(&["config", "user.email", "test@test.com"], dir);
    git(&["config", "user.name", "Test"], dir);
    git(&["add", "-A"], dir);
    git(&["commit", "-m", "init"], dir);
}

#[test]
fn e2e_workweave_isolation_with_go_ecosystem() {
    require_go!();

    // ------------------------------------------------------------------
    // Build the temp directory layout
    // ------------------------------------------------------------------
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Workspace root is tmp/ws (mirrors the naming used in workweave_test.rs).
    let ws = root.join("ws");

    // Registry marker so repoweave recognises it as a workspace root.
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();

    // ------------------------------------------------------------------
    // 1. github/chatly/protocol — Go module exporting a function
    // ------------------------------------------------------------------
    let protocol_dir = ws.join("github/chatly/protocol");
    std::fs::create_dir_all(&protocol_dir).unwrap();

    std::fs::write(
        protocol_dir.join("go.mod"),
        "module github.com/chatly/protocol\n\ngo 1.21\n",
    )
    .unwrap();

    std::fs::write(
        protocol_dir.join("protocol.go"),
        r#"package protocol

// Greeting returns a greeting string.
func Greeting() string {
    return "hello from protocol"
}
"#,
    )
    .unwrap();

    git_init_with_commit(&protocol_dir);

    // ------------------------------------------------------------------
    // 2. github/chatly/server — Go module importing protocol
    // ------------------------------------------------------------------
    let server_dir = ws.join("github/chatly/server");
    std::fs::create_dir_all(&server_dir).unwrap();

    std::fs::write(
        server_dir.join("go.mod"),
        "module github.com/chatly/server\n\ngo 1.21\n\nrequire github.com/chatly/protocol v0.0.0\n",
    )
    .unwrap();

    std::fs::write(
        server_dir.join("server.go"),
        r#"package server

import "github.com/chatly/protocol"

// Message returns a server message using the protocol package.
func Message() string {
    return protocol.Greeting()
}
"#,
    )
    .unwrap();

    git_init_with_commit(&server_dir);

    // ------------------------------------------------------------------
    // 3. projects/web-app/rwv.yaml listing both repos
    // ------------------------------------------------------------------
    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let rwv_yaml = "\
repositories:\n  \
  github/chatly/protocol:\n    \
    type: git\n    \
    url: https://github.com/chatly/protocol.git\n    \
    version: main\n    \
    role: primary\n  \
  github/chatly/server:\n    \
    type: git\n    \
    url: https://github.com/chatly/server.git\n    \
    version: main\n    \
    role: primary\n";

    std::fs::write(project_dir.join("rwv.yaml"), rwv_yaml).unwrap();

    // ------------------------------------------------------------------
    // 4. Activate the primary weave — generates go.work, verify go build
    // ------------------------------------------------------------------
    repoweave::activate::activate("web-app", &ws).expect("activate should succeed");

    let go_work_path = ws.join("go.work");
    assert!(
        go_work_path.exists(),
        "go.work should be created at the weave root after activation"
    );

    // Build from the server module — this exercises the cross-module import.
    // In go workspace mode, `go build ./...` must be run from within a module
    // directory (not the workspace root itself which contains no Go files).
    let build_output = Command::new("go")
        .args(["build", "./..."])
        .current_dir(&server_dir)
        .output()
        .expect("failed to run `go build ./...` in server module");

    let stdout = String::from_utf8_lossy(&build_output.stdout);
    let stderr = String::from_utf8_lossy(&build_output.stderr);
    assert!(
        build_output.status.success(),
        "`go build ./...` in primary weave (server module) failed.\nstdout: {stdout}\nstderr: {stderr}"
    );

    // ------------------------------------------------------------------
    // 5. Create workweave agent-1
    // ------------------------------------------------------------------
    let workweave_root = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&workweave_root).unwrap();

    // Set RWV_WORKWEAVE_DIR so workweaves land in a known location.
    std::env::set_var("RWV_WORKWEAVE_DIR", &workweave_root);

    let ww_name = repoweave::manifest::WorkweaveName::new("agent-1");
    let ww_dir = repoweave::workweave::create_workweave(&ws, "web-app", &ww_name)
        .expect("create_workweave should succeed");

    // ------------------------------------------------------------------
    // 6. Verify workweave directory exists and repos are git worktrees
    // ------------------------------------------------------------------
    assert!(
        ww_dir.exists(),
        "workweave directory should exist at {}",
        ww_dir.display()
    );

    // Both repos should be present as git worktrees (.git is a FILE not a dir).
    for repo_rel in &["github/chatly/protocol", "github/chatly/server"] {
        let wt_path = ww_dir.join(repo_rel);
        assert!(
            wt_path.exists(),
            "workweave should contain worktree at {repo_rel}, expected at {}",
            wt_path.display()
        );

        let dot_git = wt_path.join(".git");
        let meta = std::fs::symlink_metadata(&dot_git)
            .unwrap_or_else(|e| panic!(".git should exist in workweave worktree {repo_rel}: {e}"));
        assert!(
            meta.file_type().is_file(),
            ".git in workweave {repo_rel} should be a file (worktree), not a directory"
        );
    }

    // ------------------------------------------------------------------
    // 7. Verify workweave has its own go.work (symlinked from its project dir)
    // ------------------------------------------------------------------
    let ww_go_work = ww_dir.join("go.work");
    assert!(
        ww_go_work.exists(),
        "workweave should have its own go.work at {}",
        ww_go_work.display()
    );

    // ------------------------------------------------------------------
    // 8. Run `go build ./...` from workweave server module — must resolve
    //    the cross-module import independently via the workweave's go.work.
    //    (In workspace mode, build must be run from within a module directory.)
    // ------------------------------------------------------------------
    let ww_server_dir = ww_dir.join("github/chatly/server");
    let ww_build = Command::new("go")
        .args(["build", "./..."])
        .current_dir(&ww_server_dir)
        .output()
        .expect("failed to run `go build ./...` in workweave server module");

    let ww_stdout = String::from_utf8_lossy(&ww_build.stdout);
    let ww_stderr = String::from_utf8_lossy(&ww_build.stderr);
    assert!(
        ww_build.status.success(),
        "`go build ./...` in workweave failed.\nstdout: {ww_stdout}\nstderr: {ww_stderr}"
    );

    // ------------------------------------------------------------------
    // 9. Make a change in the workweave's protocol repo, verify build
    // ------------------------------------------------------------------
    let ww_protocol_go = ww_dir.join("github/chatly/protocol/protocol.go");
    std::fs::write(
        &ww_protocol_go,
        r#"package protocol

// Greeting returns a modified greeting string (workweave change).
func Greeting() string {
    return "hello from workweave protocol"
}
"#,
    )
    .unwrap();

    let ww_build2 = Command::new("go")
        .args(["build", "./..."])
        .current_dir(&ww_server_dir)
        .output()
        .expect("failed to run `go build ./...` after change");

    let ww_stdout2 = String::from_utf8_lossy(&ww_build2.stdout);
    let ww_stderr2 = String::from_utf8_lossy(&ww_build2.stderr);
    assert!(
        ww_build2.status.success(),
        "`go build ./...` in workweave after local change failed.\nstdout: {ww_stdout2}\nstderr: {ww_stderr2}"
    );

    // ------------------------------------------------------------------
    // 10. Verify the primary weave's protocol repo is unchanged (isolation)
    // ------------------------------------------------------------------
    let primary_protocol_content =
        std::fs::read_to_string(protocol_dir.join("protocol.go")).unwrap();
    assert!(
        primary_protocol_content.contains("hello from protocol"),
        "primary weave's protocol.go should be unchanged after workweave edit, got:\n{primary_protocol_content}"
    );
    assert!(
        !primary_protocol_content.contains("workweave"),
        "primary weave's protocol.go should not contain workweave change, got:\n{primary_protocol_content}"
    );

    // ------------------------------------------------------------------
    // 11. Delete the workweave
    // ------------------------------------------------------------------
    repoweave::workweave::delete_workweave(&ws, "web-app", &ww_name)
        .expect("delete_workweave should succeed");

    // ------------------------------------------------------------------
    // 12. Verify workweave directory is gone
    // ------------------------------------------------------------------
    assert!(
        !ww_dir.exists(),
        "workweave directory should be removed after delete, still at {}",
        ww_dir.display()
    );

    // ------------------------------------------------------------------
    // 13. Verify primary weave still works: go build from server module
    // ------------------------------------------------------------------
    let final_build = Command::new("go")
        .args(["build", "./..."])
        .current_dir(&server_dir)
        .output()
        .expect("failed to run `go build ./...` in primary weave after workweave deletion");

    let final_stdout = String::from_utf8_lossy(&final_build.stdout);
    let final_stderr = String::from_utf8_lossy(&final_build.stderr);
    assert!(
        final_build.status.success(),
        "`go build ./...` in primary weave failed after workweave deletion.\nstdout: {final_stdout}\nstderr: {final_stderr}"
    );
}
