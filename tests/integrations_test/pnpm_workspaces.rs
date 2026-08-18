// ===========================================================================
// pnpm-workspaces
// ===========================================================================

use super::*;

#[test]
fn auto_detects_repos_with_package_json() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/package.json");
    touch(root, "github/acme/web/package.json");
    touch(root, "github/acme/docs/README.md");

    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/web", Role::Owned),
        ("github/acme/docs", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = PnpmWorkspaces;
    integration.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
    assert!(content.contains("github/acme/server"));
    assert!(content.contains("github/acme/web"));
    assert!(!content.contains("github/acme/docs"));
}

#[test]
fn generates_pnpm_workspace_yaml_with_packages_list() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/chatly/protocol/package.json");
    touch(root, "github/chatly/server/package.json");
    touch(root, "github/chatly/web/package.json");

    let manifest = make_manifest(vec![
        ("github/chatly/protocol", Role::Owned),
        ("github/chatly/server", Role::Owned),
        ("github/chatly/web", Role::Fork),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = PnpmWorkspaces;
    integration.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
    // Activate now writes the `# managed by repoweave` marker above the
    // packages block — that is the ownership sentinel for the hybrid contract.
    assert!(content.contains("# managed by repoweave"));
    assert!(content.contains("packages:"));
    assert!(content.contains("  - github/chatly/protocol"));
    assert!(content.contains("  - github/chatly/server"));
    assert!(content.contains("  - github/chatly/web"));
}

#[test]
fn excludes_reference_repos() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/package.json");
    touch(root, "github/acme/reference-lib/package.json");

    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/reference-lib", Role::Reference),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = PnpmWorkspaces;
    integration.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
    assert!(content.contains("github/acme/server"));
    assert!(!content.contains("reference-lib"));
}

#[test]
fn deactivation_deletes_fully_rwv_authored_file() {
    // When the file was authored entirely by rwv (only a marker + packages
    // block, nothing user-authored), deactivation should delete it.
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "pnpm-workspace.yaml",
        "# managed by repoweave\npackages:\n  - foo\n",
    );
    assert!(root.join("pnpm-workspace.yaml").exists());

    let integration = PnpmWorkspaces;
    integration.deactivate(root).unwrap();
    assert!(!root.join("pnpm-workspace.yaml").exists());
}

#[test]
fn deactivation_leaves_hand_owned_file_alone() {
    // A file without the marker was not authored by rwv — leave it alone.
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(root, "pnpm-workspace.yaml", "packages:\n  - foo\n");
    assert!(root.join("pnpm-workspace.yaml").exists());

    let integration = PnpmWorkspaces;
    integration.deactivate(root).unwrap();
    // No marker → user took the pen → file must survive.
    assert!(root.join("pnpm-workspace.yaml").exists());
}

#[cfg(unix)]
#[test]
fn check_warns_when_pnpm_not_on_path() {
    let absent = doctor_json_on_tool_only_path(
        "pnpm-workspaces",
        "github/acme/server/package.json",
        "{\"name\": \"server\", \"version\": \"0.1.0\"}\n",
        &[],
    );
    let present = doctor_json_on_tool_only_path(
        "pnpm-workspaces",
        "github/acme/server/package.json",
        "{\"name\": \"server\", \"version\": \"0.1.0\"}\n",
        &["pnpm"],
    );

    tool_missing_fires_then_clears(&absent, &present, "pnpm-workspaces", "pnpm");
}

// -----------------------------------------------------------------------
// pnpm-workspaces — RED scenarios (turned green by C10)
// -----------------------------------------------------------------------
//
// Synthetic scenarios: no on-disk pnpm-workspace.yaml
// exists in any weave; the four scenarios use spec idioms (`catalog:`,
// `overrides:`, `peerDependencyRules:`, `# comments`). default_enabled is
// false today; tests force it on via `enabled: true` in the config.
//
// The pnpm integration uses `default_enabled=false`, but we still call
// activate/deactivate directly (the integration's own gating logic ignores
// default_enabled when invoked through trait methods).

