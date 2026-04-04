//! Runner functions that drive integrations through activation, check, and
//! deactivation lifecycles.
//!
//! The shared enablement-check and error-capture logic lives in
//! [`for_each_enabled`], which each runner delegates to. Per-integration
//! context construction is handled by [`IntegrationContextBase::build_context`].
//! Errors from individual integrations are captured as `Issue`s rather than
//! aborting — one integration failing should not prevent others from running.

use std::path::{Path, PathBuf};

use crate::integration::{is_enabled, Integration, IntegrationContext, Issue, Severity};
use crate::manifest::{IntegrationConfig, Manifest, ProjectName};

/// Shared base data for constructing `IntegrationContext` per integration.
pub struct IntegrationContextBase<'a> {
    /// Directory where generated files should be written.
    pub output_dir: &'a Path,
    /// Workspace root where repos live on disk.
    pub workspace_root: &'a Path,
    /// Active project name.
    pub project: &'a ProjectName,
    /// All git repos found on disk under registry directories.
    pub all_repos_on_disk: &'a [PathBuf],
    /// All project paths.
    pub all_project_paths: &'a [String],
}

impl<'a> IntegrationContextBase<'a> {
    /// Build an `IntegrationContext` from this base and per-integration config.
    fn build_context(
        &self,
        config: &'a IntegrationConfig,
        manifest: &'a Manifest,
    ) -> IntegrationContext<'a> {
        IntegrationContext {
            output_dir: self.output_dir,
            workspace_root: self.workspace_root,
            project: self.project,
            repos: &manifest.repositories,
            config,
            all_repos_on_disk: self.all_repos_on_disk,
            all_project_paths: self.all_project_paths,
        }
    }
}

/// Iterate over enabled integrations, calling `f` for each one. Errors
/// returned by `f` are captured as `Issue`s with `Severity::Error` so that
/// one failing integration does not prevent others from running.
fn for_each_enabled(
    integrations: &[&dyn Integration],
    manifest: &Manifest,
    mut f: impl FnMut(&dyn Integration, &IntegrationConfig) -> Result<Vec<Issue>, anyhow::Error>,
) -> Vec<Issue> {
    let default_config = IntegrationConfig::default();
    let mut issues = Vec::new();

    for integration in integrations {
        let config = manifest
            .integrations
            .get(integration.name())
            .unwrap_or(&default_config);

        if !is_enabled(*integration, config) {
            continue;
        }

        match f(*integration, config) {
            Ok(new_issues) => issues.extend(new_issues),
            Err(e) => {
                issues.push(Issue {
                    integration: integration.name().to_string(),
                    severity: Severity::Error,
                    message: e.to_string(),
                });
            }
        }
    }

    issues
}

/// Run activation for each enabled integration, collecting issues.
///
/// If an integration's `activate()` returns an error, the error is captured
/// as an `Issue` with `Severity::Error` and execution continues with the
/// remaining integrations.
pub fn run_activations(
    integrations: &[&dyn Integration],
    manifest: &Manifest,
    ctx_base: &IntegrationContextBase,
) -> Vec<Issue> {
    for_each_enabled(integrations, manifest, |integration, config| {
        let ctx = ctx_base.build_context(config, manifest);
        integration
            .activate(&ctx)
            .map_err(|e| anyhow::anyhow!("activation failed: {e}"))?;
        Ok(Vec::new())
    })
}

/// Run checks for each enabled integration, collecting issues.
///
/// If an integration's `check()` returns an error, the error is captured
/// as an `Issue` with `Severity::Error` and execution continues.
pub fn run_checks(
    integrations: &[&dyn Integration],
    manifest: &Manifest,
    ctx_base: &IntegrationContextBase,
) -> Vec<Issue> {
    for_each_enabled(integrations, manifest, |integration, config| {
        let ctx = ctx_base.build_context(config, manifest);
        integration
            .check(&ctx)
            .map_err(|e| anyhow::anyhow!("check failed: {e}"))
    })
}

