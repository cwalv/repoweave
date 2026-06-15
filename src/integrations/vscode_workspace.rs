use crate::integration::{Integration, IntegrationContext, Issue, Severity};
use crate::integrations::merge::{
    drift_issues, keypath, missing_issue, strip_deactivate, JsonDoc, KeyPath, ManagedDoc,
    RwvGeneratedMarker,
};
use anyhow::Context;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

/// Marker key written into generated `.code-workspace` files so that
/// `deactivate` can distinguish rwv-managed files from user-created ones.
const GENERATED_MARKER_KEY: &str = "rwv.generated";

/// Sub-key within the `rwv.generated` object that records which
/// `settings.files.exclude` keys were set by rwv on the last activation.
///
/// On the next activation this list is read back, those keys are removed, and
/// the new rwv-derived set is inserted — implementing "set-subtract old owned
/// set, union new" without any heuristic pattern matching.
const MARKER_EXCLUDES_KEY: &str = "files.exclude";

/// The owned key paths that vscode-workspace manages.
///
/// Used by `strip_deactivate` (which has no IntegrationContext) to identify
/// which keys to remove. Static per §4.5 of the file-ownership contract.
fn vscode_owned_keys() -> Vec<KeyPath> {
    vec![
        keypath(["folders"]),
        keypath(["settings", "git.autoRepositoryDetection"]),
        keypath(["settings", "git.repositoryScanMaxDepth"]),
        keypath(["settings", "files.exclude"]),
    ]
}

/// Per-integration settings for the vscode-workspace integration.
///
/// Deserialized from the `integrations.vscode-workspace:` block in `rwv.yaml`.
#[derive(serde::Deserialize, Default)]
struct VscodeConfig {
    /// Whether to hide dotfiles (paths starting with `.`) in the VS Code
    /// file explorer. Defaults to `true`.
    #[serde(default = "default_true", rename = "hide-dotfiles")]
    hide_dotfiles: bool,
}

fn default_true() -> bool {
    true
}

/// Collapse a set of excluded repo paths up the directory hierarchy.
///
/// Algorithm (mirrors the Python reference in reporoot v0.3.1):
/// 1. Group all on-disk repos by owner (`registry/owner`, first two path segments).
/// 2. If **all** repos under an owner are excluded → replace them with the owner path.
/// 3. Group owners by registry (first path segment).
/// 4. If **all** owners under a registry are collapsed → replace them with the registry path.
///
/// Returns the collapsed set as a sorted `Vec<String>`.
pub fn collapse_excludes(excluded: &HashSet<String>, all_repos: &[String]) -> Vec<String> {
    // Group all repos by owner (first two segments).
    let mut repos_by_owner: HashMap<String, HashSet<String>> = HashMap::new();
    for repo in all_repos {
        let parts: Vec<&str> = repo.splitn(3, '/').collect();
        if parts.len() >= 2 {
            let owner = format!("{}/{}", parts[0], parts[1]);
            repos_by_owner
                .entry(owner)
                .or_default()
                .insert(repo.clone());
        }
    }

    // Collapse at owner level.
    let mut collapsed: HashSet<String> = HashSet::new();
    let mut collapsed_owners: HashSet<String> = HashSet::new();
    for (owner, repos) in &repos_by_owner {
        if repos.is_subset(excluded) {
            collapsed.insert(owner.clone());
            collapsed_owners.insert(owner.clone());
        } else {
            for repo in repos.intersection(excluded) {
                collapsed.insert(repo.clone());
            }
        }
    }

    // Group owners by registry (first segment).
    let mut owners_by_registry: HashMap<String, HashSet<String>> = HashMap::new();
    for owner in repos_by_owner.keys() {
        let registry = owner.split('/').next().unwrap_or(owner).to_string();
        owners_by_registry
            .entry(registry)
            .or_default()
            .insert(owner.clone());
    }

    // Collapse at registry level.
    for (registry, owners) in &owners_by_registry {
        if owners.is_subset(&collapsed_owners) {
            for owner in owners {
                collapsed.remove(owner);
            }
            collapsed.insert(registry.clone());
        }
    }

    let mut result: Vec<String> = collapsed.into_iter().collect();
    result.sort();
    result
}

