//! E2E integration tests for npm workspace wiring.
//!
//! Test 1 — npm_workspace_wiring_e2e:
//!   Creates a temp weave with two Node repos and verifies that:
//!   1. `activate` generates a root `package.json` with both repos in workspaces
//!   2. `npm install` succeeds from the weave root
//!   3. `node -e "require('@chatly/shared-types')"` resolves via workspace linking
//!
//! Test 2 — npm_release_version_pin_workflow:
//!   Validates the release version-pin workflow:
//!   1. Same two-repo setup + activate (workspace baseline)
//!   2. Verify npm install + require works in workspace mode
//!   3. Tag shared-types HEAD as v1.0.0
//!   4. Remove root package.json (simulating switch from dev to release)
//!   5. Create a server-local package.json with a file: dependency on shared-types
//!   6. npm install from server directory succeeds
//!   7. node -e "require('@chatly/shared-types')" from server directory succeeds
//!   8. generate_lock records shared-types version as "v1.0.0"
//!
//! Both tests are skipped gracefully if `npm` is not on PATH.

use std::path::Path;
use std::process;

mod common;

/// Initialise a bare-minimum git repo (no commit needed — just enough for
/// `scan_repos_on_disk` to recognise the directory as a git repo).
fn git_init(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "--initial-branch=main"]);
    common::git_in(path, &["config", "user.email", "test@test.com"]);
    common::git_in(path, &["config", "user.name", "Test"]);
    // Add and commit so the repo is in a clean state.
    std::fs::write(path.join(".gitkeep"), "").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
}

#[test]
fn npm_workspace_wiring_e2e() {
    // Skip if npm is not available.
    if which::which("npm").is_err() {
        eprintln!("SKIP: npm not found on PATH");
        return;
    }

    let tmp = common::tempdir().unwrap();
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
    // Project manifest: projects/web-app/rwv.toml
    // -------------------------------------------------------------------------
    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();

    std::fs::write(
        project_dir.join("rwv.toml"),
        r#"[repositories."github/chatly/shared-types"]
type = "git"
url = "https://github.com/chatly/shared-types.git"
version = "main"
role = "owned"

[repositories."github/chatly/server"]
type = "git"
url = "https://github.com/chatly/server.git"
version = "main"
role = "owned"
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
    {
        let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(ws, None).unwrap();
        repoweave::activate::activate_intent("web-app", &ctx).expect("activate should succeed");
    }

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
    let npm_install = process::Command::new(common::node_tool("npm"))
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

#[test]
fn npm_release_version_pin_workflow() {
    // Skip if npm is not available.
    if which::which("npm").is_err() {
        eprintln!("SKIP: npm not found on PATH");
        return;
    }

    let tmp = common::tempdir().unwrap();
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
    // Project manifest: projects/web-app/rwv.toml
    // -------------------------------------------------------------------------
    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();

    std::fs::write(
        project_dir.join("rwv.toml"),
        r#"[repositories."github/chatly/shared-types"]
type = "git"
url = "https://github.com/chatly/shared-types.git"
version = "main"
role = "owned"

[repositories."github/chatly/server"]
type = "git"
url = "https://github.com/chatly/server.git"
version = "main"
role = "owned"
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
    {
        let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(ws, None).unwrap();
        repoweave::activate::activate_intent("web-app", &ctx).expect("activate should succeed");
    }

    let root_pkg = ws.join("package.json");
    assert!(
        root_pkg.exists(),
        "root package.json should exist after activate"
    );

    // -------------------------------------------------------------------------
    // Step 2: verify npm install + require works (workspace mode baseline)
    // -------------------------------------------------------------------------
    let npm_install = process::Command::new(common::node_tool("npm"))
        .args(["install", "--prefer-offline"])
        .current_dir(ws)
        .output()
        .expect("failed to run npm install");

    assert!(
        npm_install.status.success(),
        "workspace-mode npm install should succeed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&npm_install.stdout),
        String::from_utf8_lossy(&npm_install.stderr),
    );

    let node_check = process::Command::new("node")
        .args(["-e", "require('@chatly/shared-types')"])
        .current_dir(ws)
        .output()
        .expect("failed to run node");

    assert!(
        node_check.status.success(),
        "workspace-mode node require should succeed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&node_check.stdout),
        String::from_utf8_lossy(&node_check.stderr),
    );

    // -------------------------------------------------------------------------
    // Step 3: tag shared-types HEAD as v1.0.0
    // -------------------------------------------------------------------------
    common::git_in(&shared_types_dir, &["tag", "v1.0.0"]);

    // -------------------------------------------------------------------------
    // Step 4: remove root package.json symlink (simulate switch to release mode)
    // -------------------------------------------------------------------------
    std::fs::remove_file(&root_pkg).expect("should be able to remove root package.json");

    // -------------------------------------------------------------------------
    // Step 5: create server-local package.json with file: dependency
    // -------------------------------------------------------------------------
    // First clean up any workspace node_modules so they don't interfere
    let ws_node_modules = ws.join("node_modules");
    if ws_node_modules.exists() {
        std::fs::remove_dir_all(&ws_node_modules).ok();
    }

    // Write a local package.json in server/ that pins shared-types via file:.
    // Both repos live under github/chatly/, so from server/ the relative path
    // to shared-types is simply ../shared-types.
    std::fs::write(
        server_dir.join("package.json"),
        r#"{"name": "@chatly/server", "version": "1.0.0", "dependencies": {"@chatly/shared-types": "file:../shared-types"}}
"#,
    )
    .unwrap();

    // -------------------------------------------------------------------------
    // Step 6: npm install from server directory
    // -------------------------------------------------------------------------
    let npm_install_server = process::Command::new(common::node_tool("npm"))
        .args(["install", "--prefer-offline"])
        .current_dir(&server_dir)
        .output()
        .expect("failed to run npm install in server dir");

    assert!(
        npm_install_server.status.success(),
        "release-pin npm install from server dir should succeed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&npm_install_server.stdout),
        String::from_utf8_lossy(&npm_install_server.stderr),
    );

    // -------------------------------------------------------------------------
    // Step 7: node -e "require('@chatly/shared-types')" from server directory
    // -------------------------------------------------------------------------
    let node_check_server = process::Command::new("node")
        .args(["-e", "require('@chatly/shared-types')"])
        .current_dir(&server_dir)
        .output()
        .expect("failed to run node in server dir");

    assert!(
        node_check_server.status.success(),
        "node require from server dir should resolve shared-types via file: pin.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&node_check_server.stdout),
        String::from_utf8_lossy(&node_check_server.stderr),
    );

    // -------------------------------------------------------------------------
    // Step 8: generate_lock records shared-types version as "v1.0.0"
    // -------------------------------------------------------------------------
    let manifest = repoweave::manifest::Manifest::from_path(&project_dir.join("rwv.toml"))
        .expect("manifest should parse");

    let lock = repoweave::lock::generate_lock(&manifest, ws, None, true)
        .expect("generate_lock should succeed");

    let shared_types_entry = lock
        .get_entry(
            &repoweave::manifest::RepoPath::new("github/chatly/shared-types")
                .expect("known-safe literal"),
        )
        .expect("lock should contain shared-types entry");

    assert_eq!(
        shared_types_entry.version.display_str(),
        "v1.0.0",
        "generate_lock should record the tag name 'v1.0.0' for shared-types, got: {}",
        shared_types_entry.version.display_str(),
    );
}
