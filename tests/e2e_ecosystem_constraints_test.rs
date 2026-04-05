//! Behavioral documentation tests: how each ecosystem tool handles version
//! constraint mismatches in workspace / local-path mode.
//!
//! The question answered by each test:
//!   If A depends on B with a caret/compatible constraint (^1.0 or equivalent),
//!   and B's on-disk version is bumped to 2.0.0, does the ecosystem tool catch
//!   the incompatibility?
//!
//! These tests record the *actual observed behavior* so we can detect if that
//! behavior changes across tool version upgrades.  A test PASSES when the tool
//! behaves as documented, regardless of whether the behavior is "strict" or
//! "lenient".
//!
//! Observations recorded on 2026-04-05, tool versions:
//!   cargo  1.94.0
//!   go     1.26.1
//!   npm    11.9.0
//!   uv     0.11.2

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return `true` if the given binary is on PATH.
fn tool_available(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Return the major version of a tool, or 0 if it can't be determined.
fn tool_major_version(name: &str) -> u32 {
    let output = Command::new(name)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    // Parse first number-like token (e.g. "9.2.0" from "9.2.0" or "npm 11.9.0")
    output
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .find(|s| s.contains('.'))
        .and_then(|v| v.split('.').next())
        .and_then(|major| major.parse().ok())
        .unwrap_or(0)
}

/// Write `content` to `path`, creating parent directories as needed.
fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

// ---------------------------------------------------------------------------
// Test 1 — Cargo
// ---------------------------------------------------------------------------

/// Cargo CATCHES a workspace version constraint mismatch.
///
/// Observed behavior (2026-04-05, cargo 1.94.0):
///   When `app` declares `mylib = { path = "../lib", version = "^1.0" }` but
///   the on-disk `lib/Cargo.toml` has `version = "2.0.0"`, `cargo check` fails
///   immediately with:
///
///     error: failed to select a version for the requirement `mylib = "^1.0"`
///     candidate versions found which didn't match: 2.0.0
///
///   The path dependency bypasses the registry, but Cargo still validates that
///   the local version satisfies the version constraint.
#[test]
fn cargo_catches_workspace_version_mismatch() {
    if !tool_available("cargo") {
        eprintln!("SKIP: cargo not on PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // ---- workspace Cargo.toml ----
    write_file(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"lib\", \"app\"]\nresolver = \"2\"\n",
    );

    // ---- lib at v1.0.0 (baseline) ----
    write_file(
        &root.join("lib/Cargo.toml"),
        "[package]\nname = \"mylib\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
    );
    write_file(
        &root.join("lib/src/lib.rs"),
        "pub fn hello() -> &'static str { \"hello\" }\n",
    );

    // ---- app depends on mylib ^1.0 ----
    write_file(
        &root.join("app/Cargo.toml"),
        "[package]\nname = \"myapp\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\nmylib = { path = \"../lib\", version = \"^1.0\" }\n",
    );
    write_file(
        &root.join("app/src/main.rs"),
        "fn main() { println!(\"{}\", mylib::hello()); }\n",
    );

    // ---- Step 1: baseline (lib = 1.0.0) should succeed ----
    let baseline = Command::new("cargo")
        .args(["check", "--workspace"])
        .current_dir(root)
        .output()
        .expect("failed to spawn cargo check (baseline)");

    let baseline_stderr = String::from_utf8_lossy(&baseline.stderr);
    assert!(
        baseline.status.success(),
        "cargo check should succeed when lib=1.0.0 satisfies ^1.0.\nstderr:\n{baseline_stderr}"
    );

    // ---- Step 2: bump lib to 2.0.0 ----
    write_file(
        &root.join("lib/Cargo.toml"),
        "[package]\nname = \"mylib\"\nversion = \"2.0.0\"\nedition = \"2021\"\n",
    );

    // ---- Step 3: cargo check should FAIL ----
    let mismatch = Command::new("cargo")
        .args(["check", "--workspace"])
        .current_dir(root)
        .output()
        .expect("failed to spawn cargo check (mismatch)");

    let mismatch_stderr = String::from_utf8_lossy(&mismatch.stderr);

    // DOCUMENTED BEHAVIOR: Cargo rejects the incompatible local version.
    assert!(
        !mismatch.status.success(),
        "cargo check should FAIL when lib=2.0.0 does not satisfy ^1.0, but it succeeded.\nstderr:\n{mismatch_stderr}"
    );

    assert!(
        mismatch_stderr.contains("failed to select a version"),
        "expected 'failed to select a version' in cargo error output.\nActual stderr:\n{mismatch_stderr}"
    );
    assert!(
        mismatch_stderr.contains("2.0.0"),
        "error should mention the conflicting candidate version 2.0.0.\nActual stderr:\n{mismatch_stderr}"
    );
}

// ---------------------------------------------------------------------------
// Test 2 — Go
// ---------------------------------------------------------------------------

/// Go's go.work CATCHES a version mismatch via module path convention.
///
/// Go does not use semver constraints in go.mod the same way as other
/// ecosystems.  Instead, major version >1 is encoded in the module path
/// (`github.com/test/lib/v2`).  When the lib module renames its path to
/// `/v2`, the old import path in app's go.mod can no longer be resolved by
/// the workspace (the `use ./lib` directive now provides `github.com/test/lib/v2`,
/// not `github.com/test/lib`).  `go build` falls back to the module proxy and
/// fails because the module does not exist there.
///
/// Observed behavior (2026-04-05, go 1.26.1):
///   Baseline (lib module path = `github.com/test/lib`):  `go build ./...` EXIT 0
///   After renaming to `github.com/test/lib/v2`:           `go build ./...` EXIT 1
///   Error contains: `github.com/test/lib@v0.0.0: reading github.com/test/lib/go.mod`
#[test]
fn go_work_catches_version_mismatch() {
    if !tool_available("go") {
        eprintln!("SKIP: go not on PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // ---- lib module at v1 path ----
    write_file(
        &root.join("lib/go.mod"),
        "module github.com/test/lib\n\ngo 1.21\n",
    );
    write_file(
        &root.join("lib/lib.go"),
        "package lib\n\nfunc Hello() string { return \"hello\" }\n",
    );

    // ---- app imports lib at v1 path ----
    write_file(
        &root.join("app/go.mod"),
        "module github.com/test/app\n\ngo 1.21\n\nrequire github.com/test/lib v0.0.0\n",
    );
    write_file(
        &root.join("app/main.go"),
        "package main\n\nimport (\n\t\"fmt\"\n\t\"github.com/test/lib\"\n)\n\nfunc main() {\n\tfmt.Println(lib.Hello())\n}\n",
    );

    // ---- go.work wires both modules ----
    write_file(
        &root.join("go.work"),
        "go 1.21\n\nuse (\n\t./lib\n\t./app\n)\n",
    );

    // ---- Step 1: baseline (lib path = github.com/test/lib) should succeed ----
    let baseline = Command::new("go")
        .args(["build", "./..."])
        .current_dir(root.join("app"))
        .output()
        .expect("failed to spawn go build (baseline)");

    let baseline_stderr = String::from_utf8_lossy(&baseline.stderr);
    assert!(
        baseline.status.success(),
        "go build should succeed when lib module path matches the import.\nstderr:\n{baseline_stderr}"
    );

    // ---- Step 2: rename lib module path to v2 convention ----
    write_file(
        &root.join("lib/go.mod"),
        "module github.com/test/lib/v2\n\ngo 1.21\n",
    );

    // ---- Step 3: go build should FAIL ----
    // The workspace now provides `github.com/test/lib/v2` but app requires
    // `github.com/test/lib`.  Go cannot satisfy the old import path from the
    // workspace and attempts (unsuccessfully) to fetch it from the proxy.
    let mismatch = Command::new("go")
        .args(["build", "./..."])
        .current_dir(root.join("app"))
        .output()
        .expect("failed to spawn go build (mismatch)");

    let mismatch_stderr = String::from_utf8_lossy(&mismatch.stderr);

    // DOCUMENTED BEHAVIOR: Go fails to resolve the old module path.
    assert!(
        !mismatch.status.success(),
        "go build should FAIL when lib renames to /v2 but app still imports the v1 path.\nstderr:\n{mismatch_stderr}"
    );

    assert!(
        mismatch_stderr.contains("github.com/test/lib@v0.0.0"),
        "error should mention the unresolvable v1 module path.\nActual stderr:\n{mismatch_stderr}"
    );
}

// ---------------------------------------------------------------------------
// Test 3 — npm
// ---------------------------------------------------------------------------

/// npm workspace CATCHES a version constraint mismatch.
///
/// Observed behavior (2026-04-05, npm 11.9.0; npm 9.x does NOT catch this):
///   When `app/package.json` depends on `"mylib": "^1.0.0"` but the workspace
///   member `lib/package.json` declares `"version": "2.0.0"`, `npm install`
///   fails with:
///
///     npm error code ETARGET
///     npm error notarget No matching version found for mylib@^1.0.0.
///
///   npm refuses to use the local workspace package when its declared version
///   does not satisfy the constraint.  It then tries the registry, finds no
///   matching published version, and errors.
#[test]
fn npm_workspace_checks_version_mismatch() {
    if !tool_available("npm") {
        eprintln!("SKIP: npm not on PATH");
        return;
    }
    if tool_major_version("npm") < 10 {
        eprintln!("SKIP: npm {} does not enforce workspace version constraints (need 10+)", tool_major_version("npm"));
        return;
    }

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // ---- root package.json with workspaces ----
    write_file(
        &root.join("package.json"),
        "{\n  \"name\": \"test-workspace\",\n  \"private\": true,\n  \"workspaces\": [\"lib\", \"app\"]\n}\n",
    );

    // ---- lib at v1.0.0 (baseline) ----
    write_file(
        &root.join("lib/package.json"),
        "{\n  \"name\": \"mylib\",\n  \"version\": \"1.0.0\"\n}\n",
    );

    // ---- app depends on mylib ^1.0.0 ----
    write_file(
        &root.join("app/package.json"),
        "{\n  \"name\": \"myapp\",\n  \"version\": \"1.0.0\",\n  \"dependencies\": {\n    \"mylib\": \"^1.0.0\"\n  }\n}\n",
    );

    // ---- Step 1: baseline (lib = 1.0.0) should succeed ----
    let baseline = Command::new("npm")
        .args(["install"])
        .current_dir(root)
        .output()
        .expect("failed to spawn npm install (baseline)");

    let baseline_stderr = String::from_utf8_lossy(&baseline.stderr);
    let baseline_stdout = String::from_utf8_lossy(&baseline.stdout);
    assert!(
        baseline.status.success(),
        "npm install should succeed when lib=1.0.0 satisfies ^1.0.0.\nstdout:\n{baseline_stdout}\nstderr:\n{baseline_stderr}"
    );

    // ---- Step 2: bump lib to 2.0.0 ----
    write_file(
        &root.join("lib/package.json"),
        "{\n  \"name\": \"mylib\",\n  \"version\": \"2.0.0\"\n}\n",
    );

    // ---- Step 3: npm install should FAIL ----
    let mismatch = Command::new("npm")
        .args(["install"])
        .current_dir(root)
        .output()
        .expect("failed to spawn npm install (mismatch)");

    let mismatch_stderr = String::from_utf8_lossy(&mismatch.stderr);
    let mismatch_stdout = String::from_utf8_lossy(&mismatch.stdout);

    // DOCUMENTED BEHAVIOR: npm rejects the incompatible local version.
    assert!(
        !mismatch.status.success(),
        "npm install should FAIL when lib=2.0.0 does not satisfy ^1.0.0, but it succeeded.\nstdout:\n{mismatch_stdout}\nstderr:\n{mismatch_stderr}"
    );

    assert!(
        mismatch_stderr.contains("ETARGET") || mismatch_stderr.contains("notarget"),
        "expected ETARGET/notarget error in npm output.\nActual stderr:\n{mismatch_stderr}"
    );
}

// ---------------------------------------------------------------------------
// Test 4 — uv
// ---------------------------------------------------------------------------

/// uv workspace SILENTLY ignores a version constraint mismatch.
///
/// Observed behavior (2026-04-05, uv 0.11.2):
///   When `app/pyproject.toml` depends on `test-lib>=1.0,<2.0` (via
///   `[tool.uv.sources] test-lib = { workspace = true }`) but the workspace
///   member `lib/pyproject.toml` declares `version = "2.0.0"`, `uv sync`
///   succeeds and installs `test-lib==2.0.0`.
///
///   uv treats `workspace = true` as an unconditional override: when a package
///   is declared as a workspace source, the local on-disk version is always
///   used, regardless of whether it satisfies the declared version constraint.
///   This matches pip's behavior for editable / path installs.
#[test]
fn uv_workspace_silently_allows_version_mismatch() {
    if !tool_available("uv") {
        eprintln!("SKIP: uv not on PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // ---- root pyproject.toml declaring the workspace ----
    write_file(
        &root.join("pyproject.toml"),
        "[tool.uv.workspace]\nmembers = [\"lib\", \"app\"]\n",
    );

    // ---- lib at v1.0.0 (baseline) ----
    write_file(
        &root.join("lib/pyproject.toml"),
        "[project]\nname = \"test-lib\"\nversion = \"1.0.0\"\nrequires-python = \">=3.8\"\ndependencies = []\n",
    );

    // ---- app depends on test-lib>=1.0,<2.0 via workspace source ----
    write_file(
        &root.join("app/pyproject.toml"),
        "[project]\nname = \"test-app\"\nversion = \"1.0.0\"\nrequires-python = \">=3.8\"\ndependencies = [\"test-lib>=1.0,<2.0\"]\n\n[tool.uv.sources]\ntest-lib = { workspace = true }\n",
    );

    // ---- Step 1: baseline (lib = 1.0.0) should succeed ----
    let baseline = Command::new("uv")
        .args(["sync"])
        .current_dir(root)
        .output()
        .expect("failed to spawn uv sync (baseline)");

    let baseline_stderr = String::from_utf8_lossy(&baseline.stderr);
    assert!(
        baseline.status.success(),
        "uv sync should succeed when lib=1.0.0 satisfies >=1.0,<2.0.\nstderr:\n{baseline_stderr}"
    );

    // ---- Step 2: bump lib to 2.0.0 ----
    write_file(
        &root.join("lib/pyproject.toml"),
        "[project]\nname = \"test-lib\"\nversion = \"2.0.0\"\nrequires-python = \">=3.8\"\ndependencies = []\n",
    );

    // ---- Step 3: uv sync should SUCCEED (silently uses local version) ----
    let mismatch = Command::new("uv")
        .args(["sync"])
        .current_dir(root)
        .output()
        .expect("failed to spawn uv sync (mismatch)");

    let mismatch_stderr = String::from_utf8_lossy(&mismatch.stderr);

    // DOCUMENTED BEHAVIOR: uv ignores the constraint and uses the local version.
    assert!(
        mismatch.status.success(),
        "uv sync should SUCCEED (silently) even when lib=2.0.0 does not satisfy >=1.0,<2.0.\nstderr:\n{mismatch_stderr}"
    );

    // Verify that the 2.0.0 version was actually installed (not a downgrade).
    assert!(
        mismatch_stderr.contains("test-lib==2.0.0"),
        "uv should install test-lib==2.0.0 (the local on-disk version).\nActual stderr:\n{mismatch_stderr}"
    );
}
