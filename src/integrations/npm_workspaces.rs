use crate::integration::{Integration, IntegrationContext, Issue, Severity};
use crate::integrations::merge::{
    drift_issues, keypath, merge_activate, missing_issue, strip_deactivate, JsonDoc, ManagedDoc,
    OwnedValue, Ownership, StripOutcome, XRepoweaveMarker,
};
use anyhow::Context;
use std::path::Path;

pub struct NpmWorkspaces;

/// Keys stripped on deactivate.
///
/// Only `Ownership::Author` keys are listed here. `name` and `private` are
/// `Ownership::DefaultOnly` — they are never stripped on deactivate (the user
/// may have intentionally adjusted them). Both `workspaces` (array-form) and
/// `workspaces.packages` (object-form) are listed so deactivate handles either
/// shape. `JsonDoc::remove_at` prunes the now-empty `workspaces` parent when
/// only `packages` was set and `nohoist` etc. were removed during the same
/// pass; if other user sibling keys survive, the non-empty `workspaces` parent
/// stays (correct — it is user content).
fn deactivate_owned_keys() -> Vec<Vec<String>> {
    vec![
        keypath(["workspaces", "packages"]), // object-form: sub-key; prunes parent if now empty
        keypath(["workspaces"]),             // array-form: the whole key
    ]
}

/// Build owned key/value pairs for activate, respecting the on-disk shape.
///
/// Object-form `workspaces` (e.g. `{packages: [...], nohoist: [...]}`):
///   - Own only `workspaces.packages` via `OwnedValue::Object`, which merges
///     into the existing map so `nohoist` and other siblings survive.
///
/// Array-form or absent:
///   - Own `workspaces` directly as a flat sorted array.
///
/// `name` and `private` are `Ownership::DefaultOnly`: rwv sets them on a fresh
/// file but never overwrites an existing value. This means:
/// - Greenfield: `name` is set to the project name from context, `private` is
///   set to `true`.
/// - Existing file (user or prior rwv): values are preserved as-is.
/// - Deactivate: `name` and `private` are NOT stripped (user-adjustable).
///
/// `workspaces` / `workspaces.packages` remains `Ownership::Author` — rwv
/// always owns the workspace membership list.
fn build_owned(
    existing_pkg: Option<&serde_json::Value>,
    workspaces: Vec<String>,
    project_name: &str,
) -> Vec<(Vec<String>, Ownership, OwnedValue)> {
    let ws_is_object = existing_pkg
        .and_then(|v| v.get("workspaces"))
        .is_some_and(|ws| ws.is_object());

    let name_value = OwnedValue::String(project_name.to_string());
    let private_value = OwnedValue::Bool(true);
    let ws_value = OwnedValue::sorted_array(workspaces);

    if ws_is_object {
        // Object-form: own only .packages, preserve nohoist and other siblings.
        let mut packages_map = std::collections::BTreeMap::new();
        packages_map.insert("packages".to_string(), ws_value);
        vec![
            (keypath(["name"]), Ownership::DefaultOnly, name_value),
            (keypath(["private"]), Ownership::DefaultOnly, private_value),
            (
                keypath(["workspaces"]),
                Ownership::Author,
                OwnedValue::Object(packages_map),
            ),
        ]
    } else {
        // Array-form or absent: set workspaces as a flat sorted array.
        vec![
            (keypath(["name"]), Ownership::DefaultOnly, name_value),
            (keypath(["private"]), Ownership::DefaultOnly, private_value),
            (keypath(["workspaces"]), Ownership::Author, ws_value),
        ]
    }
}