/// Read back the list of `files.exclude` keys that rwv set on the last
/// activation, recorded in `obj["rwv.generated"]["files.exclude"]`.
///
/// Returns an empty set if the file is fresh, uses the legacy bool marker
/// form, or has no stored list.
fn read_prev_rwv_excludes(obj: &serde_json::Map<String, serde_json::Value>) -> HashSet<String> {
    match obj.get(GENERATED_MARKER_KEY) {
        Some(serde_json::Value::Object(m)) => {
            if let Some(serde_json::Value::Array(arr)) = m.get(MARKER_EXCLUDES_KEY) {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            } else {
                HashSet::new()
            }
        }
        _ => HashSet::new(),
    }
}

/// Merge the `settings.files.exclude` map for a vscode .code-workspace file.
///
/// Rule (plan §5f): rwv owns only the *derived* exclude keys (dotfiles sentinel,
/// collapsed repo paths, other-project paths). User-added keys survive
/// unchanged across re-activations.
///
/// To avoid false-positive pattern matching, the previously-owned exclude
/// keys are read back from the marker object (`rwv.generated.files.exclude`)
/// and subtracted before the new set is inserted.
///
/// Returns the merged map as a `serde_json::Map` ready for insertion as
/// `settings.files.exclude`.
fn merge_files_exclude(
    existing_obj: &serde_json::Map<String, serde_json::Value>,
    new_rwv_keys: &BTreeMap<String, bool>,
    prev_rwv_keys: &HashSet<String>,
) -> serde_json::Map<String, serde_json::Value> {
    // Collect user-added keys from the existing map: all keys that were NOT
    // in the previously-owned set.  A key in `prev_rwv_keys` is one rwv set
    // last time; removing it now lets stale entries disappear.
    let mut merged: BTreeMap<String, serde_json::Value> = BTreeMap::new();

    if let Some(serde_json::Value::Object(settings)) = existing_obj.get("settings") {
        if let Some(serde_json::Value::Object(fe)) = settings.get("files.exclude") {
            for (k, v) in fe {
                if !prev_rwv_keys.contains(k.as_str()) {
                    // User-added key (or an rwv key from a very old version
                    // that wasn't recorded): carry it forward.
                    merged.insert(k.clone(), v.clone());
                }
                // Keys in prev_rwv_keys are intentionally dropped here; they
                // will be re-added from new_rwv_keys if still applicable.
            }
        }
    }

    // Insert/update rwv-derived keys.
    for (k, v) in new_rwv_keys {
        merged.insert(k.clone(), serde_json::Value::Bool(*v));
    }

    merged.into_iter().collect()
}

/// Merge the `folders` array for a vscode .code-workspace file.
///
/// `folders` is a JSON array of objects, not a map — the generic `OwnedValue`
/// helper handles maps, not object-arrays. This function implements the
/// vscode-specific merge rule:
///
/// - The rwv-owned primary folder is `{"path": ".", "name": "<project> (primary)"}`.
///   It is placed at element 0.  Any existing folder with `"path": "."` is
///   replaced (rwv owns the `.` slot).
/// - All other folder objects (where `"path" != "."`) are user-added and are
///   preserved in their original relative order after element 0.
///
/// Returns a `serde_json::Value::Array`.
fn merge_folders(
    existing_obj: &serde_json::Map<String, serde_json::Value>,
    project_name: &str,
) -> serde_json::Value {
    let primary_name = format!("{} (primary)", project_name);
    let primary = serde_json::json!({ "path": ".", "name": primary_name });

    // Collect user-added folders: entries from the existing array whose "path"
    // is not "." (the rwv-owned primary slot).
    let mut user_folders: Vec<serde_json::Value> = Vec::new();
    if let Some(serde_json::Value::Array(existing)) = existing_obj.get("folders") {
        for folder in existing {
            let path = folder.get("path").and_then(|v| v.as_str()).unwrap_or("");
            if path != "." {
                user_folders.push(folder.clone());
            }
        }
    }

    // Merged array: primary at [0], then user-added folders in their original order.
    let mut folders = vec![primary];
    folders.extend(user_folders);
    serde_json::Value::Array(folders)
}

pub struct VscodeWorkspace;

impl Integration for VscodeWorkspace {
    fn name(&self) -> &str {
        "vscode-workspace"
    }

