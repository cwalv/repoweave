//! E2E tests for the integration framework: Integration trait, IntegrationContext,
//! is_enabled resolution, mock integration behavior, output_dir/workspace_root
//! split, default lock hook, and generated_files().

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

use repoweave::integration::{is_enabled, Integration, IntegrationContext, Issue, Severity};
use repoweave::manifest::{
    IntegrationConfig, ProjectName, RepoEntry, RepoPath, Role, VcsType,
};
use repoweave::vcs::RefName;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_repo_entry(role: Role) -> RepoEntry {
    RepoEntry {
        vcs_type: VcsType::Git,
        url: "https://example.com/repo.git".into(),
        version: RefName::new("main"),
        role,
    }
}

// ---------------------------------------------------------------------------
// Mock integration
// ---------------------------------------------------------------------------

/// Records calls so tests can assert on them.
#[derive(Clone)]
struct MockIntegration {
    name: String,
    default_enabled: bool,
    check_issues: Vec<Issue>,
    /// (method, detail) log for activate/deactivate/check calls.
    call_log: Arc<Mutex<Vec<(String, String)>>>,
}

impl MockIntegration {
    fn new(name: &str, default_enabled: bool) -> Self {
        Self {
            name: name.to_string(),
            default_enabled,
            check_issues: Vec::new(),
            call_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn with_check_issues(mut self, issues: Vec<Issue>) -> Self {
        self.check_issues = issues;
        self
    }

    fn calls(&self) -> Vec<(String, String)> {
        self.call_log.lock().unwrap().clone()
    }
}

impl Integration for MockIntegration {
    fn name(&self) -> &str {
        &self.name
    }

    fn default_enabled(&self) -> bool {
        self.default_enabled
    }

    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        self.call_log.lock().unwrap().push((
            "activate".into(),
            format!("project={}", ctx.project.as_str()),
        ));
        Ok(())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        self.call_log.lock().unwrap().push((
            "deactivate".into(),
            format!("root={}", root.display()),
        ));
        Ok(())
    }

    fn check(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        self.call_log.lock().unwrap().push((
            "check".into(),
            format!("project={}", ctx.project.as_str()),
        ));
        Ok(self.check_issues.clone())
    }
}

// ---------------------------------------------------------------------------
// is_enabled tests
// ---------------------------------------------------------------------------

#[test]
fn is_enabled_default_enabled_no_override() {
    let integration = MockIntegration::new("test", true);
    let config = IntegrationConfig::default(); // enabled: None
    assert!(is_enabled(&integration, &config));
}

#[test]
fn is_enabled_default_enabled_with_false_override() {
    let integration = MockIntegration::new("test", true);
    let config = IntegrationConfig::from_yaml("enabled: false");
    assert!(!is_enabled(&integration, &config));
}

#[test]
fn is_enabled_default_disabled_with_true_override() {
    let integration = MockIntegration::new("test", false);
    let config = IntegrationConfig::from_yaml("enabled: true");
    assert!(is_enabled(&integration, &config));
}

#[test]
fn is_enabled_default_disabled_no_override() {
    let integration = MockIntegration::new("test", false);
    let config = IntegrationConfig::default();
    assert!(!is_enabled(&integration, &config));
}

// ---------------------------------------------------------------------------
// IntegrationContext::active_repos tests
// ---------------------------------------------------------------------------

#[test]
fn active_repos_excludes_reference() {
    let mut repos = BTreeMap::new();
    repos.insert(RepoPath::new("ref-repo"), make_repo_entry(Role::Reference));
    repos.insert(RepoPath::new("primary-repo"), make_repo_entry(Role::Primary));

    let project = ProjectName::new("test-project");
    let config = IntegrationConfig::default();
    let ctx = IntegrationContext {
        output_dir: Path::new("/tmp/test"),
        workspace_root: Path::new("/tmp/test"),
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };

    let active: Vec<_> = ctx.active_repos().collect();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].0.as_str(), "primary-repo");
}

