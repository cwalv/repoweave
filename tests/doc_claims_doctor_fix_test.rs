//! Doc-claim anchor for the `rwv doctor --fix` migration path documented
//! in `docs/reference/roles.md` ("Migrating from `role: primary`") and
//! `docs/reference/cli.md` (`rwv doctor` section).
//!
//! Pins the end-to-end migration behaviour: a manifest with the legacy
//! `role: primary` spelling is detected by `rwv doctor`, migrated by
//! `rwv doctor --fix`, idempotent on re-run, and preserves surrounding
//! YAML structure (comments, key order, unrelated fields).
//!
//! These tests intentionally do not assert on the doctor's overall exit
//! status — the test workspace contains synthetic manifest entries
//! whose target repos are never cloned, which produces unrelated
//! `dangling-reference` errors. The contract under test is the
//! migration: detection text in stdout, and the on-disk manifest
//! content after `--fix`.

mod common;

use std::path::{Path, PathBuf};

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn git() -> std::process::Command {
    common::git()
}

/// Build a minimal workspace at `parent/<name>` with a single project
/// whose `rwv.toml` carries `role: primary`. The project repo itself is
/// a real git repo so workspace context resolves cleanly.
///
/// Returns `(workspace_root, project_dir, manifest_path)`.
fn make_workspace_with_legacy_manifest(
    parent: &Path,
    name: &str,
    manifest_yaml: &str,
) -> (PathBuf, PathBuf, PathBuf) {
    let workspace = parent.join(name);
    std::fs::create_dir_all(workspace.join("github")).unwrap();
    std::fs::create_dir_all(workspace.join("projects")).unwrap();
    let project_dir = workspace.join("projects").join("alpha");
    std::fs::create_dir_all(&project_dir).unwrap();
    let manifest_path = project_dir.join("rwv.toml");
    std::fs::write(&manifest_path, manifest_yaml).unwrap();
    // Project repo: minimal git init + commit so `rwv` resolves the
    // workspace context to a real weave root.
    let run_git = |args: &[&str], dir: &Path| {
        let out = git()
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .expect("git command failed to start");
        assert!(out.status.success(), "git {:?} failed", args);
    };
    run_git(&["init", "-b", "main"], &project_dir);
    run_git(&["add", "rwv.toml"], &project_dir);
    run_git(&["commit", "-m", "init"], &project_dir);
    std::fs::write(workspace.join(".rwv-active"), "alpha\n").unwrap();
    (workspace, project_dir, manifest_path)
}

const LEGACY_MANIFEST: &str = "# acme.alpha — auto-migrate target\n\n[repositories.\"github/acme/lib\"]\ntype = \"git\"\nurl = \"https://example.com/acme/lib.git\"\nversion = \"main\"\nrole = \"primary\"\n\n[repositories.\"github/acme/app\"]\ntype = \"git\"\nurl = \"https://example.com/acme/app.git\"\nversion = \"main\"\nrole = \"dependency\"\n";

/// Capture `rwv doctor` stdout + stderr regardless of exit status; the
/// migration contract is independent of unrelated dangling-reference
/// errors that synthetic test manifests produce.
fn doctor_output(workspace: &Path, args: &[&str]) -> String {
    let assertion = {
        let mut cmd = rwv();
        cmd.arg("doctor");
        for a in args {
            cmd.arg(a);
        }
        cmd.current_dir(workspace).assert()
    };
    let out = assertion.get_output();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    format!("{stdout}\n{stderr}")
}

/// `rwv doctor` (without `--fix`) reports the legacy spelling and
/// directs the user at `rwv doctor --fix` for the migration.
#[test]
fn doctor_detects_legacy_role_primary_in_manifest() {
    let tmp = common::tempdir().unwrap();
    let (workspace, _, _) = make_workspace_with_legacy_manifest(tmp.path(), "ws", LEGACY_MANIFEST);

    let combined = doctor_output(&workspace, &[]);
    assert!(
        combined.contains("rwv doctor --fix"),
        "doctor should direct users at `rwv doctor --fix`, got:\n{combined}"
    );
    assert!(
        combined.contains("role: primary") || combined.contains("`role: primary`"),
        "doctor should name the deprecated spelling, got:\n{combined}"
    );
}

