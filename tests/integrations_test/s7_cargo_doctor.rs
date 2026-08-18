// ===========================================================================
// cargo-workspace doctor-acceptance battery
// ===========================================================================
//
// Verify() + doctor --fix acceptance tests for cargo-workspace.
// Named `s7_cargo_doctor_*` per the spec so they are discoverable as
// a battery: `cargo test --test integrations_test s7_cargo_doctor_`.
//
// These tests drive the integration directly (verify() / activate()) rather
// than the full CLI doctor path — that is the C17-aligned style.

use super::*;
use repoweave::integration::Issue;
use repoweave::integrations::CargoWorkspace;

/// Filter verify() output to hybrid-Cargo.toml findings only.
///
/// The older tests in this module were written when `verify()` only
/// inspected the hybrid `Cargo.toml`; it was later extended to also
/// inspect the fully-owned `Cargo.lock`. To keep those
/// pre-existing tests focused on their original semantic axis
/// (Cargo.toml states) without seeding an unrelated Cargo.lock in each
/// fixture, this helper filters out Cargo.lock findings. The fully-owned
/// axis is covered separately, by the fully-owned `Cargo.lock` battery below.
fn cargo_toml_issues(issues: Vec<Issue>) -> Vec<Issue> {
    issues
        .into_iter()
        .filter(|i| !i.message.contains("Cargo.lock"))
        .collect()
}

// -----------------------------------------------------------------------
// MISSING: verify() reports MISSING when Cargo.toml is absent
// -----------------------------------------------------------------------

/// Given: cargo-workspace config with members.include = [a, b, c],
///        Cargo.toml ABSENT.
/// Then:  verify() reports a single MISSING+safe_to_fix finding.
#[test]
fn s7_cargo_doctor_missing_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Repo "github/cwalv/rvtty" with sub-packages; no root Cargo.toml.
    let config = IntegrationConfig::from_toml(
        "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\", \"client\", \"common\"]\n",
    );
    let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
    assert_eq!(
        issues.len(),
        1,
        "expected exactly one MISSING issue (Cargo.toml axis), got: {issues:?}"
    );
    let issue = &issues[0];
    assert!(issue.safe_to_fix, "MISSING issue must be safe_to_fix");
    assert!(
        issue.message.contains("missing"),
        "MISSING issue message should contain 'missing': {}",
        issue.message
    );
    assert!(
        issue.message.contains("rwv doctor --fix"),
        "MISSING issue message should mention 'rwv doctor --fix': {}",
        issue.message
    );
}

/// Given: MISSING Cargo.toml.
/// When:  activate() runs (simulating doctor --fix).
/// Then:
///   - Cargo.toml created with `# managed by rwv` markers
///   - members lists rvtty/daemon, rvtty/client, rvtty/common (alphabetical)
///   - resolver = "2"
///   - Subsequent verify() returns no issues (CLEAN).
#[test]
fn s7_cargo_doctor_missing_fixed_by_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let config = IntegrationConfig::from_toml(
        "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\", \"client\", \"common\"]\n",
    );
    let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // Pre-condition: MISSING.
    let pre_issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
    assert_eq!(pre_issues.len(), 1, "expected MISSING pre-condition");
    assert!(pre_issues[0].safe_to_fix);

    // Simulate doctor --fix: call activate().
    CargoWorkspace.activate(&ctx).unwrap();

    // Cargo.toml must exist now.
    let cargo_toml_path = root.join("Cargo.toml");
    assert!(
        cargo_toml_path.exists(),
        "Cargo.toml must be created after activate"
    );

    let content = std::fs::read_to_string(&cargo_toml_path).unwrap();

    // Markers must be present.
    assert!(
        content.contains("# managed by rwv"),
        "Cargo.toml must have '# managed by rwv' markers after activate: {content}"
    );

    // Members must be sorted alphabetically: client < common < daemon.
    assert!(
        content.contains("\"github/cwalv/rvtty/client\""),
        "members must include rvtty/client: {content}"
    );
    assert!(
        content.contains("\"github/cwalv/rvtty/common\""),
        "members must include rvtty/common: {content}"
    );
    assert!(
        content.contains("\"github/cwalv/rvtty/daemon\""),
        "members must include rvtty/daemon: {content}"
    );

    // Check alphabetical order in the raw text.
    let client_pos = content.find("rvtty/client").unwrap();
    let common_pos = content.find("rvtty/common").unwrap();
    let daemon_pos = content.find("rvtty/daemon").unwrap();
    assert!(
        client_pos < common_pos && common_pos < daemon_pos,
        "members must be alphabetically sorted: client < common < daemon"
    );

    // resolver = "2".
    assert!(
        content.contains("resolver = \"2\""),
        "Cargo.toml must set resolver = \"2\": {content}"
    );

    // Post-condition: CLEAN (no verify issues on the Cargo.toml axis).
    // Cargo.lock is still absent (activate() does not run the hook), so
    // the fully-owned arm would emit MISSING — exercised by the
    // fully-owned `Cargo.lock` battery, and out of scope here.
    let post_issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
    assert!(
        post_issues.is_empty(),
        "verify() must return no Cargo.toml issues after activate (CLEAN), got: {post_issues:?}"
    );
}

