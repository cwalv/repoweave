//! E2E tests for built-in integrations.
//!
//! Each integration is tested for:
//! 1. Auto-detection of relevant repos
//! 2. File generation matching the spec in docs/integrations.md
//! 3. Reference repos excluded from generated files
//! 4. Deactivation cleanup
//! 5. Check warnings (e.g., missing tools)

use repoweave::integration::{Integration, IntegrationContext, Severity};
use repoweave::integrations::*;
use repoweave::manifest::{
    IntegrationConfig, Manifest, ProjectName, RepoEntry, RepoPath, Role, VcsType,
};
use repoweave::vcs::RefName;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use tempfile::TempDir;

// ===========================================================================
// Test helpers
// ===========================================================================

/// Build a Manifest with the given repo entries and no integration config overrides.
fn make_manifest(repos: Vec<(&str, Role)>) -> Manifest {
    let mut repositories = BTreeMap::new();
    for (path, role) in repos {
        repositories.insert(
            RepoPath::new(path),
            RepoEntry {
                vcs_type: VcsType::Git,
                url: format!(
                    "https://github.com/test/{}.git",
                    path.split('/').last().unwrap()
                ),
                version: RefName::new("main"),
                role,
            },
        );
    }
    Manifest {
        repositories,
        integrations: BTreeMap::new(),
        workweave: None,
    }
}

/// Build an IntegrationContext from parts.
/// Both output_dir and workspace_root default to `root`.
fn make_ctx<'a>(
    root: &'a Path,
    project: &'a ProjectName,
    manifest: &'a Manifest,
    config: &'a IntegrationConfig,
    cache: &'a HashMap<String, Vec<String>>,
) -> IntegrationContext<'a> {
    IntegrationContext {
        output_dir: root,
        workspace_root: root,
        project,
        repos: &manifest.repositories,
        config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
        detection_cache: cache,
    }
}

/// Create a file inside a temp dir at the given relative path, including parent dirs.
fn touch(root: &Path, relative: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, "").unwrap();
}

/// Create a file inside a temp dir at the given relative path with content.
fn write_file(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
}

// ===========================================================================
// npm-workspaces
// ===========================================================================

mod npm_workspaces {
    use super::*;

    #[test]
    fn auto_detects_repos_with_package_json() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create repos: two with package.json, one without
        touch(root, "github/acme/server/package.json");
        touch(root, "github/acme/web/package.json");
        touch(root, "github/acme/docs/README.md");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Primary),
            ("github/acme/web", Role::Primary),
            ("github/acme/docs", Role::Primary),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = NpmWorkspaces;
        integration.activate(&ctx).unwrap();

        let pkg_json = std::fs::read_to_string(root.join("package.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&pkg_json).unwrap();
        let workspaces = parsed["workspaces"].as_array().unwrap();
        assert_eq!(workspaces.len(), 2);
        assert!(workspaces.contains(&serde_json::json!("github/acme/server")));
        assert!(workspaces.contains(&serde_json::json!("github/acme/web")));
        // docs should NOT be included (no package.json)
        assert!(!workspaces.contains(&serde_json::json!("github/acme/docs")));
    }

    #[test]
    fn generates_root_package_json_with_workspaces_array() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/chatly/protocol/package.json");
        touch(root, "github/chatly/server/package.json");
        touch(root, "github/chatly/web/package.json");

        let manifest = make_manifest(vec![
            ("github/chatly/protocol", Role::Primary),
            ("github/chatly/server", Role::Primary),
            ("github/chatly/web", Role::Fork),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = NpmWorkspaces;
        integration.activate(&ctx).unwrap();

        let pkg_json = std::fs::read_to_string(root.join("package.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&pkg_json).unwrap();
        assert_eq!(parsed["name"], "repoweave");
        assert_eq!(parsed["private"], true);
        let workspaces = parsed["workspaces"].as_array().unwrap();
        assert_eq!(workspaces.len(), 3);
        // Should be sorted
        assert_eq!(workspaces[0], "github/chatly/protocol");
        assert_eq!(workspaces[1], "github/chatly/server");
        assert_eq!(workspaces[2], "github/chatly/web");
    }

    #[test]
    fn excludes_reference_repos() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");
        touch(root, "github/acme/reference-lib/package.json");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Primary),
            ("github/acme/reference-lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = NpmWorkspaces;
        integration.activate(&ctx).unwrap();

        let pkg_json = std::fs::read_to_string(root.join("package.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&pkg_json).unwrap();
        let workspaces = parsed["workspaces"].as_array().unwrap();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0], "github/acme/server");
    }

    #[test]
    fn deactivation_removes_package_json() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "package.json",
            r#"{"name":"repoweave","private":true,"workspaces":[]}"#,
        );
        assert!(root.join("package.json").exists());

        let integration = NpmWorkspaces;
        integration.deactivate(root).unwrap();
        assert!(!root.join("package.json").exists());
    }

    #[test]
    fn deactivation_preserves_handwritten_package_json() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // A hand-written package.json without the generated name should NOT be removed
        write_file(
            root,
            "package.json",
            r#"{"name":"my-app","private":true,"workspaces":["packages/*"]}"#,
        );
        assert!(root.join("package.json").exists());

        let integration = NpmWorkspaces;
        integration.deactivate(root).unwrap();
        assert!(root.join("package.json").exists());
    }

    #[test]
    fn check_warns_when_npm_not_on_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = NpmWorkspaces;
        let issues = integration.check(&ctx).unwrap();
        // We can't guarantee npm is or isn't on PATH in CI,
        // but we can verify the check runs without error.
        // If npm is not on PATH, there should be a warning.
        if which::which("npm").is_err() {
            assert!(issues
                .iter()
                .any(|i| i.severity == Severity::Warning && i.message.contains("npm")));
        } else {
            assert!(issues.is_empty());
        }
    }
}

