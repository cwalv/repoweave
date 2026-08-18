// ===========================================================================
// static-files
// ===========================================================================

use super::*;

#[test]
fn default_disabled() {
    let integration = StaticFiles;
    assert!(!integration.default_enabled());
}

#[test]
fn name_is_static_files() {
    let integration = StaticFiles;
    assert_eq!(integration.name(), "static-files");
}

#[test]
fn generated_files_returns_configured_files() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml(
        "enabled = true\nfiles = [\"turbo.json\", \".eslintrc.json\", \".prettierrc\"]",
    );
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = StaticFiles;
    let files = integration.generated_files(&ctx);
    assert_eq!(
        files,
        vec![
            SurfacedFile::written_at_source("turbo.json"),
            SurfacedFile::written_at_source(".eslintrc.json"),
            SurfacedFile::written_at_source(".prettierrc")
        ],
        "an operator's committed file is surfaced, never written through \
             its link"
    );
}

#[test]
fn generated_files_empty_when_no_files_configured() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml("enabled = true");
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = StaticFiles;
    let files = integration.generated_files(&ctx);
    assert!(files.is_empty());
}

#[test]
fn activate_succeeds_when_files_exist() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Create the declared files in the project directory (output_dir)
    write_file(root, "turbo.json", r#"{"pipeline": {}}"#);
    write_file(root, ".eslintrc.json", "{}");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml(
        "enabled = true\nfiles = [\"turbo.json\", \".eslintrc.json\"]",
    );
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = StaticFiles;
    let result = integration.activate(&ctx);
    assert!(result.is_ok());
}

#[test]
fn activate_succeeds_even_when_files_missing() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Don't create the files — activate should still succeed (just warn)
    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml("enabled = true\nfiles = [\"turbo.json\"]");
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = StaticFiles;
    let result = integration.activate(&ctx);
    assert!(
        result.is_ok(),
        "activate should succeed even with missing files"
    );
}

#[test]
fn check_warns_on_missing_files() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Create one of two declared files
    write_file(root, "turbo.json", "{}");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml(
        "enabled = true\nfiles = [\"turbo.json\", \".eslintrc.json\"]",
    );
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = StaticFiles;
    let issues = integration.check(&ctx).unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].severity, Severity::Warning);
    assert!(issues[0].message.contains(".eslintrc.json"));
    assert_eq!(issues[0].integration, "static-files");
}

#[test]
fn check_no_issues_when_all_files_present() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(root, "turbo.json", "{}");
    write_file(root, ".prettierrc", "{}");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config =
        IntegrationConfig::from_toml("enabled = true\nfiles = [\"turbo.json\", \".prettierrc\"]");
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = StaticFiles;
    let issues = integration.check(&ctx).unwrap();
    assert!(issues.is_empty());
}

#[test]
fn check_no_issues_when_no_files_configured() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml("enabled = true");
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = StaticFiles;
    let issues = integration.check(&ctx).unwrap();
    assert!(issues.is_empty());
}

#[test]
fn deactivate_succeeds() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let integration = StaticFiles;
    let result = integration.deactivate(root);
    assert!(result.is_ok());
}

#[test]
fn activate_hook_is_noop() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml("enabled = true\nfiles = [\"turbo.json\"]");
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = StaticFiles;
    let result = integration.activate_hook(&ctx);
    assert!(
        result.is_ok(),
        "static-files activate hook should be a no-op"
    );
}

// ----- collision with workweave.link -----------

/// Regression: when the same name is declared in both
/// `static-files.files` and `workweave.link`, `activate()` MUST bail with a
/// hard error rather than silently letting the framework's predicate
/// tiebreak. The error message must name both integrations so the operator
/// can act on it without re-reading the docs.
#[test]
fn activate_fails_when_name_collides_with_workweave_link() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // The static file exists — collision detection runs before existence
    // checks, so we'd rather not give activate() a way to fail for an
    // unrelated reason.
    write_file(root, ".beads", "");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml("enabled = true\nfiles = [\".beads\"]");
    let cache = HashMap::new();
    let workweave = WorkweaveConfig {
        link: vec![".beads".to_string()],
        copy: vec![],
    };
    let ctx = make_ctx_with_workweave(root, &project, &manifest, &config, &cache, &workweave);

    let integration = StaticFiles;
    let err = integration.activate(&ctx).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains(".beads") && msg.contains("static-files") && msg.contains("workweave"),
        "activate error should name the colliding entry and both integrations; got: {msg}"
    );
}