/// Activate preserves a user catalog and comment.
#[test]
fn s6_pnpm_1_activate_preserves_catalog_and_comments() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    touch(root, "github/acme/server/package.json");

    // Pre-existing YAML with catalog (user foreign content) + rwv marker on
    // packages (previously-activated state). The catalog and rationale comment
    // must survive activate byte-stable; packages is owned and gets updated.
    write_file(
        root,
        "pnpm-workspace.yaml",
        r#"# shared dependency versions
catalog:
  react: ^18.2.0
  react-dom: ^18.2.0

# managed by repoweave
packages:
  - tools/*
"#,
    );

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml("enabled = true");
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let path = root.join("pnpm-workspace.yaml");
    contract::assert_activate_preserves_foreign(
        &path,
        || {
            PnpmWorkspaces.activate(&ctx).unwrap();
        },
        &[contract::substr_probe(
            "server in packages",
            "github/acme/server",
        )],
        &contract::substr_probe("yaml marker", "managed by repoweave"),
        &[
            "# shared dependency versions",
            "catalog:",
            "react: ^18.2.0",
            "react-dom: ^18.2.0",
        ],
    );
}

/// Deactivate strips `packages:` but keeps `overrides:`.
/// Regression vs current unconditional remove_file at pnpm_workspaces.rs:33-35.
#[test]
fn s6_pnpm_2_deactivate_strips_packages_keeps_overrides() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "pnpm-workspace.yaml",
        r#"overrides:
  lodash@<4.17.21: '>=4.17.21'

# managed by repoweave
packages:
  - github/acme/server
"#,
    );

    let path = root.join("pnpm-workspace.yaml");
    contract::assert_deactivate_strips_keeps(
        &path,
        || {
            PnpmWorkspaces.deactivate(root).unwrap();
        },
        &[contract::substr_probe("server entry", "github/acme/server")],
        &contract::substr_probe("yaml marker", "managed by repoweave"),
        &["overrides:", "lodash@<4.17.21: '>=4.17.21'"],
    );
}

/// Deactivate deletes a fully-rwv-authored file (no foreign
/// content). delete-if-empty kicks in.
///
/// Currently GREEN incidentally — current pnpm deactivate is an
/// unconditional `remove_file`, which happens to satisfy this scenario.
/// Keep ungated as a regression guard against the C10 port.
#[test]
fn s6_pnpm_3_deactivate_deletes_purely_rwv_file() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "pnpm-workspace.yaml",
        r#"# managed by repoweave
packages:
  - github/acme/server
  - github/acme/web
"#,
    );

    let path = root.join("pnpm-workspace.yaml");
    contract::assert_deactivate_deletes_when_only_owned(&path, || {
        PnpmWorkspaces.deactivate(root).unwrap();
    });
}

/// Activate is comment-safe & idempotent. peerDependencyRules
/// with an inline comment survives byte-for-byte, even when activate runs
/// twice with a member added in between.
#[test]
fn s6_pnpm_4_activate_idempotent_comments_preserved() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    touch(root, "github/acme/server/package.json");

    write_file(
        root,
        "pnpm-workspace.yaml",
        r#"peerDependencyRules:
  allowedVersions:
    react: '18'  # pin during migration
"#,
    );

    let manifest_one = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml("enabled = true");
    let cache = HashMap::new();

    // First activate: just server.
    let ctx_one = make_ctx(root, &project, &manifest_one, &config, &cache);
    PnpmWorkspaces.activate(&ctx_one).unwrap();
    let after_first = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();

    // Second activate: server + web.
    touch(root, "github/acme/web/package.json");
    let manifest_two = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/web", Role::Owned),
    ]);
    let ctx_two = make_ctx(root, &project, &manifest_two, &config, &cache);
    PnpmWorkspaces.activate(&ctx_two).unwrap();
    let after_second = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();

    // Inline comment + peerDependencyRules survive both runs.
    assert!(
        after_first.contains("# pin during migration"),
        "inline comment must survive first activate; got:\n{after_first}"
    );
    assert!(
        after_second.contains("# pin during migration"),
        "inline comment must survive second activate; got:\n{after_second}"
    );
    assert!(after_second.contains("peerDependencyRules:"));
    assert!(after_second.contains("github/acme/server"));
    assert!(after_second.contains("github/acme/web"));

    // No marker duplication: exactly one `# managed by repoweave` line.
    let marker_count = after_second
        .lines()
        .filter(|l| l.trim() == "# managed by repoweave")
        .count();
    assert_eq!(
        marker_count, 1,
        "marker must appear exactly once; got:\n{after_second}"
    );

    // No duplicated packages: blocks. Count column-0 `packages:` keys.
    let packages_count = after_second
        .lines()
        .filter(|l| l.starts_with("packages:"))
        .count();
    assert_eq!(
        packages_count, 1,
        "exactly one packages: block; got:\n{after_second}"
    );
}

// -----------------------------------------------------------------------
// Multi-package repo expansion (pnpm uses pnpm-workspace.yaml, not
// package.json workspaces — mirror of npm expansion tests but reading
// from `pnpm-workspace.yaml`'s `packages:` key in the member repo)
// -----------------------------------------------------------------------

/// A member repo with its own `pnpm-workspace.yaml` declaring sub-package
/// globs (array form) gets expanded into prefixed entries; the repo root
/// itself is NOT emitted as an entry.
#[test]
fn multi_package_repo_expands_to_prefixed_globs() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // A multi-package repo: root package.json present (triggers detection)
    // and a pnpm-workspace.yaml declaring its own sub-packages.
    touch(root, "github/acme/mono/package.json");
    write_file(
        root,
        "github/acme/mono/pnpm-workspace.yaml",
        "packages:\n  - packages/*\n  - ./clients/ts\n",
    );
    // A plain single-package repo alongside it.
    touch(root, "github/acme/server/package.json");

    let manifest = make_manifest(vec![
        ("github/acme/mono", Role::Owned),
        ("github/acme/server", Role::Owned),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = PnpmWorkspaces;
    integration.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
    // Prefixed globs from the member repo's pnpm-workspace.yaml.
    assert!(
        content.contains("github/acme/mono/packages/*"),
        "expected prefixed glob; got:\n{content}"
    );
    // Leading './' in member globs is stripped during prefixing.
    assert!(
        content.contains("github/acme/mono/clients/ts"),
        "expected ./ stripped; got:\n{content}"
    );
    // The multi-package repo root itself is NOT listed.
    assert!(
        !content.contains("  - github/acme/mono\n"),
        "repo root must not appear as bare entry; got:\n{content}"
    );
    // Single-package repo keeps the bare path entry.
    assert!(
        content.contains("github/acme/server"),
        "single-package repo must appear; got:\n{content}"
    );
}

/// A member repo with its own `pnpm-workspace.yaml` but an empty
/// `packages:` list is treated as a single-package repo (bare path entry).
#[test]
fn multi_package_repo_empty_packages_list_keeps_bare_path() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/mono/package.json");
    write_file(
        root,
        "github/acme/mono/pnpm-workspace.yaml",
        "packages: []\ncatalog:\n  react: ^18\n",
    );

    let manifest = make_manifest(vec![("github/acme/mono", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = PnpmWorkspaces;
    integration.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
    // Empty packages list → falls back to bare repo path.
    assert!(
        content.contains("github/acme/mono"),
        "empty packages list must yield bare entry; got:\n{content}"
    );
}

/// A member repo without any `pnpm-workspace.yaml` keeps the single
/// `<repo-path>` entry (existing behavior, no regression).
#[test]
fn single_package_repo_no_pnpm_workspace_yaml_keeps_bare_path() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/package.json");
    // No pnpm-workspace.yaml in this repo.

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = PnpmWorkspaces;
    integration.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
    assert!(
        content.contains("github/acme/server"),
        "single-package repo must appear as bare entry; got:\n{content}"
    );
    // And the root of that repo must NOT have been globbed into sub-entries.
    assert!(
        !content.contains("github/acme/server/"),
        "single-package repo must not produce prefixed sub-entries; got:\n{content}"
    );
}

/// This module asserts on `pnpm-workspace.yaml` by searching the whole
/// file for a member path, and the whole file is not the owned region:
/// `catalog:` and every comment are user content rwv carries through. So
/// a path sitting in either is enough to satisfy such a search without
/// being a workspace member at all.
///
/// Latent exposure, not a live defect — no sibling fixture above puts a
/// member-shaped path anywhere but the list, so every one of them is
/// correct today. This is the fixture that tells the two apart, and it
/// asserts through `verify()`, which compares the on-disk `packages:`
/// sequence against the manifest rather than searching text.
#[test]
fn a_decoy_path_in_a_comment_or_catalog_is_not_a_workspace_member() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "pnpm-workspace.yaml",
        "# github/acme/decoy moved out of the weave; note kept on purpose\n\
             catalog:\n  github/acme/decoy: ^1.0.0\n",
    );
    touch(root, "github/acme/server/package.json");

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml("enabled = true");
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    PnpmWorkspaces.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
    assert!(
        content.contains("github/acme/decoy"),
        "fixture is inert unless the decoy survives activate; got:\n{content}"
    );

    let issues = PnpmWorkspaces.verify(&ctx).unwrap();
    assert!(
        issues.is_empty(),
        "the decoy is user content, so the authored packages: list is exactly \
             the manifest's members and verify has nothing to report; got: {issues:?}"
    );
}
