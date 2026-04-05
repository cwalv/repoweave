//! E2E integration test for Go workspace wiring via the go-work integration.
//!
//! This test verifies that activating a project with two Go modules generates
//! a `go.work` file at the weave root that correctly wires the cross-module
//! import so that `go build ./...` succeeds.
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

#[test]
fn go_workspace_wiring_resolves_cross_module_import() {
    require_go!();

    // ------------------------------------------------------------------
    // 1. Create weave root with the required directory structure
    // ------------------------------------------------------------------
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Create registry and projects directories (workspace markers)
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    // ------------------------------------------------------------------
    // 2. Set up github/chatly/protocol — a Go module exporting a function
    // ------------------------------------------------------------------
    let protocol_dir = root.join("github/chatly/protocol");
    std::fs::create_dir_all(&protocol_dir).unwrap();

    // git init so scan_repos_on_disk recognises it as a repo
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(&protocol_dir)
        .status()
        .expect("git init protocol");

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

    // ------------------------------------------------------------------
    // 3. Set up github/chatly/server — imports github.com/chatly/protocol
    // ------------------------------------------------------------------
    let server_dir = root.join("github/chatly/server");
    std::fs::create_dir_all(&server_dir).unwrap();

    Command::new("git")
        .args(["init", "-q"])
        .current_dir(&server_dir)
        .status()
        .expect("git init server");

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

    // ------------------------------------------------------------------
    // 4. Create projects/web-app/rwv.yaml listing both repos as primary
    // ------------------------------------------------------------------
    let project_dir = root.join("projects/web-app");
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
    // 5. Write .rwv-active so the workspace knows which project is active
    // ------------------------------------------------------------------
    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

    // ------------------------------------------------------------------
    // 6. Call activate("web-app", root) via the repoweave library
    // ------------------------------------------------------------------
    repoweave::activate::activate("web-app", root).expect("activate should succeed");

    // ------------------------------------------------------------------
    // 7. Verify go.work exists at the weave root and lists both modules
    // ------------------------------------------------------------------
    let go_work_path = root.join("go.work");
    assert!(
        go_work_path.exists(),
        "go.work should be created at the weave root after activation"
    );

    // The symlink at root points to projects/web-app/go.work; follow it.
    let go_work_content = std::fs::read_to_string(&go_work_path).unwrap();
    assert!(
        go_work_content.contains("github/chatly/protocol"),
        "go.work should contain the protocol module path, got:\n{go_work_content}"
    );
    assert!(
        go_work_content.contains("github/chatly/server"),
        "go.work should contain the server module path, got:\n{go_work_content}"
    );

    // ------------------------------------------------------------------
    // 8. Run `go build ./...` from the server module directory.
    //
    // Go workspaces work by placing `go.work` at the repo root; `go` discovers
    // it by walking up from CWD. Running `go build ./...` inside the server
    // module directory exercises the real cross-module import resolution: the
    // compiler must locate `github.com/chatly/protocol` via the workspace
    // rather than the network.
    // ------------------------------------------------------------------
    let build_output = Command::new("go")
        .args(["build", "./..."])
        .current_dir(root.join("github/chatly/server"))
        .output()
        .expect("failed to run `go build ./...` in server module");

    let stdout = String::from_utf8_lossy(&build_output.stdout);
    let stderr = String::from_utf8_lossy(&build_output.stderr);

    assert!(
        build_output.status.success(),
        "`go build ./...` failed in server module — the go.work workspace wiring did not resolve the cross-module import.\nstdout: {stdout}\nstderr: {stderr}"
    );
}
