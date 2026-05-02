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

        let mut content = match max_go_version(&paths, ctx.workspace_root) {
            Some(v) => format!("go {v}\n\nuse (\n"),
            None => String::from("use (\n"),
        };
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

    fn generated_files(&self, ctx: &IntegrationContext) -> Vec<String> {
        if ctx.detect_repos_with_manifest("go.mod").is_empty() {
            return vec![];
        }
        vec!["go.work".to_string(), "go.sum".to_string()]
    }
}

fn max_go_version(paths: &[impl AsRef<str>], workspace_root: &Path) -> Option<String> {
    let mut max: Option<(u64, u64)> = None;
    for p in paths {
        let go_mod = workspace_root.join(p.as_ref()).join("go.mod");
        if let Ok(content) = std::fs::read_to_string(go_mod) {
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("go ") {
                    let parts: Vec<&str> = rest.trim().splitn(3, '.').collect();
                    if parts.len() >= 2 {
                        if let (Ok(maj), Ok(min)) =
                            (parts[0].parse::<u64>(), parts[1].parse::<u64>())
                        {
                            if max.is_none_or(|m| (maj, min) > m) {
                                max = Some((maj, min));
                            }
                        }
                    }
                    break;
                }
            }
        }
    }
    max.map(|(maj, min)| format!("{maj}.{min}"))
}
