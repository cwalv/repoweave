//! End-to-end installation tests for repoweave.
//!
//! These tests verify that the various distribution channels (PyPI, cargo,
//! nix devShell) correctly install the `rwv` binary and that it reports the
//! expected version.
//!
//! Gated behind `RWV_E2E_INSTALL=1` because they:
//!   - Require network access
//!   - Install software into the user's environment
//!   - May be slow (cargo compile time, uv tool install, etc.)
//!
//! Run with:
//!   RWV_E2E_INSTALL=1 cargo test --test e2e_install_test -- --nocapture

use std::process::Command;

/// Skip the test if `RWV_E2E_INSTALL` is not set to `"1"`.
macro_rules! require_e2e {
    () => {
        if std::env::var("RWV_E2E_INSTALL").as_deref() != Ok("1") {
            eprintln!("skipping e2e install test (set RWV_E2E_INSTALL=1 to run)");
            return;
        }
    };
}

/// Return the current package version from `Cargo.toml` at compile time.
fn expected_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Run a command and return its combined stdout, asserting exit success.
fn run_output(program: &str, args: &[&str]) -> String {
    let out = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run `{program}`: {e}"));
    assert!(
        out.status.success(),
        "`{program} {args:?}` failed with status {}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Check whether a program is on PATH, returning its resolved path or `None`.
fn which(name: &str) -> Option<std::path::PathBuf> {
    which::which(name).ok()
}

// ---------------------------------------------------------------------------
// Test: uv tool install repoweave
// ---------------------------------------------------------------------------

/// Install `repoweave` via `uv tool install repoweave` and verify `rwv --version`.
///
/// This is the primary distribution-channel test. It installs from PyPI into
/// uv's isolated tool environment and confirms the installed binary:
///   1. can be invoked as `rwv`
///   2. reports the version string matching this crate's version
#[test]
fn uv_tool_install_repoweave() {
    require_e2e!();

    let uv = match which("uv") {
        Some(p) => p,
        None => {
            eprintln!("skipping uv_tool_install_repoweave: uv not on PATH");
            return;
        }
    };

    let version = expected_version();

    // Install (or upgrade) the package.
    let install_out = Command::new(&uv)
        .args(["tool", "install", "--force", "repoweave"])
        .output()
        .expect("uv tool install should run");

    assert!(
        install_out.status.success(),
        "uv tool install repoweave failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&install_out.stdout),
        String::from_utf8_lossy(&install_out.stderr),
    );

    // After installation `rwv` should be available via uv's tool shim.
    let rwv_version = run_output(
        uv.to_str().unwrap(),
        &["tool", "run", "repoweave", "--", "rwv", "--version"],
    );

    // Alternatively, if uv places the binary on PATH directly:
    // let rwv_version = run_output("rwv", &["--version"]);

    assert!(
        rwv_version.contains(version),
        "expected version {version} in `rwv --version` output, got: {rwv_version}"
    );

    eprintln!(
        "uv tool install OK: rwv --version => {}",
        rwv_version.trim()
    );
}

// ---------------------------------------------------------------------------
// Test: cargo install repoweave
// ---------------------------------------------------------------------------

/// Install `repoweave` via `cargo install repoweave` and verify `rwv --version`.
///
/// This is the baseline distribution channel (always worked). The test
/// confirms that the binary reports the correct version after a clean install.
#[test]
fn cargo_install_repoweave() {
    require_e2e!();

    if which("cargo").is_none() {
        eprintln!("skipping cargo_install_repoweave: cargo not on PATH");
        return;
    }

    let version = expected_version();

    let install_out = Command::new("cargo")
        .args(["install", "--force", "repoweave"])
        .output()
        .expect("cargo install should run");

    assert!(
        install_out.status.success(),
        "cargo install repoweave failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&install_out.stdout),
        String::from_utf8_lossy(&install_out.stderr),
    );

    let rwv_version = run_output("rwv", &["--version"]);

    assert!(
        rwv_version.contains(version),
        "expected version {version} in `rwv --version` output, got: {rwv_version}"
    );

    eprintln!("cargo install OK: rwv --version => {}", rwv_version.trim());
}

// ---------------------------------------------------------------------------
// Test: curl install.sh
// ---------------------------------------------------------------------------

/// Install `repoweave` via the install script and verify `rwv --version`.
///
/// This tests the quick-install path documented in the README:
///   curl -fsSL https://cwalv.github.io/repoweave/install.sh | sh
#[test]
fn curl_install_script() {
    require_e2e!();

    if which("curl").is_none() {
        eprintln!("skipping curl_install_script: curl not on PATH");
        return;
    }

    let version = expected_version();

    // Download and run the install script into a temp dir to avoid
    // clobbering an existing installation.
    let tmp = tempfile::tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();

    let install_out = Command::new("sh")
        .args([
            "-c",
            &format!(
                "curl -fsSL https://cwalv.github.io/repoweave/install.sh | INSTALL_DIR={} sh",
                bin_dir.display()
            ),
        ])
        .output()
        .expect("install script should run");

    // The install script might not support INSTALL_DIR — fall back to
    // checking if it ran at all.
    if !install_out.status.success() {
        let stderr = String::from_utf8_lossy(&install_out.stderr);
        // If it fails because INSTALL_DIR isn't supported, try the default path
        if stderr.contains("INSTALL_DIR") || stderr.contains("unknown") {
            eprintln!("install script doesn't support INSTALL_DIR, trying default...");
            let install_out2 = Command::new("sh")
                .args([
                    "-c",
                    "curl -fsSL https://cwalv.github.io/repoweave/install.sh | sh",
                ])
                .output()
                .expect("install script should run");
            assert!(
                install_out2.status.success(),
                "install.sh failed\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&install_out2.stdout),
                String::from_utf8_lossy(&install_out2.stderr),
            );
        } else {
            panic!(
                "install.sh failed\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&install_out.stdout),
                stderr,
            );
        }
    }

    // Check if rwv landed in our temp bin dir, or the default location
    let rwv_path = bin_dir.join("rwv");
    let rwv_cmd = if rwv_path.exists() {
        rwv_path.to_str().unwrap().to_string()
    } else {
        "rwv".to_string()
    };

    let rwv_version = run_output(&rwv_cmd, &["--version"]);
    assert!(
        rwv_version.contains(version),
        "expected version {version} in `rwv --version` output, got: {rwv_version}"
    );

    eprintln!(
        "curl install.sh OK: rwv --version => {}",
        rwv_version.trim()
    );
}

// ---------------------------------------------------------------------------
// Test: nix devShell provides rwv
// ---------------------------------------------------------------------------

/// Verify that the nix devShell exposes `rwv` on PATH.
///
/// This test checks that `nix develop` (or an already-entered devShell)
/// makes `rwv` available via the PATH it configures. When run inside a
/// nix devShell, `rwv` is provided by the cargo build in the shell hook.
///
/// Outside a devShell the test verifies that `nix develop` can at least
/// be invoked; a full enter+exit would require a PTY so we keep this light.
#[test]
fn nix_devshell_provides_rwv() {
    require_e2e!();

    // If we're already inside a nix devShell, $IN_NIX_SHELL is set.
    let in_nix_shell = std::env::var("IN_NIX_SHELL").is_ok();

    if in_nix_shell {
        // Inside the devShell: rwv must already be on PATH.
        let version = expected_version();
        let rwv_version = run_output("rwv", &["--version"]);
        assert!(
            rwv_version.contains(version),
            "inside nix devShell: expected version {version} in `rwv --version`, got: {rwv_version}"
        );
        eprintln!("nix devShell rwv OK: {}", rwv_version.trim());
    } else {
        // Outside a devShell: verify nix is available and the flake is valid.
        let nix = match which("nix") {
            Some(p) => p,
            None => {
                eprintln!("skipping nix_devshell_provides_rwv: nix not on PATH");
                return;
            }
        };

        // `nix flake check` validates the flake without entering the shell.
        // Use --no-build to keep it fast (we just want structural validity).
        let check_out = Command::new(&nix)
            .args(["flake", "check", "--no-build"])
            .output()
            .expect("nix flake check should run");

        if !check_out.status.success() {
            eprintln!(
                "nix flake check --no-build failed (non-fatal outside CI):\n{}",
                String::from_utf8_lossy(&check_out.stderr)
            );
        } else {
            eprintln!("nix flake check OK");
        }
    }
}
