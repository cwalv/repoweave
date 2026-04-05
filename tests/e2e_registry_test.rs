//! E2E integration tests that exercise the release workflow recipes from
//! docs/workflows.md against real package registries.
//!
//! These tests are gated behind the `RWV_E2E_REGISTRY=1` environment variable
//! because they require network access and are slower than the other e2e tests.
//!
//! Run with:
//!   RWV_E2E_REGISTRY=1 cargo test --test e2e_registry_test
//!
//! Fixture packages published to real registries:
//!   - Go:     github.com/cwalv/repoweave/tests/fixtures/go/rwv-test-protocol v1.0.0
//!   - npm:    @cwalv/rwv-test-types 1.0.0
//!   - Cargo:  rwv-test-lib 0.1.0
//!   - Python: rwv-test-types 1.0.0

use std::process::Command;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Skip the test unless RWV_E2E_REGISTRY=1 is set.
macro_rules! require_registry_e2e {
    () => {
        if std::env::var("RWV_E2E_REGISTRY").is_err() {
            eprintln!("SKIP: set RWV_E2E_REGISTRY=1 to run registry e2e tests");
            return;
        }
    };
}

/// Skip the test if `tool` is not on PATH.
macro_rules! require_tool {
    ($tool:expr) => {
        if Command::new("which")
            .arg($tool)
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("SKIP: `{}` not found on PATH", $tool);
            return;
        }
    };
}

// ---------------------------------------------------------------------------
// Test 1 — Go registry release workflow
//
// Recipe from docs/workflows.md ("Per-ecosystem recipes", Go section):
//
//   rwv lock
//   cd github/chatly/protocol
//   git tag v1.5.0 && git push origin v1.5.0
//   cd ../server
//   go get github.com/chatly/protocol@v1.5.0   ← update go.mod pin
//   git tag v2.2.0 && git push origin v2.2.0
//
// We can't push to real GitHub repos in tests, so we exercise the registry
// half: the consumer module (`server`) pins the REAL published module
// `github.com/cwalv/repoweave/tests/fixtures/go/rwv-test-protocol@v1.0.0`
// via `go get`, verifying that the proxy resolves and go build succeeds.
// ---------------------------------------------------------------------------

#[test]
fn go_registry_release_workflow() {
    require_registry_e2e!();
    require_tool!("go");

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // -----------------------------------------------------------------------
    // 1. Create a fresh Go module that will act as the "server" consumer.
    //    It does NOT depend on the published module yet — we'll `go get` it.
    // -----------------------------------------------------------------------
    let server_dir = root.join("server");
    std::fs::create_dir_all(&server_dir).unwrap();

    // Minimal go.mod — module path is fictional, only the dependency matters.
    std::fs::write(
        server_dir.join("go.mod"),
        "module example.com/rwv-registry-test-server\n\ngo 1.21\n",
    )
    .unwrap();

    // main.go imports the published protocol package.
    std::fs::write(
        server_dir.join("main.go"),
        r#"package main

import (
	"fmt"
	protocol "github.com/cwalv/repoweave/tests/fixtures/go/rwv-test-protocol"
)

func main() {
	fmt.Println(protocol.Greeting())
}
"#,
    )
    .unwrap();

    // -----------------------------------------------------------------------
    // 2. `go get` the real published module from proxy.golang.org.
    //    This is the registry analogue of `go get github.com/chatly/protocol@v1.5.0`
    //    from the workflows.md recipe.
    // -----------------------------------------------------------------------
    let get_output = Command::new("go")
        .args([
            "get",
            "github.com/cwalv/repoweave/tests/fixtures/go/rwv-test-protocol@v1.0.0",
        ])
        .current_dir(&server_dir)
        .output()
        .expect("failed to run `go get`");

    let get_stdout = String::from_utf8_lossy(&get_output.stdout);
    let get_stderr = String::from_utf8_lossy(&get_output.stderr);
    assert!(
        get_output.status.success(),
        "`go get` failed — proxy.golang.org may be unreachable or the module is not published.\nstdout: {get_stdout}\nstderr: {get_stderr}"
    );

    // -----------------------------------------------------------------------
    // 3. Verify go.mod now contains the version pin.
    // -----------------------------------------------------------------------
    let go_mod = std::fs::read_to_string(server_dir.join("go.mod")).unwrap();
    assert!(
        go_mod.contains("github.com/cwalv/repoweave/tests/fixtures/go/rwv-test-protocol"),
        "go.mod should contain the published module path after `go get`, got:\n{go_mod}"
    );
    assert!(
        go_mod.contains("v1.0.0"),
        "go.mod should pin v1.0.0 after `go get`, got:\n{go_mod}"
    );

    // -----------------------------------------------------------------------
    // 4. `go build` succeeds using the registry-resolved version.
    //    No go.work present — the dependency resolves via the module proxy.
    // -----------------------------------------------------------------------
    let build_output = Command::new("go")
        .args(["build", "./..."])
        .current_dir(&server_dir)
        .env_remove("GOWORK") // ensure no ambient go.work interferes
        .output()
        .expect("failed to run `go build ./...`");

    let build_stdout = String::from_utf8_lossy(&build_output.stdout);
    let build_stderr = String::from_utf8_lossy(&build_output.stderr);
    assert!(
        build_output.status.success(),
        "`go build ./...` failed — registry resolution did not work.\nstdout: {build_stdout}\nstderr: {build_stderr}"
    );
}