/// Regression: `check()` MUST surface the collision as
/// `Severity::Error` so `rwv doctor` fails loudly pre-activate (the
/// signal that motivates the framework predicate).
#[test]
fn check_emits_error_for_workweave_link_collision() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    write_file(root, ".beads", "");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml("enabled = true\nfiles = [\".beads\"]");
    let cache = HashMap::new();
    let workweave = WorkweaveConfig {
        link: vec![".beads".to_string()],
        copy: vec![],
    };
    let ctx = make_ctx_with_workweave(root, &project, &manifest, &config, &cache, &workweave);

    let integration = StaticFiles;
    let issues = integration.check(&ctx).unwrap();
    let collisions: Vec<_> = issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .collect();
    assert_eq!(
        collisions.len(),
        1,
        "expected exactly one error-level collision issue, got: {issues:?}"
    );
    let issue = collisions[0];
    assert_eq!(issue.integration, "static-files");
    assert!(
        issue.message.contains(".beads")
            && issue.message.contains("workweave.link")
            && issue.message.contains("static-files.files"),
        "issue should name both integrations and the colliding entry; got: {}",
        issue.message
    );
}

/// `check()` emits one Severity::Error per colliding name (not one
/// aggregated message).
#[test]
fn check_emits_one_error_per_collision() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    write_file(root, ".beads", "");
    write_file(root, ".secrets", "");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml(
        "enabled = true\nfiles = [\".beads\", \".secrets\", \"turbo.json\"]",
    );
    let cache = HashMap::new();
    // Two collisions (.beads, .secrets) and one non-collision (turbo.json).
    let workweave = WorkweaveConfig {
        link: vec![".beads".to_string(), ".secrets".to_string()],
        copy: vec![],
    };
    let ctx = make_ctx_with_workweave(root, &project, &manifest, &config, &cache, &workweave);

    let integration = StaticFiles;
    let issues = integration.check(&ctx).unwrap();
    let collisions: Vec<_> = issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .collect();
    assert_eq!(
        collisions.len(),
        2,
        "expected one Severity::Error per collision, got: {issues:?}"
    );
    // Both colliding names should appear across the issue messages.
    let combined: String = collisions.iter().map(|i| i.message.clone()).collect();
    assert!(combined.contains(".beads"));
    assert!(combined.contains(".secrets"));
}

/// No workweave.link at all -> no collision Issues.
#[test]
fn check_no_collision_when_workweave_link_empty() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    write_file(root, ".beads", "");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml("enabled = true\nfiles = [\".beads\"]");
    let cache = HashMap::new();
    let workweave = WorkweaveConfig {
        link: vec![],
        copy: vec![],
    };
    let ctx = make_ctx_with_workweave(root, &project, &manifest, &config, &cache, &workweave);

    let integration = StaticFiles;
    let issues = integration.check(&ctx).unwrap();
    assert!(
        issues.iter().all(|i| i.severity != Severity::Error),
        "no Severity::Error expected when workweave.link is empty, got: {issues:?}"
    );
}

/// Disjoint names -> no collision Issues.
#[test]
fn check_no_collision_when_names_disjoint() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    write_file(root, ".beads", "");
    write_file(root, "turbo.json", "{}");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml("enabled = true\nfiles = [\"turbo.json\"]");
    let cache = HashMap::new();
    let workweave = WorkweaveConfig {
        link: vec![".beads".to_string()],
        copy: vec![],
    };
    let ctx = make_ctx_with_workweave(root, &project, &manifest, &config, &cache, &workweave);

    let integration = StaticFiles;
    let issues = integration.check(&ctx).unwrap();
    assert!(
        issues.iter().all(|i| i.severity != Severity::Error),
        "no Severity::Error expected when names disjoint, got: {issues:?}"
    );
}