/// `rwv doctor --fix` rewrites `role: primary` → `role: owned` in place.
#[test]
fn doctor_fix_migrates_role_primary_to_owned() {
    let tmp = common::tempdir().unwrap();
    let (workspace, _, manifest_path) =
        make_workspace_with_legacy_manifest(tmp.path(), "ws", LEGACY_MANIFEST);

    let combined = doctor_output(&workspace, &["--fix"]);
    assert!(
        combined.contains("[fixed]") && combined.contains("role: primary"),
        "fix output should announce the migration, got:\n{combined}"
    );

    let new_content = std::fs::read_to_string(&manifest_path).unwrap();
    assert!(
        new_content.contains("role: owned"),
        "fixed manifest should contain `role: owned`, got:\n{new_content}"
    );
    assert!(
        !new_content.contains("role: primary"),
        "fixed manifest should NOT contain `role: primary`, got:\n{new_content}"
    );
}

/// Running `--fix` against an already-migrated tree is a no-op: the
/// second run leaves the manifest content unchanged.
#[test]
fn doctor_fix_is_idempotent() {
    let tmp = common::tempdir().unwrap();
    let (workspace, _, manifest_path) =
        make_workspace_with_legacy_manifest(tmp.path(), "ws", LEGACY_MANIFEST);

    let _ = doctor_output(&workspace, &["--fix"]);
    let after_first = std::fs::read_to_string(&manifest_path).unwrap();

    let combined_second = doctor_output(&workspace, &["--fix"]);
    let after_second = std::fs::read_to_string(&manifest_path).unwrap();

    assert_eq!(
        after_first, after_second,
        "second `--fix` run should be a no-op; content drifted"
    );
    assert!(
        !combined_second.contains("migrated 1 `role: primary`"),
        "second run should not announce a migration, got:\n{combined_second}"
    );
}

/// The migration preserves comments, key order, and unrelated fields.
#[test]
fn doctor_fix_preserves_other_manifest_content() {
    let tmp = common::tempdir().unwrap();
    let (workspace, _, manifest_path) =
        make_workspace_with_legacy_manifest(tmp.path(), "ws", LEGACY_MANIFEST);

    let _ = doctor_output(&workspace, &["--fix"]);

    let out = std::fs::read_to_string(&manifest_path).unwrap();
    // Header comment retained.
    assert!(out.contains("# acme.alpha — auto-migrate target"));
    // Inline comments retained.
    assert!(out.contains("# inline comment kept"));
    assert!(out.contains("# legacy spelling"));
    // Key order preserved: `lib` appears before `app`.
    let lib_pos = out.find("github/acme/lib").expect("lib still present");
    let app_pos = out.find("github/acme/app").expect("app still present");
    assert!(
        lib_pos < app_pos,
        "key order should be preserved; got reordered manifest:\n{out}"
    );
    // Unrelated entry untouched.
    assert!(out.contains("role: dependency"));
}

/// After `--fix`, `rwv doctor` (without `--fix`) no longer reports the
/// `role: primary` finding — confirming the migration path is
/// end-to-end discoverable from the doctor surface alone. Other
/// unrelated findings (dangling references for the synthetic test
/// manifests) may still appear; we only assert that the legacy-role
/// signal is gone.
#[test]
fn doctor_fix_clears_legacy_role_primary_findings() {
    let tmp = common::tempdir().unwrap();
    let (workspace, _, _) = make_workspace_with_legacy_manifest(tmp.path(), "ws", LEGACY_MANIFEST);

    let _ = doctor_output(&workspace, &["--fix"]);
    let combined = doctor_output(&workspace, &[]);

    assert!(
        !combined.contains("role: primary"),
        "post-fix doctor should not report `role: primary`, got:\n{combined}"
    );
}