#[test]
fn active_repos_includes_primary_fork_dependency() {
    let mut repos = BTreeMap::new();
    repos.insert(RepoPath::new("a-primary"), make_repo_entry(Role::Primary));
    repos.insert(RepoPath::new("b-fork"), make_repo_entry(Role::Fork));
    repos.insert(RepoPath::new("c-dep"), make_repo_entry(Role::Dependency));
    repos.insert(RepoPath::new("d-ref"), make_repo_entry(Role::Reference));

    let project = ProjectName::new("test-project");
    let config = IntegrationConfig::default();
    let ctx = IntegrationContext {
        output_dir: Path::new("/tmp/test"),
        workspace_root: Path::new("/tmp/test"),
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };

    let active: Vec<_> = ctx.active_repos().collect();
    assert_eq!(active.len(), 3);

    let paths: Vec<&str> = active.iter().map(|(p, _)| p.as_str()).collect();
    assert!(paths.contains(&"a-primary"));
    assert!(paths.contains(&"b-fork"));
    assert!(paths.contains(&"c-dep"));
    assert!(!paths.contains(&"d-ref"));
}

// ---------------------------------------------------------------------------
// Mock integration: activate / deactivate / check
// ---------------------------------------------------------------------------

#[test]
fn mock_activate_receives_correct_context() {
    let mut repos = BTreeMap::new();
    repos.insert(RepoPath::new("repo-a"), make_repo_entry(Role::Primary));

    let project = ProjectName::new("my-project");
    let config = IntegrationConfig::default();
    let ctx = IntegrationContext {
        output_dir: Path::new("/workspace"),
        workspace_root: Path::new("/workspace"),
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };

    let mock = MockIntegration::new("cargo", true);
    mock.activate(&ctx).unwrap();

    let calls = mock.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "activate");
    assert_eq!(calls[0].1, "project=my-project");
}

#[test]
fn mock_deactivate_receives_correct_root() {
    let mock = MockIntegration::new("cargo", true);
    let root = PathBuf::from("/workspace/weaves/hotfix");
    mock.deactivate(&root).unwrap();

    let calls = mock.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "deactivate");
    assert_eq!(calls[0].1, "root=/workspace/weaves/hotfix");
}

#[test]
fn mock_check_returns_issues() {
    let issues = vec![
        Issue {
            integration: "cargo".into(),
            severity: Severity::Warning,
            message: "missing dependency".into(),
        },
        Issue {
            integration: "cargo".into(),
            severity: Severity::Error,
            message: "build failure".into(),
        },
    ];

    let mut repos = BTreeMap::new();
    repos.insert(RepoPath::new("repo-a"), make_repo_entry(Role::Primary));

    let project = ProjectName::new("check-project");
    let config = IntegrationConfig::default();
    let ctx = IntegrationContext {
        output_dir: Path::new("/workspace"),
        workspace_root: Path::new("/workspace"),
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };

    let mock = MockIntegration::new("cargo", true).with_check_issues(issues);
    let result = mock.check(&ctx).unwrap();

    assert_eq!(result.len(), 2);
    assert_eq!(result[0].severity, Severity::Warning);
    assert_eq!(result[0].message, "missing dependency");
    assert_eq!(result[1].severity, Severity::Error);
    assert_eq!(result[1].message, "build failure");

    // Verify check was logged
    let calls = mock.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "check");
}

// ---------------------------------------------------------------------------
// Issue and Severity construction
// ---------------------------------------------------------------------------

#[test]
fn issue_creation_with_warning_severity() {
    let issue = Issue {
        integration: "npm".into(),
        severity: Severity::Warning,
        message: "outdated lockfile".into(),
    };
    assert_eq!(issue.integration, "npm");
    assert_eq!(issue.severity, Severity::Warning);
    assert_eq!(issue.message, "outdated lockfile");
}

#[test]
fn issue_creation_with_error_severity() {
    let issue = Issue {
        integration: "cargo".into(),
        severity: Severity::Error,
        message: "unresolvable version conflict".into(),
    };
    assert_eq!(issue.integration, "cargo");
    assert_eq!(issue.severity, Severity::Error);
    assert_eq!(issue.message, "unresolvable version conflict");
}

// ---------------------------------------------------------------------------
// IntegrationContext: output_dir / workspace_root split
// ---------------------------------------------------------------------------

/// Helper to create a file at a relative path under a directory.
fn touch(dir: &Path, relative: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, "").unwrap();
}

