//! E2E integration test for Cargo workspace wiring.
//!
//! Creates a temp directory as a weave with two Rust crates, activates a
//! project, verifies the generated root `Cargo.toml` workspace, and then
//! runs `cargo check --workspace` and `cargo test --workspace` to confirm
//! the workspace compiles correctly.
//!
//! Requires `cargo` on PATH. The test skips gracefully if cargo is absent.

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// Set up the weave directory structure for the cargo workspace e2e test.
///
/// Layout:
///   {tmp}/
///     github/
///       chatly/
///         protocol/   <- chatly-protocol crate (git repo)
///         server/     <- chatly-server crate (git repo, depends on protocol)
///     projects/
///       web-app/
///         rwv.yaml
fn setup_weave(tmp: &Path) {
    // ---- directories ----
    std::fs::create_dir_all(tmp.join("github/chatly/protocol/src")).unwrap();
    std::fs::create_dir_all(tmp.join("github/chatly/server/src")).unwrap();
    std::fs::create_dir_all(tmp.join("projects/web-app")).unwrap();

    // ---- chatly-protocol ----
    std::fs::write(
        tmp.join("github/chatly/protocol/Cargo.toml"),
        "[package]\nname = \"chatly-protocol\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("github/chatly/protocol/src/lib.rs"),
        "/// Returns the protocol version string.\npub fn version() -> &'static str { \"1.0\" }\n",
    )
    .unwrap();

    // ---- chatly-server ----
    // The path dependency is relative from the server crate dir to the protocol
    // crate dir: server is at github/chatly/server, protocol is at
    // github/chatly/protocol, so the relative path is ../../chatly/protocol.
    // However, cargo resolves path deps relative to the workspace root Cargo.toml
    // when members are workspace paths. Actually, path deps in member Cargo.toml
    // are relative to that member's directory. From server/ the protocol dir is
    // at ../protocol.
    std::fs::write(
        tmp.join("github/chatly/server/Cargo.toml"),
        "[package]\nname = \"chatly-server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\nchatly-protocol = { path = \"../protocol\" }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("github/chatly/server/src/main.rs"),
        "fn main() {\n    println!(\"{}\", chatly_protocol::version());\n}\n",
    )
    .unwrap();

    // ---- git init for each repo (required by scan_repos_on_disk) ----
    for repo_rel in &["github/chatly/protocol", "github/chatly/server"] {
        let repo_path = tmp.join(repo_rel);
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo_path)
            .status()
            .expect("git should be available");
        assert!(status.success(), "git init failed in {repo_rel}");
    }

    // ---- rwv.yaml manifest ----
    let manifest = "\
repositories:
  github/chatly/protocol:
    type: git
    url: https://github.com/chatly/protocol.git
    version: main
    role: primary
  github/chatly/server:
    type: git
    url: https://github.com/chatly/server.git
    version: main
    role: primary
";
    std::fs::write(tmp.join("projects/web-app/rwv.yaml"), manifest).unwrap();

    // ---- .rwv-active ----
    std::fs::write(tmp.join(".rwv-active"), "web-app\n").unwrap();
}

#[test]
fn e2e_cargo_workspace_wiring() {
    // Skip if cargo is not available.
    if which::which("cargo").is_err() {
        eprintln!("skipping e2e_cargo_test: cargo not on PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    setup_weave(root);

    // ---- Step 1: activate("web-app", root) generates root Cargo.toml ----
    repoweave::activate::activate("web-app", root)
        .expect("activate should succeed");

    // ---- Step 2: verify root Cargo.toml exists and is a symlink ----
    let root_cargo = root.join("Cargo.toml");
    assert!(
        root_cargo.exists(),
        "root Cargo.toml should exist after activation"
    );
    // The root Cargo.toml is a symlink pointing into projects/web-app/Cargo.toml.
    assert!(
        root_cargo
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "root Cargo.toml should be a symlink to the project dir"
    );

    // ---- Step 3: verify the generated Cargo.toml contains [workspace] ----
    let cargo_content = std::fs::read_to_string(&root_cargo).unwrap();
    assert!(
        cargo_content.contains("[workspace]"),
        "generated Cargo.toml should contain [workspace], got:\n{cargo_content}"
    );
    assert!(
        cargo_content.contains("github/chatly/protocol"),
        "generated Cargo.toml should list protocol member, got:\n{cargo_content}"
    );
    assert!(
        cargo_content.contains("github/chatly/server"),
        "generated Cargo.toml should list server member, got:\n{cargo_content}"
    );

    // ---- Step 4: cargo check --workspace ----
    let check_status = Command::new("cargo")
        .args(["check", "--workspace"])
        .current_dir(root)
        .status()
        .expect("failed to run cargo check");
    assert!(
        check_status.success(),
        "cargo check --workspace should succeed"
    );

    // ---- Step 5: cargo test --workspace ----
    let test_status = Command::new("cargo")
        .args(["test", "--workspace"])
        .current_dir(root)
        .status()
        .expect("failed to run cargo test");
    assert!(
        test_status.success(),
        "cargo test --workspace should succeed"
    );
}
