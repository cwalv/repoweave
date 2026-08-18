// ===========================================================================
// vscode-workspace
// ===========================================================================

use super::*;

#[test]
fn auto_detects_all_repos() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // vscode-workspace uses all repos (not filtered by manifest file)
    let manifest = make_manifest(vec![
        ("github/acme/server", Role::Owned),
        ("github/acme/web", Role::Fork),
    ]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = VscodeWorkspace;
    integration.activate(&ctx).unwrap();
    assert!(root.join("test-project.code-workspace").exists());
}

#[test]
fn generates_code_workspace_file_with_folders_and_settings() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![
        ("github/chatly/server", Role::Owned),
        ("github/chatly/web", Role::Owned),
    ]);
    let project = ProjectName::new("web-app").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = VscodeWorkspace;
    integration.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("web-app.code-workspace")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    let folders = parsed["folders"].as_array().unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0]["path"], ".");
    assert_eq!(folders[0]["name"], "web-app (primary)");

    assert_eq!(
        parsed["settings"]["git.autoRepositoryDetection"],
        "subFolders"
    );
    assert_eq!(parsed["settings"]["git.repositoryScanMaxDepth"], 3);

    // Should include the generated marker so deactivate can identify it.
    // The marker is now an object { "managed": true, "files.exclude": [...] }
    // (was plain `true` previously — has_marker tolerates both forms).
    assert_eq!(parsed["rwv.generated"]["managed"], true);
}

#[test]
fn project_name_appears_in_filename() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("my-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = VscodeWorkspace;
    integration.activate(&ctx).unwrap();
    assert!(root.join("my-project.code-workspace").exists());
}

#[test]
fn preserves_user_customizations_on_reactivation() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Pre-existing rwv-managed workspace file with user customizations.
    // The marker is what makes this a re-activation rather than a seizure
    // of a hand-authored file.
    write_file(
        root,
        "test-project.code-workspace",
        r#"{
  "rwv.generated": true,
  "folders": [{ "path": ".", "name": "old-name" }],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3,
    "editor.fontSize": 14
  },
  "extensions": {
    "recommendations": ["rust-analyzer"]
  }
}"#,
    );

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = VscodeWorkspace;
    integration.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    // Folders should be updated
    let folders = parsed["folders"].as_array().unwrap();
    assert_eq!(folders[0]["name"], "test-project (primary)");

    // Managed settings should be updated
    assert_eq!(
        parsed["settings"]["git.autoRepositoryDetection"],
        "subFolders"
    );
    assert_eq!(parsed["settings"]["git.repositoryScanMaxDepth"], 3);

    // User customizations should survive
    assert_eq!(parsed["settings"]["editor.fontSize"], 14);
    assert!(parsed["extensions"]["recommendations"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("rust-analyzer")));
}

#[test]
fn deactivation_removes_generated_code_workspace_files() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Write a file with the rwv.generated marker (as activate produces).
    write_file(
        root,
        "test-project.code-workspace",
        r#"{"rwv.generated": true, "folders": []}"#,
    );
    assert!(root.join("test-project.code-workspace").exists());

    let integration = VscodeWorkspace;
    integration.deactivate(root).unwrap();
    assert!(!root.join("test-project.code-workspace").exists());
}

#[test]
fn deactivation_preserves_handwritten_code_workspace_files() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // A user-created .code-workspace without the rwv marker.
    write_file(
        root,
        "my-project.code-workspace",
        r#"{"folders": [{"path": "."}]}"#,
    );

    let integration = VscodeWorkspace;
    integration.deactivate(root).unwrap();
    assert!(
        root.join("my-project.code-workspace").exists(),
        "hand-written .code-workspace should be preserved"
    );
}

#[test]
fn check_validates_workspace_file_exists() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // No .code-workspace file present
    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let integration = VscodeWorkspace;
    let issues = integration.check(&ctx).unwrap();
    assert!(issues
        .iter()
        .any(|i| i.severity == Severity::Warning && i.message.contains("code-workspace")));
}

