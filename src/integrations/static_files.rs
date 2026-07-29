//! Static-files integration.
//!
//! Symlinks declared files from the project directory to the workspace root on
//! activate, and removes them on deactivate. Configured in the `integrations:`
//! section of `rwv.yaml` with a list of filenames. Default disabled.
//!
//! Example config:
//!
//! ```yaml
//! integrations:
//!   static-files:
//!     enabled: true
//!     files: [turbo.json, .eslintrc.json, .prettierrc]
//! ```
//!
//! # Collision with `workweave.link`
//!
//! A name that appears in BOTH `integrations.static-files.files` AND
//! `workweave.link` is rejected as a hard `Severity::Error`. The two
//! sections have incompatible semantics in a workweave:
//!
//! - `workweave.link` creates an **absolute** symlink to the primary's
//!   canonical path (shared state — same on-disk file from primary's
//!   perspective).
//! - `static-files.files` creates a **relative** symlink into the workweave's
//!   own project checkout (surfacing project content for ecosystem tools).
//!
//! Silently picking one of the two is a footgun. The fix has two layers: the
//! framework's owner-scoped removal predicate preserves the `workweave.link`
//! symlink at activation time (defensive), and this integration raises a loud
//! Severity::Error pre-activate so an operator who wrote the conflicting
//! config sees a clear message rather than relying on the framework's
//! tie-breaking (defense in depth).

use crate::integration::{Integration, IntegrationContext, Issue, IssueKind, Severity};
use serde::Deserialize;
use std::path::Path;

/// Integration-specific settings for the `static-files` integration.
#[derive(Deserialize, Default)]
struct StaticFilesConfig {
    #[serde(default)]
    files: Vec<String>,
}

pub struct StaticFiles;

/// Names declared in BOTH `static-files.files` and `workweave.link`, sorted.
///
/// Operators who write this combination almost certainly meant only one of
/// the two; the integration refuses to guess. Returns the sorted unique
/// collision set so callers can emit one Issue per collision or fail with a
/// single aggregate error message.
fn collision_names(cfg: &StaticFilesConfig, ctx: &IntegrationContext) -> Vec<String> {
    let Some(workweave) = ctx.workweave else {
        return Vec::new();
    };
    if workweave.link.is_empty() || cfg.files.is_empty() {
        return Vec::new();
    }
    let link_set: std::collections::BTreeSet<&str> =
        workweave.link.iter().map(|s| s.as_str()).collect();
    let mut hits: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for f in &cfg.files {
        if link_set.contains(f.as_str()) {
            hits.insert(f.clone());
        }
    }
    hits.into_iter().collect()
}

/// Human-readable message for a single colliding name. Names both integrations
/// so the operator can fix the rwv.yaml without re-reading the doc string.
fn collision_message(name: &str) -> String {
    format!(
        "name '{name}' is declared in both static-files.files and workweave.link; \
         pick one (workweave.link absolutizes to the primary, static-files surfaces project content)"
    )
}

impl Integration for StaticFiles {
    fn name(&self) -> &str {
        "static-files"
    }

    fn default_enabled(&self) -> bool {
        false
    }

    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        // Static files live in the project directory and are symlinked to the
        // workspace root by the activation framework (via `generated_files()`).
        // The activate hook itself is a no-op — it does not need to generate
        // any files. The files are expected to already exist in the project dir.
        //
        // We validate here that declared files actually exist so that the user
        // gets early feedback (activation still succeeds — missing files are
        // simply skipped by the symlink machinery in activate.rs).
        let cfg: StaticFilesConfig = ctx.config.settings()?;

        // Defense in depth: even though `run_checks` runs in Context-mode
        // activation (and `run_activations` itself drives `check`-then-bail in
        // Intent mode via report_and_check_activation_issues), a hand-edited
        // rwv.yaml mid-session or a future call site that skips checks would
        // silently fall through to the framework predicate. A loud bail here
        // keeps the contract explicit: static-files refuses to author symlinks
        // for a name owned by workweave.link.
        let collisions = collision_names(&cfg, ctx);
        if !collisions.is_empty() {
            // bail with the first collision's message — operators almost
            // always have a single offending entry, so a focused message is
            // more actionable than a comma-joined list. check() reports them
            // all individually for `rwv doctor`.
            anyhow::bail!("{}", collision_message(&collisions[0]));
        }

        for file in &cfg.files {
            let path = ctx.output_dir.join(file);
            if !path.exists() {
                eprintln!(
                    "[warning] static-files: declared file '{}' not found in project directory",
                    file
                );
            }
        }
        Ok(())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        // Symlink removal is handled by the activation framework
        // (remove_activation_symlinks in activate.rs), which removes any
        // symlink at the workspace root whose target points into `projects/`.
        // We don't need to do anything extra here since the static files are
        // plain symlinks into the project directory.
        let _ = root;
        Ok(())
    }

    fn check(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        let cfg: StaticFilesConfig = ctx.config.settings()?;
        let mut issues = Vec::new();

        // Collision with workweave.link: hard Severity::Error so
        // `rwv doctor` surfaces it pre-activation. The same predicate is
        // re-run in `activate()` as a defense-in-depth bail.
        for name in collision_names(&cfg, ctx) {
            issues.push(Issue {
                integration: self.name().to_string(),
                severity: Severity::Error,
                message: collision_message(&name),
                kind: IssueKind::ConfigRejected,
                safe_to_fix: true,
            });
        }

        for file in &cfg.files {
            let path = ctx.output_dir.join(file);
            if !path.exists() {
                issues.push(Issue {
                    integration: self.name().to_string(),
                    severity: Severity::Warning,
                    message: format!("declared file '{}' not found in project directory", file),
                    kind: IssueKind::ConfigRejected,
                    safe_to_fix: true,
                });
            }
        }
        Ok(issues)
    }

    fn generated_files(&self, ctx: &IntegrationContext) -> Vec<String> {
        ctx.config
            .settings::<StaticFilesConfig>()
            .unwrap_or_default()
            .files
    }
}
