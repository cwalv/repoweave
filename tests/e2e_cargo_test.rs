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

mod common;

/// Run a git command in `dir`, asserting success.
fn git(args: &[&str], dir: &Path) {
    let status = common::git()
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
        let status = common::git()
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
    role: owned
  github/chatly/server:
    type: git
    url: https://github.com/chatly/server.git
    version: main
    role: owned
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
    repoweave::activate::activate_intent("web-app", root).expect("activate should succeed");

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
    repoweave::activate::activate_intent("web-app", root).expect("activate should succeed");

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

    let protocol_key =
        repoweave::manifest::RepoPath::new("github/chatly/protocol").expect("known-safe literal");
    let protocol_entry = lock
        .get_entry(&protocol_key)
        .expect("lock should contain protocol entry");

    // After the typed-ResolvedRevisionId refactor, `as_str` returns the canonical SHA
    // and the tag-form is preserved as the display form (which is also what
    // gets serialized into rwv.lock).
    assert_eq!(
        protocol_entry.version.display_str(),
        "v0.1.0",
        "generate_lock should prefer the tag over the raw SHA for protocol"
    );
    // Canonical SHA must still be a hex commit — the tag dereferences to a real commit.
    assert_eq!(
        protocol_entry.version.as_str().len(),
        40,
        "protocol canonical SHA should be a full 40-char hex string"
    );

    // Server has no tag, so its display form is the SHA (no tag form).
    let server_key =
        repoweave::manifest::RepoPath::new("github/chatly/server").expect("known-safe literal");
    let server_entry = lock
        .get_entry(&server_key)
        .expect("lock should contain server entry");

    assert_ne!(
        server_entry.version.display_str(),
        "v0.1.0",
        "server should have a SHA, not the protocol tag"
    );
    assert!(
        !server_entry.version.as_str().is_empty(),
        "server version should be a non-empty SHA"
    );
}