#[test]
fn detect_repos_with_manifest_uses_workspace_root_not_output_dir() {
    // Set up two separate directories: workspace_root has the repos,
    // output_dir is an empty workweave directory.
    let ws_tmp = TempDir::new().unwrap();
    let out_tmp = TempDir::new().unwrap();
    let workspace_root = ws_tmp.path();
    let output_dir = out_tmp.path();

    // Create manifest files under workspace_root only
    touch(workspace_root, "github/acme/server/Cargo.toml");
    touch(workspace_root, "github/acme/web/Cargo.toml");

    // output_dir has no repos — detection should still find them
    // because it looks in workspace_root.

    let mut repos = BTreeMap::new();
    repos.insert(RepoPath::new("github/acme/server"), make_repo_entry(Role::Primary));
    repos.insert(RepoPath::new("github/acme/web"), make_repo_entry(Role::Primary));

    let project = ProjectName::new("test-project");
    let config = IntegrationConfig::default();
    let ctx = IntegrationContext {
        output_dir,
        workspace_root,
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };

    let detected = ctx.detect_repos_with_manifest("Cargo.toml");
    assert_eq!(detected, vec!["github/acme/server", "github/acme/web"]);
}

#[test]
fn detect_repos_with_manifest_ignores_output_dir_manifests() {
    // Manifest files exist only in output_dir but NOT in workspace_root.
    // detect_repos_with_manifest should return nothing because it checks
    // workspace_root, not output_dir.
    let ws_tmp = TempDir::new().unwrap();
    let out_tmp = TempDir::new().unwrap();
    let workspace_root = ws_tmp.path();
    let output_dir = out_tmp.path();

    // Put manifest file only in output_dir
    touch(output_dir, "github/acme/server/Cargo.toml");

    let mut repos = BTreeMap::new();
    repos.insert(RepoPath::new("github/acme/server"), make_repo_entry(Role::Primary));

    let project = ProjectName::new("test-project");
    let config = IntegrationConfig::default();
    let ctx = IntegrationContext {
        output_dir,
        workspace_root,
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };

    let detected = ctx.detect_repos_with_manifest("Cargo.toml");
    assert!(detected.is_empty(), "should not detect repos in output_dir");
}

#[test]
fn context_output_dir_and_workspace_root_can_be_same() {
    // In the primary workspace, both point to the same directory.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/package.json");

    let mut repos = BTreeMap::new();
    repos.insert(RepoPath::new("github/acme/server"), make_repo_entry(Role::Primary));

    let project = ProjectName::new("test-project");
    let config = IntegrationConfig::default();
    let ctx = IntegrationContext {
        output_dir: root,
        workspace_root: root,
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };

    let detected = ctx.detect_repos_with_manifest("package.json");
    assert_eq!(detected, vec!["github/acme/server"]);
}

// ---------------------------------------------------------------------------
// Default lock hook (no-op)
// ---------------------------------------------------------------------------

#[test]
fn default_lock_hook_is_noop() {
    let mock = MockIntegration::new("test-integration", true);

    let repos = BTreeMap::new();
    let project = ProjectName::new("test-project");
    let config = IntegrationConfig::default();
    let ctx = IntegrationContext {
        output_dir: Path::new("/workspace"),
        workspace_root: Path::new("/workspace"),
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };

    // Default lock() should succeed and do nothing
    let result = mock.lock(&ctx);
    assert!(result.is_ok());

    // No calls should have been logged (lock is a default no-op)
    assert!(mock.calls().is_empty());
}

/// A mock integration that overrides the lock hook.
#[derive(Clone)]
struct MockIntegrationWithLock {
    name: String,
    call_log: Arc<Mutex<Vec<(String, String)>>>,
}

impl MockIntegrationWithLock {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            call_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<(String, String)> {
        self.call_log.lock().unwrap().clone()
    }
}

impl Integration for MockIntegrationWithLock {
    fn name(&self) -> &str {
        &self.name
    }

    fn default_enabled(&self) -> bool {
        true
    }

    fn activate(&self, _ctx: &IntegrationContext) -> anyhow::Result<()> {
        Ok(())
    }