// -----------------------------------------------------------------------
// DRIFT: verify() reports DRIFT when markers are present but
//      on-disk content doesn't match config
// -----------------------------------------------------------------------

/// Given: Cargo.toml exists with rwv markers but outdated members list.
/// Then:  verify() reports a single DRIFT+safe_to_fix finding.
#[test]
fn s7_cargo_doctor_drift_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Write a Cargo.toml with per-key rwv markers but only one member (outdated).
    // Marker format: `# managed by rwv` as a prefix decoration on each owned key,
    // matching what TomlDoc's merge_activate produces.
    write_file(
        root,
        "Cargo.toml",
        "[workspace]\n# managed by rwv\nmembers = [\"github/cwalv/rvtty/daemon\"]\n# managed by rwv\nresolver = \"2\"\n",
    );

    let config = IntegrationConfig::from_toml(
        // Config now has two members (drift: common was added to config but not file).
        "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\", \"common\"]\n",
    );
    let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
    assert_eq!(
        issues.len(),
        1,
        "expected exactly one DRIFT issue (Cargo.toml axis), got: {issues:?}"
    );
    let issue = &issues[0];
    assert!(issue.safe_to_fix, "DRIFT issue must be safe_to_fix");
    assert!(
        issue.message.contains("drift"),
        "DRIFT issue message should contain 'drift': {}",
        issue.message
    );
}

/// Given: DRIFT Cargo.toml.
/// When:  activate() runs.
/// Then:  verify() returns no issues (CLEAN).
#[test]
fn s7_cargo_doctor_drift_fixed_by_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Seed: only daemon in members (drift), with per-key markers.
    write_file(
        root,
        "Cargo.toml",
        "[workspace]\n# managed by rwv\nmembers = [\"github/cwalv/rvtty/daemon\"]\n# managed by rwv\nresolver = \"2\"\n",
    );

    let config = IntegrationConfig::from_toml(
        "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\", \"common\"]\n",
    );
    let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // Pre-condition: DRIFT.
    let pre_issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
    assert_eq!(pre_issues.len(), 1, "expected DRIFT pre-condition");

    // Simulate fix.
    CargoWorkspace.activate(&ctx).unwrap();

    // Post-condition: CLEAN on the Cargo.toml axis.
    let post_issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
    assert!(
        post_issues.is_empty(),
        "verify() must return no Cargo.toml issues after activate (CLEAN), got: {post_issues:?}"
    );

    // Confirm common is now in the file.
    let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(
        content.contains("\"github/cwalv/rvtty/common\""),
        "common must be in members after fix: {content}"
    );
}

// -----------------------------------------------------------------------
// USER-HELD: verify() reports USER-HELD, doctor --fix is a no-op
// -----------------------------------------------------------------------

/// Given: Cargo.toml exists with [workspace] members/resolver, NO markers.
/// Then:  verify() reports a single USER-HELD+!safe_to_fix finding.
#[test]
fn s7_cargo_doctor_user_held_reports_finding() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // No "# managed by rwv" marker — user holds the pen.
    write_file(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"github/cwalv/rvtty/daemon\"]\nresolver = \"2\"\n",
    );

    let config =
        IntegrationConfig::from_toml("[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\"]\n");
    let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
    assert_eq!(
        issues.len(),
        1,
        "expected exactly one USER-HELD issue (Cargo.toml axis), got: {issues:?}"
    );
    let issue = &issues[0];
    assert!(
        !issue.safe_to_fix,
        "USER-HELD issue must NOT be safe_to_fix (safe_to_fix=false)"
    );
    assert!(
        issue.message.contains("NOT auto-take-over") || issue.message.contains("not auto"),
        "USER-HELD issue must describe no-takeover: {}",
        issue.message
    );
}

/// Given: USER-HELD Cargo.toml (no markers).
/// When:  activate() runs (simulating what doctor --fix would call if safe_to_fix
///        were true — but it won't, so this tests the merge's own guard).
/// Then:  The file is UNCHANGED (merge_activate's verify-and-warn semantics
///        protect the user-held keys).
#[test]
fn s7_cargo_doctor_user_held_file_unchanged_after_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let original_content =
        "[workspace]\nmembers = [\"github/cwalv/rvtty/daemon\"]\nresolver = \"2\"\n";
    write_file(root, "Cargo.toml", original_content);

    let config = IntegrationConfig::from_toml(
        "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\", \"common\"]\n",
    );
    let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // Verify reports USER-HELD with safe_to_fix=false.
    let issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
    assert_eq!(issues.len(), 1);
    assert!(
        !issues[0].safe_to_fix,
        "must be USER-HELD (not safe_to_fix)"
    );

    // Even if activate() is called (guard: doctor --fix does NOT call it
    // for user-held issues; this test verifies the merge's own protection),
    // the [workspace] content is left intact (merge defers to user).
    CargoWorkspace.activate(&ctx).unwrap();

    let after_content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(
        !after_content.contains("# managed by rwv"),
        "user-held file must NOT have rwv markers added by activate: {after_content}"
    );
    // The user's original members list is preserved (common was NOT added).
    assert!(
        !after_content.contains("rvtty/common"),
        "user-held members must not be modified by activate: {after_content}"
    );
}