/// R34 end-to-end regression (fo-t9x0l1.4): an out-of-band cargo invocation
/// rewriting the fully-owned `Cargo.lock` as VALID TOML must surface a
/// digest-mismatch WARNING in `rwv doctor` — pre-fix, doctor exited 0 with
/// no report at all.
///
/// Drives the REAL accept-and-stamp path: `activate_intent` runs the cargo
/// activation hook (`cargo generate-lockfile`), which stamps the accepted
/// generation's SHA-256 into `.rwv-owned-digests`. The out-of-band mutation
/// is a genuine cargo rewrite (member version bump + direct
/// `cargo generate-lockfile`, not through rwv).
///
/// Also anchors the TL decision's exit-semantics claim: the finding is
/// Warning severity, so doctor's exit status is UNCHANGED by the mutation
/// (report-not-mandate).
#[test]
fn e2e_cargo_lock_out_of_band_rewrite_surfaces_digest_warning() {
    // Skip if cargo is not available.
    if which::which("cargo").is_err() {
        eprintln!("skipping e2e_cargo_lock_out_of_band_rewrite: cargo not on PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    setup_weave(root);

    // ---- Step 1: activation runs the hook: generate-lockfile + stamp ----
    repoweave::activate::activate_intent("web-app", root).expect("activate should succeed");

    let project_lock = root.join("projects/web-app/Cargo.lock");
    assert!(
        project_lock.exists(),
        "hook must have generated Cargo.lock into the project dir (via the symlink)"
    );
    let digests_path = root.join("projects/web-app/.rwv-owned-digests");
    assert!(
        digests_path.exists(),
        "hook must stamp the accepted generation's digest at the accept moment"
    );
    let accepted_lock = std::fs::read_to_string(&project_lock).unwrap();

    // ---- Step 2: doctor baseline — no digest-mismatch report ----
    let out_clean = common::rwv()
        .arg("doctor")
        .current_dir(root)
        .output()
        .expect("rwv doctor should run");
    let stdout_clean = String::from_utf8_lossy(&out_clean.stdout).to_string();
    assert!(
        !stdout_clean.contains("rwv-accepted generation"),
        "freshly-stamped lock must not report a digest mismatch:\n{stdout_clean}"
    );

    // ---- Step 3: out-of-band cargo rewrite (valid TOML) ----
    // Bump the protocol crate version and re-run cargo DIRECTLY (not through
    // rwv) — exactly the R34 evidence shape. cargo rewrites the lock as
    // valid TOML; the parse check cannot see this.
    let protocol_manifest = root.join("github/chatly/protocol/Cargo.toml");
    let manifest_text = std::fs::read_to_string(&protocol_manifest).unwrap();
    std::fs::write(
        &protocol_manifest,
        manifest_text.replace("version = \"0.1.0\"", "version = \"0.9.9\""),
    )
    .unwrap();
    let status = Command::new("cargo")
        .arg("generate-lockfile")
        .current_dir(root)
        .status()
        .expect("cargo generate-lockfile should run");
    assert!(status.success(), "out-of-band cargo run should succeed");

    let rewritten_lock = std::fs::read_to_string(&project_lock).unwrap();
    assert_ne!(
        accepted_lock, rewritten_lock,
        "sanity: the out-of-band cargo run must actually rewrite the lock"
    );

    // ---- Step 4: doctor surfaces the WARNING; exit status unchanged ----
    let out_drift = common::rwv()
        .arg("doctor")
        .current_dir(root)
        .output()
        .expect("rwv doctor should run");
    let stdout_drift = String::from_utf8_lossy(&out_drift.stdout).to_string();
    assert!(
        stdout_drift.contains("differs from the last rwv-accepted generation"),
        "doctor must report the digest mismatch (R34):\n{stdout_drift}"
    );
    assert!(
        stdout_drift.contains("accept the new content"),
        "finding must name the accept exit:\n{stdout_drift}"
    );
    assert!(
        stdout_drift.contains("restore the file"),
        "finding must name the restore exit:\n{stdout_drift}"
    );
    assert!(
        stdout_drift.contains("[warning]"),
        "digest mismatch must be warning severity:\n{stdout_drift}"
    );
    assert_eq!(
        out_clean.status.code(),
        out_drift.status.code(),
        "warning severity must leave doctor's exit status unchanged \
         (clean stdout:\n{stdout_clean}\ndrift stdout:\n{stdout_drift})"
    );

    // ---- Step 5: the ACCEPT exit — re-activation re-stamps → clean ----
    repoweave::activate::activate_intent("web-app", root).expect("re-activate should succeed");
    let out_restamped = common::rwv()
        .arg("doctor")
        .current_dir(root)
        .output()
        .expect("rwv doctor should run");
    let stdout_restamped = String::from_utf8_lossy(&out_restamped.stdout).to_string();
    assert!(
        !stdout_restamped.contains("rwv-accepted generation"),
        "re-activation must re-stamp and clear the finding:\n{stdout_restamped}"
    );
}

/// fo-t9x0l1.3 end-to-end: `patch-surface: cargo-config` reaches a
/// nested-workspace opt-out via cargo's UPWARD config discovery. The
/// bead's whole point.
///
/// Setup:
/// - Active weave member `github/chatly/protocol` (a package).
/// - Active weave member `github/chatly/server` (a package that
///   consumes `chatly-protocol` as a registry dep — drives the derived
///   scan to emit a patch keyed by `chatly-protocol`).
/// - Nested-workspace opt-out `github/xai-org/grok-build` whose root
///   declares `[workspace]`. It hosts a sub-crate `grok-consumer` that
///   ALSO uses `chatly-protocol` as a registry dep. Manifest surface
///   cannot patch this — cargo hard-errors on nested workspace
///   membership; the workspace's `[patch]` never applies to the opt-out
///   repo's builds.
/// - `patch: derived`, `patch-surface: cargo-config`,
///   `exclude: [github/xai-org/grok-build]`.
///
/// Assertions:
/// 1. Activation writes `.cargo/config.toml` at the weave root with a
///    `[patch.crates-io].chatly-protocol` entry whose path is
///    `../github/chatly/protocol` (probe P3/P7 — relative to `.cargo/`'s
///    logical location; NO canonicalization).
/// 2. From INSIDE the nested-workspace consumer dir, `cargo metadata`
///    resolves `chatly-protocol` to the in-weave source (upward
///    discovery finds the weave-root config — probe P4). This is the
///    LIVE-cargo half of the test; the same live surface the manifest
///    surface CANNOT reach.
///
/// Skips gracefully if cargo is absent.
#[test]
fn e2e_cargo_config_surface_reaches_nested_workspace_opt_out() {
    if which::which("cargo").is_err() {
        eprintln!("skipping e2e_cargo_config_surface: cargo not on PATH");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // ---- Base weave (two active Rust repos, one project) ----
    setup_weave(root);

    // Nested-workspace opt-out (grok-build shape). Root declares
    // `[workspace]`. This is what cargo hard-errors on for weave
    // membership — and what the manifest-surface `[patch]` cannot reach.
    std::fs::create_dir_all(root.join("github/xai-org/grok-build/crates/consumer/src")).unwrap();
    std::fs::write(
        root.join("github/xai-org/grok-build/Cargo.toml"),
        "[workspace]\nmembers = [\"crates/consumer\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("github/xai-org/grok-build/crates/consumer/Cargo.toml"),
        "[package]\nname = \"grok-consumer\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\nchatly-protocol = \"0.1\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("github/xai-org/grok-build/crates/consumer/src/lib.rs"),
        "pub fn v() -> &'static str { chatly_protocol::version() }\n",
    )
    .unwrap();
    // git init so scan_repos_on_disk sees it.
    let status = common::git()
        .args(["init", "-q"])
        .current_dir(root.join("github/xai-org/grok-build"))
        .status()
        .expect("git should be available");
    assert!(status.success());

    // ---- Extend manifest with grok-build + config-surface opt-in ----
    let manifest = "\
repositories:
  github/chatly/protocol:
    type: git
    url: https://github.com/chatly/protocol.git
    version: main
    role: owned
  github/chatly/server:
    type: git
    url: https://github.com/chatly/server.git
    version: main
    role: owned
  github/xai-org/grok-build:
    type: git
    url: https://github.com/xai-org/grok-build.git
    version: main
    role: owned
integrations:
  cargo-workspace:
    patch: derived
    patch-surface: cargo-config
    exclude:
      - github/xai-org/grok-build
";
    std::fs::write(root.join("projects/web-app/rwv.yaml"), manifest).unwrap();

    // The base setup_weave writes chatly-server with a committed `path=`
    // dep on protocol; for this test we need it to be a REGISTRY dep so
    // derived mode's registry-scan emits the patch. Rewrite server's
    // Cargo.toml.
    std::fs::write(
        root.join("github/chatly/server/Cargo.toml"),
        "[package]\nname = \"chatly-server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\nchatly-protocol = \"0.1\"\n",
    )
    .unwrap();

    // ---- Activate ----
    repoweave::activate::activate_intent("web-app", root).expect("activate should succeed");

    // ---- Assertion 1: config surface written with the correct relative path ----
    // The .cargo/config.toml is symlinked from weave root into the
    // project dir; check the file (via the symlink).
    let weave_config = root.join(".cargo").join("config.toml");
    assert!(
        weave_config.exists(),
        "weave-root .cargo/config.toml must exist after activation"
    );
    let config_text = std::fs::read_to_string(&weave_config).unwrap();
    assert!(
        config_text.contains("[patch.crates-io"),
        "config surface must carry [patch.crates-io]; got:\n{config_text}"
    );
    assert!(
        config_text.contains("chatly-protocol"),
        "patch must key on `chatly-protocol`; got:\n{config_text}"
    );
    // Path stays weave-root-relative: `github/chatly/protocol`. Cargo
    // resolves relative patch paths against the PARENT of `.cargo/`
    // (measured directly 2026-07-17 — the design doc's "config's logical
    // location" means the owning dir of `.cargo/`, not `.cargo/` itself);
    // our `.cargo/` sits directly under the weave root, so the manifest-
    // surface path shape (`github/chatly/protocol`) is what resolves
    // correctly here too. NO canonicalization — probe P1 preserved.
    assert!(
        config_text.contains("\"github/chatly/protocol\""),
        "path must be weave-root-relative `github/chatly/protocol`; got:\n{config_text}"
    );

    // ---- Assertion 2: cargo metadata from the nested-workspace opt-out
    // sees the in-weave source via upward config discovery (probe P4) ----
    //
    // Use full metadata (WITHOUT `--no-deps`) so the resolver runs and
    // returns the RESOLVED source per dep — that is where the patch shows
    // up (as a `path+file://...` source). With `--no-deps`, the output
    // reports the manifest's as-declared registry req, which is what
    // caused an earlier false negative.
    let consumer_dir = root.join("github/xai-org/grok-build/crates/consumer");
    let output = Command::new("cargo")
        .args(["metadata", "--format-version=1"])
        .current_dir(&consumer_dir)
        .output()
        .expect("cargo metadata should run");
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        panic!(
            "cargo metadata failed in the nested-workspace consumer — the \
             config-surface patch did not reach via upward discovery.\n\
             stderr:\n{stderr}"
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    // The metadata output includes the resolved source path for
    // `chatly-protocol`. It must point at the in-weave protocol dir via
    // the config-surface patch. Cargo emits resolved sources as
    // `path+file:///abs/dir/#name@version`, so we check the substring for
    // the canonical protocol dir.
    let expected_source = format!(
        "path+file://{}/github/chatly/protocol",
        root.canonicalize().unwrap().display()
    );
    let non_canonical = format!("path+file://{}/github/chatly/protocol", root.display());
    assert!(
        stdout.contains(&expected_source) || stdout.contains(&non_canonical),
        "cargo metadata did not resolve chatly-protocol to the in-weave source \
         via upward config discovery — Finding 2 / probe P4 failure.\n\
         expected substring one of:\n  {expected_source}\n  {non_canonical}\n\
         cargo metadata stdout:\n{stdout}"
    );
}
