//! Tests for provenance checks in `rwv doctor`.
//!
//! Covers two sub-kinds of `Provenance` violations:
//!
//!   1. `origin-url-mismatch` — a clone's `origin` remote URL differs from
//!      the URL recorded in the manifest. Warning severity; report-only.
//!
//!   2. `lock-sha-unreachable` — a SHA pinned in `rwv.lock` is absent from
//!      the local object store. Error severity; report-only.
//!
//! Each sub-kind has:
//!   - a fixture test that triggers the violation (must be flagged)
//!   - a clean-state test that must produce no false positives

use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Workspace / fixture construction helpers
// ---------------------------------------------------------------------------

/// Create a minimal primary workspace at `root/ws/` with `github/` and
/// `projects/` directories. Returns the workspace root path.
fn make_primary(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    ws
}

/// Initialise a bare-minimum git repo at `path` with one commit (so HEAD is
/// valid), configure the user identity inside the repo, and return the SHA
/// of the initial commit.
fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git()
        .args(["init"])
        .current_dir(path)
        .output()
        .unwrap();
    common::git()
        .args(["config", "user.email", "test@test.com"])
        .current_dir(path)
        .output()
        .unwrap();
    common::git()
        .args(["config", "user.name", "Test"])
        .current_dir(path)
        .output()
        .unwrap();
    // Add a file and commit so HEAD is valid.
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    common::git()
        .args(["add", "."])
        .current_dir(path)
        .output()
        .unwrap();
    common::git()
        .args(["commit", "-m", "init"])
        .current_dir(path)
        .output()
        .unwrap();
    // Return the SHA of HEAD.
    let out = common::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Write a minimal `rwv.toml` manifest for project `name` at
/// `ws/projects/<name>/rwv.toml`. `entries` is a list of
/// `(repo_path, url, role)` tuples.
fn write_manifest(ws: &Path, name: &str, entries: &[(&str, &str, &str)]) {
    let project_dir = ws.join("projects").join(name);
    std::fs::create_dir_all(&project_dir).unwrap();
    let mut content = String::new();
    content.push_str("[repositories]\n");
    for (repo_path, url, role) in entries {
        content.push_str(&format!(
            "[repositories.\"{repo_path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"{role}\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), content).unwrap();
}

/// Write a minimal `rwv.lock` at `ws/projects/<name>/rwv.lock` with one
/// entry pointing `repo_path` at `sha`.
fn write_lock(ws: &Path, name: &str, repo_path: &str, url: &str, sha: &str) {
    let project_dir = ws.join("projects").join(name);
    std::fs::create_dir_all(&project_dir).unwrap();
    common::fixture_lock(&project_dir, &[(repo_path, url, sha)]);
}

/// Add a remote named `remote_name` pointing at `remote_url` to the repo at
/// `repo_path`.
fn add_remote(repo_path: &Path, remote_name: &str, remote_url: &str) {
    common::git()
        .args(["remote", "add", remote_name, remote_url])
        .current_dir(repo_path)
        .output()
        .unwrap();
}

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

// ===========================================================================
// 1. origin-url-mismatch
// ===========================================================================

/// A clone whose `origin` remote URL differs from the manifest URL must be
/// flagged as an `origin-url-mismatch` warning.
#[test]
fn origin_url_mismatch_is_reported() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());

    // Create a "remote" bare repo.
    let remote_dir = tmp.path().join("remote.git");
    std::fs::create_dir_all(&remote_dir).unwrap();
    common::git()
        .args(["init", "--bare"])
        .current_dir(&remote_dir)
        .output()
        .unwrap();

    // Clone the repo into the workspace.
    let repo_abs = ws.join("github/myorg/myrepo");
    common::git()
        .args([
            "clone",
            remote_dir.to_str().unwrap(),
            repo_abs.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    // Give the repo a commit and HEAD so doctor doesn't complain about missing
    // HEAD (it may not have one if the bare repo was empty).
    // If clone fails to create a HEAD, we init manually.
    if !repo_abs.join(".git").is_dir() {
        init_repo(&repo_abs);
    }

    // Manifest records a different URL than the clone's `origin`.
    let manifest_url = "https://github.com/myorg/myrepo.git";
    write_manifest(
        &ws,
        "my-project",
        &[("github/myorg/myrepo", manifest_url, "owned")],
    );

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("origin-url-mismatch")
            || stdout.contains("URL mismatch")
            || stdout.contains("origin URL mismatch"),
        "doctor should report origin-url-mismatch; got:\n{stdout}"
    );
}

/// When the clone's `origin` URL matches the manifest URL, no
/// `origin-url-mismatch` violation must be emitted.
#[test]
fn matching_origin_url_is_clean() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());

    // Create a repo with an `origin` remote.
    let repo_abs = ws.join("github/myorg/myrepo");
    init_repo(&repo_abs);
    let manifest_url = "https://github.com/myorg/myrepo.git";
    add_remote(&repo_abs, "origin", manifest_url);

    write_manifest(
        &ws,
        "my-project",
        &[("github/myorg/myrepo", manifest_url, "owned")],
    );

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("origin-url-mismatch"),
        "matching origin URL should not produce origin-url-mismatch; got:\n{stdout}"
    );
}

