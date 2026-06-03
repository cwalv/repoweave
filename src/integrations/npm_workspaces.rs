use crate::integration::{Integration, IntegrationContext, Issue, Severity};
use crate::integrations::merge::{
    keypath, merge_activate, strip_deactivate, JsonDoc, ManagedDoc, OwnedValue, Ownership,
    XRepoweaveMarker,
};
use anyhow::Context;
use std::path::Path;

pub struct NpmWorkspaces;

/// Keys stripped on deactivate.
///
/// Both `workspaces` (array-form) and `workspaces.packages` (object-form) are
/// listed so deactivate handles either shape. `JsonDoc::remove_at` prunes the
/// now-empty `workspaces` parent when only `packages` was set and `nohoist`
/// etc. were removed during the same pass; if other user sibling keys survive,
/// the non-empty `workspaces` parent stays (correct — it is user content).
fn deactivate_owned_keys() -> Vec<Vec<String>> {
    vec![
        keypath(["name"]),
        keypath(["private"]),
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
/// In both cases `name` and `private` are set as a convenience for npm
/// tooling. They are no longer the ownership proof — that role belongs
/// exclusively to the `x-repoweave` marker written by JsonDoc<XRepoweaveMarker>.
fn build_owned(
    existing_pkg: Option<&serde_json::Value>,
    workspaces: Vec<String>,
) -> Vec<(Vec<String>, Ownership, OwnedValue)> {
    let ws_is_object = existing_pkg
        .and_then(|v| v.get("workspaces"))
        .is_some_and(|ws| ws.is_object());

    let name_value = OwnedValue::String("repoweave".to_string());
    let private_value = OwnedValue::Bool(true);
    let ws_value = OwnedValue::sorted_array(workspaces);

    if ws_is_object {
        // Object-form: own only .packages, preserve nohoist and other siblings.
        let mut packages_map = std::collections::BTreeMap::new();
        packages_map.insert("packages".to_string(), ws_value);
        vec![
            (keypath(["name"]), Ownership::Author, name_value),
            (keypath(["private"]), Ownership::Author, private_value),
            (
                keypath(["workspaces"]),
                Ownership::Author,
                OwnedValue::Object(packages_map),
            ),
        ]
    } else {
        // Array-form or absent: set workspaces as a flat sorted array.
        vec![
            (keypath(["name"]), Ownership::Author, name_value),
            (keypath(["private"]), Ownership::Author, private_value),
            (keypath(["workspaces"]), Ownership::Author, ws_value),
        ]
    }
}

/// Return true if the package.json at `path` carries the x-repoweave marker.
fn has_our_marker(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(doc) = JsonDoc::<XRepoweaveMarker>::parse(&text) else {
        return false;
    };
    doc.has_marker(&[])
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

        let path = ctx.output_dir.join("package.json");

        // Read the existing package.json (if any) to detect the workspaces
        // shape before calling merge_activate.
        let existing: Option<serde_json::Value> = path
            .exists()
            .then(|| std::fs::read_to_string(&path).ok())
            .flatten()
            .and_then(|c| serde_json::from_str(&c).ok());

        let owned = build_owned(existing.as_ref(), paths);

        // merge_activate handles: read-or-empty, marker-gated key ownership,
        // set_marker, write back, and preserves all foreign keys untouched.
        // The x-repoweave marker is written by JsonDoc<XRepoweaveMarker>.
        merge_activate::<JsonDoc<XRepoweaveMarker>>(&path, &owned)?;
        Ok(())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        let path = root.join("package.json");

        // Probe the marker BEFORE stripping so we can gate lockfile removal on
        // it. strip_deactivate returns without doing anything when the marker is
        // absent (user holds the pen), so we must capture ownership proof now.
        let we_owned = has_our_marker(&path);

        // strip_deactivate: gates on x-repoweave marker; strips owned keys;
        // prunes empty parents; deletes the file only if nothing else remains.
        let owned_keys = deactivate_owned_keys();
        strip_deactivate::<JsonDoc<XRepoweaveMarker>>(&path, &owned_keys)?;

        // Remove package-lock.json only when we owned the package.json.
        // (§6 npm scenario 4a; fixes the generated_files asymmetry.)
        if we_owned {
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

    fn generated_files(&self, ctx: &IntegrationContext) -> Vec<String> {
        if ctx.detect_repos_with_manifest("package.json").is_empty() {
            return vec![];
        }
        vec!["package.json".to_string(), "package-lock.json".to_string()]
    }
}