#[test]
fn files_exclude_hides_non_project_repos() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Active project has github/chatly/server.
    // github/acme/web is on disk but not in the project.
    let manifest = make_manifest(vec![("github/chatly/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();

    let all_repos_on_disk: Vec<RepoPath> = vec![
        RepoPath::new("github/chatly/server").expect("known-safe literal"),
        RepoPath::new("github/acme/web").expect("known-safe literal"),
    ];

    let cache = HashMap::new();
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
        all_repos_on_disk: &all_repos_on_disk,
        all_project_paths: &[],
        detection_cache: &cache,
        workweave: None,
    };

    VscodeWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    let exclude = &parsed["settings"]["files.exclude"];
    // github/acme/web should be excluded (only repo under github/acme, so
    // collapse_excludes will produce "github/acme")
    assert_eq!(exclude["github/acme"], serde_json::Value::Bool(true));
    // github/chatly/server is active — must NOT be excluded
    assert!(exclude.get("github/chatly/server").is_none());
    assert!(exclude.get("github/chatly").is_none());
    assert!(exclude.get("github").is_none());
}

#[test]
fn files_exclude_hides_other_projects() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("proj-a").unwrap();
    let config = IntegrationConfig::default();

    let all_repos_on_disk: Vec<RepoPath> =
        vec![RepoPath::new("github/acme/server").expect("known-safe literal")];
    let all_project_paths = vec!["proj-a".to_string(), "proj-b".to_string()];

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
        all_repos_on_disk: &all_repos_on_disk,
        all_project_paths: &all_project_paths,
        detection_cache: &HashMap::new(),
        workweave: None,
    };

    VscodeWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("proj-a.code-workspace")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    let exclude = &parsed["settings"]["files.exclude"];

    // The other project directory should be excluded.
    assert_eq!(exclude["projects/proj-b"], serde_json::Value::Bool(true));
    // The active project should NOT be excluded.
    assert!(exclude.get("projects/proj-a").is_none());
}

#[test]
fn files_exclude_hides_dotfiles_by_default() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    VscodeWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(
        parsed["settings"]["files.exclude"][".*"],
        serde_json::Value::Bool(true),
        "dotfiles should be hidden by default"
    );
}

#[test]
fn files_exclude_respects_hide_dotfiles_false() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml("hide-dotfiles = false");
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    VscodeWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert!(
        parsed["settings"]["files.exclude"].get(".*").is_none(),
        "dotfiles should not be hidden when hide-dotfiles is false"
    );
}

#[test]
fn files_exclude_collapses_paths() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Active project has only github/acme/server.
    // All other repos are under github/other — should collapse to github/other.
    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();

    let all_repos_on_disk: Vec<RepoPath> = vec![
        RepoPath::new("github/acme/server").expect("known-safe literal"),
        RepoPath::new("github/other/alpha").expect("known-safe literal"),
        RepoPath::new("github/other/beta").expect("known-safe literal"),
    ];

    let cache = HashMap::new();
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
        all_repos_on_disk: &all_repos_on_disk,
        all_project_paths: &[],
        detection_cache: &cache,
        workweave: None,
    };

    VscodeWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    let exclude = &parsed["settings"]["files.exclude"];

    // All repos under github/other excluded → should collapse to owner path.
    assert_eq!(exclude["github/other"], serde_json::Value::Bool(true));
    // Individual paths should NOT appear (they were collapsed).
    assert!(exclude.get("github/other/alpha").is_none());
    assert!(exclude.get("github/other/beta").is_none());
    // Active repo and its owner must not be excluded.
    assert!(exclude.get("github/acme").is_none());
    assert!(exclude.get("github/acme/server").is_none());
}

// -----------------------------------------------------------------------
// DefaultOnly semantics for git.* settings
//
// git.autoRepositoryDetection and git.repositoryScanMaxDepth are
// DefaultOnly: rwv seeds them at greenfield but never overwrites a value
// the user has explicitly set.
// -----------------------------------------------------------------------

/// git.* defaults are written on a fresh (empty) workspace.
#[test]
fn git_settings_seeded_on_fresh_workspace() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    VscodeWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(
        parsed["settings"]["git.autoRepositoryDetection"], "subFolders",
        "default must be seeded on fresh workspace"
    );
    assert_eq!(
        parsed["settings"]["git.repositoryScanMaxDepth"], 3,
        "default depth must be seeded on fresh workspace"
    );
}

/// User-customized git.* values survive re-activation (DefaultOnly: no overwrite).
#[test]
fn git_settings_user_values_preserved_on_reactivate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Pre-existing workspace with user-customized git settings and rwv marker.
    write_file(
        root,
        "test-project.code-workspace",
        r#"{
  "rwv.generated": { "managed": true, "files.exclude": [] },
  "folders": [{ "path": ".", "name": "test-project (primary)" }],
  "settings": {
    "git.autoRepositoryDetection": "always",
    "git.repositoryScanMaxDepth": 10,
    "files.exclude": { ".*": true }
  }
}"#,
    );

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    VscodeWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(
        parsed["settings"]["git.autoRepositoryDetection"], "always",
        "user-set git.autoRepositoryDetection must not be overwritten on re-activate"
    );
    assert_eq!(
        parsed["settings"]["git.repositoryScanMaxDepth"], 10,
        "user-set git.repositoryScanMaxDepth must not be overwritten on re-activate"
    );
}