// ===========================================================================
// 2. lock-sha-unreachable
// ===========================================================================

/// A lock file that references a SHA absent from the repo's object store
/// must be flagged as `lock-sha-unreachable` at Error severity.
#[test]
fn lock_sha_unreachable_is_reported() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());

    let repo_abs = ws.join("github/myorg/myrepo");
    let _sha = init_repo(&repo_abs);
    let manifest_url = "https://github.com/myorg/myrepo.git";
    add_remote(&repo_abs, "origin", manifest_url);

    write_manifest(
        &ws,
        "my-project",
        &[("github/myorg/myrepo", manifest_url, "owned")],
    );

    // Write a lock with a SHA that is definitely not in the repo's object
    // store (all-zero SHA is safe for this purpose).
    let absent_sha = "0000000000000000000000000000000000000000";
    write_lock(
        &ws,
        "my-project",
        "github/myorg/myrepo",
        manifest_url,
        absent_sha,
    );

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("lock-sha-unreachable")
            || stdout.contains("absent from")
            || stdout.contains("missing the pinned revision"),
        "doctor should report lock-sha-unreachable; got:\n{stdout}"
    );
    // Should mention the absent SHA.
    assert!(
        stdout.contains(absent_sha),
        "report should name the unreachable SHA; got:\n{stdout}"
    );
}

/// A lock file that references the actual HEAD SHA must not trigger
/// `lock-sha-unreachable`.
#[test]
fn reachable_lock_sha_is_clean() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());

    let repo_abs = ws.join("github/myorg/myrepo");
    let sha = init_repo(&repo_abs);
    let manifest_url = "https://github.com/myorg/myrepo.git";
    add_remote(&repo_abs, "origin", manifest_url);

    write_manifest(
        &ws,
        "my-project",
        &[("github/myorg/myrepo", manifest_url, "owned")],
    );
    write_lock(&ws, "my-project", "github/myorg/myrepo", manifest_url, &sha);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("lock-sha-unreachable"),
        "reachable lock SHA should not produce lock-sha-unreachable; got:\n{stdout}"
    );
}

// ===========================================================================
// 3. JSON output includes provenance kind
// ===========================================================================

/// `rwv doctor --json` must include a `provenance` kind entry when an
/// `origin-url-mismatch` violation exists.
#[test]
fn json_output_includes_provenance_kind() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());

    let repo_abs = ws.join("github/myorg/myrepo");
    init_repo(&repo_abs);
    // Set `origin` to a URL that differs from the manifest.
    add_remote(&repo_abs, "origin", "https://github.com/myorg/myrepo.git");

    // Manifest records a different URL.
    let manifest_url = "https://gitlab.com/myorg/myrepo.git";
    write_manifest(
        &ws,
        "my-project",
        &[("github/myorg/myrepo", manifest_url, "owned")],
    );

    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json produced invalid JSON: {e}\noutput: {stdout}"));

    let violations = json["violations"].as_array().expect("violations is array");
    let found = violations.iter().any(|v| v["kind"] == "provenance");
    assert!(
        found,
        "doctor --json must include a provenance violation; violations: {violations:?}"
    );
}

/// The wire token for this finding is `origin-url-mismatch`, and it names a
/// git remote on purpose.
///
/// Renaming it to something backend-neutral is a decision this repo has
/// weighed and declined — the token is published in the committed schema and
/// keyed in the reference page, and a `--json` consumer matching it stops
/// matching *silently* when it moves, because a selector against a renamed key
/// yields nothing rather than erroring. The reasoning, and what it was weighed
/// against, is in `docs/explanation/joints/vcs-as-seam.md`.
///
/// This is the pin that makes the decision a decision rather than an
/// accident. Regenerating the committed schema after a rename produces a diff
/// a reviewer has to notice; nothing else fails. A test that names the token
/// turns a silent wire break into a conversation with the choice above.
///
/// Asserted on the key of the `sub_kind` object, which is where the token
/// actually reaches a consumer — not on the rendered message, whose "origin
/// URL mismatch" phrasing is satisfied whatever the kind is called.
#[test]
fn the_provenance_sub_kind_token_is_origin_url_mismatch() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());

    let repo_abs = ws.join("github/myorg/myrepo");
    init_repo(&repo_abs);
    add_remote(&repo_abs, "origin", "https://github.com/myorg/myrepo.git");
    write_manifest(
        &ws,
        "my-project",
        &[(
            "github/myorg/myrepo",
            "https://gitlab.com/myorg/myrepo.git",
            "owned",
        )],
    );

    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json produced invalid JSON: {e}\noutput: {stdout}"));

    let tokens: Vec<String> = json["violations"]
        .as_array()
        .expect("violations is array")
        .iter()
        .filter(|v| v["kind"] == "provenance")
        .filter_map(|v| v["sub_kind"].as_object())
        .flat_map(|o| o.keys().cloned())
        .collect();

    assert!(
        tokens.iter().any(|t| t == "origin-url-mismatch"),
        "the published sub_kind token must stay `origin-url-mismatch`; got {tokens:?}"
    );
}