    fn deactivate(&self, _root: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn check(&self, _ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        Ok(vec![])
    }

    fn lock(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        self.call_log.lock().unwrap().push((
            "lock".into(),
            format!("project={}", ctx.project.as_str()),
        ));
        Ok(())
    }
}

#[test]
fn overridden_lock_hook_is_called() {
    let integration = MockIntegrationWithLock::new("cargo");

    let repos = BTreeMap::new();
    let project = ProjectName::new("my-project");
    let config = IntegrationConfig::default();
    let ctx = IntegrationContext {
        output_dir: Path::new("/workspace"),
        workspace_root: Path::new("/workspace"),
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };

    integration.lock(&ctx).unwrap();

    let calls = integration.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "lock");
    assert_eq!(calls[0].1, "project=my-project");
}

// ---------------------------------------------------------------------------
// Default generated_files() (empty)
// ---------------------------------------------------------------------------

#[test]
fn default_generated_files_returns_empty() {
    let mock = MockIntegration::new("test-integration", true);

    let repos = BTreeMap::new();
    let project = ProjectName::new("test-project");
    let config = IntegrationConfig::default();
    let ctx = IntegrationContext {
        output_dir: Path::new("/workspace"),
        workspace_root: Path::new("/workspace"),
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };

    let files = mock.generated_files(&ctx);
    assert!(files.is_empty(), "default generated_files should return empty vec");
}

// ---------------------------------------------------------------------------
// generated_files() for built-in integrations
// ---------------------------------------------------------------------------

#[test]
fn cargo_workspace_generated_files() {
    use repoweave::integrations::CargoWorkspace;

    // No matching repos → empty
    let repos = BTreeMap::new();
    let project = ProjectName::new("test-project");
    let config = IntegrationConfig::default();
    let tmp = TempDir::new().unwrap();
    let ctx = IntegrationContext {
        output_dir: tmp.path(),
        workspace_root: tmp.path(),
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };
    assert_eq!(CargoWorkspace.generated_files(&ctx), Vec::<String>::new());

    // Repos with Cargo.toml present → files returned
    let mut repos_with_manifest = BTreeMap::new();
    repos_with_manifest.insert(RepoPath::new("github/acme/mylib"), make_repo_entry(Role::Primary));
    touch(tmp.path(), "github/acme/mylib/Cargo.toml");
    let ctx2 = IntegrationContext {
        output_dir: tmp.path(),
        workspace_root: tmp.path(),
        project: &project,
        repos: &repos_with_manifest,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };
    assert_eq!(CargoWorkspace.generated_files(&ctx2), vec!["Cargo.toml", "Cargo.lock"]);
}

#[test]
fn npm_workspaces_generated_files() {
    use repoweave::integrations::NpmWorkspaces;

    // No matching repos → empty
    let repos = BTreeMap::new();
    let project = ProjectName::new("test-project");
    let config = IntegrationConfig::default();
    let tmp = TempDir::new().unwrap();
    let ctx = IntegrationContext {
        output_dir: tmp.path(),
        workspace_root: tmp.path(),
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };
    assert_eq!(NpmWorkspaces.generated_files(&ctx), Vec::<String>::new());

    // Repos with package.json present → files returned
    let mut repos_with_manifest = BTreeMap::new();
    repos_with_manifest.insert(RepoPath::new("github/acme/webapp"), make_repo_entry(Role::Primary));
    touch(tmp.path(), "github/acme/webapp/package.json");
    let ctx2 = IntegrationContext {
        output_dir: tmp.path(),
        workspace_root: tmp.path(),
        project: &project,
        repos: &repos_with_manifest,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };
    assert_eq!(NpmWorkspaces.generated_files(&ctx2), vec!["package.json", "package-lock.json"]);
}

#[test]
fn pnpm_workspaces_generated_files() {
    use repoweave::integrations::PnpmWorkspaces;

    // No matching repos → empty
    let repos = BTreeMap::new();
    let project = ProjectName::new("test-project");
    let config = IntegrationConfig::default();
    let tmp = TempDir::new().unwrap();
    let ctx = IntegrationContext {
        output_dir: tmp.path(),
        workspace_root: tmp.path(),
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };
    assert_eq!(PnpmWorkspaces.generated_files(&ctx), Vec::<String>::new());

    // Repos with package.json present → files returned
    let mut repos_with_manifest = BTreeMap::new();
    repos_with_manifest.insert(RepoPath::new("github/acme/frontend"), make_repo_entry(Role::Primary));
    touch(tmp.path(), "github/acme/frontend/package.json");
    let ctx2 = IntegrationContext {
        output_dir: tmp.path(),
        workspace_root: tmp.path(),
        project: &project,
        repos: &repos_with_manifest,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };
    assert_eq!(PnpmWorkspaces.generated_files(&ctx2), vec!["pnpm-workspace.yaml", "pnpm-lock.yaml"]);
}

