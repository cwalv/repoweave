// ===========================================================================
// doctor verify() — npm-workspaces
// ===========================================================================

use super::*;
use repoweave::integrations::NpmWorkspaces;

// -----------------------------------------------------------------------
// MISSING: verify() reports MISSING when package.json is absent
// -----------------------------------------------------------------------

/// Given: npm repos detected but package.json absent.
/// Then:  verify() reports a single MISSING+safe_to_fix finding.
#[test]
fn s7_npm_doctor_missing_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/package.json");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    project_on_disk(root, "test-project");
    let session = WorkspaceSession::new(root);
    let base = IntegrationContextBase::new(&session, &project, &cache, None);
    let ctx = base.build_context(&config, &manifest);

    let issues = NpmWorkspaces.verify(&ctx).unwrap();
    assert_eq!(
        issues.len(),
        1,
        "expected exactly one MISSING issue, got: {issues:?}"
    );
    let issue = &issues[0];
    assert!(issue.safe_to_fix, "MISSING issue must be safe_to_fix");
    assert!(
        issue.message.to_string().contains("missing"),
        "MISSING issue message should contain 'missing': {}",
        issue.message
    );
    assert!(
        issue.message.to_string().contains("rwv doctor --fix"),
        "MISSING issue message should mention 'rwv doctor --fix': {}",
        issue.message
    );
}

/// Given: MISSING package.json.
/// When:  activate() runs (simulating doctor --fix).
/// Then:  package.json created with x-repoweave marker; verify() returns CLEAN.
#[test]
fn s7_npm_doctor_missing_fixed_by_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    let project_dir = root.join("projects/test-project");
    project_on_disk(root, "test-project");

    touch(root, "github/acme/server/package.json");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let session = WorkspaceSession::new(root);
    let base = IntegrationContextBase::new(&session, &project, &cache, None);
    let ctx = base.build_context(&config, &manifest);

    // Pre-condition: MISSING.
    let pre_issues = NpmWorkspaces.verify(&ctx).unwrap();
    assert_eq!(pre_issues.len(), 1, "expected MISSING pre-condition");
    assert!(pre_issues[0].safe_to_fix);

    // Simulate doctor --fix.
    NpmWorkspaces.activate(&ctx).unwrap();

    let pkg_path = project_dir.join("package.json");
    assert!(
        pkg_path.exists(),
        "package.json must be created after activate"
    );

    let content = std::fs::read_to_string(&pkg_path).unwrap();
    assert!(
        content.contains("x-repoweave"),
        "package.json must have x-repoweave marker after activate: {content}"
    );

    // Post-condition: CLEAN.
    let post_issues = NpmWorkspaces.verify(&ctx).unwrap();
    assert!(
        post_issues.is_empty(),
        "verify() must return no issues after activate (CLEAN), got: {post_issues:?}"
    );
}

// -----------------------------------------------------------------------
// DRIFT: verify() reports DRIFT when marker present but content differs
// -----------------------------------------------------------------------

/// Given: package.json with x-repoweave marker but outdated workspaces list.
/// Then:  verify() reports a single DRIFT+safe_to_fix finding.
#[test]
fn s7_npm_doctor_drift_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    let project_dir = root.join("projects/test-project");
    project_on_disk(root, "test-project");

    // Write package.json with marker but only one workspace (outdated).
    write_file(
        &project_dir,
        "package.json",
        r#"{"x-repoweave":{"managed":true},"name":"test-project","private":true,"workspaces":["github/acme/server"]}"#,
    );

    // Both repos have package.json on disk.
    touch(root, "github/acme/server/package.json");
    touch(root, "github/acme/web/package.json");

    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/web", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let session = WorkspaceSession::new(root);
    let base = IntegrationContextBase::new(&session, &project, &cache, None);
    let ctx = base.build_context(&config, &manifest);

    let issues = NpmWorkspaces.verify(&ctx).unwrap();
    assert_eq!(
        issues.len(),
        1,
        "expected exactly one DRIFT issue, got: {issues:?}"
    );
    let issue = &issues[0];
    assert!(issue.safe_to_fix, "DRIFT issue must be safe_to_fix");
    assert!(
        issue.message.to_string().contains("drift"),
        "DRIFT issue message should contain 'drift': {}",
        issue.message
    );
}