/// Run lock hooks for each enabled integration, collecting issues.
///
/// Called after `rwv lock` writes `rwv.lock`. Each integration's `lock()`
/// method is invoked so it can run ecosystem-specific lock commands
/// (e.g., `cargo generate-lockfile`). If a lock hook returns an error,
/// it is captured as an `Issue` and execution continues.
pub fn run_lock_hooks(
    integrations: &[&dyn Integration],
    manifest: &Manifest,
    ctx_base: &IntegrationContextBase,
) -> Vec<Issue> {
    for_each_enabled(integrations, manifest, |integration, config| {
        let ctx = ctx_base.build_context(config, manifest);
        integration
            .lock(&ctx)
            .map_err(|e| anyhow::anyhow!("lock hook failed: {e}"))?;
        Ok(Vec::new())
    })
}

/// Run deactivation for each enabled integration, collecting issues.
///
/// Deactivation only needs the root path (no per-integration context).
/// If an integration's `deactivate()` returns an error, it is captured
/// as an `Issue` and execution continues.
pub fn run_deactivations(
    integrations: &[&dyn Integration],
    manifest: &Manifest,
    root: &Path,
) -> Vec<Issue> {
    for_each_enabled(integrations, manifest, |integration, _config| {
        integration
            .deactivate(root)
            .map_err(|e| anyhow::anyhow!("deactivation failed: {e}"))?;
        Ok(Vec::new())
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{RepoEntry, RepoPath};
    use crate::vcs::RefName;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    // -- Mock integration -----------------------------------------------------

    #[derive(Clone)]
    struct MockIntegration {
        name: String,
        default_enabled: bool,
        activate_err: Option<String>,
        check_issues: Vec<Issue>,
        check_err: Option<String>,
        deactivate_err: Option<String>,
        lock_err: Option<String>,
        call_log: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl MockIntegration {
        fn new(name: &str, default_enabled: bool) -> Self {
            Self {
                name: name.to_string(),
                default_enabled,
                activate_err: None,
                check_issues: Vec::new(),
                check_err: None,
                deactivate_err: None,
                lock_err: None,
                call_log: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn with_activate_err(mut self, msg: &str) -> Self {
            self.activate_err = Some(msg.to_string());
            self
        }

        fn with_check_err(mut self, msg: &str) -> Self {
            self.check_err = Some(msg.to_string());
            self
        }

        fn with_check_issues(mut self, issues: Vec<Issue>) -> Self {
            self.check_issues = issues;
            self
        }

        fn with_deactivate_err(mut self, msg: &str) -> Self {
            self.deactivate_err = Some(msg.to_string());
            self
        }

        fn with_lock_err(mut self, msg: &str) -> Self {
            self.lock_err = Some(msg.to_string());
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
            if let Some(ref msg) = self.activate_err {
                anyhow::bail!("{msg}");
            }
            Ok(())
        }

        fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
            self.call_log.lock().unwrap().push((
                "deactivate".into(),
                format!("root={}", root.display()),
            ));
            if let Some(ref msg) = self.deactivate_err {
                anyhow::bail!("{msg}");
            }
            Ok(())
        }

        fn check(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
            self.call_log.lock().unwrap().push((
                "check".into(),
                format!("project={}", ctx.project.as_str()),
            ));
            if let Some(ref msg) = self.check_err {
                anyhow::bail!("{msg}");
            }
            Ok(self.check_issues.clone())
        }

        fn lock(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
            self.call_log.lock().unwrap().push((
                "lock".into(),
                format!("project={}", ctx.project.as_str()),
            ));
            if let Some(ref msg) = self.lock_err {
                anyhow::bail!("{msg}");
            }
            Ok(())
        }
    }

    // -- Helpers --------------------------------------------------------------

    fn make_manifest(integration_configs: BTreeMap<String, IntegrationConfig>) -> Manifest {
        let mut repos = BTreeMap::new();
        repos.insert(
            RepoPath::new("github/acme/server"),
            RepoEntry {
                vcs_type: crate::manifest::VcsType::Git,
                url: "https://github.com/acme/server.git".into(),
                version: RefName::new("main"),
                role: crate::manifest::Role::Primary,
            },
        );
        Manifest {
            repositories: repos,
            integrations: integration_configs,
            workweave: None,
        }
    }

    fn make_ctx_base(project: &ProjectName) -> IntegrationContextBase<'_> {
        IntegrationContextBase {
            output_dir: Path::new("/workspace"),
            workspace_root: Path::new("/workspace"),
            project,
            all_repos_on_disk: &[],
            all_project_paths: &[],
        }
    }

    // ========================================================================
    // run_activations tests
    // ========================================================================

    #[test]
    fn activations_runs_enabled_skips_disabled() {
        let enabled = MockIntegration::new("cargo", true);
        let disabled = MockIntegration::new("npm", false);
        let integrations: Vec<&dyn Integration> = vec![&enabled, &disabled];

        let manifest = make_manifest(BTreeMap::new());
        let project = ProjectName::new("test-project");
        let ctx_base = make_ctx_base(&project);

        let issues = run_activations(&integrations, &manifest, &ctx_base);
        assert!(issues.is_empty());

        assert_eq!(enabled.calls().len(), 1);
        assert_eq!(enabled.calls()[0].0, "activate");
        assert!(disabled.calls().is_empty());
    }

    #[test]
    fn activations_config_override_enables_disabled() {
        let integration = MockIntegration::new("npm", false);
        let integrations: Vec<&dyn Integration> = vec![&integration];

        let mut configs = BTreeMap::new();
        configs.insert(
            "npm".to_string(),
            IntegrationConfig {
                enabled: Some(true),
                ..Default::default()
            },
        );
        let manifest = make_manifest(configs);
        let project = ProjectName::new("test-project");
        let ctx_base = make_ctx_base(&project);

        let issues = run_activations(&integrations, &manifest, &ctx_base);
        assert!(issues.is_empty());
        assert_eq!(integration.calls().len(), 1);
    }

    #[test]
    fn activations_config_override_disables_enabled() {
        let integration = MockIntegration::new("cargo", true);
        let integrations: Vec<&dyn Integration> = vec![&integration];

        let mut configs = BTreeMap::new();
        configs.insert(
            "cargo".to_string(),
            IntegrationConfig {
                enabled: Some(false),
                ..Default::default()
            },
        );
        let manifest = make_manifest(configs);
        let project = ProjectName::new("test-project");
        let ctx_base = make_ctx_base(&project);

        let issues = run_activations(&integrations, &manifest, &ctx_base);
        assert!(issues.is_empty());
        assert!(integration.calls().is_empty());
    }

    #[test]
    fn activations_error_captured_as_issue_others_still_run() {
        let failing = MockIntegration::new("cargo", true).with_activate_err("kaboom");
        let succeeding = MockIntegration::new("npm", true);
        let integrations: Vec<&dyn Integration> = vec![&failing, &succeeding];

        let manifest = make_manifest(BTreeMap::new());
        let project = ProjectName::new("test-project");
        let ctx_base = make_ctx_base(&project);

        let issues = run_activations(&integrations, &manifest, &ctx_base);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].integration, "cargo");
        assert_eq!(issues[0].severity, Severity::Error);
        assert!(issues[0].message.contains("kaboom"));

        // succeeding integration should still have been called
        assert_eq!(succeeding.calls().len(), 1);
        assert_eq!(succeeding.calls()[0].0, "activate");
    }

    #[test]
    fn activations_passes_correct_config_per_integration() {
        let cargo = MockIntegration::new("cargo", true);
        let npm = MockIntegration::new("npm", true);
        let integrations: Vec<&dyn Integration> = vec![&cargo, &npm];

        let mut configs = BTreeMap::new();
        configs.insert(
            "cargo".to_string(),
            IntegrationConfig {
                enabled: Some(true),
                ..Default::default()
            },
        );
        // npm gets default config (no entry)
        let manifest = make_manifest(configs);
        let project = ProjectName::new("test-project");
        let ctx_base = make_ctx_base(&project);

        let issues = run_activations(&integrations, &manifest, &ctx_base);
        assert!(issues.is_empty());

        // Both should have been called since both are default_enabled=true
        assert_eq!(cargo.calls().len(), 1);
        assert_eq!(npm.calls().len(), 1);
    }

    // ========================================================================
    // run_checks tests
    // ========================================================================

    #[test]
    fn checks_collects_issues_from_all_integrations() {
        let cargo = MockIntegration::new("cargo", true).with_check_issues(vec![Issue {
            integration: "cargo".into(),
            severity: Severity::Warning,
            message: "missing dep".into(),
        }]);
        let npm = MockIntegration::new("npm", true).with_check_issues(vec![Issue {
            integration: "npm".into(),
            severity: Severity::Error,
            message: "lockfile mismatch".into(),
        }]);
        let integrations: Vec<&dyn Integration> = vec![&cargo, &npm];

        let manifest = make_manifest(BTreeMap::new());
        let project = ProjectName::new("test-project");
        let ctx_base = make_ctx_base(&project);

        let issues = run_checks(&integrations, &manifest, &ctx_base);
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].integration, "cargo");
        assert_eq!(issues[1].integration, "npm");
    }

    #[test]
    fn checks_skips_disabled_integrations() {
        let disabled = MockIntegration::new("cargo", false);
        let integrations: Vec<&dyn Integration> = vec![&disabled];

        let manifest = make_manifest(BTreeMap::new());
        let project = ProjectName::new("test-project");
        let ctx_base = make_ctx_base(&project);

        let issues = run_checks(&integrations, &manifest, &ctx_base);
        assert!(issues.is_empty());
        assert!(disabled.calls().is_empty());
    }

    #[test]
    fn checks_error_captured_others_still_run() {
        let failing = MockIntegration::new("cargo", true).with_check_err("check exploded");
        let succeeding = MockIntegration::new("npm", true).with_check_issues(vec![Issue {
            integration: "npm".into(),
            severity: Severity::Warning,
            message: "minor issue".into(),
        }]);
        let integrations: Vec<&dyn Integration> = vec![&failing, &succeeding];

        let manifest = make_manifest(BTreeMap::new());
        let project = ProjectName::new("test-project");
        let ctx_base = make_ctx_base(&project);

        let issues = run_checks(&integrations, &manifest, &ctx_base);
        assert_eq!(issues.len(), 2);
        // First issue is the error from "cargo"
        assert_eq!(issues[0].integration, "cargo");
        assert_eq!(issues[0].severity, Severity::Error);
        assert!(issues[0].message.contains("check exploded"));
        // Second is the normal issue from "npm"
        assert_eq!(issues[1].integration, "npm");
        assert_eq!(issues[1].severity, Severity::Warning);
    }

    // ========================================================================
    // run_deactivations tests
    // ========================================================================

    #[test]
    fn deactivations_runs_enabled_skips_disabled() {
        let enabled = MockIntegration::new("cargo", true);
        let disabled = MockIntegration::new("npm", false);
        let integrations: Vec<&dyn Integration> = vec![&enabled, &disabled];

        let manifest = make_manifest(BTreeMap::new());
        let root = Path::new("/workspace");

        let issues = run_deactivations(&integrations, &manifest, root);
        assert!(issues.is_empty());

        assert_eq!(enabled.calls().len(), 1);
        assert_eq!(enabled.calls()[0].0, "deactivate");
        assert!(disabled.calls().is_empty());
    }

    #[test]
    fn deactivations_error_captured_others_still_run() {
        let failing = MockIntegration::new("cargo", true).with_deactivate_err("cleanup failed");
        let succeeding = MockIntegration::new("npm", true);
        let integrations: Vec<&dyn Integration> = vec![&failing, &succeeding];

        let manifest = make_manifest(BTreeMap::new());
        let root = Path::new("/workspace");

        let issues = run_deactivations(&integrations, &manifest, root);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].integration, "cargo");
        assert!(issues[0].message.contains("cleanup failed"));

        assert_eq!(succeeding.calls().len(), 1);
        assert_eq!(succeeding.calls()[0].0, "deactivate");
    }

    #[test]
    fn deactivations_passes_root_to_integration() {
        let integration = MockIntegration::new("cargo", true);
        let integrations: Vec<&dyn Integration> = vec![&integration];

        let manifest = make_manifest(BTreeMap::new());
        let root = Path::new("/my/workspace");

        let issues = run_deactivations(&integrations, &manifest, root);
        assert!(issues.is_empty());

        let calls = integration.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, "root=/my/workspace");
    }

    // ========================================================================
    // run_lock_hooks tests
    // ========================================================================

    #[test]
    fn lock_hooks_runs_enabled_skips_disabled() {
        let enabled = MockIntegration::new("cargo", true);
        let disabled = MockIntegration::new("npm", false);
        let integrations: Vec<&dyn Integration> = vec![&enabled, &disabled];

        let manifest = make_manifest(BTreeMap::new());
        let project = ProjectName::new("test-project");
        let ctx_base = make_ctx_base(&project);

        let issues = run_lock_hooks(&integrations, &manifest, &ctx_base);
        assert!(issues.is_empty());

        let enabled_calls: Vec<_> = enabled.calls().into_iter().filter(|(m, _)| m == "lock").collect();
        assert_eq!(enabled_calls.len(), 1);
        assert!(disabled.calls().is_empty());
    }

    #[test]
    fn lock_hooks_error_captured_others_still_run() {
        let failing = MockIntegration::new("cargo", true).with_lock_err("lockfile generation failed");
        let succeeding = MockIntegration::new("npm", true);
        let integrations: Vec<&dyn Integration> = vec![&failing, &succeeding];

        let manifest = make_manifest(BTreeMap::new());
        let project = ProjectName::new("test-project");
        let ctx_base = make_ctx_base(&project);

        let issues = run_lock_hooks(&integrations, &manifest, &ctx_base);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].integration, "cargo");
        assert_eq!(issues[0].severity, Severity::Error);
        assert!(issues[0].message.contains("lockfile generation failed"));

        // succeeding integration should still have been called
        let npm_lock_calls: Vec<_> = succeeding.calls().into_iter().filter(|(m, _)| m == "lock").collect();
        assert_eq!(npm_lock_calls.len(), 1);
    }

    #[test]
    fn lock_hooks_config_override_enables_disabled() {
        let integration = MockIntegration::new("npm", false);
        let integrations: Vec<&dyn Integration> = vec![&integration];

        let mut configs = BTreeMap::new();
        configs.insert(
            "npm".to_string(),
            IntegrationConfig {
                enabled: Some(true),
                ..Default::default()
            },
        );
        let manifest = make_manifest(configs);
        let project = ProjectName::new("test-project");
        let ctx_base = make_ctx_base(&project);

        let issues = run_lock_hooks(&integrations, &manifest, &ctx_base);
        assert!(issues.is_empty());

        let lock_calls: Vec<_> = integration.calls().into_iter().filter(|(m, _)| m == "lock").collect();
        assert_eq!(lock_calls.len(), 1);
    }

    #[test]
    fn lock_hooks_passes_correct_project() {
        let integration = MockIntegration::new("cargo", true);
        let integrations: Vec<&dyn Integration> = vec![&integration];

        let manifest = make_manifest(BTreeMap::new());
        let project = ProjectName::new("my-special-project");
        let ctx_base = make_ctx_base(&project);

        let issues = run_lock_hooks(&integrations, &manifest, &ctx_base);
        assert!(issues.is_empty());

        let lock_calls: Vec<_> = integration.calls().into_iter().filter(|(m, _)| m == "lock").collect();
        assert_eq!(lock_calls.len(), 1);
        assert_eq!(lock_calls[0].1, "project=my-special-project");
    }
}
