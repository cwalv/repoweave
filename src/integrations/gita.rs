use crate::integration::{
    Integration, IntegrationContext, Issue, IssueKind, OwnedPath, Severity, SurfacedFile,
};
use crate::integrations::merge::{fully_owned_drift_issue, missing_issue};
use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::Path;

pub struct Gita;

impl Gita {
    /// The exact bytes rwv authors for each CSV, or `None` when the project has
    /// no active repos and gita authors nothing.
    ///
    /// One producer for the write path and the ownership test alike: two
    /// spellings of "what rwv would write" would let the second drift into
    /// reporting a file rwv authored as one it did not.
    fn authored_csvs(ctx: &IntegrationContext) -> Option<Vec<(&'static str, String)>> {
        let active: Vec<_> = ctx.active_repos().collect();
        if active.is_empty() {
            return None;
        }

        let basename = |rp: &crate::manifest::RepoPath| {
            rp.as_str()
                .rsplit('/')
                .next()
                .unwrap_or(rp.as_str())
                .to_string()
        };

        // repos.csv — sorted by repo name (basename)
        let mut repo_entries: Vec<(String, String)> = active
            .iter()
            .map(|(rp, _)| {
                let abs_path = crate::path_spelling::native_join(ctx.workspace_root, rp.as_str());
                (abs_path, basename(rp))
            })
            .collect();
        repo_entries.sort_by(|a, b| a.1.cmp(&b.1));

        // gita reads both files with fixed field names and no header row:
        // `path,name,type,flags` and `name:repos:path`.
        let mut repos = csv::Writer::from_writer(Vec::new());
        for (abs_path, name) in &repo_entries {
            repos
                .write_record([abs_path.as_str(), name.as_str(), "", ""])
                .ok()?;
        }

        // groups.csv — group by role, sorted by group name
        let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (rp, entry) in &active {
            grouped
                .entry(entry.role.as_str().to_string())
                .or_default()
                .push(basename(rp));
        }

        let mut groups = csv::WriterBuilder::new()
            .delimiter(b':')
            .from_writer(Vec::new());
        for (group, mut names) in grouped {
            names.sort();
            groups
                .write_record([group.as_str(), names.join(" ").as_str(), ""])
                .ok()?;
        }

        Some(vec![
            (
                "gita/repos.csv",
                String::from_utf8(repos.into_inner().ok()?).ok()?,
            ),
            (
                "gita/groups.csv",
                String::from_utf8(groups.into_inner().ok()?).ok()?,
            ),
        ])
    }
}

impl Integration for Gita {
    fn name(&self) -> &str {
        "gita"
    }

    fn default_enabled(&self) -> bool {
        false
    }

    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        let Some(authored) = Self::authored_csvs(ctx) else {
            return Ok(Vec::new());
        };

        let gita_dir = ctx.output_dir.join("gita");
        std::fs::create_dir_all(&gita_dir)?;
        for (name, body) in authored {
            std::fs::write(ctx.output_dir.join(name), body)?;
        }
        Ok(Vec::new())
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
                kind: IssueKind::ToolMissing,
                safe_to_fix: true,
            });
        }
        Ok(issues)
    }

    /// Each CSV against the bytes [`Self::activate`] would write now: absent is
    /// MISSING and any other bytes are DRIFT, both repaired by regenerating.
    /// Fully owned, so there is no USER-HELD state to protect.
    fn verify(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        let mut issues = Vec::new();
        for (name, body) in Self::authored_csvs(ctx).unwrap_or_default() {
            let path = ctx.output_dir.join(name);
            match std::fs::read(&path) {
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    issues.push(missing_issue(self.name(), &path));
                }
                Err(e) => issues.push(fully_owned_drift_issue(
                    self.name(),
                    &path,
                    &format!("read failed: {e:#}"),
                )),
                Ok(on_disk) if on_disk != body.as_bytes() => {
                    issues.push(fully_owned_drift_issue(
                        self.name(),
                        &path,
                        "content differs from what rwv.toml implies",
                    ));
                }
                Ok(_) => {}
            }
        }
        Ok(issues)
    }

    /// The two CSVs, each one only where its bytes are the bytes rwv would
    /// author right now.
    ///
    /// Presence at the declared path is NOT the evidence, and gita is the
    /// integration where that distinction has teeth: keeping a hand-written
    /// `gita/repos.csv` is a reason an operator disables this integration in
    /// the first place, so attributing the file to rwv because it sits where
    /// rwv would write one is a finding whose remedy deletes the very thing the
    /// config change was protecting.
    ///
    /// Regeneration-compare is available here and is exact. These CSVs are a
    /// function of the manifest alone, so "is this mine" is decidable by
    /// authoring the content again and comparing — the same question a marker
    /// answers for a hybrid file, and the question a lockfile cannot answer at
    /// all, which is what the digest ledger exists for.
    fn owned_paths_on_disk(&self, ctx: &IntegrationContext) -> Vec<OwnedPath> {
        Self::authored_csvs(ctx)
            .unwrap_or_default()
            .into_iter()
            .filter(|(name, body)| {
                std::fs::read(ctx.output_dir.join(name))
                    .is_ok_and(|on_disk| on_disk == body.as_bytes())
            })
            .map(|(name, _)| OwnedPath::WholeFile(name.to_string()))
            .collect()
    }

    fn generated_files(&self, _ctx: &IntegrationContext) -> Vec<SurfacedFile> {
        vec![
            SurfacedFile::written_at_source("gita/repos.csv"),
            SurfacedFile::written_at_source("gita/groups.csv"),
        ]
    }
}