#[test]
fn go_work_generated_files() {
    use repoweave::integrations::GoWork;

    // No matching repos → empty
    let repos = BTreeMap::new();
    let project = ProjectName::new("test-project");
    let config = IntegrationConfig::default();
    let tmp = TempDir::new().unwrap();
    let ctx = IntegrationContext {
        output_dir: tmp.path(),
        workspace_root: tmp.path(),
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };
    assert_eq!(GoWork.generated_files(&ctx), Vec::<String>::new());

    // Repos with go.mod present → files returned
    let mut repos_with_manifest = BTreeMap::new();
    repos_with_manifest.insert(RepoPath::new("github/acme/svc"), make_repo_entry(Role::Primary));
    touch(tmp.path(), "github/acme/svc/go.mod");
    let ctx2 = IntegrationContext {
        output_dir: tmp.path(),
        workspace_root: tmp.path(),
        project: &project,
        repos: &repos_with_manifest,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };
    assert_eq!(GoWork.generated_files(&ctx2), vec!["go.work", "go.sum"]);
}

#[test]
fn uv_workspace_generated_files() {
    use repoweave::integrations::UvWorkspace;

    // No matching repos → empty
    let repos = BTreeMap::new();
    let project = ProjectName::new("test-project");
    let config = IntegrationConfig::default();
    let tmp = TempDir::new().unwrap();
    let ctx = IntegrationContext {
        output_dir: tmp.path(),
        workspace_root: tmp.path(),
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };
    assert_eq!(UvWorkspace.generated_files(&ctx), Vec::<String>::new());

    // Repos with pyproject.toml present → files returned
    let mut repos_with_manifest = BTreeMap::new();
    repos_with_manifest.insert(RepoPath::new("github/acme/pylib"), make_repo_entry(Role::Primary));
    touch(tmp.path(), "github/acme/pylib/pyproject.toml");
    let ctx2 = IntegrationContext {
        output_dir: tmp.path(),
        workspace_root: tmp.path(),
        project: &project,
        repos: &repos_with_manifest,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };
    assert_eq!(UvWorkspace.generated_files(&ctx2), vec!["pyproject.toml", "uv.lock"]);
}

#[test]
fn gita_generated_files() {
    use repoweave::integrations::Gita;

    let repos = BTreeMap::new();
    let project = ProjectName::new("test-project");
    let config = IntegrationConfig::default();
    let ctx = IntegrationContext {
        output_dir: Path::new("/workspace"),
        workspace_root: Path::new("/workspace"),
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };

    let files = Gita.generated_files(&ctx);
    assert_eq!(files, vec!["gita/repos.csv", "gita/groups.csv"]);
}

#[test]
fn vscode_workspace_generated_files_includes_project_name() {
    use repoweave::integrations::VscodeWorkspace;

    let repos = BTreeMap::new();
    let project = ProjectName::new("web-app");
    let config = IntegrationConfig::default();
    let ctx = IntegrationContext {
        output_dir: Path::new("/workspace"),
        workspace_root: Path::new("/workspace"),
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };

    let files = VscodeWorkspace.generated_files(&ctx);
    assert_eq!(files, vec!["web-app.code-workspace"]);
}

#[test]
fn vscode_workspace_generated_files_varies_with_project() {
    use repoweave::integrations::VscodeWorkspace;

    let repos = BTreeMap::new();
    let config = IntegrationConfig::default();

    // Different project name produces different filename
    let project = ProjectName::new("mobile-app");
    let ctx = IntegrationContext {
        output_dir: Path::new("/workspace"),
        workspace_root: Path::new("/workspace"),
        project: &project,
        repos: &repos,
        config: &config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
    };

    let files = VscodeWorkspace.generated_files(&ctx);
    assert_eq!(files, vec!["mobile-app.code-workspace"]);
}
