//! Integration trait and context.
//!
//! Integrations are pluggable units that derive config for one ecosystem tool
//! from the repo list. Each integration participates in activation (write path)
//! and check (read-only inspection).

use crate::manifest::{IntegrationConfig, ProjectName, RepoEntry, RepoPath};
use std::collections::BTreeMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// Integration context — shared input for all integrations
// ---------------------------------------------------------------------------

/// Everything an integration needs to do its work.
///
/// Constructed once per activation/check cycle and passed to each integration.
/// Immutable — integrations read this, then write to the filesystem.
pub struct IntegrationContext<'a> {
    /// The directory where generated files should be written
    /// (primary root or workweave directory).
    pub output_dir: &'a Path,

    /// The workspace root where repos live on disk. Used for detecting
    /// manifest files (e.g., `Cargo.toml`, `package.json`) inside repos.
    /// In the primary workspace this equals `output_dir`; in a workweave it
    /// points to the primary workspace root so that repo detection still
    /// works even when repo clones are not duplicated into the workweave dir.
    pub workspace_root: &'a Path,

    /// The active project name.
    pub project: &'a ProjectName,

    /// Repo entries from the project's `rwv.yaml`, keyed by local path.
    pub repos: &'a BTreeMap<RepoPath, RepoEntry>,

    /// Per-integration config from the `integrations:` key in `rwv.yaml`.
    pub config: &'a IntegrationConfig,

    /// All git repos found on disk under registry directories (relative paths).
    /// Computed once, shared across integrations.
    pub all_repos_on_disk: &'a [RepoPath],

    /// All project paths (e.g., `["web-app", "mobile-app"]`).
    /// Computed once, shared across integrations.
    pub all_project_paths: &'a [String],
}

impl<'a> IntegrationContext<'a> {
    /// Repos that should appear in ecosystem workspace configs.
    /// Excludes `reference` repos — they're read-only, not part of the build graph.
    pub fn active_repos(&self) -> impl Iterator<Item = (&RepoPath, &RepoEntry)> {
        self.repos.iter().filter(|(_, e)| e.role.is_active())
    }

    /// Active repos whose directory contains a given manifest file.
    ///
    /// Shared helper for ecosystem integrations (npm, pnpm, Go, uv, Cargo)
    /// that all need the same "find repos with manifest X" logic.
    ///
    /// Uses `workspace_root` (not `output_dir`) to check for manifest files,
    /// so that repo detection works even when the output directory differs
    /// from where repos live (e.g., in weaves).
    pub fn detect_repos_with_manifest(&self, filename: &str) -> Vec<String> {
        let mut paths: Vec<String> = self
            .active_repos()
            .filter(|(rp, _)| self.workspace_root.join(rp.as_str()).join(filename).exists())
            .map(|(rp, _)| rp.as_str().to_string())
            .collect();
        paths.sort();
        paths
    }
}

// ---------------------------------------------------------------------------
// Check results
// ---------------------------------------------------------------------------

/// Severity of an issue found by a check hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

/// A single issue reported by an integration's check hook.
#[derive(Debug, Clone)]
pub struct Issue {
    pub integration: String,
    pub severity: Severity,
    pub message: String,
}

// ---------------------------------------------------------------------------
// The Integration trait
// ---------------------------------------------------------------------------

/// A pluggable unit that derives config for one tool from the repo list.
///
/// Integrations are stateless — all input comes through `IntegrationContext`,
/// all output goes to the filesystem or is returned as `Issue`s.
///
/// Built-in integrations are compiled in. The trait is object-safe so that
/// future versions can load integrations dynamically (e.g., from shared
/// libraries or WASM modules) and store them as `Box<dyn Integration>`.
pub trait Integration {
    /// Unique identifier (e.g., `"npm-workspaces"`).
    fn name(&self) -> &str;

    /// Whether this integration runs without explicit opt-in.
    fn default_enabled(&self) -> bool;

    /// Generate config files and run install commands.
    /// Called during activation, workweave creation, sync, add, and remove.
    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<()>;

    /// Remove generated files. Called during deactivation.
    fn deactivate(&self, root: &Path) -> anyhow::Result<()>;

    /// Read-only inspection. Returns issues without changing state.
    /// Called by `rwv check`.
    fn check(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>>;

    /// Lock hook — run after `rwv lock` writes `rwv.lock`.
    ///
    /// Integrations can override this to run ecosystem lock commands
    /// (e.g., `npm install --package-lock-only`, `cargo generate-lockfile`).
    /// The default implementation is a no-op.
    fn lock(&self, _ctx: &IntegrationContext) -> anyhow::Result<()> {
        Ok(())
    }

    /// Return the filenames (relative to `output_dir`) that this integration
    /// generates during activation.
    ///
    /// Used by the framework to track generated files for cleanup, diffing,
    /// and `.gitignore` management. Integrations that generate files should
    /// override this. The default returns an empty list.
    fn generated_files(&self, _ctx: &IntegrationContext) -> Vec<String> {
        Vec::new()
    }
}

/// Whether an integration should run, considering its default and any override.
pub fn is_enabled(integration: &dyn Integration, config: &IntegrationConfig) -> bool {
    config.enabled().unwrap_or_else(|| integration.default_enabled())
}
