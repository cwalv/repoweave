use crate::integration::{Integration, IntegrationContext, Issue, Severity};
use crate::integrations::merge::{
    keypath, merge_activate, strip_deactivate, JsonDoc, ManagedDoc, OwnedValue, Ownership,
    XRepoweaveMarker,
};
use anyhow::Context;
use std::path::Path;

/// Extract the on-disk `workspaces` array from a parsed `package.json` value.
///
/// Handles both array-form (`workspaces: [...]`) and object-form
/// (`workspaces: {packages: [...]}`).  Returns `None` if the key is absent.
fn workspaces_array(pkg: &serde_json::Value) -> Option<Vec<String>> {
    let ws = pkg.get("workspaces")?;
    let arr = match ws {
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
            return Ok(vec![Issue {
                integration: self.name().to_string(),
                severity: Severity::Warning,
                message: format!(
                    "npm-workspaces managed file missing: {}; run rwv doctor --fix to regenerate",
                    path.display()
                ),
                safe_to_fix: true,
            }]);
        }

        // Parse the on-disk file.
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {} for verify", path.display()))?;
        let doc = JsonDoc::<XRepoweaveMarker>::parse(&text)
            .with_context(|| format!("parsing {} for verify", path.display()))?;
        let pkg: serde_json::Value = serde_json::from_str(&text)
            .with_context(|| format!("parsing {} for verify (serde_json)", path.display()))?;

        let marker_present = doc.has_marker(&[]);

        // ── USER-HELD ──────────────────────────────────────────────────────
        // File has `workspaces` key but no x-repoweave marker.
        if !marker_present && pkg.get("workspaces").is_some() {
            return Ok(vec![Issue {
                integration: self.name().to_string(),
                severity: Severity::Warning,
                message: format!(
                    "npm-workspaces managed file present but unmarked: {}; \
                     rwv will NOT auto-take-over (would discard user content). \
                     Cut over manually or add the x-repoweave marker",
                    path.display()
                ),
                safe_to_fix: false,
            }]);
        }

        // ── DRIFT ──────────────────────────────────────────────────────────
        // Regenerate what activate() would produce and compare.
        let expected_paths = expand_workspace_entries(ctx.workspace_root, repo_paths);
        // OwnedValue::sorted_array sorts and dedupes — mirror that here.
        let expected_ws: Vec<String> = {
            let mut sorted = expected_paths;
            sorted.sort();
            sorted.dedup();
            sorted
        };

        let on_disk_ws = workspaces_array(&pkg);

        let drift = on_disk_ws.as_deref() != Some(expected_ws.as_slice());

        if drift {
            return Ok(vec![Issue {
                integration: self.name().to_string(),
                severity: Severity::Warning,
                message: format!(
                    "npm-workspaces managed file has drift: {}; \
                     on-disk workspaces content differs from rwv.yaml config. \
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
