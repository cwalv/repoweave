//! E2E integration test for npm workspace wiring.
//!
//! Creates a temp weave with two Node repos and verifies that:
//! 1. `activate` generates a root `package.json` with both repos in workspaces
//! 2. `npm install` succeeds from the weave root
//! 3. `node -e "require('@chatly/shared-types')"` resolves via workspace linking
//!
//! This test is skipped if `npm` is not on PATH.

use std::path::Path;
use std::process;

/// Run a git command in `dir`, silencing output.
fn git(args: &[&str], dir: &Path) {
    let status = process::Command::new("git")
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

/// Initialise a bare-minimum git repo (no commit needed — just enough for
/// `scan_repos_on_disk` to recognise the directory as a git repo).
fn git_init(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    // Add and commit so the repo is in a clean state.
    std::fs::write(path.join(".gitkeep"), "").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

#[test]
fn npm_workspace_wiring_e2e() {
    // Skip if npm is not available.
    if process::Command::new("which")
        .arg("npm")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("SKIP: npm not found on PATH");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();

    // -------------------------------------------------------------------------
    // Create weave layout: github/ marker + projects/ dir
    // -------------------------------------------------------------------------
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();

    // -------------------------------------------------------------------------
    // Repo 1: github/chatly/shared-types
    // -------------------------------------------------------------------------
    let shared_types_dir = ws.join("github/chatly/shared-types");
    git_init(&shared_types_dir);

    std::fs::write(
        shared_types_dir.join("package.json"),
        r#"{"name": "@chatly/shared-types", "version": "1.0.0", "main": "index.js"}
"#,
    )
    .unwrap();

    std::fs::write(
        shared_types_dir.join("index.js"),
        r#"module.exports = { greeting: "hello from shared-types" };
"#,
    )
    .unwrap();

    // -------------------------------------------------------------------------
    // Repo 2: github/chatly/server
    // -------------------------------------------------------------------------
    let server_dir = ws.join("github/chatly/server");
    git_init(&server_dir);

    std::fs::write(
        server_dir.join("package.json"),
        r#"{"name": "@chatly/server", "version": "1.0.0", "dependencies": {"@chatly/shared-types": "*"}}
"#,
    )
    .unwrap();

    std::fs::write(
        server_dir.join("index.js"),
        r#"const types = require("@chatly/shared-types");
console.log(types.greeting);
"#,
    )
    .unwrap();

    // -------------------------------------------------------------------------
    // Project manifest: projects/web-app/rwv.yaml
    // -------------------------------------------------------------------------
    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();

    std::fs::write(
        project_dir.join("rwv.yaml"),
        r#"repositories:
  github/chatly/shared-types:
    type: git
    url: https://github.com/chatly/shared-types.git
    version: main
    role: primary
  github/chatly/server:
    type: git
    url: https://github.com/chatly/server.git
    version: main
    role: primary
"#,
    )
    .unwrap();

    // -------------------------------------------------------------------------
    // Write .rwv-active so activate can resolve the workspace root
    // -------------------------------------------------------------------------
    std::fs::write(ws.join(".rwv-active"), "web-app").unwrap();

    // -------------------------------------------------------------------------
    // Step 1: activate("web-app") to generate root package.json with workspaces
    // -------------------------------------------------------------------------
    repoweave::activate::activate("web-app", ws).expect("activate should succeed");

    // -------------------------------------------------------------------------
    // Step 2: verify root package.json exists and contains both repos
    // -------------------------------------------------------------------------
    let root_pkg = ws.join("package.json");
    assert!(
        root_pkg.exists(),
        "root package.json should exist after activate"
    );

    let pkg_content = std::fs::read_to_string(&root_pkg).unwrap();
    assert!(
        pkg_content.contains("github/chatly/shared-types"),
        "package.json workspaces should include shared-types, got:\n{pkg_content}"
    );
    assert!(
        pkg_content.contains("github/chatly/server"),
        "package.json workspaces should include server, got:\n{pkg_content}"
    );

    // Verify it is a proper npm workspaces object.
    let pkg_json: serde_json::Value =
        serde_json::from_str(&pkg_content).expect("package.json should be valid JSON");
    let workspaces = pkg_json
        .get("workspaces")
        .and_then(|w| w.as_array())
        .expect("package.json should have a workspaces array");

    assert!(
        workspaces
            .iter()
            .any(|w| w.as_str() == Some("github/chatly/shared-types")),
        "workspaces array should contain shared-types path"
    );
    assert!(
        workspaces
            .iter()
            .any(|w| w.as_str() == Some("github/chatly/server")),
        "workspaces array should contain server path"
    );

    // -------------------------------------------------------------------------
    // Step 3: npm install from weave root
    // -------------------------------------------------------------------------
    let npm_install = process::Command::new("npm")
        .args(["install", "--prefer-offline"])
        .current_dir(ws)
        .output()
        .expect("failed to run npm install");

    assert!(
        npm_install.status.success(),
        "npm install should succeed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&npm_install.stdout),
        String::from_utf8_lossy(&npm_install.stderr),
    );

    // -------------------------------------------------------------------------
    // Step 4: verify workspace resolution via node
    // -------------------------------------------------------------------------
    let node_check = process::Command::new("node")
        .args(["-e", "require('@chatly/shared-types')"])
        .current_dir(ws)
        .output()
        .expect("failed to run node");

    assert!(
        node_check.status.success(),
        "node should be able to require '@chatly/shared-types' via workspace resolution.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&node_check.stdout),
        String::from_utf8_lossy(&node_check.stderr),
    );
}