    fn default_enabled(&self) -> bool {
        true
    }

    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<()> {
        let cfg: VscodeConfig = ctx.config.settings()?;

        let filename = format!("{}.code-workspace", ctx.project.as_str());
        let filepath = ctx.output_dir.join(&filename);

        // Parse the existing file, bailing loudly on malformed content (fix #4).
        // An empty or missing file starts as an empty map.
        let mut obj: serde_json::Map<String, serde_json::Value> = if filepath.exists() {
            let content = std::fs::read_to_string(&filepath)
                .with_context(|| format!("reading {}", filepath.display()))?;
            let text = content.trim();
            if text.is_empty() {
                serde_json::Map::new()
            } else {
                let v: serde_json::Value = serde_json::from_str(&content).with_context(|| {
                    format!(
                        "malformed JSON in {}: fix or delete the file and re-run `rwv activate`",
                        filepath.display()
                    )
                })?;
                match v {
                    serde_json::Value::Object(m) => m,
                    _ => anyhow::bail!("{} must be a JSON object", filepath.display()),
                }
            }
        } else {
            serde_json::Map::new()
        };

        // Read back the previously-recorded rwv-owned exclude keys so we can
        // remove stale entries on this run (set-subtract-old-owned-set).
        let prev_rwv_excludes = read_prev_rwv_excludes(&obj);

        // Merge the `folders` array (fix #2): primary at [0], user folders
        // preserved in their original order.
        let folders_value = merge_folders(&obj, ctx.project.as_str());
        obj.insert("folders".to_string(), folders_value);

        // Compute rwv-derived files.exclude keys.
        let active_repo_set: HashSet<String> = ctx
            .repos
            .iter()
            .map(|(rp, _)| rp.as_str().to_string())
            .collect();

        let all_repos_on_disk: Vec<String> = ctx
            .all_repos_on_disk
            .iter()
            .map(|p| p.as_str().to_string())
            .collect();

        let excluded_repos: HashSet<String> = all_repos_on_disk
            .iter()
            .filter(|r| !active_repo_set.contains(*r))
            .cloned()
            .collect();

        let collapsed = collapse_excludes(&excluded_repos, &all_repos_on_disk);

        let mut rwv_exclude_keys: BTreeMap<String, bool> = BTreeMap::new();

        if cfg.hide_dotfiles {
            rwv_exclude_keys.insert(".*".to_string(), true);
        }

        for path in collapsed {
            rwv_exclude_keys.insert(path, true);
        }

        let active_project = ctx.project.as_str();
        for project_path in ctx.all_project_paths {
            if project_path != active_project {
                rwv_exclude_keys.insert(format!("projects/{}", project_path), true);
            }
        }

        // Compute the merged files.exclude map before mutating `obj` (Rust
        // borrow rules: shared borrow for read, then exclusive for write).
        let merged_exclude = merge_files_exclude(&obj, &rwv_exclude_keys, &prev_rwv_excludes);

        // Update the `rwv.generated` marker to record the new owned exclude
        // key list.  This enables accurate stale-key removal on the next run.
        let rwv_excludes_list: Vec<serde_json::Value> = rwv_exclude_keys
            .keys()
            .map(|k| serde_json::Value::String(k.clone()))
            .collect();
        obj.insert(
            GENERATED_MARKER_KEY.to_string(),
            serde_json::json!({ "managed": true, MARKER_EXCLUDES_KEY: rwv_excludes_list }),
        );

        // Merge settings: DefaultOnly for git.* keys (write only when absent
        // so user-changed values survive re-activate); per-key merge for
        // files.exclude (fix #1).
        let settings = obj
            .entry("settings".to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if let Some(settings_map) = settings.as_object_mut() {
            // DefaultOnly: seed sensible git defaults at greenfield but never
            // overwrite a value the user has explicitly set.
            if !settings_map.contains_key("git.autoRepositoryDetection") {
                settings_map.insert(
                    "git.autoRepositoryDetection".to_string(),
                    serde_json::Value::String("subFolders".to_string()),
                );
            }
            if !settings_map.contains_key("git.repositoryScanMaxDepth") {
                settings_map.insert(
                    "git.repositoryScanMaxDepth".to_string(),
                    serde_json::Value::Number(3.into()),
                );
            }
            settings_map.insert(
                "files.exclude".to_string(),
                serde_json::Value::Object(merged_exclude),
            );
        }

        let content = serde_json::to_string_pretty(&serde_json::Value::Object(obj))? + "\n";
        std::fs::write(&filepath, content)?;
        Ok(())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        // Strip-not-delete (fix #3): for each .code-workspace file that carries
        // the rwv.generated marker, remove only the owned keys and the marker.
        // Delete the file only when nothing user-authored remains; otherwise
        // rewrite the stripped document as a hand-owned workspace.
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("code-workspace") {
                    continue;
                }
                let owned_keys = vscode_owned_keys();
                strip_deactivate::<JsonDoc<RwvGeneratedMarker>>(&path, &owned_keys)?;
            }
        }
        Ok(())
    }

    fn check(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        let filename = format!("{}.code-workspace", ctx.project.as_str());
        let filepath = ctx.output_dir.join(&filename);

        let mut issues = Vec::new();
        if !filepath.is_file() {
            issues.push(Issue {
                integration: self.name().to_string(),
                severity: Severity::Warning,
                message: format!("{} does not exist", filename),
                safe_to_fix: true,
            });
        }
        Ok(issues)
    }

    /// Content-correct check (Axis-2) for `<project>.code-workspace`.
    ///
    /// States mirrored from cargo-workspace:
    ///
    /// - **MISSING** (`safe_to_fix=true`): file absent.
    /// - **Parse-error** (Error): malformed JSON — bail, can't assess drift.
    /// - **USER-HELD** (`safe_to_fix=false`): file present but NO `rwv.generated`
    ///   marker — user created the workspace file; don't auto-clobber.
    /// - **DRIFT** (`safe_to_fix=true`): marker present but `folders[0]` (the rwv
    ///   primary folder entry) doesn't match the expected primary for this project.
    /// - **CLEAN**: marker present and primary folder entry matches.
    ///
    /// Note: `settings.files.exclude` is not checked for drift since its content
    /// depends on the runtime all_repos_on_disk / all_project_paths sets, which
    /// are not reproducible from the manifest alone.
    fn verify(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        let filename = format!("{}.code-workspace", ctx.project.as_str());
        let filepath = ctx.output_dir.join(&filename);

        // ── MISSING ────────────────────────────────────────────────────────
        if !filepath.exists() {
            return Ok(vec![missing_issue(self.name(), &filepath)]);
        }

        let text = std::fs::read_to_string(&filepath)
            .with_context(|| format!("reading {} for verify", filepath.display()))?;
        let doc = JsonDoc::<RwvGeneratedMarker>::parse(&text)
            .with_context(|| format!("parsing {} for verify", filepath.display()))?;
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .with_context(|| format!("parsing {} for verify (serde_json)", filepath.display()))?;

        let marker_present = doc.has_marker(&[]);

        // vscode's USER-HELD is marker-absence alone (file present but no
        // `rwv.generated` marker → user created it). The shared helper treats
        // USER-HELD as `!marker_present && owned_key_present`, so we pass
        // `owned_key_present = true` unconditionally: the file's mere presence
        // is the "owned region" here.
        //
        // DRIFT is the `folders[0]` primary-entry compare (NOT an array of
        // members like the other five). We serialize the on-disk and expected
        // primary tuple to a single deterministic string so the shared
        // array-compare reduces to "primary matches?". `settings.files.exclude`
        // is intentionally NOT checked — its content depends on runtime
        // all_repos_on_disk / all_project_paths sets and is not reproducible
        // from the manifest alone.
        let on_disk_primary = parsed
            .get("folders")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .map(|f| {
                (
                    f.get("path").and_then(|v| v.as_str()).map(str::to_string),
                    f.get("name").and_then(|v| v.as_str()).map(str::to_string),
                )
            });
        let expected_primary = Some((
            Some(".".to_string()),
            Some(format!("{} (primary)", ctx.project.as_str())),
        ));

        let on_disk = vec![format!("{on_disk_primary:?}")];
        let expected = vec![format!("{expected_primary:?}")];

        Ok(drift_issues(
            self.name(),
            &filepath,
            marker_present,
            /* owned_key_present = */ true,
            Some(&on_disk),
            &expected,
            "Cut over manually or add the rwv.generated marker",
            "on-disk folders[0] (primary entry) differs from expected.",
        ))
    }

    fn generated_files(&self, ctx: &IntegrationContext) -> Vec<String> {
        vec![format!("{}.code-workspace", ctx.project.as_str())]
    }
}

