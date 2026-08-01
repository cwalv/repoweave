//! Doc-claim anchor for the pre-TOML manifest refusal documented in
//! `docs/reference/formats.md` and `docs/reference/doctor-findings.md`.
//!
//! Every test here pins a prohibition rather than a capability, because the
//! design decision this file guards is a decision *not* to act: an `rwv.yaml`
//! must not be silently parsed, must not be silently skipped, and must not be
//! auto-converted. The first two fail open — a workspace that reads as healthy
//! while a project is invisible — so a test asserting the refusal *appears* is
//! the only thing standing between them and a green suite.
//!
//! These tests do not assert on the doctor's overall exit status: the fixtures
//! carry synthetic manifest entries whose repos are never cloned, which
//! produces unrelated `dangling-reference` findings.

mod common;

use std::path::{Path, PathBuf};

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn git() -> std::process::Command {
    common::git()
}

const LEGACY_MANIFEST: &str = "\
# acme.alpha
repositories:
  github/acme/lib:
    type: git
    url: https://example.com/acme/lib.git
    version: main
    role: owned
";

const LEGACY_ROLE_MANIFEST: &str = "\
[repositories.\"github/acme/lib\"]
type = \"git\"
url = \"https://example.com/acme/lib.git\"
version = \"main\"
role = \"primary\"
";

/// Build a workspace at `parent/<name>` holding one project whose manifest is
/// written under `manifest_name`. The project repo is a real git repo so
/// workspace context resolves cleanly.
///
/// Returns `(workspace_root, project_dir, manifest_path)`.
fn make_workspace(
    parent: &Path,
    name: &str,
    manifest_name: &str,
    manifest: &str,
) -> (PathBuf, PathBuf, PathBuf) {
    let workspace = parent.join(name);
    std::fs::create_dir_all(workspace.join("github")).unwrap();
    std::fs::create_dir_all(workspace.join("projects")).unwrap();
    let project_dir = workspace.join("projects").join("alpha");
    std::fs::create_dir_all(&project_dir).unwrap();
    let manifest_path = project_dir.join(manifest_name);
    std::fs::write(&manifest_path, manifest).unwrap();
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
    run_git(&["add", manifest_name], &project_dir);
    run_git(&["commit", "-m", "init"], &project_dir);
    std::fs::write(workspace.join(".rwv-active"), "alpha\n").unwrap();
    (workspace, project_dir, manifest_path)
}

/// Capture stdout + stderr regardless of exit status.
fn output(workspace: &Path, args: &[&str]) -> String {
    let assertion = {
        let mut cmd = rwv();
        for a in args {
            cmd.arg(a);
        }
        cmd.current_dir(workspace).assert()
    };
    let out = assertion.get_output();
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A project holding only the pre-TOML manifest is reported, not passed over.
///
/// This is the finding's whole reason for existing. `rwv doctor` reaches a
/// project by looking for the manifest it can read, so without a check keyed
/// on the legacy name the project is skipped and the workspace reports clean
/// — the one failure mode an operator cannot see.
#[test]
fn doctor_reports_a_project_whose_only_manifest_is_the_legacy_one() {
    let tmp = common::tempdir().unwrap();
    let (workspace, _, _) = make_workspace(tmp.path(), "ws", "rwv.yaml", LEGACY_MANIFEST);

    let combined = output(&workspace, &["doctor"]);
    assert!(
        combined.contains("rwv.yaml"),
        "doctor must name the legacy manifest it found, got:\n{combined}"
    );
    assert!(
        combined.contains("rwv.toml"),
        "doctor must name the manifest rwv reads, got:\n{combined}"
    );
}

/// The finding is on the machine-readable surface under its own kind, so a
/// caller consuming `--json` can distinguish it from a parse failure.
#[test]
fn doctor_json_carries_the_legacy_manifest_kind() {
    let tmp = common::tempdir().unwrap();
    let (workspace, _, _) = make_workspace(tmp.path(), "ws", "rwv.yaml", LEGACY_MANIFEST);

    let combined = output(&workspace, &["doctor", "--json"]);
    assert!(
        combined.contains("legacy-manifest-format"),
        "doctor --json must carry the finding kind, got:\n{combined}"
    );
}

/// `--fix` must not convert the manifest, and must not touch it.
///
/// The file is hand-authored: its comments and key order carry intent no
/// mechanical cross-format rewrite can place. A conversion that guessed would
/// be worse than the refusal, so the prohibition is that `--fix` leaves both
/// the legacy file and the absence of a TOML one exactly as it found them.
#[test]
fn doctor_fix_neither_converts_nor_modifies_the_legacy_manifest() {
    let tmp = common::tempdir().unwrap();
    let (workspace, project_dir, manifest_path) =
        make_workspace(tmp.path(), "ws", "rwv.yaml", LEGACY_MANIFEST);

    output(&workspace, &["doctor", "--fix"]);

    assert_eq!(
        std::fs::read_to_string(&manifest_path).unwrap(),
        LEGACY_MANIFEST,
        "--fix must leave the legacy manifest byte-identical"
    );
    assert!(
        !project_dir.join("rwv.toml").exists(),
        "--fix must not synthesise a TOML manifest from the legacy one"
    );
}

/// A command that needs the manifest refuses by naming the file and the
/// remedy, rather than reporting the manifest as missing.
#[test]
fn a_verb_refuses_with_the_legacy_manifest_named() {
    let tmp = common::tempdir().unwrap();
    let (workspace, _, _) = make_workspace(tmp.path(), "ws", "rwv.yaml", LEGACY_MANIFEST);

    let combined = output(&workspace, &["lock"]);
    assert!(
        combined.contains("rwv.yaml") && combined.contains("rwv.toml"),
        "the refusal must name both the file found and the one rwv reads, got:\n{combined}"
    );
    assert!(
        combined.contains("by hand"),
        "the refusal must name hand conversion as the remedy, got:\n{combined}"
    );
}

/// The legacy *role* spelling inside a TOML manifest is refused with the
/// spelling that replaced it, and located at the line carrying it.
///
/// Nothing rewrites the file, so this sentence is the entire remedy.
#[test]
fn a_legacy_role_spelling_is_refused_with_the_replacement_named() {
    let tmp = common::tempdir().unwrap();
    let (workspace, _, _) = make_workspace(tmp.path(), "ws", "rwv.toml", LEGACY_ROLE_MANIFEST);

    let combined = output(&workspace, &["doctor"]);
    assert!(
        combined.contains("no longer accepted"),
        "the refusal must carry the migration sentence, got:\n{combined}"
    );
    assert!(
        combined.contains("`owned`"),
        "the refusal must name the replacement spelling, got:\n{combined}"
    );
    assert!(
        combined.contains("line 5"),
        "the refusal must locate the offending line, got:\n{combined}"
    );
}

/// One defect yields one finding.
///
/// The legacy role spelling used to be reported twice — once as an
/// `unparseable-project` error from the loader and once as a warning from an
/// independent text scan of the same file — which reads as two problems.
#[test]
fn a_legacy_role_spelling_is_reported_once() {
    let tmp = common::tempdir().unwrap();
    let (workspace, _, _) = make_workspace(tmp.path(), "ws", "rwv.toml", LEGACY_ROLE_MANIFEST);

    let combined = output(&workspace, &["doctor", "--json"]);
    let hits = combined.matches("no longer accepted").count();
    assert_eq!(
        hits, 1,
        "the legacy role spelling must be reported exactly once, saw {hits} in:\n{combined}"
    );
}
