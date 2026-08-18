// ===========================================================================
// doctor verify() — uv-workspace
// ===========================================================================

use super::*;
use repoweave::integrations::UvWorkspace;

// -----------------------------------------------------------------------
// MISSING
// -----------------------------------------------------------------------

/// Given: Python repos detected but pyproject.toml absent.
/// Then:  verify() reports a single MISSING+safe_to_fix finding.
#[test]
fn s7_uv_doctor_missing_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/pyproject.toml");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = UvWorkspace.verify(&ctx).unwrap();
    assert_eq!(
        issues.len(),
        1,
        "expected exactly one MISSING issue, got: {issues:?}"
    );
    let issue = &issues[0];
    assert!(issue.safe_to_fix, "MISSING issue must be safe_to_fix");
    assert!(
        issue.message.contains("missing"),
        "MISSING message should contain 'missing': {}",
        issue.message
    );
    assert!(
        issue.message.contains("rwv doctor --fix"),
        "MISSING message should mention 'rwv doctor --fix': {}",
        issue.message
    );
}

/// Given: MISSING pyproject.toml.
/// When:  activate() runs.
/// Then:  file created with marker; verify() returns CLEAN.
#[test]
fn s7_uv_doctor_missing_fixed_by_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/pyproject.toml");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // Pre-condition: MISSING.
    let pre = UvWorkspace.verify(&ctx).unwrap();
    assert_eq!(pre.len(), 1, "expected MISSING pre-condition");
    assert!(pre[0].safe_to_fix);

    UvWorkspace.activate(&ctx).unwrap();

    let path = root.join("pyproject.toml");
    assert!(
        path.exists(),
        "pyproject.toml must be created after activate"
    );

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
        content.contains("# managed by rwv"),
        "file must have '# managed by rwv' marker after activate: {content}"
    );
    assert!(
        content.contains("github/acme/server"),
        "members must include the repo: {content}"
    );

    let post = UvWorkspace.verify(&ctx).unwrap();
    assert!(
        post.is_empty(),
        "verify() must return no issues after activate (CLEAN), got: {post:?}"
    );
}

// -----------------------------------------------------------------------
// DRIFT
// -----------------------------------------------------------------------

/// Given: pyproject.toml with marker but outdated members list.
/// Then:  verify() reports DRIFT+safe_to_fix.
#[test]
fn s7_uv_doctor_drift_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Seed: only server in members (drift — web was added to manifest).
    write_file(
        root,
        "pyproject.toml",
        "[tool.uv.workspace]\n# managed by rwv\nmembers = [\"github/acme/server\"]\n",
    );
    touch(root, "github/acme/server/pyproject.toml");
    touch(root, "github/acme/web/pyproject.toml");

    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/web", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = UvWorkspace.verify(&ctx).unwrap();
    assert_eq!(
        issues.len(),
        1,
        "expected exactly one DRIFT issue, got: {issues:?}"
    );
    let issue = &issues[0];
    assert!(issue.safe_to_fix, "DRIFT issue must be safe_to_fix");
    assert!(
        issue.message.contains("drift"),
        "DRIFT message should contain 'drift': {}",
        issue.message
    );
}

/// Given: DRIFT pyproject.toml.
/// When:  activate() runs.
/// Then:  verify() returns CLEAN.
#[test]
fn s7_uv_doctor_drift_fixed_by_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "pyproject.toml",
        "[tool.uv.workspace]\n# managed by rwv\nmembers = [\"github/acme/server\"]\n",
    );
    touch(root, "github/acme/server/pyproject.toml");
    touch(root, "github/acme/web/pyproject.toml");

    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/web", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // Pre-condition: DRIFT.
    let pre = UvWorkspace.verify(&ctx).unwrap();
    assert_eq!(pre.len(), 1, "expected DRIFT pre-condition");

    UvWorkspace.activate(&ctx).unwrap();

    let post = UvWorkspace.verify(&ctx).unwrap();
    assert!(
        post.is_empty(),
        "verify() must return no issues after activate (CLEAN), got: {post:?}"
    );

    let content = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
    assert!(
        content.contains("github/acme/web"),
        "web must be in members after fix: {content}"
    );
}

// -----------------------------------------------------------------------
// USER-HELD
// -----------------------------------------------------------------------

/// Given: pyproject.toml with [tool.uv.workspace].members but NO marker.
/// Then:  verify() reports USER-HELD+!safe_to_fix.
#[test]
fn s7_uv_doctor_user_held_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // No "# managed by rwv" marker.
    write_file(
        root,
        "pyproject.toml",
        "[tool.uv.workspace]\nmembers = [\"github/acme/server\"]\n",
    );
    touch(root, "github/acme/server/pyproject.toml");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = UvWorkspace.verify(&ctx).unwrap();
    assert_eq!(
        issues.len(),
        1,
        "expected exactly one USER-HELD issue, got: {issues:?}"
    );
    let issue = &issues[0];
    assert!(
        !issue.safe_to_fix,
        "USER-HELD issue must NOT be safe_to_fix"
    );
    assert!(
        issue.message.contains("NOT auto-take-over")
            || issue.message.contains("not auto")
            || issue.message.contains("unmarked"),
        "USER-HELD message must describe no-takeover: {}",
        issue.message
    );
}

/// Given: USER-HELD pyproject.toml.
/// When:  activate() runs (merge's guard).
/// Then:  The members key is NOT clobbered.
#[test]
fn s7_uv_doctor_user_held_file_unchanged_after_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let original = "[tool.uv.workspace]\nmembers = [\"github/acme/server\"]\n";
    write_file(root, "pyproject.toml", original);
    touch(root, "github/acme/server/pyproject.toml");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = UvWorkspace.verify(&ctx).unwrap();
    assert_eq!(issues.len(), 1);
    assert!(!issues[0].safe_to_fix, "must be USER-HELD");

    // Even if activate() is called, the members key must not be overwritten.
    UvWorkspace.activate(&ctx).unwrap();

    let after = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
    assert!(
        !after.contains("# managed by rwv"),
        "user-held file must NOT get rwv marker from activate: {after}"
    );
}

// -----------------------------------------------------------------------
// CLEAN
// -----------------------------------------------------------------------

/// Given: pyproject.toml written by activate() (marker + correct members).
/// Then:  verify() returns no issues (CLEAN).
#[test]
fn s7_uv_doctor_clean_after_fresh_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/pyproject.toml");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    UvWorkspace.activate(&ctx).unwrap();

    let issues = UvWorkspace.verify(&ctx).unwrap();
    assert!(
        issues.is_empty(),
        "verify() must return no issues for a freshly-activated pyproject.toml, got: {issues:?}"
    );
}
