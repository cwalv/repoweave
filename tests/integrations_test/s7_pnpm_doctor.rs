// ===========================================================================
// doctor verify() — pnpm-workspaces
// ===========================================================================

use super::*;
use repoweave::integrations::PnpmWorkspaces;

// -----------------------------------------------------------------------
// MISSING
// -----------------------------------------------------------------------

/// Given: pnpm repos detected but pnpm-workspace.yaml absent.
/// Then:  verify() reports a single MISSING+safe_to_fix finding.
#[test]
fn s7_pnpm_doctor_missing_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/package.json");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = PnpmWorkspaces.verify(&ctx).unwrap();
    assert_eq!(
        issues.len(),
        1,
        "expected exactly one MISSING issue, got: {issues:?}"
    );
    let issue = &issues[0];
    assert!(issue.safe_to_fix, "MISSING issue must be safe_to_fix");
    assert!(
        issue.message.contains("missing"),
        "MISSING issue message should contain 'missing': {}",
        issue.message
    );
}

/// Given: MISSING pnpm-workspace.yaml.
/// When:  activate() runs.
/// Then:  file created with marker; verify() returns CLEAN.
#[test]
fn s7_pnpm_doctor_missing_fixed_by_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/package.json");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // Pre-condition: MISSING.
    let pre = PnpmWorkspaces.verify(&ctx).unwrap();
    assert_eq!(pre.len(), 1, "expected MISSING pre-condition");
    assert!(pre[0].safe_to_fix);

    PnpmWorkspaces.activate(&ctx).unwrap();

    let path = root.join("pnpm-workspace.yaml");
    assert!(
        path.exists(),
        "pnpm-workspace.yaml must exist after activate"
    );

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
        content.contains("# managed by repoweave"),
        "file must have marker after activate: {content}"
    );

    let post = PnpmWorkspaces.verify(&ctx).unwrap();
    assert!(
        post.is_empty(),
        "verify() must return no issues after activate (CLEAN), got: {post:?}"
    );
}

// -----------------------------------------------------------------------
// DRIFT
// -----------------------------------------------------------------------

/// Given: pnpm-workspace.yaml with marker but outdated packages list.
/// Then:  verify() reports DRIFT+safe_to_fix.
#[test]
fn s7_pnpm_doctor_drift_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "pnpm-workspace.yaml",
        "# managed by repoweave\npackages:\n  - github/acme/server\n",
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
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = PnpmWorkspaces.verify(&ctx).unwrap();
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

/// Given: DRIFT pnpm-workspace.yaml.
/// When:  activate() runs.
/// Then:  verify() returns CLEAN.
#[test]
fn s7_pnpm_doctor_drift_fixed_by_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "pnpm-workspace.yaml",
        "# managed by repoweave\npackages:\n  - github/acme/server\n",
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
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // Pre-condition: DRIFT.
    let pre = PnpmWorkspaces.verify(&ctx).unwrap();
    assert_eq!(pre.len(), 1, "expected DRIFT pre-condition");

    PnpmWorkspaces.activate(&ctx).unwrap();

    let post = PnpmWorkspaces.verify(&ctx).unwrap();
    assert!(
        post.is_empty(),
        "verify() must return no issues after activate (CLEAN), got: {post:?}"
    );
}

// -----------------------------------------------------------------------
// USER-HELD
// -----------------------------------------------------------------------

/// Given: pnpm-workspace.yaml with packages: but NO marker.
/// Then:  verify() reports USER-HELD+!safe_to_fix.
#[test]
fn s7_pnpm_doctor_user_held_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // No marker line.
    write_file(
        root,
        "pnpm-workspace.yaml",
        "packages:\n  - github/acme/server\n",
    );
    touch(root, "github/acme/server/package.json");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = PnpmWorkspaces.verify(&ctx).unwrap();
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

/// Given: USER-HELD pnpm-workspace.yaml.
/// When:  activate() runs (merge's guard).
/// Then:  The file is byte-identical and still USER-HELD — activate never
///        takes the pen from a file it does not already hold.
#[test]
fn s7_pnpm_doctor_user_held_file_unchanged_after_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let original = "packages:\n  - github/acme/server\n";
    write_file(root, "pnpm-workspace.yaml", original);
    touch(root, "github/acme/server/package.json");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = PnpmWorkspaces.verify(&ctx).unwrap();
    assert_eq!(issues.len(), 1);
    assert!(!issues[0].safe_to_fix, "must be USER-HELD");

    PnpmWorkspaces.activate(&ctx).unwrap();

    let after = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
    assert_eq!(
        after, original,
        "activate must leave a USER-HELD pnpm-workspace.yaml byte-identical"
    );

    let post = PnpmWorkspaces.verify(&ctx).unwrap();
    assert_eq!(post.len(), 1, "expected the USER-HELD finding to persist");
    assert!(
        !post[0].safe_to_fix,
        "post-activate finding must still be USER-HELD, got: {post:?}"
    );
}

// -----------------------------------------------------------------------
// CLEAN
// -----------------------------------------------------------------------

/// Given: pnpm-workspace.yaml was written by activate() (marker + correct content).
/// Then:  verify() returns no issues (CLEAN).
#[test]
fn s7_pnpm_doctor_clean_after_fresh_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/package.json");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    PnpmWorkspaces.activate(&ctx).unwrap();

    let issues = PnpmWorkspaces.verify(&ctx).unwrap();
    assert!(
        issues.is_empty(),
        "verify() must return no issues for a freshly-activated pnpm-workspace.yaml, got: {issues:?}"
    );
}

// -----------------------------------------------------------------------
// CLEAN — duplicate/overlapping globs must not cause false DRIFT
// -----------------------------------------------------------------------

/// Regression: when a member repo's pnpm-workspace.yaml has duplicate
/// glob entries, expand_workspace_entries() produces a list with repeated
/// items.  activate() writes the deduped set (via OwnedValue::sorted_array)
/// but the old verify() only sorted — not deduped — its expected list,
/// making a CLEAN file look like DRIFT.
///
/// Given: member repo whose pnpm-workspace.yaml lists the same glob twice.
/// When:  activate() runs (on-disk is deduped), then verify() runs.
/// Then:  verify() returns no issues (CLEAN).
#[test]
fn s7_pnpm_doctor_clean_when_member_has_duplicate_globs() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Multi-package repo whose pnpm-workspace.yaml repeats "packages/*".
    // expand_workspace_entries() will emit the glob twice; activate() dedupes
    // it before writing.  verify() must also dedup before comparing.
    touch(root, "github/acme/mono/package.json");
    write_file(
        root,
        "github/acme/mono/pnpm-workspace.yaml",
        "packages:\n  - packages/*\n  - packages/*\n",
    );

    let manifest = make_manifest(vec![("github/acme/mono", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // activate() writes the deduped, sorted set.
    PnpmWorkspaces.activate(&ctx).unwrap();

    // verify() must agree with what activate() wrote — no false DRIFT.
    let issues = PnpmWorkspaces.verify(&ctx).unwrap();
    assert!(
        issues.is_empty(),
        "verify() must return CLEAN when on-disk matches the deduped member globs, got: {issues:?}"
    );
}