// ===========================================================================
// Unit tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn all_repos(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // -----------------------------------------------------------------------
    // collapse_excludes
    // -----------------------------------------------------------------------

    #[test]
    fn collapse_all_repos_under_owner() {
        // All repos under github/acme are excluded → collapse to owner.
        let all = all_repos(&["github/acme/server", "github/acme/web", "github/chatly/api"]);
        let excluded = set(&["github/acme/server", "github/acme/web"]);
        let result = collapse_excludes(&excluded, &all);
        assert_eq!(result, vec!["github/acme"]);
    }

    #[test]
    fn no_collapse_for_mixed_owner() {
        // Only some repos under github/acme are excluded → list individually.
        let all = all_repos(&["github/acme/server", "github/acme/web", "github/acme/docs"]);
        let excluded = set(&["github/acme/server", "github/acme/web"]);
        let result = collapse_excludes(&excluded, &all);
        assert_eq!(result, vec!["github/acme/server", "github/acme/web"]);
    }

    #[test]
    fn collapse_all_owners_under_registry() {
        // All repos under github/ are excluded → collapse to registry.
        let all = all_repos(&["github/acme/server", "github/acme/web", "github/chatly/api"]);
        let excluded = set(&["github/acme/server", "github/acme/web", "github/chatly/api"]);
        let result = collapse_excludes(&excluded, &all);
        assert_eq!(result, vec!["github"]);
    }

    #[test]
    fn collapse_some_owners_but_not_registry() {
        // All repos under github/acme excluded, but github/chatly has an active repo.
        let all = all_repos(&[
            "github/acme/server",
            "github/acme/web",
            "github/chatly/api",
            "github/chatly/frontend",
        ]);
        let excluded = set(&["github/acme/server", "github/acme/web", "github/chatly/api"]);
        let mut result = collapse_excludes(&excluded, &all);
        result.sort();
        assert!(result.contains(&"github/acme".to_string()));
        assert!(result.contains(&"github/chatly/api".to_string()));
        assert!(!result.contains(&"github".to_string()));
    }

    #[test]
    fn empty_excluded_set_returns_empty() {
        let all = all_repos(&["github/acme/server", "github/chatly/api"]);
        let excluded = set(&[]);
        let result = collapse_excludes(&excluded, &all);
        assert!(result.is_empty());
    }

    #[test]
    fn all_repos_empty_returns_empty() {
        let excluded = set(&["github/acme/server"]);
        let result = collapse_excludes(&excluded, &[]);
        assert!(result.is_empty());
    }

    // -----------------------------------------------------------------------
    // read_prev_rwv_excludes and merge_files_exclude
    // -----------------------------------------------------------------------

    #[test]
    fn read_prev_rwv_excludes_legacy_bool_returns_empty() {
        // Old marker shape: `"rwv.generated": true` — no stored list.
        let mut obj = serde_json::Map::new();
        obj.insert(
            GENERATED_MARKER_KEY.to_string(),
            serde_json::Value::Bool(true),
        );
        let prev = read_prev_rwv_excludes(&obj);
        assert!(prev.is_empty(), "legacy bool marker has no stored excludes");
    }

    #[test]
    fn read_prev_rwv_excludes_object_form() {
        // New marker shape: `"rwv.generated": {"managed": true, "files.exclude": [...]}`
        let mut obj = serde_json::Map::new();
        obj.insert(
            GENERATED_MARKER_KEY.to_string(),
            serde_json::json!({ "managed": true, "files.exclude": [".*", "github/acme"] }),
        );
        let prev = read_prev_rwv_excludes(&obj);
        assert!(prev.contains(".*"));
        assert!(prev.contains("github/acme"));
        assert_eq!(prev.len(), 2);
    }

    #[test]
    fn merge_files_exclude_preserves_user_keys_removes_stale() {
        // Existing file has: rwv-owned ".*" + "github/acme" (stale) and
        // user-added "**/target" + "dist".  New run sets ".*" + "github/chatly/api".
        let mut settings_map = serde_json::Map::new();
        settings_map.insert(
            "files.exclude".to_string(),
            serde_json::json!({
                ".*": true,
                "github/acme": true,
                "**/target": true,
                "dist": true
            }),
        );
        let mut obj = serde_json::Map::new();
        obj.insert(
            "settings".to_string(),
            serde_json::Value::Object(settings_map),
        );
        // Mark what rwv owned last time.
        obj.insert(
            GENERATED_MARKER_KEY.to_string(),
            serde_json::json!({ "managed": true, "files.exclude": [".*", "github/acme"] }),
        );

        let prev = read_prev_rwv_excludes(&obj);
        let mut new_keys = BTreeMap::new();
        new_keys.insert(".*".to_string(), true);
        new_keys.insert("github/chatly/api".to_string(), true);

        let merged = merge_files_exclude(&obj, &new_keys, &prev);
        let merged_v = serde_json::Value::Object(merged);

        // Stale rwv key removed.
        assert!(
            merged_v.get("github/acme").is_none(),
            "stale rwv key must be removed"
        );
        // New rwv key added.
        assert_eq!(merged_v[".*"], serde_json::Value::Bool(true));
        assert_eq!(merged_v["github/chatly/api"], serde_json::Value::Bool(true));
        // User keys preserved.
        assert_eq!(merged_v["**/target"], serde_json::Value::Bool(true));
        assert_eq!(merged_v["dist"], serde_json::Value::Bool(true));
    }
}