/// `ctx.workweave == None` -> no collision Issues (projects without a
/// `workweave:` section in rwv.toml).
#[test]
fn check_no_collision_when_workweave_absent() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    write_file(root, ".beads", "");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml("enabled = true\nfiles = [\".beads\"]");
    let cache = HashMap::new();
    // make_ctx -> workweave: None
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = StaticFiles;
    let issues = integration.check(&ctx).unwrap();
    assert!(
        issues.iter().all(|i| i.severity != Severity::Error),
        "no Severity::Error expected when ctx.workweave is None, got: {issues:?}"
    );
}

// -----------------------------------------------------------------------
// static-files — RED scenarios
// -----------------------------------------------------------------------
//
// is already covered above by
// `activate_fails_when_name_collides_with_workweave_link` /
// `check_emits_error_for_workweave_link_collision` (the C13 hard-error
// path). This realizes the remaining plan scenarios:
//
// — deactivate strips only static-files-owned symlinks;
// foreign symlinks and user files survive. The integration's
// `deactivate(root)` is a no-op — symlink removal is the framework's job,
// so the subject is `repoweave::activate::unsurface_names`.
//
// — missing declared file skipped with warning (already
// covered by `check_warns_on_missing_files` and
// `activate_succeeds_even_when_files_missing` above; we leave them in
// place rather than duplicate).

/// the framework's symlink reaping is owner-scoped on
/// BOTH legs of its conjunction: a declared name is unlinked only when the
/// name is one rwv surfaces AND the link's target is the shape activation
/// would have written (`projects/<project>/<that name>`).
///
/// The defect this catches is the target-shape leg going blind: a
/// `workweave.link` entry is an absolute symlink into the source weave, so
/// a name declared by both it and `static-files.files` — what an operator
/// migrating a name between the two holds for one activation — would be
/// unlinked out from under the operator by a name-only predicate.
///
/// `tests/integration_framework_test.rs::owner_scoped_removal_preserves_unowned_symlinks`
/// drives the other leg (an owner-shaped target at a name no integration
/// claims) and is blind to this one.
#[test]
fn s6_static_files_2_deactivate_owner_scoped_symlink_removal() {
    use repoweave::symlink::{create as symlink_to, LinkTarget};

    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    let source_weave = common::tempdir().unwrap();

    // Surfaced out of the project the way activation writes it: a relative
    // `projects/<project>/<name>` target.
    write_file(root, "projects/test-project/.prettierrc", "{}\n");
    symlink_to(
        Path::new("projects/test-project/.prettierrc"),
        &root.join(".prettierrc"),
        LinkTarget::File,
    )
    .unwrap();

    // A workweave.link: an absolute link at a name the removal set also
    // names. Only the target shape distinguishes it.
    let foreign_target = source_weave.path().join("turbo.json");
    std::fs::write(&foreign_target, "{\"pipeline\": {}}\n").unwrap();
    symlink_to(&foreign_target, &root.join("turbo.json"), LinkTarget::File).unwrap();

    // A declared name the operator wrote by hand — not a link at all.
    let hand_written = "{\"extends\": \"../base\"}\n";
    write_file(root, ".eslintrc.json", hand_written);

    let names = vec![
        ".prettierrc".to_string(),
        "turbo.json".to_string(),
        ".eslintrc.json".to_string(),
    ];
    repoweave::activate::unsurface_names(root, &names).unwrap();

    assert!(
        root.join(".prettierrc").symlink_metadata().is_err(),
        "the surfaced static-files symlink must be removed"
    );

    assert_eq!(
        std::fs::read_link(root.join("turbo.json")).ok(),
        Some(foreign_target),
        "a declared name whose link points outside projects/<project>/ is \
             not rwv's surfacing and must survive"
    );

    assert_eq!(
        std::fs::read_to_string(root.join(".eslintrc.json")).ok(),
        Some(hand_written.to_string()),
        "a declared name the operator wrote as a real file must be \
             byte-identical"
    );
}
