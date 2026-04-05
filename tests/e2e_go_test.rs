//! E2E integration test for Go workspace wiring via the go-work integration.
//!
//! This test verifies that activating a project with two Go modules generates
//! a `go.work` file at the weave root that correctly wires the cross-module
//! import so that `go build ./...` succeeds.
//!
//! Requires `go` on PATH — the test skips gracefully if it is not available.

use std::path::Path;
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

// ---------------------------------------------------------------------------
// Helper: run a git command and assert success, returning stdout.
// ---------------------------------------------------------------------------
fn git_run(args: &[&str], dir: &Path) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap_or_else(|e| panic!("git {:?} failed to spawn: {e}", args));
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Validate the "release version pin" workflow:
///
/// After developing with go.work (workspace wiring), you can switch to
/// published version pins for release. The rwv.lock records the tag name
/// when HEAD is tagged — this is the bridge between "what we tested in the
/// workspace" and "what version to publish against."
#[test]
fn go_release_version_pin_workflow() {
    require_go!();

    // ------------------------------------------------------------------
    // 1. Create weave root with the required directory structure
    // ------------------------------------------------------------------
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    // ------------------------------------------------------------------
    // 2. Set up github/chatly/protocol — a Go module exporting a function
    // ------------------------------------------------------------------
    let protocol_dir = root.join("github/chatly/protocol");
    std::fs::create_dir_all(&protocol_dir).unwrap();

    git_run(&["init", "-q", "-b", "main"], &protocol_dir);

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

    // Commit so HEAD exists and the repo is clean
    git_run(&["add", "."], &protocol_dir);
    git_run(&["commit", "-m", "initial protocol"], &protocol_dir);

    // ------------------------------------------------------------------
    // 3. Set up github/chatly/server — imports github.com/chatly/protocol
    // ------------------------------------------------------------------
    let server_dir = root.join("github/chatly/server");
    std::fs::create_dir_all(&server_dir).unwrap();

    git_run(&["init", "-q", "-b", "main"], &server_dir);

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

    // Commit so HEAD exists and the repo is clean
    git_run(&["add", "."], &server_dir);
    git_run(&["commit", "-m", "initial server"], &server_dir);

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
    // 5. Write .rwv-active and activate to generate go.work
    // ------------------------------------------------------------------
    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();
    repoweave::activate::activate("web-app", root).expect("activate should succeed");

    let go_work_path = root.join("go.work");
    assert!(
        go_work_path.exists(),
        "go.work should exist after activation"
    );

    // ------------------------------------------------------------------
    // 6. Tag protocol at v1.0.0 (the "release" tag)
    // ------------------------------------------------------------------
    git_run(&["tag", "v1.0.0"], &protocol_dir);

    // ------------------------------------------------------------------
    // 7. Verify go build works WITH go.work (workspace/dev mode — baseline)
    // ------------------------------------------------------------------
    let build_with_workspace = Command::new("go")
        .args(["build", "./..."])
        .current_dir(&server_dir)
        .output()
        .expect("failed to spawn `go build ./...` (workspace mode)");

    let ws_stdout = String::from_utf8_lossy(&build_with_workspace.stdout);
    let ws_stderr = String::from_utf8_lossy(&build_with_workspace.stderr);
    assert!(
        build_with_workspace.status.success(),
        "`go build ./...` (workspace mode) failed — baseline should pass.\nstdout: {ws_stdout}\nstderr: {ws_stderr}"
    );

    // ------------------------------------------------------------------
    // 8. Remove the go.work symlink — simulate switching to release mode
    // ------------------------------------------------------------------
    std::fs::remove_file(&go_work_path).expect("failed to remove go.work symlink at weave root");

    // ------------------------------------------------------------------
    // 9. Pin server's go.mod to protocol v1.0.0 via a local replace
    //    directive (since this module is not on a Go proxy).
    // ------------------------------------------------------------------
    let mod_edit = Command::new("go")
        .args([
            "mod",
            "edit",
            "-replace",
            "github.com/chatly/protocol=../protocol",
        ])
        .current_dir(&server_dir)
        .output()
        .expect("failed to spawn `go mod edit -replace`");

    assert!(
        mod_edit.status.success(),
        "`go mod edit -replace` failed: {}",
        String::from_utf8_lossy(&mod_edit.stderr)
    );

    // ------------------------------------------------------------------
    // 10. Verify go build works WITHOUT go.work (release/replace mode)
    // ------------------------------------------------------------------
    let build_without_workspace = Command::new("go")
        .args(["build", "./..."])
        .current_dir(&server_dir)
        .env_remove("GOWORK") // ensure no ambient go.work override
        .output()
        .expect("failed to spawn `go build ./...` (replace mode)");

    let rel_stdout = String::from_utf8_lossy(&build_without_workspace.stdout);
    let rel_stderr = String::from_utf8_lossy(&build_without_workspace.stderr);
    assert!(
        build_without_workspace.status.success(),
        "`go build ./...` (replace/release mode) failed — replace directive should resolve the dependency.\nstdout: {rel_stdout}\nstderr: {rel_stderr}"
    );

    // ------------------------------------------------------------------
    // 11. Generate rwv.lock and verify protocol's version is "v1.0.0"
    //     (HEAD is tagged, so generate_lock should record the tag name)
    // ------------------------------------------------------------------
    let manifest = repoweave::manifest::Manifest::from_path(&project_dir.join("rwv.yaml"))
        .expect("failed to parse rwv.yaml");

    let lock = repoweave::lock::generate_lock(&manifest, root, None, true)
        .expect("generate_lock should succeed");

    let protocol_entry = lock
        .repositories
        .get(&repoweave::manifest::RepoPath::new(
            "github/chatly/protocol",
        ))
        .expect("lock should contain github/chatly/protocol");

    assert_eq!(
        protocol_entry.version.as_str(),
        "v1.0.0",
        "rwv.lock should record the tag name 'v1.0.0' for the protocol repo when HEAD is tagged"
    );
}
