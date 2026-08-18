// ===========================================================================
// doctor verify() — vscode-workspace
// ===========================================================================

use super::*;
use repoweave::integrations::VscodeWorkspace;

// -----------------------------------------------------------------------
// MISSING
// -----------------------------------------------------------------------

/// Given: No .code-workspace file.
/// Then:  verify() reports MISSING+safe_to_fix.
#[test]
fn s7_vscode_doctor_missing_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = VscodeWorkspace.verify(&ctx).unwrap();
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
}

/// Given: MISSING .code-workspace.
/// When:  activate() runs.
/// Then:  file created with rwv.generated marker; verify() returns CLEAN.
#[test]
fn s7_vscode_doctor_missing_fixed_by_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // Pre-condition: MISSING.
    let pre = VscodeWorkspace.verify(&ctx).unwrap();
    assert_eq!(pre.len(), 1, "expected MISSING pre-condition");
    assert!(pre[0].safe_to_fix);

    VscodeWorkspace.activate(&ctx).unwrap();

    let filepath = root.join("test-project.code-workspace");
    assert!(
        filepath.exists(),
        "code-workspace must be created after activate"
    );

    let content = std::fs::read_to_string(&filepath).unwrap();
    assert!(
        content.contains("rwv.generated"),
        "file must have rwv.generated marker after activate: {content}"
    );

    let post = VscodeWorkspace.verify(&ctx).unwrap();
    assert!(
        post.is_empty(),
        "verify() must return no issues after activate (CLEAN), got: {post:?}"
    );
}

// -----------------------------------------------------------------------
// DRIFT
// -----------------------------------------------------------------------

/// Given: .code-workspace with rwv.generated marker but wrong primary folder.
/// Then:  verify() reports DRIFT+safe_to_fix.
#[test]
fn s7_vscode_doctor_drift_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Wrong project name in the primary folder.
    write_file(
        root,
        "test-project.code-workspace",
        r#"{
  "rwv.generated": {"managed": true, "files.exclude": []},
  "folders": [{"path": ".", "name": "old-project (primary)"}],
  "settings": {}
}
"#,
    );

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = VscodeWorkspace.verify(&ctx).unwrap();
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

/// Given: DRIFT .code-workspace.
/// When:  activate() runs.
/// Then:  verify() returns CLEAN.
#[test]
fn s7_vscode_doctor_drift_fixed_by_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "test-project.code-workspace",
        r#"{
  "rwv.generated": {"managed": true, "files.exclude": []},
  "folders": [{"path": ".", "name": "old-project (primary)"}],
  "settings": {}
}
"#,
    );

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // Pre-condition: DRIFT.
    let pre = VscodeWorkspace.verify(&ctx).unwrap();
    assert_eq!(pre.len(), 1, "expected DRIFT pre-condition");

    VscodeWorkspace.activate(&ctx).unwrap();

    let post = VscodeWorkspace.verify(&ctx).unwrap();
    assert!(
        post.is_empty(),
        "verify() must return no issues after activate (CLEAN), got: {post:?}"
    );
}

// -----------------------------------------------------------------------
// USER-HELD
// -----------------------------------------------------------------------

/// Given: .code-workspace file with NO rwv.generated marker.
/// Then:  verify() reports USER-HELD+!safe_to_fix.
#[test]
fn s7_vscode_doctor_user_held_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // No rwv.generated marker.
    write_file(
        root,
        "test-project.code-workspace",
        r#"{
  "folders": [{"path": ".", "name": "test-project (primary)"}],
  "settings": {}
}
"#,
    );

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = VscodeWorkspace.verify(&ctx).unwrap();
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

/// Given: USER-HELD .code-workspace.
/// When:  activate() runs.
/// Then:  The file is byte-identical and still USER-HELD — activate never
///        takes the pen from a file it does not already hold.
#[test]
fn s7_vscode_doctor_user_held_file_unchanged_after_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let original = r#"{
  "folders": [{"path": ".", "name": "test-project (primary)"}],
  "settings": {}
}
"#;
    write_file(root, "test-project.code-workspace", original);

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = VscodeWorkspace.verify(&ctx).unwrap();
    assert_eq!(issues.len(), 1);
    assert!(!issues[0].safe_to_fix, "must be USER-HELD");

    VscodeWorkspace.activate(&ctx).unwrap();

    let after = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
    assert_eq!(
        after, original,
        "activate must leave a USER-HELD file byte-identical"
    );

    // Still USER-HELD: activate did not convert doctor's finding by
    // stamping the marker.
    let post = VscodeWorkspace.verify(&ctx).unwrap();
    assert_eq!(post.len(), 1, "expected the USER-HELD finding to persist");
    assert!(
        !post[0].safe_to_fix,
        "post-activate finding must still be USER-HELD, got: {post:?}"
    );
}

// -----------------------------------------------------------------------
// CLEAN
// -----------------------------------------------------------------------

/// Given: .code-workspace written by activate() (marker + correct primary).
/// Then:  verify() returns no issues (CLEAN).
#[test]
fn s7_vscode_doctor_clean_after_fresh_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    VscodeWorkspace.activate(&ctx).unwrap();

    let issues = VscodeWorkspace.verify(&ctx).unwrap();
    assert!(
        issues.is_empty(),
        "verify() must return no issues for a freshly-activated .code-workspace, got: {issues:?}"
    );
}