// ===========================================================================
// pnpm-workspaces
// ===========================================================================

mod pnpm_workspaces {
    use super::*;

    #[test]
    fn auto_detects_repos_with_package_json() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");
        touch(root, "github/acme/web/package.json");
        touch(root, "github/acme/docs/README.md");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Primary),
            ("github/acme/web", Role::Primary),
            ("github/acme/docs", Role::Primary),
        ]);
        let project = ProjectName::new("test-project");
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
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/chatly/protocol/package.json");
        touch(root, "github/chatly/server/package.json");
        touch(root, "github/chatly/web/package.json");

        let manifest = make_manifest(vec![
            ("github/chatly/protocol", Role::Primary),
            ("github/chatly/server", Role::Primary),
            ("github/chatly/web", Role::Fork),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = PnpmWorkspaces;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).unwrap();
        let expected = "packages:\n  - github/chatly/protocol\n  - github/chatly/server\n  - github/chatly/web\n";
        assert_eq!(content, expected);
    }

    #[test]
    fn excludes_reference_repos() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");
        touch(root, "github/acme/reference-lib/package.json");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Primary),
            ("github/acme/reference-lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project");
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
    fn deactivation_removes_pnpm_workspace_yaml() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(root, "pnpm-workspace.yaml", "packages:\n  - foo\n");
        assert!(root.join("pnpm-workspace.yaml").exists());

        let integration = PnpmWorkspaces;
        integration.deactivate(root).unwrap();
        assert!(!root.join("pnpm-workspace.yaml").exists());
    }

    #[test]
    fn check_warns_when_pnpm_not_on_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/package.json");

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = PnpmWorkspaces;
        let issues = integration.check(&ctx).unwrap();
        if which::which("pnpm").is_err() {
            assert!(issues
                .iter()
                .any(|i| i.severity == Severity::Warning && i.message.contains("pnpm")));
        } else {
            assert!(issues.is_empty());
        }
    }
}

// ===========================================================================
// go-work
// ===========================================================================

mod go_work {
    use super::*;

