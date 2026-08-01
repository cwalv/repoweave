use crate::integration::{Integration, IntegrationContext, Issue, IssueKind, Severity};
use crate::integrations::merge::{
    drift_issues, merge_activate, missing_issue, orphaned_region_issues, strip_deactivate, KeyPath,
    ManagedDoc, OwnedValue, Ownership, YamlDoc,
};
use anyhow::Context;
use saphyr::{LoadableYamlNode, YamlOwned};
use std::path::Path;

pub struct PnpmWorkspaces;

/// Return the owned [`KeyPath`] for the `packages:` key.
///
/// `packages:` is the only key rwv manages in pnpm-workspace.yaml.
/// `catalog:`, `overrides:`, `peerDependencyRules:`, `hoistPattern:`,
/// and any user comments are strictly user content and are never touched.
fn packages_key() -> KeyPath {
    vec!["packages".to_string()]
}

/// Expand detected repos into pnpm workspace entries.
///
/// pnpm reads workspace member globs from `pnpm-workspace.yaml`'s `packages:`
/// key — it does **not** use `package.json`'s `workspaces` key at all.
///
/// A repo whose root `pnpm-workspace.yaml` declares its own `packages:` list
/// is a multi-package (monorepo) member. Listing the repo root in the
/// weave-root `pnpm-workspace.yaml` would orphan its sub-packages from pnpm's
/// install/link graph. Instead, emit `<repo-path>/<glob>` for each member glob.
///
/// Repos without a `pnpm-workspace.yaml`, or whose `pnpm-workspace.yaml` has
/// no `packages:` list, keep the single `<repo-path>` entry (existing
/// behavior).
fn expand_workspace_entries(workspace_root: &Path, repo_paths: Vec<String>) -> Vec<String> {
    let mut entries = Vec::new();
    for repo in repo_paths {
        let yaml_path = workspace_root.join(&repo).join("pnpm-workspace.yaml");
        let globs = read_pnpm_packages_globs(&yaml_path);
        match globs {
            Some(globs) if !globs.is_empty() => {
                entries.extend(
                    globs
                        .iter()
                        .map(|g| format!("{}/{}", repo, g.strip_prefix("./").unwrap_or(g))),
                );
            }
            _ => entries.push(repo),
        }
    }
    entries
}

/// Read the `packages:` sequence from a `pnpm-workspace.yaml` file.
///
/// Returns `None` if the file is absent, unreadable, not valid YAML, or
/// has no `packages:` key. Non-string entries in the sequence are skipped.
fn read_pnpm_packages_globs(path: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(path).ok()?;
    let docs = YamlOwned::load_from_str(&text).ok()?;
    let packages = docs.first()?.as_mapping_get("packages")?.as_sequence()?;
    Some(
        packages
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
    )
}

impl Integration for PnpmWorkspaces {
    fn name(&self) -> &str {
        "pnpm-workspaces"
    }

    fn default_enabled(&self) -> bool {
        false
    }

    fn detection_manifests(&self) -> &[&str] {
        &["package.json"]
    }

    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        let paths = ctx.detect_repos_with_manifest("package.json");
        // The authored `packages:` list is a function of the manifest alone.
        // Returning early instead would make it a function of history too: the
        // last member's glob stays behind, in a marked key rwv still owns and
        // would no longer author.
        if paths.is_empty() {
            return Self::strip_managed_region(ctx.output_dir);
        }

        let path = ctx.output_dir.join("pnpm-workspace.yaml");

        // Expand multi-package member repos into prefixed globs before sorting.
        let paths = expand_workspace_entries(ctx.workspace_root, paths);

        // Sorted list of member paths — deterministic output.
        let mut members: Vec<String> = paths.into_iter().map(|p| p.to_string()).collect();
        members.sort();

        let owned = vec![(
            packages_key(),
            Ownership::Author,
            OwnedValue::sorted_array(members),
        )];

        merge_activate::<YamlDoc>(&path, &owned)
            .with_context(|| format!("pnpm-workspaces: activate {}", path.display()))?;

