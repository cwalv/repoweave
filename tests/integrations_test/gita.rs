// ===========================================================================
// gita
// ===========================================================================

use super::*;

#[test]
fn auto_detects_all_repos() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // gita uses all repos, not just those with a specific manifest file
    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/web", Role::Fork),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = Gita;
    integration.activate(&ctx).unwrap();

    let repos_csv = std::fs::read_to_string(root.join("gita/repos.csv")).unwrap();
    assert!(repos_csv.contains("server"));
    assert!(repos_csv.contains("web"));
}

#[test]
fn generates_repos_csv_with_correct_format() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![
        ("github/chatly/server", Role::Owned),
        ("github/chatly/web", Role::Owned),
        ("github/chatly/protocol", Role::Fork),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = Gita;
    integration.activate(&ctx).unwrap();

    let repos_csv = std::fs::read_to_string(root.join("gita/repos.csv")).unwrap();
    assert!(repos_csv.starts_with("path,name,flags\n"));

    let lines: Vec<&str> = repos_csv.lines().collect();
    // Header + 3 repos
    assert_eq!(lines.len(), 4);

    // Should be sorted by name (basename)
    assert!(lines[1].contains(",protocol,"));
    assert!(lines[2].contains(",server,"));
    assert!(lines[3].contains(",web,"));

    // Paths should be absolute
    let abs_prefix = root.to_string_lossy();
    assert!(lines[1].starts_with(&*abs_prefix));
}

#[test]
fn generates_groups_csv_by_role() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![
        ("github/chatly/server", Role::Owned),
        ("github/chatly/web", Role::Owned),
        ("github/chatly/protocol", Role::Fork),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = Gita;
    integration.activate(&ctx).unwrap();

    let groups_csv = std::fs::read_to_string(root.join("gita/groups.csv")).unwrap();
    assert!(groups_csv.starts_with("group,repos\n"));
    assert!(groups_csv.contains("fork,protocol\n"));
    assert!(groups_csv.contains("owned,server web\n"));
}

#[test]
fn excludes_reference_repos() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/reference-lib", Role::Reference),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = Gita;
    integration.activate(&ctx).unwrap();

    let repos_csv = std::fs::read_to_string(root.join("gita/repos.csv")).unwrap();
    assert!(repos_csv.contains("server"));
    assert!(!repos_csv.contains("reference-lib"));

    let groups_csv = std::fs::read_to_string(root.join("gita/groups.csv")).unwrap();
    assert!(!groups_csv.contains("reference"));
}

#[test]
fn deactivation_removes_gita_directory() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    std::fs::create_dir_all(root.join("gita")).unwrap();
    write_file(root, "gita/repos.csv", "path,name,flags\n");
    write_file(root, "gita/groups.csv", "group,repos\n");
    assert!(root.join("gita").exists());

    let integration = Gita;
    integration.deactivate(root).unwrap();
    assert!(!root.join("gita").exists());
}

#[test]
fn repos_csv_paths_use_workspace_root_not_output_dir() {
    let workspace_tmp = common::tempdir().unwrap();
    let workspace_root = workspace_tmp.path();
    let weave_tmp = common::tempdir().unwrap();
    let output_dir = weave_tmp.path();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = IntegrationContext {
        output_dir,
        workspace_root,
        container_kind: ContainerKind::Primary,
        project: &project,
        repos: manifest
            .iter_entries()
            .map(|(rp, e)| (rp.clone(), e.clone()))
            .collect(),
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
        detection_cache: &cache,
        workweave: None,
    };

    let integration = Gita;
    integration.activate(&ctx).unwrap();

    let repos_csv = std::fs::read_to_string(output_dir.join("gita/repos.csv")).unwrap();
    let ws_prefix = workspace_root.to_string_lossy();
    let out_prefix = output_dir.to_string_lossy();
    // Repo paths must point to workspace_root (where repos live), not output_dir
    assert!(
        repos_csv.contains(&*ws_prefix),
        "repos.csv should contain workspace_root path: {}",
        repos_csv
    );
    // output_dir and workspace_root are different TempDirs, so output_dir
    // should NOT appear in the path column.
    let data_lines: Vec<&str> = repos_csv.lines().skip(1).collect();
    for line in &data_lines {
        let path_field = line.split(',').next().unwrap();
        assert!(
            !path_field.starts_with(&*out_prefix),
            "repo path should not start with output_dir: {}",
            line
        );
    }
}

