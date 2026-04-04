use crate::integration::{Integration, IntegrationContext, Issue, Severity};
use std::path::Path;

pub struct GoWork;

impl Integration for GoWork {
    fn name(&self) -> &str {
        "go-work"
    }

    fn default_enabled(&self) -> bool {
        true
    }

    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        let paths = ctx.detect_repos_with_manifest("go.mod");
        if paths.is_empty() {
            return Ok(());
        }

        let mut content = String::from("go 1.21\n\nuse (\n");
        for p in &paths {
            content.push_str(&format!("    ./{}\n", p));
        }
        content.push_str(")\n");

        std::fs::write(ctx.output_dir.join("go.work"), content)?;
        Ok(())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        let path = root.join("go.work");
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    fn check(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        let paths = ctx.detect_repos_with_manifest("go.mod");
        if paths.is_empty() {
            return Ok(vec![]);
        }

        let mut issues = Vec::new();
        if which::which("go").is_err() {
            issues.push(Issue {
                integration: self.name().to_string(),
                severity: Severity::Warning,
                message: "go is not on PATH".to_string(),
            });
        }
        Ok(issues)
    }

    fn generated_files(&self, _ctx: &IntegrationContext) -> Vec<String> {
        vec!["go.work".to_string()]
    }
}
