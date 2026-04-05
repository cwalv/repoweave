use crate::integration::{Integration, IntegrationContext, Issue, Severity};
use std::path::Path;

const GENERATED_HEADER: &str = "repoweave";

pub struct NpmWorkspaces;

impl Integration for NpmWorkspaces {
    fn name(&self) -> &str {
        "npm-workspaces"
    }

    fn default_enabled(&self) -> bool {
        true
    }

    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        let paths = ctx.detect_repos_with_manifest("package.json");
        if paths.is_empty() {
            return Ok(());
        }

        let workspaces: Vec<serde_json::Value> = paths
            .iter()
            .map(|p| serde_json::Value::String(p.clone()))
            .collect();

        let obj = serde_json::json!({
            "name": GENERATED_HEADER,
            "private": true,
            "workspaces": workspaces,
        });

        let content = serde_json::to_string_pretty(&obj)? + "\n";
        std::fs::write(ctx.output_dir.join("package.json"), content)?;
        Ok(())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        let path = root.join("package.json");
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                if val.get("name").and_then(|v| v.as_str()) == Some(GENERATED_HEADER) {
                    std::fs::remove_file(path)?;
                }
            }
        }
        Ok(())
    }

    fn check(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        let paths = ctx.detect_repos_with_manifest("package.json");
        if paths.is_empty() {
            return Ok(vec![]);
        }

        let mut issues = Vec::new();
        if which::which("npm").is_err() {
            issues.push(Issue {
                integration: self.name().to_string(),
                severity: Severity::Warning,
                message: "npm is not on PATH".to_string(),
            });
        }
        Ok(issues)
    }

    fn lock(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        let paths = ctx.detect_repos_with_manifest("package.json");
        if paths.is_empty() {
            return Ok(());
        }

        let status = std::process::Command::new("npm")
            .args(["install", "--package-lock-only"])
            .current_dir(ctx.output_dir)
            .status()
            .map_err(|e| anyhow::anyhow!("failed to run npm: {e}"))?;

        if !status.success() {
            anyhow::bail!("npm install --package-lock-only failed (exit {})", status);
        }

        Ok(())
    }

    fn generated_files(&self, ctx: &IntegrationContext) -> Vec<String> {
        if ctx.detect_repos_with_manifest("package.json").is_empty() {
            return vec![];
        }
        vec!["package.json".to_string(), "package-lock.json".to_string()]
    }
}
