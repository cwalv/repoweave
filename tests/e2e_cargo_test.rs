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

/// Run a git command in `dir`, asserting success.
fn git(args: &[&str], dir: &Path) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

/// Commit all files in `repo` with a minimal author identity and message.
fn git_commit_all(repo: &Path, message: &str) {
    git(
        &[
            "-c",
            "user.email=test@test.com",
            "-c",
            "user.name=Test",
            "add",
            "-A",
        ],
        repo,
    );
    git(
        &[
            "-c",
            "user.email=test@test.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            message,
        ],
        repo,
    );
}

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
    repoweave::activate::activate("web-app", root).expect("activate should succeed");

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

#[test]
fn cargo_release_version_pin_workflow() {
    // Skip if cargo is not available.
    if which::which("cargo").is_err() {
        eprintln!("skipping cargo_release_version_pin_workflow: cargo not on PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // ---- Step 1: set up the weave with two Rust repos ----
    setup_weave(root);

    // Commit all files in each repo so HEAD exists and generate_lock can read it.
    for repo_rel in &["github/chatly/protocol", "github/chatly/server"] {
        git_commit_all(&root.join(repo_rel), "initial commit");
    }

    // Activate to generate the root Cargo.toml workspace symlink.
    repoweave::activate::activate("web-app", root).expect("activate should succeed");

    // ---- Step 2: verify cargo check --workspace works (baseline) ----
    let check_status = Command::new("cargo")
        .args(["check", "--workspace"])
        .current_dir(root)
        .status()
        .expect("failed to run cargo check --workspace");
    assert!(
        check_status.success(),
        "cargo check --workspace should succeed as baseline"
    );

    // ---- Step 3: tag protocol with v0.1.0 ----
    let protocol_dir = root.join("github/chatly/protocol");
    git(&["tag", "v0.1.0"], &protocol_dir);

    // ---- Step 4: remove the workspace Cargo.toml symlink at weave root ----
    let root_cargo = root.join("Cargo.toml");
    assert!(
        root_cargo
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "root Cargo.toml should be a symlink before removal"
    );
    std::fs::remove_file(&root_cargo).expect("should be able to remove root Cargo.toml symlink");
    assert!(
        !root_cargo.exists(),
        "root Cargo.toml should be gone after removal"
    );

    // ---- Step 5: cargo check from server dir still works via path dep ----
    // Cargo path dependencies (`path = "../protocol"`) resolve relative to the
    // crate that declares them, without requiring a workspace Cargo.toml.
    let server_dir = root.join("github/chatly/server");
    let server_check = Command::new("cargo")
        .args(["check"])
        .current_dir(&server_dir)
        .status()
        .expect("failed to run cargo check in server dir");
    assert!(
        server_check.success(),
        "cargo check in server/ should succeed with just the path dep — no workspace needed"
    );

    // ---- Step 6: generate_lock captures the tag for protocol ----
    // Load the manifest directly from the project dir.
    let manifest_path = root.join("projects/web-app/rwv.yaml");
    let manifest =
        repoweave::manifest::Manifest::from_path(&manifest_path).expect("manifest should load");

    // dirty=true because the server repo still has untracked build artifacts
    // from `cargo check` (or simply because we don't need a pristine check here).
    let lock = repoweave::lock::generate_lock(&manifest, root, None, /*dirty=*/ true)
        .expect("generate_lock should succeed");

    let protocol_key = repoweave::manifest::RepoPath::new("github/chatly/protocol");
    let protocol_entry = lock
        .repositories
        .get(&protocol_key)
        .expect("lock should contain protocol entry");

    assert_eq!(
        protocol_entry.version.as_str(),
        "v0.1.0",
        "generate_lock should prefer the tag over the raw SHA for protocol"
    );

    // Server has no tag, so its version should be a SHA (non-empty, not a tag).
    let server_key = repoweave::manifest::RepoPath::new("github/chatly/server");
    let server_entry = lock
        .repositories
        .get(&server_key)
        .expect("lock should contain server entry");

    assert_ne!(
        server_entry.version.as_str(),
        "v0.1.0",
        "server should have a SHA, not the protocol tag"
    );
    assert!(
        !server_entry.version.as_str().is_empty(),
        "server version should be a non-empty SHA"
    );
}