    #[test]
    fn auto_detects_repos_with_go_mod() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/go.mod");
        touch(root, "github/acme/web/go.mod");
        touch(root, "github/acme/docs/README.md");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Primary),
            ("github/acme/web", Role::Primary),
            ("github/acme/docs", Role::Primary),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(content.contains("github/acme/web"));
        assert!(!content.contains("github/acme/docs"));
    }

    #[test]
    fn generates_go_work_with_use_directives() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/chatly/protocol/go.mod");
        touch(root, "github/chatly/server/go.mod");

        let manifest = make_manifest(vec![
            ("github/chatly/protocol", Role::Primary),
            ("github/chatly/server", Role::Primary),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("go.work")).unwrap();
        let expected =
            "go 1.21\n\nuse (\n    ./github/chatly/protocol\n    ./github/chatly/server\n)\n";
        assert_eq!(content, expected);
    }

    #[test]
    fn excludes_reference_repos() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/go.mod");
        touch(root, "github/acme/reference-lib/go.mod");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Primary),
            ("github/acme/reference-lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(!content.contains("reference-lib"));
    }

    #[test]
    fn deactivation_removes_go_work() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(root, "go.work", "go 1.21\n\nuse (\n)\n");
        assert!(root.join("go.work").exists());

        let integration = GoWork;
        integration.deactivate(root).unwrap();
        assert!(!root.join("go.work").exists());
    }

    #[test]
    fn check_warns_when_go_not_on_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/go.mod");

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        let issues = integration.check(&ctx).unwrap();
        if which::which("go").is_err() {
            assert!(issues
                .iter()
                .any(|i| i.severity == Severity::Warning && i.message.contains("go")));
        } else {
            assert!(issues.is_empty());
        }
    }
}

// ===========================================================================
// uv-workspace
// ===========================================================================

mod uv_workspace {
    use super::*;

    #[test]
    fn auto_detects_repos_with_pyproject_toml() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/pyproject.toml");
        touch(root, "github/acme/web/pyproject.toml");
        touch(root, "github/acme/docs/README.md");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Primary),
            ("github/acme/web", Role::Primary),
            ("github/acme/docs", Role::Primary),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = UvWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(content.contains("github/acme/web"));
        assert!(!content.contains("github/acme/docs"));
    }

    #[test]
    fn generates_pyproject_toml_with_uv_workspace() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/chatly/protocol/pyproject.toml");
        touch(root, "github/chatly/server/pyproject.toml");

        let manifest = make_manifest(vec![
            ("github/chatly/protocol", Role::Primary),
            ("github/chatly/server", Role::Primary),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = UvWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        assert!(content.starts_with("# Generated by rwv \u{2014} do not edit\n"));
        assert!(content.contains("[tool.uv.workspace]"));
        assert!(content.contains("\"github/chatly/protocol\""));
        assert!(content.contains("\"github/chatly/server\""));
    }

    #[test]
    fn excludes_reference_repos() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/pyproject.toml");
        touch(root, "github/acme/reference-lib/pyproject.toml");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Primary),
            ("github/acme/reference-lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = UvWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(!content.contains("reference-lib"));
    }

    #[test]
    fn deactivation_removes_pyproject_toml() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(
            root,
            "pyproject.toml",
            "# Generated by rwv \u{2014} do not edit\n[tool.uv.workspace]\nmembers = []\n",
        );
        assert!(root.join("pyproject.toml").exists());

        let integration = UvWorkspace;
        integration.deactivate(root).unwrap();
        assert!(!root.join("pyproject.toml").exists());
    }

    #[test]
    fn deactivation_preserves_handwritten_pyproject_toml() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // A hand-written pyproject.toml without the generated header should NOT be removed
        write_file(
            root,
            "pyproject.toml",
            "[project]\nname = \"my-project\"\nversion = \"0.1.0\"\n",
        );
        assert!(root.join("pyproject.toml").exists());

        let integration = UvWorkspace;
        integration.deactivate(root).unwrap();
        assert!(root.join("pyproject.toml").exists());
    }

    #[test]
    fn check_warns_when_uv_not_on_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/pyproject.toml");

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = UvWorkspace;
        let issues = integration.check(&ctx).unwrap();
        if which::which("uv").is_err() {
            assert!(issues
                .iter()
                .any(|i| i.severity == Severity::Warning && i.message.contains("uv")));
        } else {
            assert!(issues.is_empty());
        }
    }
}

// ===========================================================================
// cargo-workspace
// ===========================================================================

mod cargo_workspace {
    use super::*;