/// Given: DRIFT package.json.
/// When:  activate() runs.
/// Then:  verify() returns CLEAN.
#[test]
fn s7_npm_doctor_drift_fixed_by_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    let project_dir = root.join("projects/test-project");
    project_on_disk(root, "test-project");

    write_file(
        &project_dir,
        "package.json",
        r#"{"x-repoweave":{"managed":true},"name":"test-project","private":true,"workspaces":["github/acme/server"]}"#,
    );
    touch(root, "github/acme/server/package.json");
    touch(root, "github/acme/web/package.json");

    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/web", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let session = WorkspaceSession::new(root);
    let base = IntegrationContextBase::new(&session, &project, &cache, None);
    let ctx = base.build_context(&config, &manifest);

    // Pre-condition: DRIFT.
    let pre_issues = NpmWorkspaces.verify(&ctx).unwrap();
    assert_eq!(pre_issues.len(), 1, "expected DRIFT pre-condition");

    // Simulate fix.
    NpmWorkspaces.activate(&ctx).unwrap();

    // Post-condition: CLEAN.
    let post_issues = NpmWorkspaces.verify(&ctx).unwrap();
    assert!(
        post_issues.is_empty(),
        "verify() must return no issues after activate (CLEAN), got: {post_issues:?}"
    );

    let content = std::fs::read_to_string(project_dir.join("package.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    let ws = parsed["workspaces"].as_array().unwrap();
    assert_eq!(ws.len(), 2, "both repos must be in workspaces after fix");
}

// -----------------------------------------------------------------------
// USER-HELD: verify() reports USER-HELD, doctor --fix is a no-op
// -----------------------------------------------------------------------

/// Given: package.json with workspaces but NO x-repoweave marker.
/// Then:  verify() reports USER-HELD+!safe_to_fix.
#[test]
fn s7_npm_doctor_user_held_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    let project_dir = root.join("projects/test-project");
    project_on_disk(root, "test-project");

    // No x-repoweave marker — user holds the pen.
    write_file(
        &project_dir,
        "package.json",
        r#"{"name":"test-project","private":true,"workspaces":["github/acme/server"]}"#,
    );
    touch(root, "github/acme/server/package.json");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let session = WorkspaceSession::new(root);
    let base = IntegrationContextBase::new(&session, &project, &cache, None);
    let ctx = base.build_context(&config, &manifest);

    let issues = NpmWorkspaces.verify(&ctx).unwrap();
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
        issue.message.to_string().contains("NOT auto-take-over")
            || issue.message.to_string().contains("not auto")
            || issue.message.to_string().contains("unmarked"),
        "USER-HELD message must describe no-takeover: {}",
        issue.message
    );
}

/// Given: USER-HELD package.json.
/// When:  activate() runs (merge's own guard).
/// Then:  The workspaces content is left intact (merge defers to user).
#[test]
fn s7_npm_doctor_user_held_file_unchanged_after_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    let project_dir = root.join("projects/test-project");
    project_on_disk(root, "test-project");

    let original = r#"{"name":"test-project","private":true,"workspaces":["github/acme/server"]}"#;
    write_file(&project_dir, "package.json", original);
    touch(root, "github/acme/server/package.json");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let session = WorkspaceSession::new(root);
    let base = IntegrationContextBase::new(&session, &project, &cache, None);
    let ctx = base.build_context(&config, &manifest);

    // Verify reports USER-HELD with safe_to_fix=false.
    let issues = NpmWorkspaces.verify(&ctx).unwrap();
    assert_eq!(issues.len(), 1);
    assert!(
        !issues[0].safe_to_fix,
        "must be USER-HELD (not safe_to_fix)"
    );

    // Even if activate() is called, the workspaces key is left intact.
    NpmWorkspaces.activate(&ctx).unwrap();

    let after = std::fs::read_to_string(project_dir.join("package.json")).unwrap();
    // Merge defers: the user's workspaces array is not overwritten.
    assert!(
        !after.contains("x-repoweave"),
        "user-held file must NOT have x-repoweave marker added by activate: {after}"
    );
}

// -----------------------------------------------------------------------
// CLEAN: verify() returns no issues when file is up to date
// -----------------------------------------------------------------------

/// Given: package.json was written by activate() (marker + correct content).
/// Then:  verify() returns no issues (CLEAN).
#[test]
fn s7_npm_doctor_clean_after_fresh_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/package.json");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    project_on_disk(root, "test-project");
    let session = WorkspaceSession::new(root);
    let base = IntegrationContextBase::new(&session, &project, &cache, None);
    let ctx = base.build_context(&config, &manifest);

    NpmWorkspaces.activate(&ctx).unwrap();

    let issues = NpmWorkspaces.verify(&ctx).unwrap();
    assert!(
        issues.is_empty(),
        "verify() must return no issues for a freshly-activated package.json, got: {issues:?}"
    );
}