#[cfg(unix)]
#[test]
fn check_warns_when_gita_not_on_path() {
    let absent = doctor_json_on_tool_only_path(
        "gita",
        "github/acme/server/package.json",
        "{\"name\": \"server\", \"version\": \"0.1.0\"}\n",
        &[],
    );
    let present = doctor_json_on_tool_only_path(
        "gita",
        "github/acme/server/package.json",
        "{\"name\": \"server\", \"version\": \"0.1.0\"}\n",
        &["gita"],
    );

    tool_missing_fires_then_clears(&absent, &present, "gita", "gita");
}

/// A repo path containing a comma must be emitted as a properly-quoted CSV
/// field and must round-trip through csv::Reader without corruption.
/// Pre-fix, the concat-based writer produced a malformed row.
#[test]
fn csv_escaping_roundtrips_path_with_comma() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Use a repo path whose basename contains a comma. The manifest helper
    // only sets the `url` from the last path segment, so we synthesise the
    // manifest YAML directly to include an unusual path key.
    let yaml = "[repositories.\"github/owner/with,comma\"]\ntype = \"git\"\nurl = \"https://github.com/owner/withcomma.git\"\nversion = \"main\"\nrole = \"owned\"\n";
    let manifest = repoweave::manifest::Manifest::from_toml_str(yaml).unwrap();
    let project = repoweave::manifest::ProjectName::new("test-project").unwrap();
    let config = repoweave::manifest::IntegrationConfig::default();
    let cache = std::collections::HashMap::new();
    let ctx = IntegrationContext {
        output_dir: root,
        workspace_root: root,
        container_kind: ContainerKind::Primary,
        project: &project,
        repos: manifest
            .iter_entries()
            .map(|(rp, e)| (rp.clone(), e.clone()))
            .collect(),
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
        detection_cache: &cache,
        workweave: None,
    };

    Gita.activate(&ctx).unwrap();

    // Round-trip: parse with csv::Reader and verify the path field is intact.
    let repos_csv_path = root.join("gita/repos.csv");
    let mut rdr = csv::Reader::from_path(&repos_csv_path).unwrap();
    let records: Vec<_> = rdr.records().collect::<Result<_, _>>().unwrap();
    assert_eq!(records.len(), 1, "expected exactly one data row");
    let path_field = &records[0][0];
    // The path column should end with the repo path including the comma.
    assert!(
        path_field.ends_with("github/owner/with,comma"),
        "path field should contain the comma-repo path, got: {path_field:?}"
    );
    // And the name field (basename) should be correct too.
    let name_field = &records[0][1];
    assert_eq!(
        name_field, "with,comma",
        "name field should be the basename, got: {name_field:?}"
    );
}

/// Deactivate must remove the two rwv-owned CSVs but leave a user-parked
/// file (e.g. notes.txt) and the gita/ directory intact.
#[test]
fn deactivate_preserves_user_parked_file() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    std::fs::create_dir_all(root.join("gita")).unwrap();
    write_file(root, "gita/repos.csv", "path,name,flags\n");
    write_file(root, "gita/groups.csv", "group,repos\n");
    write_file(root, "gita/notes.txt", "keep me\n");

    Gita.deactivate(root).unwrap();

    assert!(
        !root.join("gita/repos.csv").exists(),
        "repos.csv should be removed"
    );
    assert!(
        !root.join("gita/groups.csv").exists(),
        "groups.csv should be removed"
    );
    assert!(
        root.join("gita/notes.txt").exists(),
        "user-parked notes.txt should survive deactivate"
    );
    assert!(
        root.join("gita").exists(),
        "gita/ directory should survive when non-empty"
    );
}

/// Deactivate must remove the gita/ directory entirely when it becomes
/// empty after removing the two rwv-owned CSVs.
#[test]
fn deactivate_removes_empty_gita_dir() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    std::fs::create_dir_all(root.join("gita")).unwrap();
    write_file(root, "gita/repos.csv", "path,name,flags\n");
    write_file(root, "gita/groups.csv", "group,repos\n");

    Gita.deactivate(root).unwrap();

    assert!(
        !root.join("gita/repos.csv").exists(),
        "repos.csv should be removed"
    );
    assert!(
        !root.join("gita/groups.csv").exists(),
        "groups.csv should be removed"
    );
    assert!(
        !root.join("gita").exists(),
        "gita/ directory should be removed when empty"
    );
}