// -----------------------------------------------------------------------
// CLEAN: verify() returns no issues when file is up to date
// -----------------------------------------------------------------------

/// Given: Cargo.toml was written by activate() (markers present, content
///        matches config).
/// Then:  verify() returns no issues (CLEAN).
#[test]
fn s7_cargo_doctor_clean_after_fresh_activate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let config = IntegrationConfig::from_toml(
        "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\", \"client\"]\n",
    );
    let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    CargoWorkspace.activate(&ctx).unwrap();

    let issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
    assert!(
        issues.is_empty(),
        "verify() must return no Cargo.toml issues for a freshly-activated Cargo.toml, got: {issues:?}"
    );
}

// -----------------------------------------------------------------------
// resolver DefaultOnly
// -----------------------------------------------------------------------

/// Greenfield: empty Cargo.toml gets `resolver = "2"` set by activate().
///
/// Given: fresh empty Cargo.toml (or no file at all).
/// When:  activate() runs.
/// Then:  resolver = "2" appears in the file.
#[test]
fn resolver_default_only_greenfield_sets_resolver_2() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/cwalv/myrepo/Cargo.toml");

    let manifest = make_manifest(vec![("github/cwalv/myrepo", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    CargoWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(
        content.contains("resolver = \"2\""),
        "greenfield activate must write resolver = \"2\": {content}"
    );
}

/// Existing without resolver: file with marker + no resolver key →
/// DefaultOnly sets "2".
///
/// Given: Cargo.toml with managed marker on members but no resolver key.
/// When:  activate() runs.
/// Then:  resolver = "2" is added to the file.
#[test]
fn resolver_default_only_no_resolver_key_sets_resolver_2() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // File has marker+members but no resolver.
    write_file(
        root,
        "Cargo.toml",
        "[workspace]\n# managed by rwv\nmembers = [\"github/cwalv/rvtty/daemon\"]\n",
    );
    touch(root, "github/cwalv/rvtty/daemon/Cargo.toml");

    let config =
        IntegrationConfig::from_toml("[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\"]\n");
    let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    CargoWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(
        content.contains("resolver = \"2\""),
        "activate must write resolver = \"2\" when key is absent: {content}"
    );
}

/// Operator override: existing Cargo.toml with marker + `resolver = "1"` →
/// after activate, resolver still "1" (DefaultOnly does not overwrite).
///
/// Given: Cargo.toml with managed markers AND resolver = "1" (compat setting).
/// When:  activate() runs.
/// Then:  resolver is still "1" in the file (not overwritten to "2").
#[test]
fn resolver_default_only_operator_override_preserved() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Cargo.toml seeded with resolver = "1" and the managed marker.
    write_file(
        root,
        "Cargo.toml",
        "[workspace]\n# managed by rwv\nmembers = [\"github/cwalv/rvtty/daemon\"]\n\
             # managed by rwv\nresolver = \"1\"\n",
    );
    touch(root, "github/cwalv/rvtty/daemon/Cargo.toml");

    let config =
        IntegrationConfig::from_toml("[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\"]\n");
    let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    CargoWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(
        content.contains("resolver = \"1\""),
        "activate must NOT overwrite operator's resolver = \"1\": {content}"
    );
    assert!(
        !content.contains("resolver = \"2\""),
        "resolver must not be changed to \"2\" when operator set \"1\": {content}"
    );
}

/// Resolver drift is CLEAN: file with marker + resolver = "1" → verify()
/// returns no issues (DefaultOnly drift is always CLEAN).
///
/// Given: Cargo.toml with managed markers and members matching config,
///        but resolver = "1" (differs from rwv's default "2").
/// Then:  verify() returns no issues (CLEAN — resolver drift is not a
///        DRIFT finding for DefaultOnly keys).
#[test]
fn resolver_default_only_drift_is_clean() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/cwalv/rvtty/daemon/Cargo.toml");

    // members matches config; resolver deviates from default "2".
    write_file(
        root,
        "Cargo.toml",
        "[workspace]\n# managed by rwv\nmembers = [\"github/cwalv/rvtty/daemon\"]\n\
             # managed by rwv\nresolver = \"1\"\n",
    );

    let config =
        IntegrationConfig::from_toml("[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\"]\n");
    let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
    assert!(
        issues.is_empty(),
        "resolver drift (DefaultOnly) must be CLEAN on Cargo.toml axis — got: {issues:?}"
    );
}