/// Expand detected repos into workspace entries.
///
/// A repo whose root package.json declares its own `workspaces` (array form,
/// or object form `.packages`) is a multi-package repo. npm does not support
/// nested workspaces, so listing the repo root would orphan its sub-packages
/// from the weave-root install/link graph. Instead, emit `<repo-path>/<glob>`
/// for each member glob. Repos without a `workspaces` key keep the single
/// `<repo-path>` entry (existing behavior).
fn expand_workspace_entries(workspace_root: &Path, repo_paths: Vec<String>) -> Vec<String> {
    let mut entries = Vec::new();
    for repo in repo_paths {
        let pkg_path = workspace_root.join(&repo).join("package.json");
        let globs = std::fs::read_to_string(&pkg_path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|pkg| member_globs(pkg.get("workspaces")));
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

/// Member globs from a package.json `workspaces` value: array form, or
/// object form `{packages: [...]}`. Non-string members are skipped.
fn member_globs(ws: Option<&serde_json::Value>) -> Option<Vec<String>> {
    let arr = match ws? {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(o) => o.get("packages")?.as_array()?,
        _ => return None,
    };
    Some(
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
    )
}

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
        let paths = expand_workspace_entries(ctx.workspace_root, paths);

        let path = ctx.output_dir.join("package.json");

        // Read the existing package.json (if any) to detect the workspaces
        // shape before calling merge_activate.
        let existing: Option<serde_json::Value> = path
            .exists()
            .then(|| std::fs::read_to_string(&path).ok())
            .flatten()
            .and_then(|c| serde_json::from_str(&c).ok());

        let owned = build_owned(existing.as_ref(), paths, ctx.project.as_str());

        // merge_activate handles: read-or-empty, marker-gated key ownership,
        // set_marker, write back, and preserves all foreign keys untouched.
        // The x-repoweave marker is written by JsonDoc<XRepoweaveMarker>.
        merge_activate::<JsonDoc<XRepoweaveMarker>>(&path, &owned)?;
        Ok(())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        let path = root.join("package.json");

        // strip_deactivate: gates on x-repoweave marker; strips owned keys;
        // prunes empty parents; deletes the file only if nothing else remains.
        let owned_keys = deactivate_owned_keys();
        let outcome = strip_deactivate::<JsonDoc<XRepoweaveMarker>>(&path, &owned_keys)?;

        // Remove package-lock.json only when we owned package.json.
        if outcome == StripOutcome::Stripped {
            let lock_path = root.join("package-lock.json");
            if lock_path.exists() {
                std::fs::remove_file(&lock_path)
                    .with_context(|| format!("removing {}", lock_path.display()))?;
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
                safe_to_fix: true,
            });
        }
        Ok(issues)
    }

    /// Content-correct check (Axis-2) for `package.json`.
    ///
    /// States mirrored from cargo-workspace:
    ///
    /// - **MISSING** (`safe_to_fix=true`): `package.json` absent but repos detected.
    /// - **Parse-error** (Error): malformed JSON — can't assess drift; bail.
    /// - **USER-HELD** (`safe_to_fix=false`): file present, has `workspaces` key,
    ///   but NO `x-repoweave` marker.
    /// - **DRIFT** (`safe_to_fix=true`): marker present, `workspaces` content
    ///   diverges from what the current config would generate.
    /// - **CLEAN**: marker present and content matches.
    fn verify(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        let repo_paths = ctx.detect_repos_with_manifest("package.json");
        if repo_paths.is_empty() {
            return Ok(vec![]);
        }

        let path = ctx.output_dir.join("package.json");

        // ── MISSING ────────────────────────────────────────────────────────
        if !path.exists() {
            return Ok(vec![missing_issue(self.name(), &path)]);
        }

        // Parse the on-disk file.
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {} for verify", path.display()))?;
        let doc = JsonDoc::<XRepoweaveMarker>::parse(&text)
            .with_context(|| format!("parsing {} for verify", path.display()))?;
        let pkg: serde_json::Value = serde_json::from_str(&text)
            .with_context(|| format!("parsing {} for verify (serde_json)", path.display()))?;

        let marker_present = doc.has_marker(&[]);

        // Locate owned key + compute expected vs on-disk, then defer the
        // four-state dispatch (USER-HELD → DRIFT → CLEAN) to the shared helper.
        let owned_key_present = pkg.get("workspaces").is_some();
        let expected = expand_workspace_entries(ctx.workspace_root, repo_paths);
        // `None` (no `workspaces` key) is distinct from present-but-empty:
        // an absent key is always DRIFT — preserves the pre-lift Option compare.
        let on_disk = member_globs(pkg.get("workspaces"));

        Ok(drift_issues(
            self.name(),
            &path,
            marker_present,
            owned_key_present,
            on_disk.as_deref(),
            &expected,
            "Cut over manually or add the x-repoweave marker",
            "on-disk workspaces content differs from rwv.yaml config.",
        ))
    }

    fn activate_hook(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        let paths = ctx.detect_repos_with_manifest("package.json");
        if paths.is_empty() {
            return Ok(());
        }

        // Full `npm install` (not `--package-lock-only`): activation is
        // the moment the workspace's membership becomes current, so
        // `node_modules` should be in sync.
        //
        // Run from workspace_root: the symlink at the root points at the
        // canonical package.json in output_dir, and workspace member
        // paths are resolved relative to the symlink location.
        let status = std::process::Command::new("npm")
            .args(["install"])
            .current_dir(ctx.workspace_root)
            .status()
            .context("failed to run npm")?;

        if !status.success() {
            anyhow::bail!("npm install failed (exit {})", status);
        }

        Ok(())
    }

    /// `package-lock.json` is fully-owned — gitignore-eligible, whole-deletable.
    fn generated_files(&self, ctx: &IntegrationContext) -> Vec<String> {
        if ctx.detect_repos_with_manifest("package.json").is_empty() {
            return vec![];
        }
        vec!["package-lock.json".to_string()]
    }

    /// `package.json` is **hybrid** (rwv owns `workspaces`/`workspaces.packages`
    /// plus the `name`/`private` `DefaultOnly` seeds inside a user-authored
    /// file). It MUST NOT appear in `generated_files()` — that would mark it
    /// gitignore-eligible and whole-deletable, discarding the user's
    /// dependencies and scripts.
    fn managed_files(&self, ctx: &IntegrationContext) -> Vec<String> {
        let mut files = self.generated_files(ctx);
        if !ctx.detect_repos_with_manifest("package.json").is_empty() {
            files.push("package.json".to_string());
        }
        files
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{IntegrationConfig, Manifest, ProjectName, Role};
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn make_manifest(repos: &[(&str, Role)]) -> Manifest {
        let mut yaml = String::from("repositories:\n");
        for (path, role) in repos {
            let last = path.split('/').next_back().unwrap();
            yaml.push_str(&format!(
                "  {path}:\n    type: git\n    url: https://github.com/test/{last}.git\n    version: main\n    role: {}\n",
                role.as_str()
            ));
        }
        Manifest::from_yaml_str(&yaml).unwrap()
    }

    fn make_ctx<'a>(
        root: &'a Path,
        project: &'a ProjectName,
        manifest: &'a Manifest,
        config: &'a IntegrationConfig,
        cache: &'a HashMap<String, Vec<String>>,
    ) -> IntegrationContext<'a> {
        IntegrationContext {
            output_dir: root,
            workspace_root: root,
            project,
            repos: manifest
                .iter_entries()
                .map(|(rp, e)| (rp.clone(), e.clone()))
                .collect(),
            config,
            all_repos_on_disk: &[],
            all_project_paths: &[],
            detection_cache: cache,
            workweave: None,
        }
    }

    fn touch(root: &Path, rel: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    /// Regression for the managed/generated split: `package.json` is hybrid
    /// (rwv owns `workspaces` inside a user-authored file) while
    /// `package-lock.json` is fully-owned. If `managed_files()` ever silently
    /// reverted to the `Integration` trait's default (`generated_files(ctx)`),
    /// `package.json` would go back to being gitignore-eligible and
    /// whole-deletable — the exact loss path this split closes.
    #[test]
    fn managed_files_includes_hybrid_package_json_not_just_the_lockfile() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        touch(root, "github/test/pkg/package.json");
        let manifest = make_manifest(&[("github/test/pkg", Role::Owned)]);
        let project = ProjectName::new("test-project").unwrap();
        let config = IntegrationConfig::default();
        let cache = HashMap::new();
        let ctx = make_ctx(root, &project, &manifest, &config, &cache);

        let integration = NpmWorkspaces;
        let generated = integration.generated_files(&ctx);
        let managed = integration.managed_files(&ctx);

        assert_eq!(generated, vec!["package-lock.json".to_string()]);
        assert!(
            managed.contains(&"package.json".to_string()),
            "managed_files must include the hybrid package.json: {managed:?}"
        );
        assert!(
            !generated.contains(&"package.json".to_string()),
            "package.json must never be gitignore/whole-delete eligible: {generated:?}"
        );
    }

    /// StripOutcome regression: deactivate must gate `package-lock.json`
    /// removal on whether the strip actually happened (marker present), not
    /// merely on having called `strip_deactivate`. A hand-authored,
    /// unmarked `package.json` means the user holds the pen — the
    /// co-requisite lockfile must survive.
    #[test]
    fn deactivate_leaves_lockfile_when_user_holds_the_pen() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("package.json"),
            r#"{"name": "hand-authored", "workspaces": ["a", "b"]}"#,
        )
        .unwrap();
        std::fs::write(root.join("package-lock.json"), "{}").unwrap();

        NpmWorkspaces.deactivate(root).unwrap();

        assert!(
            root.join("package-lock.json").exists(),
            "must not remove a co-requisite lockfile for a package.json the user holds the pen on"
        );
        assert!(root.join("package.json").exists());
    }

    /// The other side of the same regression: once rwv's ownership is
    /// confirmed (marker present, strip actually runs), the co-requisite
    /// lockfile IS removed.
    #[test]
    fn deactivate_removes_lockfile_once_rwv_owned_package_json() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("package.json"),
            r#"{"workspaces": ["a"], "x-repoweave": {"managed": true}}"#,
        )
        .unwrap();
        std::fs::write(root.join("package-lock.json"), "{}").unwrap();

        NpmWorkspaces.deactivate(root).unwrap();

        assert!(
            !root.join("package-lock.json").exists(),
            "must remove the co-requisite lockfile once the strip confirmed rwv's ownership"
        );
    }
}