    #[test]
    fn auto_detects_repos_with_cargo_toml() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/Cargo.toml");
        touch(root, "github/acme/web/Cargo.toml");
        touch(root, "github/acme/docs/README.md");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Primary),
            ("github/acme/web", Role::Primary),
            ("github/acme/docs", Role::Primary),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = CargoWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(content.contains("github/acme/web"));
        assert!(!content.contains("github/acme/docs"));
    }

    #[test]
    fn generates_cargo_toml_with_workspace_section() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/chatly/protocol/Cargo.toml");
        touch(root, "github/chatly/server/Cargo.toml");

        let manifest = make_manifest(vec![
            ("github/chatly/protocol", Role::Primary),
            ("github/chatly/server", Role::Primary),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = CargoWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(content.starts_with("# Generated by rwv \u{2014} do not edit\n"));
        assert!(content.contains("[workspace]"));
        assert!(content.contains("\"github/chatly/protocol\""));
        assert!(content.contains("\"github/chatly/server\""));
        assert!(content.contains("resolver = \"2\""));
    }

    #[test]
    fn excludes_reference_repos() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/Cargo.toml");
        touch(root, "github/acme/reference-lib/Cargo.toml");

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Primary),
            ("github/acme/reference-lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = CargoWorkspace;
        integration.activate(&ctx).unwrap();

        let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(content.contains("github/acme/server"));
        assert!(!content.contains("reference-lib"));
    }

    #[test]
    fn deactivation_removes_generated_cargo_toml() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Only removes if it starts with the generated-file header
        write_file(
            root,
            "Cargo.toml",
            "# Generated by rwv \u{2014} do not edit\n\n[workspace]\nmembers = []\nresolver = \"2\"\n",
        );
        assert!(root.join("Cargo.toml").exists());

        let integration = CargoWorkspace;
        integration.deactivate(root).unwrap();
        assert!(!root.join("Cargo.toml").exists());
    }

    #[test]
    fn deactivation_preserves_handwritten_cargo_toml() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // A hand-written Cargo.toml without the generated header should NOT be removed
        write_file(
            root,
            "Cargo.toml",
            "[package]\nname = \"my-project\"\nversion = \"0.1.0\"\n",
        );
        assert!(root.join("Cargo.toml").exists());

        let integration = CargoWorkspace;
        integration.deactivate(root).unwrap();
        assert!(root.join("Cargo.toml").exists());
    }

    #[test]
    fn check_warns_when_cargo_not_on_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/Cargo.toml");

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = CargoWorkspace;
        let issues = integration.check(&ctx).unwrap();
        if which::which("cargo").is_err() {
            assert!(issues
                .iter()
                .any(|i| i.severity == Severity::Warning && i.message.contains("cargo")));
        } else {
            assert!(issues.is_empty());
        }
    }
}

// ===========================================================================
// gita
// ===========================================================================

mod gita {
    use super::*;

    #[test]
    fn auto_detects_all_repos() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // gita uses all repos, not just those with a specific manifest file
        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Primary),
            ("github/acme/web", Role::Fork),
        ]);
        let project = ProjectName::new("test-project");
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
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![
            ("github/chatly/server", Role::Primary),
            ("github/chatly/web", Role::Primary),
            ("github/chatly/protocol", Role::Fork),
        ]);
        let project = ProjectName::new("test-project");
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
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![
            ("github/chatly/server", Role::Primary),
            ("github/chatly/web", Role::Primary),
            ("github/chatly/protocol", Role::Fork),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = Gita;
        integration.activate(&ctx).unwrap();

        let groups_csv = std::fs::read_to_string(root.join("gita/groups.csv")).unwrap();
        assert!(groups_csv.starts_with("group,repos\n"));
        assert!(groups_csv.contains("fork,protocol\n"));
        assert!(groups_csv.contains("primary,server web\n"));
    }

    #[test]
    fn excludes_reference_repos() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Primary),
            ("github/acme/reference-lib", Role::Reference),
        ]);
        let project = ProjectName::new("test-project");
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
        let tmp = TempDir::new().unwrap();
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
        let workspace_tmp = TempDir::new().unwrap();
        let workspace_root = workspace_tmp.path();
        let weave_tmp = TempDir::new().unwrap();
        let output_dir = weave_tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = IntegrationContext {
            output_dir,
            workspace_root,
            project: &project,
            repos: &manifest.repositories,
            config: &config,
            all_repos_on_disk: &[],
            all_project_paths: &[],
            detection_cache: &cache,
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

    #[test]
    fn check_warns_when_gita_not_on_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = Gita;
        let issues = integration.check(&ctx).unwrap();
        if which::which("gita").is_err() {
            assert!(issues
                .iter()
                .any(|i| i.severity == Severity::Warning && i.message.contains("gita")));
        } else {
            assert!(issues.is_empty());
        }
    }
}

// ===========================================================================
// vscode-workspace
// ===========================================================================

mod vscode_workspace {
    use super::*;

