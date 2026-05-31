use crate::integration::{Integration, IntegrationContext, Issue, Severity};
use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::Path;

pub struct Gita;

impl Integration for Gita {
    fn name(&self) -> &str {
        "gita"
    }

    fn default_enabled(&self) -> bool {
        false
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

        {
            let repos_file = std::fs::File::create(gita_dir.join("repos.csv"))?;
            let mut wtr = csv::Writer::from_writer(repos_file);
            wtr.write_record(["path", "name", "flags"])?;
            for (abs_path, name) in &repo_entries {
                wtr.write_record([abs_path.as_str(), name.as_str(), ""])?;
            }
            wtr.flush()?;
        }

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

        {
            let groups_file = std::fs::File::create(gita_dir.join("groups.csv"))?;
            let mut wtr = csv::Writer::from_writer(groups_file);
            wtr.write_record(["group", "repos"])?;
            for (group, mut repos) in groups {
                repos.sort();
                wtr.write_record([group.as_str(), repos.join(" ").as_str()])?;
            }
            wtr.flush()?;
        }

        Ok(())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        let gita_dir = root.join("gita");
        if !gita_dir.exists() {
            return Ok(());
        }

        // Remove the two rwv-owned CSVs; ignore NotFound in case one was
        // already absent.
        for filename in &["repos.csv", "groups.csv"] {
            match std::fs::remove_file(gita_dir.join(filename)) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }

        // Remove the directory only when it is now empty; if the user has
        // parked anything else under gita/ (e.g. notes.txt), leave it alone.
        // std::fs::remove_dir returns an error for non-empty directories;
        // we silently swallow that case.
        match std::fs::remove_dir(&gita_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) if e.kind() == ErrorKind::DirectoryNotEmpty => {}
            // On some platforms a non-empty remove_dir returns Other/ENOTEMPTY.
            // We do a best-effort check: if the directory still exists, it is
            // non-empty and that is intentional.
            Err(_) if gita_dir.exists() => {}
            Err(e) => return Err(e.into()),
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
                safe_to_fix: true,
            });
        }
        Ok(issues)
    }

    fn generated_files(&self, _ctx: &IntegrationContext) -> Vec<String> {
        vec!["gita/repos.csv".to_string(), "gita/groups.csv".to_string()]
    }
}
