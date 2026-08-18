// ===========================================================================
// doctor verify() — go-work
// ===========================================================================

use super::*;
use repoweave::integrations::GoWork;

// Whether these fixtures exercise go-work's `go work` path or its
// hand-parse fallback is decided by whether `go` is on PATH here, and this
// file cannot decide it: the `FORCE_GOWORK_FALLBACK` override is
// `#[cfg(test)]`-gated inside the library, so neither it nor the branch
// reading it exists in the build an integration test links against.
// Measured: forcing either answer leaves all seven green, so no assertion
// below separates the two paths — treat a green here as saying nothing
// about which one ran.

fn write_go_mod(root: &Path, repo: &str) {
    write_file(
        root,
        &format!("{repo}/go.mod"),
        "module example.com/x\n\ngo 1.20\n",
    );
}

// -----------------------------------------------------------------------
// MISSING
// -----------------------------------------------------------------------

/// Given: Go repos detected but go.work absent.
/// Then:  verify() reports a single MISSING+safe_to_fix finding.
#[test]
fn s7_go_work_doctor_missing_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_go_mod(root, "github/acme/server");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = GoWork.verify(&ctx).unwrap();
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

/// Given: MISSING go.work.
/// When:  activate() runs.
/// Then:  file created with marker; verify() returns CLEAN.
#[test]
fn s7_go_work_doctor_missing_fixed_by_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_go_mod(root, "github/acme/server");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // Pre-condition: MISSING.
    let pre = GoWork.verify(&ctx).unwrap();
    assert_eq!(pre.len(), 1, "expected MISSING pre-condition");
    assert!(pre[0].safe_to_fix);

    GoWork.activate(&ctx).unwrap();

    let path = root.join("go.work");
    assert!(path.exists(), "go.work must be created after activate");

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
        content.contains("// managed by repoweave"),
        "go.work must have '// managed by repoweave' marker: {content}"
    );

    let post = GoWork.verify(&ctx).unwrap();
    assert!(
        post.is_empty(),
        "verify() must return no issues after activate (CLEAN), got: {post:?}"
    );
}

// -----------------------------------------------------------------------
// DRIFT
// -----------------------------------------------------------------------

/// Given: go.work with marker but outdated use entries.
/// Then:  verify() reports DRIFT+safe_to_fix.
#[test]
fn s7_go_work_doctor_drift_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "go.work",
        concat!(
            "go 1.20\n\n",
            "// managed by repoweave\n",
            "use (\n",
            "\t./github/acme/server\n",
            ")\n"
        ),
    );
    write_go_mod(root, "github/acme/server");
    write_go_mod(root, "github/acme/web");

    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/web", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = GoWork.verify(&ctx).unwrap();
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

/// Given: DRIFT go.work.
/// When:  activate() runs.
/// Then:  verify() returns CLEAN.
#[test]
fn s7_go_work_doctor_drift_fixed_by_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "go.work",
        concat!(
            "go 1.20\n\n",
            "// managed by repoweave\n",
            "use (\n",
            "\t./github/acme/server\n",
            ")\n"
        ),
    );
    write_go_mod(root, "github/acme/server");
    write_go_mod(root, "github/acme/web");

    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/web", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // Pre-condition: DRIFT.
    let pre = GoWork.verify(&ctx).unwrap();
    assert_eq!(pre.len(), 1, "expected DRIFT pre-condition");

    GoWork.activate(&ctx).unwrap();

    let post = GoWork.verify(&ctx).unwrap();
    assert!(
        post.is_empty(),
        "verify() must return no issues after activate (CLEAN), got: {post:?}"
    );

    let content = std::fs::read_to_string(root.join("go.work")).unwrap();
    assert!(
        content.contains("github/acme/web"),
        "web must be in use entries after fix: {content}"
    );
}

// -----------------------------------------------------------------------
// USER-HELD
// -----------------------------------------------------------------------

/// Given: go.work with use block but NO `// managed by repoweave` marker.
/// Then:  verify() reports USER-HELD+!safe_to_fix.
#[test]
fn s7_go_work_doctor_user_held_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // No marker.
    write_file(
        root,
        "go.work",
        "go 1.20\n\nuse (\n\t./github/acme/server\n)\n",
    );
    write_go_mod(root, "github/acme/server");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = GoWork.verify(&ctx).unwrap();
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

/// Given: USER-HELD go.work (use block present, no marker).
/// When:  activate() runs (forced-fallback path).
/// Then:  the file is byte-for-byte unchanged — the ownership guard
///        short-circuits before any mutation.
///
/// This is the parity test with cargo-workspace's equivalent invariant:
/// a present-but-unmarked managed file is left strictly alone.
#[test]
fn s7_go_work_doctor_user_held_file_unchanged_after_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let original = "go 1.20\n\nuse (\n\t./github/acme/server\n)\n";
    write_file(root, "go.work", original);
    write_go_mod(root, "github/acme/server");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // Verify the pre-condition: USER-HELD detected before activate.
    let issues = GoWork.verify(&ctx).unwrap();
    assert_eq!(issues.len(), 1);
    assert!(
        !issues[0].safe_to_fix,
        "must be USER-HELD (safe_to_fix=false)"
    );

    // Read the file before activate.
    let before = std::fs::read(root.join("go.work")).unwrap();

    // Call activate() — the ownership guard must short-circuit; no mutation.
    GoWork.activate(&ctx).unwrap();

    // Read the file after activate.
    let after = std::fs::read(root.join("go.work")).unwrap();

    assert_eq!(
        before, after,
        "user-held go.work must be byte-for-byte unchanged after activate()"
    );

    // Confirm the file still has no rwv marker (takeover did NOT happen).
    let text = std::fs::read_to_string(root.join("go.work")).unwrap();
    assert!(
        !text.contains("managed by repoweave"),
        "marker must NOT be injected into a user-held file: {text}"
    );
    assert!(
        text.contains("./github/acme/server"),
        "user use entry must survive unchanged: {text}"
    );
}

// -----------------------------------------------------------------------
// CLEAN
// -----------------------------------------------------------------------

/// Given: go.work written by activate() (marker + correct use entries).
/// Then:  verify() returns no issues (CLEAN).
#[test]
fn s7_go_work_doctor_clean_after_fresh_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_go_mod(root, "github/acme/server");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    GoWork.activate(&ctx).unwrap();

    let issues = GoWork.verify(&ctx).unwrap();
    assert!(
        issues.is_empty(),
        "verify() must return no issues for a freshly-activated go.work, got: {issues:?}"
    );
}