    #[test]
    fn auto_detects_all_repos() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // vscode-workspace uses all repos (not filtered by manifest file)
        let manifest = make_manifest(vec![
            ("github/acme/server", Role::Primary),
            ("github/acme/web", Role::Fork),
        ]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = VscodeWorkspace;
        integration.activate(&ctx).unwrap();
        assert!(root.join("test-project.code-workspace").exists());
    }

    #[test]
    fn generates_code_workspace_file_with_folders_and_settings() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![
            ("github/chatly/server", Role::Primary),
            ("github/chatly/web", Role::Primary),
        ]);
        let project = ProjectName::new("web-app");
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
        assert_eq!(parsed["rwv.generated"], true);
    }

    #[test]
    fn project_name_appears_in_filename() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("my-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = VscodeWorkspace;
        integration.activate(&ctx).unwrap();
        assert!(root.join("my-project.code-workspace").exists());
    }

    #[test]
    fn preserves_user_customizations_on_reactivation() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Pre-existing workspace file with user customizations
        write_file(
            root,
            "test-project.code-workspace",
            r#"{
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

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
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
        let tmp = TempDir::new().unwrap();
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
        let tmp = TempDir::new().unwrap();
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
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // No .code-workspace file present
        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
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
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Active project has github/chatly/server.
        // github/acme/web is on disk but not in the project.
        let manifest = make_manifest(vec![("github/chatly/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();

        let all_repos_on_disk: Vec<RepoPath> = vec![
            RepoPath::new("github/chatly/server"),
            RepoPath::new("github/acme/web"),
        ];

        let cache = HashMap::new();
        let ctx = IntegrationContext {
            output_dir: root,
            workspace_root: root,
            project: &project,
            repos: &manifest.repositories,
            config: &config,
            all_repos_on_disk: &all_repos_on_disk,
            all_project_paths: &[],
            detection_cache: &cache,
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
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("proj-a");
        let config = IntegrationConfig::default();

        let all_repos_on_disk: Vec<RepoPath> = vec![RepoPath::new("github/acme/server")];
        let all_project_paths = vec!["proj-a".to_string(), "proj-b".to_string()];

        let ctx = IntegrationContext {
            output_dir: root,
            workspace_root: root,
            project: &project,
            repos: &manifest.repositories,
            config: &config,
            all_repos_on_disk: &all_repos_on_disk,
            all_project_paths: &all_project_paths,
            detection_cache: &HashMap::new(),
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
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
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
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("hide-dotfiles: false");
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
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Active project has only github/acme/server.
        // All other repos are under github/other — should collapse to github/other.
        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();

        let all_repos_on_disk: Vec<RepoPath> = vec![
            RepoPath::new("github/acme/server"),
            RepoPath::new("github/other/alpha"),
            RepoPath::new("github/other/beta"),
        ];

        let cache = HashMap::new();
        let ctx = IntegrationContext {
            output_dir: root,
            workspace_root: root,
            project: &project,
            repos: &manifest.repositories,
            config: &config,
            all_repos_on_disk: &all_repos_on_disk,
            all_project_paths: &[],
            detection_cache: &cache,
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
}

// ===========================================================================
// Integration lock hooks
// ===========================================================================
//
// Each ecosystem integration should have a lock hook that runs the
// ecosystem's lock command. Non-ecosystem integrations (gita, vscode)
// should have no-op lock hooks.

mod lock_hooks {
    use super::*;

    // -----------------------------------------------------------------------
    // npm-workspaces: `npm install --package-lock-only`
    // -----------------------------------------------------------------------

    #[test]
    fn npm_workspaces_lock_runs_npm_install_package_lock_only() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Set up a repo with a valid package.json so npm integration detects it
        write_file(
            root,
            "github/acme/server/package.json",
            "{\"name\": \"server\", \"version\": \"0.1.0\"}",
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Activate first so the root package.json exists
        let integration = NpmWorkspaces;
        integration.activate(&ctx).unwrap();

        // Lock should succeed (runs `npm install --package-lock-only`)
        let result = integration.lock(&ctx);
        if which::which("npm").is_ok() {
            assert!(
                result.is_ok(),
                "npm lock hook should succeed when npm is available: {:?}",
                result.err()
            );
            // After lock, a package-lock.json should exist
            assert!(
                root.join("package-lock.json").exists(),
                "npm lock hook should create package-lock.json"
            );
        } else {
            // When npm is not on PATH, the lock hook should fail gracefully
            assert!(
                result.is_err(),
                "npm lock hook should fail when npm is not available"
            );
        }
    }

    #[test]
    fn npm_workspaces_lock_noop_when_no_repos_detected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // No package.json in any repo — lock should be a no-op
        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = NpmWorkspaces;
        let result = integration.lock(&ctx);
        assert!(
            result.is_ok(),
            "npm lock should be no-op when no repos detected"
        );
        assert!(
            !root.join("package-lock.json").exists(),
            "no package-lock.json should be created when no repos detected"
        );
    }

    // -----------------------------------------------------------------------
    // cargo-workspace: `cargo generate-lockfile`
    // -----------------------------------------------------------------------

    #[test]
    fn cargo_workspace_lock_runs_cargo_generate_lockfile() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create a minimal Cargo.toml and src/lib.rs in the repo
        write_file(
            root,
            "github/acme/server/Cargo.toml",
            "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write_file(root, "github/acme/server/src/lib.rs", "");

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Activate first so the root Cargo.toml workspace exists
        let integration = CargoWorkspace;
        integration.activate(&ctx).unwrap();

        // Lock should succeed (runs `cargo generate-lockfile`)
        let result = integration.lock(&ctx);
        if which::which("cargo").is_ok() {
            assert!(
                result.is_ok(),
                "cargo lock hook should succeed when cargo is available: {:?}",
                result.err()
            );
            // After lock, a Cargo.lock should exist
            assert!(
                root.join("Cargo.lock").exists(),
                "cargo lock hook should create Cargo.lock"
            );
        } else {
            assert!(
                result.is_err(),
                "cargo lock hook should fail when cargo is not available"
            );
        }
    }

    #[test]
    fn cargo_workspace_lock_noop_when_no_repos_detected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // No Cargo.toml in any repo
        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = CargoWorkspace;
        let result = integration.lock(&ctx);
        assert!(
            result.is_ok(),
            "cargo lock should be no-op when no repos detected"
        );
        assert!(
            !root.join("Cargo.lock").exists(),
            "no Cargo.lock should be created when no repos detected"
        );
    }

    // -----------------------------------------------------------------------
    // uv-workspace: `uv lock`
    // -----------------------------------------------------------------------

    #[test]
    fn uv_workspace_lock_runs_uv_lock() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create a minimal pyproject.toml in the repo
        write_file(
            root,
            "github/acme/server/pyproject.toml",
            "[project]\nname = \"server\"\nversion = \"0.1.0\"\n",
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Activate first so the root pyproject.toml exists
        let integration = UvWorkspace;
        integration.activate(&ctx).unwrap();

        // Lock should succeed (runs `uv lock`)
        let result = integration.lock(&ctx);
        if which::which("uv").is_ok() {
            assert!(
                result.is_ok(),
                "uv lock hook should succeed when uv is available: {:?}",
                result.err()
            );
            // After lock, a uv.lock should exist
            assert!(
                root.join("uv.lock").exists(),
                "uv lock hook should create uv.lock"
            );
        } else {
            assert!(
                result.is_err(),
                "uv lock hook should fail when uv is not available"
            );
        }
    }

    #[test]
    fn uv_workspace_lock_noop_when_no_repos_detected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // No pyproject.toml in any repo
        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = UvWorkspace;
        let result = integration.lock(&ctx);
        assert!(
            result.is_ok(),
            "uv lock should be no-op when no repos detected"
        );
        assert!(
            !root.join("uv.lock").exists(),
            "no uv.lock should be created when no repos detected"
        );
    }

    // -----------------------------------------------------------------------
    // pnpm-workspaces: `pnpm install --lockfile-only`
    // -----------------------------------------------------------------------

    #[test]
    fn pnpm_workspaces_lock_runs_pnpm_install_lockfile_only() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Set up a repo with a valid package.json so pnpm integration detects it
        write_file(
            root,
            "github/acme/server/package.json",
            "{\"name\": \"server\", \"version\": \"0.1.0\"}",
        );

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        // Activate first so the pnpm-workspace.yaml exists
        let integration = PnpmWorkspaces;
        integration.activate(&ctx).unwrap();

        // Lock should succeed (runs `pnpm install --lockfile-only`)
        let result = integration.lock(&ctx);
        if which::which("pnpm").is_ok() {
            assert!(
                result.is_ok(),
                "pnpm lock hook should succeed when pnpm is available: {:?}",
                result.err()
            );
            // After lock, a pnpm-lock.yaml should exist
            assert!(
                root.join("pnpm-lock.yaml").exists(),
                "pnpm lock hook should create pnpm-lock.yaml"
            );
        } else {
            assert!(
                result.is_err(),
                "pnpm lock hook should fail when pnpm is not available"
            );
        }
    }

    #[test]
    fn pnpm_workspaces_lock_noop_when_no_repos_detected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // No package.json in any repo
        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = PnpmWorkspaces;
        let result = integration.lock(&ctx);
        assert!(
            result.is_ok(),
            "pnpm lock should be no-op when no repos detected"
        );
        assert!(
            !root.join("pnpm-lock.yaml").exists(),
            "no pnpm-lock.yaml should be created when no repos detected"
        );
    }

    // -----------------------------------------------------------------------
    // go-work: no lock hook (uses default no-op)
    // -----------------------------------------------------------------------

    #[test]
    fn go_work_lock_is_noop() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        touch(root, "github/acme/server/go.mod");

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = GoWork;
        let result = integration.lock(&ctx);
        assert!(result.is_ok(), "go-work lock should be a no-op");
        // No lock file should be created
        assert!(
            !root.join("go.sum").exists(),
            "go-work lock should not create go.sum"
        );
    }

    // -----------------------------------------------------------------------
    // gita: no-op lock hook
    // -----------------------------------------------------------------------

    #[test]
    fn gita_lock_is_noop() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = Gita;
        let result = integration.lock(&ctx);
        assert!(result.is_ok(), "gita lock should be a no-op");
    }

    // -----------------------------------------------------------------------
    // vscode-workspace: no-op lock hook
    // -----------------------------------------------------------------------

    #[test]
    fn vscode_workspace_lock_is_noop() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = VscodeWorkspace;
        let result = integration.lock(&ctx);
        assert!(result.is_ok(), "vscode-workspace lock should be a no-op");
    }
}

