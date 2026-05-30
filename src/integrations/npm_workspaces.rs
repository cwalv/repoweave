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

        // Read any existing package.json and merge into it, preserving user-owned
        // fields (scripts, devDependencies, engines, version, etc.).  Only the
        // three rwv-owned keys are overwritten; everything else survives untouched.
        let path = ctx.output_dir.join("package.json");
        let mut obj: serde_json::Map<String, serde_json::Value> = path
            .exists()
            .then(|| std::fs::read_to_string(&path).ok())
            .flatten()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default();

        obj.insert(
            "name".into(),
            serde_json::Value::String(GENERATED_HEADER.into()),
        );
        obj.insert("private".into(), serde_json::Value::Bool(true));
        obj.insert("workspaces".into(), serde_json::Value::Array(workspaces));

        let content = serde_json::to_string_pretty(&obj)? + "\n";
        std::fs::write(&path, content)?;
        Ok(())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        let path = root.join("package.json");
        if !path.exists() {
            return Ok(());
        }

        let content = std::fs::read_to_string(&path)?;
        let Ok(serde_json::Value::Object(mut obj)) =
            serde_json::from_str::<serde_json::Value>(&content)
        else {
            // Not valid JSON or not an object — leave the file alone.
            return Ok(());
        };

        // Only touch files that rwv owns (identified by the sentinel name).
        if obj.get("name").and_then(|v| v.as_str()) != Some(GENERATED_HEADER) {
            return Ok(());
        }

        // Strip the three rwv-owned keys.
        obj.remove("name");
        obj.remove("private");
        obj.remove("workspaces");

        if obj.is_empty() {
            // Nothing left — remove the file entirely.
            std::fs::remove_file(path)?;
        } else {
            // User-authored fields remain — write the stripped object back.
            let content = serde_json::to_string_pretty(&serde_json::Value::Object(obj))? + "\n";
            std::fs::write(path, content)?;
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

    fn activate_hook(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        let paths = ctx.detect_repos_with_manifest("package.json");
        if paths.is_empty() {
            return Ok(());
        }

        // Full `npm install` (not `--package-lock-only`): activation is
        // the moment the workspace's membership becomes current, so
        // `node_modules` should be in sync. The hook used to be
        // `--package-lock-only` because it fired from `rwv lock`, which
        // is the wrong trigger.
        //
        // Run from workspace_root: the symlink at the root points at the
        // canonical package.json in output_dir, and workspace member
        // paths are resolved relative to the symlink location.
        let status = std::process::Command::new("npm")
            .args(["install"])
            .current_dir(ctx.workspace_root)
            .status()
            .map_err(|e| anyhow::anyhow!("failed to run npm: {e}"))?;

        if !status.success() {
            anyhow::bail!("npm install failed (exit {})", status);
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