/// Members still drift: file with marker + correct resolver but wrong members
/// → DRIFT finding (members is still Author).
///
/// Given: Cargo.toml with managed markers, resolver = "2", but members
///        does not match config (drift on members, not resolver).
/// Then:  verify() reports exactly one DRIFT issue.
#[test]
fn resolver_default_only_members_drift_still_reported() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // members is stale (only daemon), config expects daemon + client.
    write_file(
        root,
        "Cargo.toml",
        "[workspace]\n# managed by rwv\nmembers = [\"github/cwalv/rvtty/daemon\"]\n\
             # managed by rwv\nresolver = \"2\"\n",
    );

    let config = IntegrationConfig::from_toml(
        "[members.\"github/cwalv/rvtty\"]\ninclude = [\"daemon\", \"client\"]\n",
    );
    let manifest = make_manifest(vec![("github/cwalv/rvtty", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = cargo_toml_issues(CargoWorkspace.verify(&ctx).unwrap());
    assert_eq!(
        issues.len(),
        1,
        "members drift must still produce a DRIFT issue on Cargo.toml axis, got: {issues:?}"
    );
    assert!(issues[0].safe_to_fix, "DRIFT issue must be safe_to_fix");
    assert!(
        issues[0].message.contains("drift"),
        "DRIFT issue message should contain 'drift': {}",
        issues[0].message
    );
}

// -----------------------------------------------------------------------
// Fully-owned Cargo.lock verify
//
// The three-state verify() shape (MISSING / DRIFT / USER-HELD) was
// originally hybrid-only; USER-HELD requires an owned-key + marker pair
// that fully-owned files don't have. This battery covers the
// fully-owned axis on `Cargo.lock`:
//
//   - MISSING (file absent when generation expected) → DRIFT, safe_to_fix
//   - Parse-fail (garbage bytes / cargo half-write) → DRIFT, safe_to_fix
//   - Present + parseable → CLEAN
//
// Anchors the audit finding: previously `verify()` ignored Cargo.lock
// entirely — any mutation was invisible to doctor.
// -----------------------------------------------------------------------

/// Helper: build a fixture where cargo-workspace has active work
/// (`Cargo.toml` present, marker+members correct) so verify() reaches the
/// Cargo.lock arm without short-circuiting on the hybrid arm.
///
/// Returns (tempdir, project, manifest, config, cache) to keep the borrow
/// pattern the other tests use.
fn s7_6_fixture() -> (
    TempDir,
    ProjectName,
    Manifest,
    IntegrationConfig,
    HashMap<String, Vec<String>>,
) {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Repo with a Cargo.toml so `has_active_cargo_work` returns true.
    touch(root, "github/cwalv/mylib/Cargo.toml");

    // Write a clean, marker-decorated root Cargo.toml matching the config
    // so the hybrid Cargo.toml arm of verify() is CLEAN.
    write_file(
        root,
        "Cargo.toml",
        "[workspace]\n# managed by rwv\nmembers = [\"github/cwalv/mylib\"]\n\
             # managed by rwv\nresolver = \"2\"\n",
    );

    let config = IntegrationConfig::default();
    let manifest = make_manifest(vec![("github/cwalv/mylib", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let cache = HashMap::new();

    (tmp, project, manifest, config, cache)
}

/// Given: Cargo.toml is CLEAN (marker + matching members), Cargo.lock
///        is ABSENT.
/// Then:  verify() reports a MISSING finding for Cargo.lock naming the
///        file, the state, and the `rwv doctor --fix` repair verb.
///
/// Regression: pre-fix, doctor exited 0 with no report even when
/// the fully-owned lockfile was gone.
#[test]
fn s7_6_cargo_lock_missing_reports_drift_naming_doctor_fix() {
    let (tmp, project, manifest, config, cache) = s7_6_fixture();
    let root = tmp.path();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // Confirm Cargo.lock is absent (fixture doesn't create it).
    assert!(!root.join("Cargo.lock").exists());

    let issues = CargoWorkspace.verify(&ctx).unwrap();
    assert_eq!(
        issues.len(),
        1,
        "expected exactly one MISSING finding for Cargo.lock, got: {issues:?}"
    );
    let issue = &issues[0];
    assert!(
        issue.safe_to_fix,
        "MISSING Cargo.lock must be safe_to_fix (doctor --fix regenerates)"
    );
    // Message must name the file (house pattern: name the file).
    assert!(
        issue.message.contains("Cargo.lock"),
        "MISSING message must name the file: {}",
        issue.message
    );
    // Message must name the state (house pattern: name the state).
    assert!(
        issue.message.contains("missing"),
        "MISSING message must name the state ('missing'): {}",
        issue.message
    );
    // Message must name the repair verb (house pattern: name the repair).
    assert!(
        issue.message.contains("rwv doctor --fix"),
        "MISSING message must name `rwv doctor --fix`: {}",
        issue.message
    );
}

/// Given: Cargo.lock present but not valid TOML (out-of-band mutation
///        or interrupted cargo write leaves garbage bytes).
/// Then:  verify() reports a DRIFT finding naming Cargo.lock, "drift",
///        and the `rwv doctor --fix` repair verb.
#[test]
fn s7_6_cargo_lock_corrupt_reports_drift_naming_doctor_fix() {
    let (tmp, project, manifest, config, cache) = s7_6_fixture();
    let root = tmp.path();

    // Write garbage bytes — not a valid TOML document.
    write_file(root, "Cargo.lock", "this is not toml \x00 [[[");

    let ctx = make_ctx(root, &project, &manifest, &config, &cache);
    let issues = CargoWorkspace.verify(&ctx).unwrap();
    assert_eq!(
        issues.len(),
        1,
        "expected exactly one DRIFT finding for corrupt Cargo.lock, got: {issues:?}"
    );
    let issue = &issues[0];
    assert!(
        issue.safe_to_fix,
        "corrupt Cargo.lock is safe_to_fix (doctor --fix regenerates)"
    );
    assert!(
        issue.message.contains("Cargo.lock"),
        "DRIFT message must name the file: {}",
        issue.message
    );
    assert!(
        issue.message.contains("drift"),
        "DRIFT message must name the state ('drift'): {}",
        issue.message
    );
    assert!(
        issue.message.contains("rwv doctor --fix"),
        "DRIFT message must name `rwv doctor --fix`: {}",
        issue.message
    );
}

/// Given: Cargo.lock present and parseable as TOML.
/// Then:  verify() reports no Cargo.lock finding (CLEAN).
///
/// This anchors the intentional scope bound: deep content-drift (cargo
/// silently rewrote pinned versions) is NOT detected without running
/// cargo. Present-and-parseable is CLEAN.
#[test]
fn s7_6_cargo_lock_present_and_parseable_is_clean() {
    let (tmp, project, manifest, config, cache) = s7_6_fixture();
    let root = tmp.path();

    // Minimal valid Cargo.lock shape (top-level version + empty package
    // array is enough for the parse-only check).
    write_file(
        root,
        "Cargo.lock",
        "version = 3\n\n[[package]]\nname = \"mylib\"\nversion = \"0.1.0\"\n",
    );

    let ctx = make_ctx(root, &project, &manifest, &config, &cache);
    let issues = CargoWorkspace.verify(&ctx).unwrap();
    assert!(
        issues.is_empty(),
        "present + parseable Cargo.lock must be CLEAN, got: {issues:?}"
    );
}

/// Regression: fully-owned Cargo.lock MUST NOT be reported as USER-HELD
/// even in the pathological case where a file is present without markers.
/// USER-HELD is a hybrid-marker concept and does not apply to fully-owned
/// files.
#[test]
fn s7_6_cargo_lock_never_reports_user_held() {
    let (tmp, project, manifest, config, cache) = s7_6_fixture();
    let root = tmp.path();

    // A "user-authored" Cargo.lock analog: valid TOML, no rwv marker.
    // Fully-owned semantics say this is CLEAN, not USER-HELD.
    write_file(root, "Cargo.lock", "version = 3\n");

    let ctx = make_ctx(root, &project, &manifest, &config, &cache);
    let issues = CargoWorkspace.verify(&ctx).unwrap();

    // No issue at all. If there were an issue, it must NOT be
    // safe_to_fix=false (the USER-HELD signature).
    for issue in &issues {
        assert!(
            issue.safe_to_fix,
            "fully-owned Cargo.lock must never emit a USER-HELD (safe_to_fix=false) issue: {issue:?}"
        );
    }
    assert!(
        issues.is_empty(),
        "present+parseable fully-owned file must be CLEAN, got: {issues:?}"
    );
}

/// Regression: hybrid Cargo.toml USER-HELD detection must survive the
/// verify() split (Cargo.toml first, Cargo.lock second). A Cargo.toml
/// with unmarked members + present Cargo.lock still emits exactly one
/// USER-HELD finding for the hybrid file — the fully-owned arm stays
/// CLEAN when Cargo.lock is present-and-parseable.
#[test]
fn s7_6_hybrid_user_held_unchanged_by_fully_owned_split() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // No "# managed by rwv" marker — user holds the pen on Cargo.toml.
    write_file(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"github/cwalv/mylib\"]\nresolver = \"2\"\n",
    );
    touch(root, "github/cwalv/mylib/Cargo.toml");
    // Cargo.lock is present + parseable so the fully-owned arm is CLEAN.
    write_file(root, "Cargo.lock", "version = 3\n");

    let config =
        IntegrationConfig::from_toml("[members.\"github/cwalv/mylib\"]\ninclude = [\".\"]\n");
    let manifest = make_manifest(vec![("github/cwalv/mylib", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let issues = CargoWorkspace.verify(&ctx).unwrap();
    assert_eq!(
        issues.len(),
        1,
        "expected exactly one USER-HELD issue for Cargo.toml, got: {issues:?}"
    );
    assert!(
        !issues[0].safe_to_fix,
        "USER-HELD hybrid finding must NOT be safe_to_fix, got: {:?}",
        issues[0]
    );
}

// -----------------------------------------------------------------------
// C3 regeneration gap: activate_hook refuses cleanly when the
//      managed file is missing, naming `rwv doctor --fix`.
//
// Empirical evidence from the audit: a repo that acquired its
// Cargo.toml AFTER `rwv add` never had its managed Cargo.toml generated,
// so `activate` blew up in the activate_hook (cargo generate-lockfile
// has no root manifest to lock against) with the confusing "workspace
// may be partially activated" wrap. This battery pins the FALLBACK
// branch of the design's either/or: activate is a context verb
// and must not author — activate_hook precheck bails with a clear
// message pointing to the intent-mode recovery verb.
// -----------------------------------------------------------------------

/// Given: cargo-workspace has active work but the managed Cargo.toml
///        was never generated (the "acquired manifest after add" gap).
/// When:  activate_hook runs.
/// Then:  it bails with a clear error naming `rwv doctor --fix`
///        BEFORE running cargo (which would fail with a confusing wrap).
#[test]
fn s7_7_activate_hook_refuses_when_managed_file_missing() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Repo has a Cargo.toml so has_active_cargo_work=true, but the
    // ROOT (output_dir) Cargo.toml is absent — the C3 gap shape.
    touch(root, "github/cwalv/mylib/Cargo.toml");
    assert!(!root.join("Cargo.toml").exists());

    let config = IntegrationConfig::default();
    let manifest = make_manifest(vec![("github/cwalv/mylib", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let err = CargoWorkspace
        .activate_hook(&ctx)
        .expect_err("activate_hook must refuse when managed file is missing");
    let msg = format!("{err:#}");

    // Names the file.
    assert!(
        msg.contains("Cargo.toml"),
        "error must name the missing managed file: {msg}"
    );
    // Names the recovery verb — the reason we bail early is to give
    // ONE actionable message instead of the "partially activated" wrap.
    assert!(
        msg.contains("rwv doctor --fix"),
        "error must name the recovery verb `rwv doctor --fix`: {msg}"
    );
}

/// Given: managed Cargo.toml is missing.
/// When:  `activate_intent` (the intent-mode write path that
///        `rwv doctor --fix` invokes) runs.
/// Then:  Cargo.toml is authored — the intent path self-heals the C3
///        gap, closing the loop that `verify()` opens.
///
/// This is the DOCTOR-FIX-REPAIRS-IT half of the pair — the activate
/// (context) path refuses cleanly (previous test), and the doctor
/// (intent) path repairs.
#[test]
fn s7_7_activate_intent_regenerates_missing_managed_file() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/cwalv/mylib/Cargo.toml");
    assert!(!root.join("Cargo.toml").exists());

    let config = IntegrationConfig::default();
    let manifest = make_manifest(vec![("github/cwalv/mylib", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // Intent mode (what doctor --fix invokes) DOES author.
    CargoWorkspace.activate(&ctx).unwrap();

    assert!(
        root.join("Cargo.toml").exists(),
        "activate() (intent mode) must regenerate the missing managed file"
    );
    let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(
        content.contains("# managed by rwv"),
        "regenerated Cargo.toml must carry the rwv marker: {content}"
    );

    // And verify() must now be CLEAN on the hybrid arm.
    let post_issues = CargoWorkspace.verify(&ctx).unwrap();
    // Cargo.lock is still absent (activate() doesn't run the hook), so
    // exactly ONE MISSING finding for Cargo.lock — but the Cargo.toml
    // hybrid arm is CLEAN.
    assert_eq!(
        post_issues.len(),
        1,
        "post-regeneration verify() must have exactly one issue \
             (fully-owned Cargo.lock still MISSING; hybrid Cargo.toml CLEAN), \
             got: {post_issues:?}"
    );
    assert!(
        post_issues[0].message.contains("Cargo.lock"),
        "remaining issue must be for Cargo.lock, got: {}",
        post_issues[0].message
    );
}

// -----------------------------------------------------------------------
// Recorded-digest verify: cargo rewriting Cargo.lock as VALID TOML
//      (invisible to the parse check).
//
// rwv cannot recompute lock content (cargo generate-lockfile output
// depends on registry state), so the activation hook stamps a SHA-256 of
// each accepted generation into `.rwv-owned-digests` (output_dir) and
// verify() compares. Report-not-mandate: WARNING severity,
// safe_to_fix=false, both exits named. Pre-upgrade workspaces (no digest
// state) skip the axis silently.
//
// These tests stamp via the same helper the hook calls
// (stamp_owned_digest) — the hook itself needs a real cargo run and is
// covered by the e2e battery in e2e_cargo_test.rs.
// -----------------------------------------------------------------------

use repoweave::owned_state::stamp_owned_digest;

/// The regression test proper.
///
/// Given: Cargo.lock stamped at generation, then rewritten out-of-band
///        as DIFFERENT but VALID TOML (what a cargo invocation does).
/// Then:  verify() reports a WARNING naming the file, the state
///        ("differs from the last rwv-accepted generation"), and BOTH
///        consents, spelled as they are invoked. NOT safe_to_fix — the
///        operator chooses, and each named verb runs in a workweave,
///        which is where this finding is most often read.
#[test]
fn s7_8_cargo_rewrite_valid_toml_reports_warning_with_both_exits() {
    let (tmp, project, manifest, config, cache) = s7_6_fixture();
    let root = tmp.path();

    // The generation rwv accepted (simulating the activation hook's
    // stamp — the hook itself needs real cargo; e2e covers it).
    let accepted = "version = 3\n\n[[package]]\nname = \"mylib\"\nversion = \"0.1.0\"\n";
    write_file(root, "Cargo.lock", accepted);
    stamp_owned_digest(root, "Cargo.lock", accepted.as_bytes()).unwrap();

    // Out-of-band cargo rewrite: still perfectly valid TOML — the parse
    // check CANNOT see this. (A dep version was bumped.)
    let rewritten = "version = 3\n\n[[package]]\nname = \"mylib\"\nversion = \"0.2.0\"\n";
    write_file(root, "Cargo.lock", rewritten);

    let ctx = make_ctx(root, &project, &manifest, &config, &cache);
    let issues = CargoWorkspace.verify(&ctx).unwrap();
    assert_eq!(
        issues.len(),
        1,
        "expected exactly one digest-mismatch finding, got: {issues:?}"
    );
    let issue = &issues[0];
    assert_eq!(
        issue.severity,
        Severity::Warning,
        "report-not-mandate: warning severity keeps doctor exit semantics unchanged"
    );
    assert!(
        !issue.safe_to_fix,
        "digest mismatch must NOT be auto-fixed — the operator chooses an exit: {issue:?}"
    );
    // House pattern: name the file.
    assert!(
        issue.message.contains("Cargo.lock"),
        "must name the file: {}",
        issue.message
    );
    // Name the state.
    assert!(
        issue
            .message
            .contains("differs from the last rwv-accepted generation"),
        "must name the state: {}",
        issue.message
    );
    // Name BOTH consents, spelled as they are invoked. A remedy the
    // operator cannot run in the checkout the finding printed in is a dead
    // end, and `rwv activate` — what this used to name — is refused in a
    // workweave.
    assert!(
        issue.message.contains("rwv materialize --adopt-drifted"),
        "must name the adopt exit: {}",
        issue.message
    );
    assert!(
        issue
            .message
            .contains("rwv materialize --regenerate-drifted"),
        "must name the regenerate exit: {}",
        issue.message
    );
    assert!(
        issue.message.contains("restore the file"),
        "must name the restore exit: {}",
        issue.message
    );
    assert!(
        !issue.message.contains("rwv activate"),
        "naming a verb the workweave refuses is the defect this fixed: {}",
        issue.message
    );
}

/// Given: digest mismatch (previous test's shape).
/// When:  activation re-runs and re-stamps (the ACCEPT exit — simulated
///        via the same stamp helper the hook calls).
/// Then:  verify() is clean.
#[test]
fn s7_8_reactivation_restamp_returns_clean() {
    let (tmp, project, manifest, config, cache) = s7_6_fixture();
    let root = tmp.path();

    write_file(root, "Cargo.lock", "version = 3\n");
    stamp_owned_digest(root, "Cargo.lock", b"version = 3\n").unwrap();

    // Out-of-band rewrite → mismatch.
    let rewritten = "version = 4\n";
    write_file(root, "Cargo.lock", rewritten);
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);
    assert_eq!(
        CargoWorkspace.verify(&ctx).unwrap().len(),
        1,
        "precondition: mismatch must be reported"
    );

    // ACCEPT exit: re-activation re-runs the hook, which re-stamps the
    // now-current content.
    stamp_owned_digest(root, "Cargo.lock", rewritten.as_bytes()).unwrap();

    let issues = CargoWorkspace.verify(&ctx).unwrap();
    assert!(
        issues.is_empty(),
        "re-stamp must accept the new content (clean), got: {issues:?}"
    );
}

/// Given: digest mismatch.
/// When:  the operator takes the RESTORE exit (puts the recorded content
///        back, e.g. via VCS).
/// Then:  verify() is clean — without any re-stamp.
#[test]
fn s7_8_restore_exit_returns_clean_without_restamp() {
    let (tmp, project, manifest, config, cache) = s7_6_fixture();
    let root = tmp.path();

    let accepted = "version = 3\n";
    write_file(root, "Cargo.lock", accepted);
    stamp_owned_digest(root, "Cargo.lock", accepted.as_bytes()).unwrap();
    write_file(root, "Cargo.lock", "version = 4\n");

    let ctx = make_ctx(root, &project, &manifest, &config, &cache);
    assert_eq!(
        CargoWorkspace.verify(&ctx).unwrap().len(),
        1,
        "precondition: mismatch must be reported"
    );

    // RESTORE exit: put the accepted bytes back.
    write_file(root, "Cargo.lock", accepted);

    let issues = CargoWorkspace.verify(&ctx).unwrap();
    assert!(
        issues.is_empty(),
        "restoring the recorded content must be clean without re-stamp, got: {issues:?}"
    );
}

/// Backward compat: a pre-upgrade workspace has a generated Cargo.lock
/// but NO digest state. The axis is skipped silently — present +
/// parseable stays CLEAN, exactly the pre-digest behavior.
#[test]
fn s7_8_no_digest_state_skips_axis_silently() {
    let (tmp, project, manifest, config, cache) = s7_6_fixture();
    let root = tmp.path();

    // Any valid-TOML content; no .rwv-owned-digests anywhere.
    write_file(root, "Cargo.lock", "version = 3\n");
    assert!(!root.join(".rwv-owned-digests").exists());

    let ctx = make_ctx(root, &project, &manifest, &config, &cache);
    let issues = CargoWorkspace.verify(&ctx).unwrap();
    assert!(
        issues.is_empty(),
        "no digest state must skip the axis silently (backward compat), got: {issues:?}"
    );
}

/// Digest state must survive doctor --fix of OTHER issues untouched.
///
/// Given: stamped Cargo.lock (digest matches) + a Cargo.toml DRIFT
///        (stale members under markers — a safe_to_fix issue).
/// When:  the doctor --fix write path repairs the Cargo.toml drift
///        (unit-level: activate(), which authors the hybrid file but
///        does not run hooks).
/// Then:  `.rwv-owned-digests` is byte-identical, and verify() is fully
///        clean (toml repaired; lock digest still matches).
#[test]
fn s7_8_digest_state_survives_fix_of_other_issues() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/cwalv/mylib/Cargo.toml");

    // Cargo.toml with markers but STALE members (drift: config expects
    // mylib, file names a repo that no longer exists).
    write_file(
        root,
        "Cargo.toml",
        "[workspace]\n# managed by rwv\nmembers = [\"github/cwalv/oldlib\"]\n\
             # managed by rwv\nresolver = \"2\"\n",
    );

    // Stamped, matching Cargo.lock.
    let lock = "version = 3\n";
    write_file(root, "Cargo.lock", lock);
    stamp_owned_digest(root, "Cargo.lock", lock.as_bytes()).unwrap();
    let digest_before = std::fs::read_to_string(root.join(".rwv-owned-digests")).unwrap();

    let config = IntegrationConfig::default();
    let manifest = make_manifest(vec![("github/cwalv/mylib", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // Precondition: exactly one issue, and it is the Cargo.toml drift
    // (safe_to_fix) — the lock axis is clean.
    let pre = CargoWorkspace.verify(&ctx).unwrap();
    assert_eq!(pre.len(), 1, "precondition: only the toml drift: {pre:?}");
    assert!(pre[0].safe_to_fix && !pre[0].message.contains("Cargo.lock"));

    // doctor --fix write path for the OTHER issue.
    CargoWorkspace.activate(&ctx).unwrap();

    // Digest state untouched.
    let digest_after = std::fs::read_to_string(root.join(".rwv-owned-digests")).unwrap();
    assert_eq!(
        digest_before, digest_after,
        "fixing an unrelated issue must not touch the digest state"
    );

    // And everything is now clean.
    let post = CargoWorkspace.verify(&ctx).unwrap();
    assert!(
        post.is_empty(),
        "toml repaired + lock digest still matching must be clean, got: {post:?}"
    );
}

/// Adversarial: parse-fail beats digest-compare. If the out-of-band
/// mutation left the lock UNPARSEABLE, the finding is the parse-fail
/// DRIFT (safe_to_fix=true — regeneration is the only sane exit), not a
/// digest mismatch on garbage bytes.
#[test]
fn s7_8_unparseable_mutation_reports_parse_fail_not_digest_mismatch() {
    let (tmp, project, manifest, config, cache) = s7_6_fixture();
    let root = tmp.path();

    write_file(root, "Cargo.lock", "version = 3\n");
    stamp_owned_digest(root, "Cargo.lock", b"version = 3\n").unwrap();
    // Mutation produced garbage, not valid TOML.
    write_file(root, "Cargo.lock", "half a write [[[");

    let ctx = make_ctx(root, &project, &manifest, &config, &cache);
    let issues = CargoWorkspace.verify(&ctx).unwrap();
    assert_eq!(issues.len(), 1, "exactly one finding, got: {issues:?}");
    assert!(
        issues[0].safe_to_fix,
        "parse-fail must win (regeneration is the exit): {issues:?}"
    );
    assert!(
        issues[0].message.contains("rwv doctor --fix"),
        "parse-fail names the regeneration verb: {}",
        issues[0].message
    );
}