// ===========================================================================
// static-files
// ===========================================================================

mod static_files {
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
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml(
            "enabled: true\nfiles: [turbo.json, .eslintrc.json, .prettierrc]",
        );
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let files = integration.generated_files(&ctx);
        assert_eq!(files, vec!["turbo.json", ".eslintrc.json", ".prettierrc"]);
    }

    #[test]
    fn generated_files_empty_when_no_files_configured() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let files = integration.generated_files(&ctx);
        assert!(files.is_empty());
    }

    #[test]
    fn activate_succeeds_when_files_exist() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create the declared files in the project directory (output_dir)
        write_file(root, "turbo.json", r#"{"pipeline": {}}"#);
        write_file(root, ".eslintrc.json", "{}");

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config =
            IntegrationConfig::from_yaml("enabled: true\nfiles: [turbo.json, .eslintrc.json]");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let result = integration.activate(&ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn activate_succeeds_even_when_files_missing() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Don't create the files — activate should still succeed (just warn)
        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true\nfiles: [turbo.json]");
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
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create one of two declared files
        write_file(root, "turbo.json", "{}");

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config =
            IntegrationConfig::from_yaml("enabled: true\nfiles: [turbo.json, .eslintrc.json]");
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
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(root, "turbo.json", "{}");
        write_file(root, ".prettierrc", "{}");

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config =
            IntegrationConfig::from_yaml("enabled: true\nfiles: [turbo.json, .prettierrc]");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let issues = integration.check(&ctx).unwrap();
        assert!(issues.is_empty());
    }

    #[test]
    fn check_no_issues_when_no_files_configured() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let issues = integration.check(&ctx).unwrap();
        assert!(issues.is_empty());
    }

    #[test]
    fn deactivate_succeeds() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let integration = StaticFiles;
        let result = integration.deactivate(root);
        assert!(result.is_ok());
    }

    #[test]
    fn lock_is_noop() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let manifest = make_manifest(vec![("github/acme/server", Role::Primary)]);
        let project = ProjectName::new("test-project");
        let config = IntegrationConfig::from_yaml("enabled: true\nfiles: [turbo.json]");
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = StaticFiles;
        let result = integration.lock(&ctx);
        assert!(result.is_ok(), "static-files lock should be a no-op");
    }
}
