use crate::integration::{Integration, IntegrationContext, Issue, Severity};
use std::collections::BTreeMap;
use std::path::Path;

pub struct Gita;

impl Integration for Gita {
    fn name(&self) -> &str {
        "gita"
    }

    fn default_enabled(&self) -> bool {
        true
    }

    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        let active: Vec<_> = ctx.active_repos().collect();
        if active.is_empty() {
            return Ok(());
        }

        let gita_dir = ctx.output_dir.join("gita");
        std::fs::create_dir_all(&gita_dir)?;

        // repos.csv — sorted by repo name (basename)
        let mut repo_entries: Vec<(String, String)> = active
            .iter()
            .map(|(rp, _)| {
                let abs_path = ctx.workspace_root.join(rp.as_str());
                let name = rp
                    .as_str()
                    .rsplit('/')
                    .next()
                    .unwrap_or(rp.as_str())
                    .to_string();
                (abs_path.to_string_lossy().into_owned(), name)
            })
            .collect();
        repo_entries.sort_by(|a, b| a.1.cmp(&b.1));

        let mut repos_csv = String::from("path,name,flags\n");
        for (abs_path, name) in &repo_entries {
            repos_csv.push_str(&format!("{},{},\n", abs_path, name));
        }
        std::fs::write(gita_dir.join("repos.csv"), repos_csv)?;

        // groups.csv — group by role, sorted by group name
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (rp, entry) in &active {
            let role_str = entry.role.as_str();
            let name = rp
                .as_str()
                .rsplit('/')
                .next()
                .unwrap_or(rp.as_str())
                .to_string();
            groups.entry(role_str.to_string()).or_default().push(name);
        }

        let mut groups_csv = String::from("group,repos\n");
        for (group, mut repos) in groups {
            repos.sort();
            groups_csv.push_str(&format!("{},{}\n", group, repos.join(" ")));
        }
        std::fs::write(gita_dir.join("groups.csv"), groups_csv)?;

        Ok(())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        let gita_dir = root.join("gita");
        if gita_dir.exists() {
            std::fs::remove_dir_all(gita_dir)?;
        }
        Ok(())
    }

    fn check(&self, _ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        let mut issues = Vec::new();
        if which::which("gita").is_err() {
            issues.push(Issue {
                integration: self.name().to_string(),
                severity: Severity::Warning,
                message: "gita is not on PATH".to_string(),
            });
        }
        Ok(issues)
    }

    fn generated_files(&self, _ctx: &IntegrationContext) -> Vec<String> {
        vec!["gita/repos.csv".to_string(), "gita/groups.csv".to_string()]
    }
}