// ---------------------------------------------------------------------------
// Test 2 — npm registry release workflow
//
// Recipe from docs/workflows.md ("Per-ecosystem recipes", Node section):
//
//   cd github/chatly/shared-types
//   npm version 1.3.0 && npm publish
//   cd ../server
//   npm install @chatly/shared-types@1.3.0   ← update pin
//   npm version 2.1.0 && npm publish
//
// We consume the REAL published package `@cwalv/rwv-test-types@1.0.0` from
// npmjs.com, verifying that `npm install` resolves from the real registry and
// that the installed module is callable.
// ---------------------------------------------------------------------------

#[test]
fn npm_registry_release_workflow() {
    require_registry_e2e!();
    require_tool!("npm");

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // -----------------------------------------------------------------------
    // 1. Create a minimal package.json depending on the published package.
    // -----------------------------------------------------------------------
    std::fs::write(
        dir.join("package.json"),
        r#"{
  "name": "rwv-registry-test-consumer",
  "version": "0.0.1",
  "private": true,
  "dependencies": {
    "@cwalv/rwv-test-types": "1.0.0"
  }
}
"#,
    )
    .unwrap();

    // -----------------------------------------------------------------------
    // 2. `npm install` — resolves @cwalv/rwv-test-types from the real npm registry.
    //    This is the release-pin analogue of `npm install @chatly/shared-types@1.3.0`.
    // -----------------------------------------------------------------------
    let install_output = Command::new("npm")
        .args(["install"])
        .current_dir(dir)
        .output()
        .expect("failed to run `npm install`");

    let install_stdout = String::from_utf8_lossy(&install_output.stdout);
    let install_stderr = String::from_utf8_lossy(&install_output.stderr);
    assert!(
        install_output.status.success(),
        "`npm install` failed — npmjs.com may be unreachable or the package is not published.\nstdout: {install_stdout}\nstderr: {install_stderr}"
    );

    // -----------------------------------------------------------------------
    // 3. Verify node_modules/@cwalv/rwv-test-types exists.
    // -----------------------------------------------------------------------
    let module_dir = dir.join("node_modules/@cwalv/rwv-test-types");
    assert!(
        module_dir.exists(),
        "node_modules/@cwalv/rwv-test-types should exist after `npm install`"
    );

    // -----------------------------------------------------------------------
    // 4. Verify the installed module is callable via node.
    // -----------------------------------------------------------------------
    let node_output = Command::new("node")
        .args([
            "-e",
            "const t = require('@cwalv/rwv-test-types'); console.log(t.greeting());",
        ])
        .current_dir(dir)
        .output()
        .expect("failed to run `node`");

    let node_stdout = String::from_utf8_lossy(&node_output.stdout);
    let node_stderr = String::from_utf8_lossy(&node_output.stderr);
    assert!(
        node_output.status.success(),
        "`node` invocation failed.\nstdout: {node_stdout}\nstderr: {node_stderr}"
    );
    assert!(
        node_stdout.trim().contains("hello from rwv-test-types"),
        "node output should contain 'hello from rwv-test-types', got: {node_stdout}"
    );
}

// ---------------------------------------------------------------------------
// Test 3 — Cargo registry release workflow
//
// Recipe from docs/workflows.md ("Per-ecosystem recipes", Cargo section):
//
//   cd github/chatly/protocol
//   cargo publish                        ← publish to crates.io
//   cd ../server
//   # update Cargo.toml: protocol = "1.5.0"
//   cargo publish
//
// We create a minimal crate that declares `rwv-test-lib = "0.1.0"` (a
// version dependency, NOT a path dep) and verify that `cargo check` resolves
// it from the real crates.io registry and that Cargo.lock records the crate.
// ---------------------------------------------------------------------------

