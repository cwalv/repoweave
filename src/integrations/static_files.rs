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

use crate::integration::{Integration, IntegrationContext, Issue, Severity};
use std::path::Path;

pub struct StaticFiles;

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
        for file in &ctx.config.files {
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
        let mut issues = Vec::new();
        for file in &ctx.config.files {
            let path = ctx.output_dir.join(file);
            if !path.exists() {
                issues.push(Issue {
                    integration: self.name().to_string(),
                    severity: Severity::Warning,
                    message: format!(
                        "declared file '{}' not found in project directory",
                        file
                    ),
                });
            }
        }
        Ok(issues)
    }

    fn generated_files(&self, ctx: &IntegrationContext) -> Vec<String> {
        ctx.config.files.clone()
    }
}
