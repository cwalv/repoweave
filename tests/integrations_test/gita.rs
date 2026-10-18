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
    let lines: Vec<&str> = repos_csv.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "one row per repo and no header:\n{repos_csv}"
    );

    // Should be sorted by name (basename)
    assert!(lines[0].ends_with(",protocol,,"));
    assert!(lines[1].ends_with(",server,,"));
    assert!(lines[2].ends_with(",web,,"));

    // Paths should be absolute
    let abs_prefix = root.to_string_lossy();
    assert!(lines[0].starts_with(&*abs_prefix));
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
    assert_eq!(groups_csv, "fork:protocol:\nowned:server web:\n");
}

/// Both CSVs match what gita itself writes for the same repos and groups.
///
/// The fixtures are gita 0.16.8.2's own output from `gita add` and
/// `gita group add -n`, rows sorted by name and the root directory replaced
/// by `{root}`. gita reads both files with fixed field names and no header —
/// `path,name,type,flags` and colon-separated `name:repos:path` — so a header
/// row is read as a repo or a group, and a comma in groups.csv puts the whole
/// line in the group name. Python's csv module writes `\r\n` and reads either
/// terminator, so the comparison is over lines.
///
/// A repo path is gita's `os.path.abspath`, which joins in the host's own
/// separator, so the expected row joins the fixture's tail onto the root one
/// component at a time.
#[test]
fn csvs_match_the_format_gita_writes() {
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

    Gita.activate(&ctx).unwrap();

    for (written, fixture) in [
        ("gita/repos.csv", include_str!("../fixtures/gita/repos.csv")),
        (
            "gita/groups.csv",
            include_str!("../fixtures/gita/groups.csv"),
        ),
    ] {
        let expected: Vec<String> = fixture
            .lines()
            .map(|line| match line.strip_prefix("{root}/") {
                Some(rooted) => {
                    let (tail, fields) = rooted.split_once(',').unwrap();
                    let path = common::under(root, tail);
                    format!("{},{fields}", path.to_string_lossy())
                }
                None => line.to_owned(),
            })
            .collect();
        assert!(!expected.is_empty(), "fixture {written} must hold rows");
        let actual = std::fs::read_to_string(root.join(written)).unwrap();
        assert_eq!(
            actual.lines().collect::<Vec<_>>(),
            expected,
            "{written} must be in the format gita writes"
        );
    }
}

/// On Windows gita's `os.path.abspath` turns every `/` into `\`; the expected
/// row is what CPython's `ntpath.abspath` returns for this root and repo. The
/// root is read for its shape, so this runs the Windows spelling on any host.
#[test]
fn repos_csv_under_a_windows_root_is_backslashed_throughout() {
    let tmp = common::tempdir().unwrap();
    let manifest = make_manifest(vec![("github/chatly/protocol", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = IntegrationContext {
        workspace_root: Path::new(r"C:\Users\runneradmin\AppData\Local\Temp\.tmpOC0a9S"),
        ..make_ctx(tmp.path(), &project, &manifest, &config, &cache)
    };

    Gita.activate(&ctx).unwrap();

    let repos_csv = std::fs::read_to_string(tmp.path().join("gita/repos.csv")).unwrap();
    assert_eq!(
        repos_csv.lines().collect::<Vec<_>>(),
        [r"C:\Users\runneradmin\AppData\Local\Temp\.tmpOC0a9S\github\chatly\protocol,protocol,,"]
    );
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
    let data_lines: Vec<&str> = repos_csv.lines().collect();
    assert!(!data_lines.is_empty(), "repos.csv must hold a row");
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
    // only sets the `url` from the last path segment, so we synthesize the
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
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_path(&repos_csv_path)
        .unwrap();
    let records: Vec<_> = rdr.records().collect::<Result<_, _>>().unwrap();
    assert_eq!(records.len(), 1, "expected exactly one data row");
    let path_field = &records[0][0];
    assert_eq!(
        path_field,
        common::under(root, "github/owner/with,comma").to_string_lossy(),
        "path field should hold the comma-repo path whole"
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

/// An enabled gita with nothing on disk reports each CSV MISSING, fixable, so
/// `rwv doctor --fix` has a finding to author it from. Activation clears both.
#[test]
fn verify_reports_each_absent_csv_missing_until_activated() {
    use repoweave::integration::IssueKind;

    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let before = Gita.verify(&ctx).unwrap();
    let mut missing: Vec<&str> = before
        .iter()
        .filter(|i| i.kind == IssueKind::ManagedFileMissing && i.safe_to_fix)
        .map(|i| i.message.as_str())
        .collect();
    missing.sort();
    assert_eq!(missing.len(), 2, "one MISSING per CSV: {before:?}");
    assert!(missing[0].contains("groups.csv") && missing[1].contains("repos.csv"));

    Gita.activate(&ctx).unwrap();
    let after = Gita.verify(&ctx).unwrap();
    assert!(after.is_empty(), "activated CSVs are clean: {after:?}");
}

/// A CSV whose bytes are not what rwv would author is DRIFT, fixable — the
/// state a CSV written in an older format is in.
#[test]
fn verify_reports_a_csv_rwv_would_not_author_as_drift() {
    use repoweave::integration::IssueKind;

    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    Gita.activate(&ctx).unwrap();
    write_file(root, "gita/groups.csv", "group,repos\nowned,server\n");

    let issues = Gita.verify(&ctx).unwrap();
    assert_eq!(issues.len(), 1, "only the rewritten CSV drifts: {issues:?}");
    assert_eq!(issues[0].kind, IssueKind::ManagedFileDrift);
    assert!(issues[0].safe_to_fix);
    assert!(issues[0].message.contains("groups.csv"));
    assert!(issues[0].message.contains("rwv doctor --fix"));
}
