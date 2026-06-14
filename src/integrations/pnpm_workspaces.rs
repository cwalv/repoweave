use crate::integration::{Integration, IntegrationContext, Issue, Severity};
use crate::integrations::merge::{
    merge_activate, strip_deactivate, KeyPath, ManagedDoc, OwnedValue, Ownership, YamlDoc,
};
use anyhow::Context;
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
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    let packages = doc.get("packages")?.as_sequence()?;
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

    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        let paths = ctx.detect_repos_with_manifest("package.json");
        if paths.is_empty() {
            return Ok(());
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
        let path = root.join("pnpm-workspace.yaml");

        // Marker gate: strip_deactivate leaves the file alone if the marker
        // is absent (user took the pen). Deletes only if empty after strip.
        let owned_keys = [packages_key()];
        strip_deactivate::<YamlDoc>(&path, &owned_keys)
            .with_context(|| format!("pnpm-workspaces: deactivate {}", path.display()))?;

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
            return Ok(vec![]);
        }

        let path = ctx.output_dir.join("pnpm-workspace.yaml");

        // ── MISSING ────────────────────────────────────────────────────────
        if !path.exists() {
            return Ok(vec![Issue {
                integration: self.name().to_string(),
                severity: Severity::Warning,
                message: format!(
                    "pnpm-workspaces managed file missing: {}; run rwv doctor --fix to regenerate",
                    path.display()
                ),
                safe_to_fix: true,
            }]);
        }

        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {} for verify", path.display()))?;
        let doc = YamlDoc::parse(&text)
            .with_context(|| format!("parsing {} for verify", path.display()))?;

        let owned_keys = [packages_key()];
        let marker_present = doc.has_marker(&owned_keys);

        // ── USER-HELD ──────────────────────────────────────────────────────
        if !marker_present && doc.key_present(&packages_key()) {
            return Ok(vec![Issue {
                integration: self.name().to_string(),
                severity: Severity::Warning,
                message: format!(
                    "pnpm-workspaces managed file present but unmarked: {}; \
                     rwv will NOT auto-take-over (would discard user content). \
                     Cut over manually or add the '# managed by repoweave' marker",
                    path.display()
                ),
                safe_to_fix: false,
            }]);
        }

        // ── DRIFT ──────────────────────────────────────────────────────────
        // Regenerate what activate() would produce and compare.
        let expanded = expand_workspace_entries(ctx.workspace_root, repo_paths);
        let mut expected: Vec<String> = expanded.into_iter().map(|p| p.to_string()).collect();
        expected.sort();

        // Read on-disk packages from the YAML text (reuse existing helper).
        let on_disk = read_pnpm_packages_globs(&path).unwrap_or_default();

        if on_disk != expected {
            return Ok(vec![Issue {
                integration: self.name().to_string(),
                severity: Severity::Warning,
                message: format!(
                    "pnpm-workspaces managed file has drift: {}; \
                     on-disk packages: content differs from rwv.yaml config. \
                     Run rwv doctor --fix to regenerate",
                    path.display()
                ),
                safe_to_fix: true,
            }]);
        }

        // ── CLEAN ──────────────────────────────────────────────────────────
        Ok(vec![])
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