        Ok(())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        Self::strip_managed_region(root)
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
                kind: IssueKind::ToolMissing,
                safe_to_fix: true,
            });
        }
        Ok(issues)
    }

    /// Content-correct check (Axis-2) for `pnpm-workspace.yaml`.
    ///
    /// States mirrored from cargo-workspace:
    ///
    /// - **MISSING** (`safe_to_fix=true`): file absent but repos detected.
    /// - **USER-HELD** (`safe_to_fix=false`): file present, has `packages:` key,
    ///   but NO `# managed by repoweave` marker.
    /// - **DRIFT** (`safe_to_fix=true`): marker present, `packages:` content
    ///   diverges from what the current config would generate.
    /// - **CLEAN**: marker present and content matches.
    fn verify(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        let repo_paths = ctx.detect_repos_with_manifest("package.json");
        if repo_paths.is_empty() {
            return Ok(orphaned_region_issues::<YamlDoc>(
                self.name(),
                &ctx.output_dir.join("pnpm-workspace.yaml"),
                &[packages_key()],
                "rwv.yaml declares no npm members, so packages: no longer \
                 belongs to rwv.",
            ));
        }

        let path = ctx.output_dir.join("pnpm-workspace.yaml");

        // ── MISSING ────────────────────────────────────────────────────────
        if !path.exists() {
            return Ok(vec![missing_issue(self.name(), &path)]);
        }

        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {} for verify", path.display()))?;
        let doc = YamlDoc::parse(&text)
            .with_context(|| format!("parsing {} for verify", path.display()))?;

        let owned_keys = [packages_key()];
        let marker_present = doc.has_marker(&owned_keys);
        let owned_key_present = doc.key_present(&packages_key());

        // Compute expected vs on-disk; the shared helper normalizes both
        // (sort + dedup) so overlapping/repeated globs never cause a false
        // DRIFT, then dispatches USER-HELD → DRIFT → CLEAN.
        let expected: Vec<String> = expand_workspace_entries(ctx.workspace_root, repo_paths);
        // Pre-lift pnpm collapsed absent → empty (`unwrap_or_default`) before
        // comparing; preserve that by passing `Some` of the (possibly empty) vec.
        let on_disk = read_pnpm_packages_globs(&path).unwrap_or_default();

        Ok(drift_issues(
            self.name(),
            &path,
            marker_present,
            owned_key_present,
            Some(&on_disk),
            &expected,
            "Cut over manually or add the '# managed by repoweave' marker",
            "on-disk packages: content differs from rwv.yaml config.",
        ))
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
        // pnpm-lock.yaml is fully-rwv-owned (generated by `pnpm install`).
        vec!["pnpm-lock.yaml".to_string()]
    }

    fn managed_files(&self, ctx: &IntegrationContext) -> Vec<String> {
        if ctx.detect_repos_with_manifest("package.json").is_empty() {
            return vec![];
        }
        // pnpm-workspace.yaml is a hybrid file: rwv owns the `packages:`
        // block; the user owns `catalog:`, `overrides:`, etc. It is
        // symlinked (via managed_files) but never gitignored, and
        // deactivate strips the owned region rather than deleting wholesale.
        vec!["pnpm-workspace.yaml".to_string()]
    }
}

