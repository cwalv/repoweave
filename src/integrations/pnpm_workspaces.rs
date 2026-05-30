use crate::integration::{Integration, IntegrationContext, Issue, Severity};
use anyhow::Context;
use std::path::Path;

pub struct PnpmWorkspaces;

impl Integration for PnpmWorkspaces {
    fn name(&self) -> &str {
        "pnpm-workspaces"
    }

    fn default_enabled(&self) -> bool {
        false
    }

    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        let paths = ctx.detect_repos_with_manifest("package.json");
        if paths.is_empty() {
            return Ok(());
        }

        let mut content = String::from("packages:\n");
        for p in &paths {
            content.push_str(&format!("  - {}\n", p));
        }

        std::fs::write(ctx.output_dir.join("pnpm-workspace.yaml"), content)?;
        Ok(())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        let path = root.join("pnpm-workspace.yaml");
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    fn check(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        let paths = ctx.detect_repos_with_manifest("package.json");
        if paths.is_empty() {
            return Ok(vec![]);
        }

        let mut issues = Vec::new();
        if which::which("pnpm").is_err() {
            issues.push(Issue {
                integration: self.name().to_string(),
                severity: Severity::Warning,
                message: "pnpm is not on PATH".to_string(),
            });
        }
        Ok(issues)
    }

    fn activate_hook(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        let paths = ctx.detect_repos_with_manifest("package.json");
        if paths.is_empty() {
            return Ok(());
        }

        // Full `pnpm install` (not `--lockfile-only`): activation is the
        // moment to bring `node_modules` in sync; the hook fired from
        // `rwv lock` previously and used `--lockfile-only` for that
        // reason. Run from workspace_root for the same reason npm does —
        // that's where the symlinks make member paths resolve correctly.
        let status = std::process::Command::new("pnpm")
            .args(["install"])
            .current_dir(ctx.workspace_root)
            .status()
            .context("failed to run pnpm")?;

        if !status.success() {
            anyhow::bail!("pnpm install failed (exit {})", status);
        }

        Ok(())
    }

    fn generated_files(&self, ctx: &IntegrationContext) -> Vec<String> {
        if ctx.detect_repos_with_manifest("package.json").is_empty() {
            return vec![];
        }
        vec![
            "pnpm-workspace.yaml".to_string(),
            "pnpm-lock.yaml".to_string(),
        ]
    }
}