#[test]
fn cargo_registry_release_workflow() {
    require_registry_e2e!();
    require_tool!("cargo");

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // -----------------------------------------------------------------------
    // 1. Create src/ so Cargo recognises this as a valid lib crate.
    // -----------------------------------------------------------------------
    std::fs::create_dir_all(dir.join("src")).unwrap();

    // -----------------------------------------------------------------------
    // 2. Cargo.toml with a version dependency on the published crate.
    //    This is the release-pin form (`rwv-test-lib = "0.1.0"`) rather than
    //    the workspace path dep used during development.
    // -----------------------------------------------------------------------
    std::fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "rwv-registry-test-consumer"
version = "0.1.0"
edition = "2021"

[dependencies]
rwv-test-lib = "0.1.0"
"#,
    )
    .unwrap();

    std::fs::write(
        dir.join("src/lib.rs"),
        r#"pub fn hello() -> &'static str {
    rwv_test_lib::greeting()
}
"#,
    )
    .unwrap();

    // -----------------------------------------------------------------------
    // 3. `cargo check` — resolves rwv-test-lib from crates.io.
    // -----------------------------------------------------------------------
    let check_output = Command::new("cargo")
        .args(["check"])
        .current_dir(dir)
        .output()
        .expect("failed to run `cargo check`");

    let check_stdout = String::from_utf8_lossy(&check_output.stdout);
    let check_stderr = String::from_utf8_lossy(&check_output.stderr);
    assert!(
        check_output.status.success(),
        "`cargo check` failed — crates.io may be unreachable or rwv-test-lib is not published.\nstdout: {check_stdout}\nstderr: {check_stderr}"
    );

    // -----------------------------------------------------------------------
    // 4. Verify Cargo.lock contains rwv-test-lib.
    // -----------------------------------------------------------------------
    let lock_path = dir.join("Cargo.lock");
    assert!(
        lock_path.exists(),
        "Cargo.lock should exist after `cargo check`"
    );

    let lock_content = std::fs::read_to_string(&lock_path).unwrap();
    assert!(
        lock_content.contains("rwv-test-lib"),
        "Cargo.lock should contain 'rwv-test-lib', got:\n{lock_content}"
    );
    assert!(
        lock_content.contains("0.1.0"),
        "Cargo.lock should contain version '0.1.0', got:\n{lock_content}"
    );
}

// ---------------------------------------------------------------------------
// Test 4 — Python (PyPI) registry release workflow
//
// Analogous to the other ecosystems: install the real published package
// `rwv-test-types==1.0.0` from PyPI into a venv using `uv pip install`,
// then verify the installed module is importable and callable.
// ---------------------------------------------------------------------------

#[test]
fn python_registry_release_workflow() {
    require_registry_e2e!();
    require_tool!("uv");

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // -----------------------------------------------------------------------
    // 1. Create a venv.
    // -----------------------------------------------------------------------
    let venv_dir = dir.join(".venv");
    let venv_output = Command::new("uv")
        .args(["venv", venv_dir.to_str().unwrap()])
        .current_dir(dir)
        .output()
        .expect("failed to run `uv venv`");

    let venv_stdout = String::from_utf8_lossy(&venv_output.stdout);
    let venv_stderr = String::from_utf8_lossy(&venv_output.stderr);
    assert!(
        venv_output.status.success(),
        "`uv venv` failed.\nstdout: {venv_stdout}\nstderr: {venv_stderr}"
    );

    // -----------------------------------------------------------------------
    // 2. `uv pip install rwv-test-types==1.0.0` into the venv.
    // -----------------------------------------------------------------------
    let install_output = Command::new("uv")
        .args([
            "pip",
            "install",
            "--python",
            venv_dir.join("bin/python").to_str().unwrap(),
            "rwv-test-types==1.0.0",
        ])
        .current_dir(dir)
        .output()
        .expect("failed to run `uv pip install`");

    let install_stdout = String::from_utf8_lossy(&install_output.stdout);
    let install_stderr = String::from_utf8_lossy(&install_output.stderr);
    assert!(
        install_output.status.success(),
        "`uv pip install rwv-test-types==1.0.0` failed — PyPI may be unreachable or the package is not published.\nstdout: {install_stdout}\nstderr: {install_stderr}"
    );

    // -----------------------------------------------------------------------
    // 3. Verify the installed module is importable and callable.
    // -----------------------------------------------------------------------
    let python_bin = venv_dir.join("bin/python");
    let python_output = Command::new(&python_bin)
        .args([
            "-c",
            "from rwv_test_types import greeting; print(greeting())",
        ])
        .current_dir(dir)
        .output()
        .expect("failed to run python");

    let python_stdout = String::from_utf8_lossy(&python_output.stdout);
    let python_stderr = String::from_utf8_lossy(&python_output.stderr);
    assert!(
        python_output.status.success(),
        "python invocation failed.\nstdout: {python_stdout}\nstderr: {python_stderr}"
    );
    assert!(
        python_stdout.trim().contains("hello from rwv-test-types"),
        "python output should contain 'hello from rwv-test-types', got: {python_stdout}"
    );
}