impl PnpmWorkspaces {
    /// Remove rwv's `packages:` list from the `pnpm-workspace.yaml` under
    /// `root`, leaving user-authored content untouched.
    ///
    /// Both callers reach this from the same premise — rwv has no `packages:`
    /// list to author — and they differ only in why: the project is going away,
    /// or its npm membership emptied. Marker-gated and idempotent, so it is safe
    /// over an absent file and over one the user holds the pen on.
    fn strip_managed_region(root: &Path) -> anyhow::Result<()> {
        let path = root.join("pnpm-workspace.yaml");
        strip_deactivate::<YamlDoc>(&path, &[packages_key()])
            .with_context(|| format!("pnpm-workspaces: strip {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::IssueKind;
    use crate::manifest::{IntegrationConfig, ProjectName};
    use crate::workspace::ContainerKind;
    use std::collections::HashMap;
    use tempfile::TempDir;

    /// A context over an empty manifest: no repo is declared, so nothing is
    /// detected and the integration has no `packages:` list to author.
    fn ctx_over_no_members<'a>(
        root: &'a Path,
        project: &'a ProjectName,
        config: &'a IntegrationConfig,
        cache: &'a HashMap<String, Vec<String>>,
    ) -> IntegrationContext<'a> {
        IntegrationContext {
            output_dir: root,
            workspace_root: root,
            container_kind: ContainerKind::Primary,
            project,
            repos: vec![],
            config,
            all_repos_on_disk: &[],
            all_project_paths: &[],
            detection_cache: cache,
            workweave: None,
        }
    }

    /// The `packages:` list rwv authored while it had members must not outlive
    /// them: an emptied membership strips it, and reports it until something
    /// does.
    #[test]
    fn emptied_membership_strips_the_marked_packages_list() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let path = root.join("pnpm-workspace.yaml");
        std::fs::write(
            &path,
            "# managed by repoweave\npackages:\n  - github/test/pkg\n",
        )
        .unwrap();

        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = ctx_over_no_members(root, &project, &config, &cache);

        let issues = PnpmWorkspaces.verify(&ctx).unwrap();
        assert_eq!(issues.len(), 1, "expected one finding, got: {issues:?}");
        assert_eq!(issues[0].kind, IssueKind::ManagedFileDrift);
        assert!(issues[0].safe_to_fix, "the strip is the fix");

        PnpmWorkspaces.activate(&ctx).unwrap();
        assert!(
            !path.exists(),
            "nothing user-authored remained, so the file goes: {}",
            std::fs::read_to_string(&path).unwrap_or_default()
        );
    }

    #[test]
    fn emptied_membership_leaves_an_unmarked_list_alone() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let path = root.join("pnpm-workspace.yaml");
        let hand_written = "packages:\n  - github/test/pkg\n";
        std::fs::write(&path, hand_written).unwrap();

        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = ctx_over_no_members(root, &project, &config, &cache);

        assert!(
            PnpmWorkspaces.verify(&ctx).unwrap().is_empty(),
            "with no members there is nothing to cut over to, so nothing to say"
        );
        PnpmWorkspaces.activate(&ctx).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            hand_written,
            "rwv must not strip a list it never marked"
        );
    }

    #[test]
    fn read_pnpm_packages_globs_absent_file_is_none() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            read_pnpm_packages_globs(&tmp.path().join("pnpm-workspace.yaml")),
            None
        );
    }

    #[test]
    fn read_pnpm_packages_globs_unreadable_path_is_none() {
        let tmp = TempDir::new().unwrap();
        // A directory is not readable as file text — read_to_string fails.
        assert_eq!(read_pnpm_packages_globs(tmp.path()), None);
    }

    #[test]
    fn read_pnpm_packages_globs_invalid_yaml_is_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("pnpm-workspace.yaml");
        // Unterminated flow sequence.
        std::fs::write(&path, "packages: [foo, bar\n").unwrap();
        assert_eq!(read_pnpm_packages_globs(&path), None);
    }

    #[test]
    fn read_pnpm_packages_globs_no_packages_key_is_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("pnpm-workspace.yaml");
        std::fs::write(&path, "catalog:\n  react: ^18\n").unwrap();
        assert_eq!(read_pnpm_packages_globs(&path), None);
    }

    #[test]
    fn read_pnpm_packages_globs_block_style() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("pnpm-workspace.yaml");
        std::fs::write(&path, "packages:\n  - packages/*\n  - apps/web\n").unwrap();
        assert_eq!(
            read_pnpm_packages_globs(&path),
            Some(vec!["packages/*".to_string(), "apps/web".to_string()])
        );
    }

    #[test]
    fn read_pnpm_packages_globs_flow_style() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("pnpm-workspace.yaml");
        std::fs::write(&path, "packages: [\"packages/*\", \"apps/web\"]\n").unwrap();
        assert_eq!(
            read_pnpm_packages_globs(&path),
            Some(vec!["packages/*".to_string(), "apps/web".to_string()])
        );
    }

    #[test]
    fn read_pnpm_packages_globs_skips_non_string_entries() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("pnpm-workspace.yaml");
        std::fs::write(&path, "packages:\n  - packages/*\n  - 42\n  - true\n").unwrap();
        assert_eq!(
            read_pnpm_packages_globs(&path),
            Some(vec!["packages/*".to_string()])
        );
    }
}
